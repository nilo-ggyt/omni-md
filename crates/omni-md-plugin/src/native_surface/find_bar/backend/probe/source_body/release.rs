//! 调用方原请求和提交错误跨关闭保留，全部业务资源归零后才归还这些弱票。

use super::*;
use crate::native_surface::{
    paged_body::NativeMarkdownPagedBodyError,
    paged_input::NativeMarkdownPagedInput,
    paged_text_view::{NativeMarkdownPagedCloseFailure, NativeMarkdownPagedCloseReceipt},
    viewport_pixels::NativeMarkdownViewportPixelObserver,
};
use objc2_quartz_core::CATransaction;
use omni_markdown_core::MarkdownDocumentBufferError;

/// 只供本原Source回收EIO夹具消费重复报告；调用方仍须先证同一backing首errno及真实绑定来源。
/// 容器内每个叶都必须是原来的精确EIO形态；任一Worker、字形I/O、Poisoned或其他次因都失败。
pub(super) fn injected_source_eio_only(error: &NativeMarkdownPagedBodyError) -> bool {
    use crate::native_surface::paged_font_fallback::NativeMarkdownFontFallbackError as Trial;

    fn trial_source_eio_only(error: &Trial) -> bool {
        match error {
            Trial::Source(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno)) => {
                *errno == libc::EIO
            }
            Trial::TrialCleanup { cause, cleanup } => {
                trial_source_eio_only(cause) && trial_source_eio_only(cleanup)
            } // 同一次退休可再次报告原Source；双侧均须精确通过，不能因链首EIO就忽略真实清理错误。
            _ => false,
        }
    }

    match error {
        NativeMarkdownPagedBodyError::View(ViewError::Document(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
        )) => *errno == libc::EIO,
        NativeMarkdownPagedBodyError::Fallback(error) => trial_source_eio_only(error),
        NativeMarkdownPagedBodyError::PreparationCleanup { cause, cleanup } => {
            injected_source_eio_only(cause) && injected_source_eio_only(cleanup)
        } // Body只组合本次原准备/退休错误；不接受“含任意Source”的一般判断或字符串匹配。
        _ => false,
    }
}

/// 原release事件一次交付；父窗口仍活时就核像素，不靠随后close窗口或外层pool退场通过。
fn wait_close(receipt: &Rc<NativeMarkdownPagedCloseReceipt>) -> Result<(), String> {
    let arrived = Rc::new(Cell::new(false));
    let observed = Rc::clone(&arrived);
    let run_loop = CFRunLoop::current().ok_or("关闭资源缺少原主runloop")?;
    receipt.when_released(move || {
        observed.set(true);
        run_loop.stop(); // 只响应本次真实最后资源relay；回调不保Rig、View、Doc或Frame。
    });
    // SAFETY: 原default mode只交付一次有限3秒；超时仍走原Rig清理并报告失败，不轮询资源。
    unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 3.0, false) };
    if !arrived.get() {
        return Err("原正文关闭在3秒内没有真实像素归还事件".into());
    }
    receipt.verify_resources_released()
}

pub(super) struct Retirement {
    cohort: crate::native_surface::deallocation::NativeMarkdownDeallocationCohort,
    close_started: Instant,
    native: Vec<(String, NativeWeak<AnyObject>)>,
    ledger: crate::native_surface::resource_ledger::NativeMarkdownOwnedSnapshot,
    borrowed: crate::native_surface::NativeMarkdownBorrowedEditorSnapshot,
    document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    input: std::rc::Weak<NativeMarkdownPagedInput>,
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    backing: omni_markdown_core::MarkdownOriginalBackingObserver,
    pixels: NativeMarkdownViewportPixelObserver,
    requests: Vec<Rc<NativeMarkdownPagedFindRevealRequest>>,
    errors: Vec<NativeMarkdownFindExecutionError>,
    receipts: Vec<NativeMarkdownPagedBodyResourceReceipts>,
    retired: Vec<NativeMarkdownPagedBodyPreparationResources>,
    close_error: Option<String>,
}

fn native_weak(value: &AnyObject) -> NativeWeak<AnyObject> {
    NativeWeak::from(value)
}

/// 仅含固定“目标”的长文/导航夹具调用；纯close不猜正文，也不替RootLeaf或Cell制造Find。
pub(super) fn hold_target_fixture_pending(rig: &mut Rig) -> Result<(), String> {
    if rig.view.session_is_current() {
        // 不运行队列，原未绑定pending票由Rig保留至Retirement资源门之后；关闭必须真实撤销它。
        rig.bar.query.setStringValue(ns_string!("目标"));
        let request = cases::pending(rig, MarkdownFindAction::Next)?;
        if !request.pending() || rig.body.probe_bound_find_source().is_some() {
            return Err("关闭控制没有真实未绑定的在途Find".into());
        }
    }
    Ok(())
}

pub(super) fn close(rig: Rig, failed: bool) -> Result<Retirement, String> {
    let close_started = Instant::now();
    let mut close_error = None;
    let mut native = rig
        .body
        .native_weaks()
        .map_err(|cause| cause.to_string())?
        .into_iter()
        .map(|(role, weak)| (role.to_owned(), weak))
        .collect::<Vec<_>>();
    native.push(("find-panel".into(), native_weak(&rig.panel)));
    native.push(("find-controller".into(), native_weak(&rig.controller)));
    let input = rig.body.probe_find_input_weak();
    let weak_bar = Rc::downgrade(&rig.bar);
    let pixels = rig.view.pixel_observer().ok_or("真实Body没有像素观察者")?;
    let mut ledger = rig.ledger.snapshot();
    ledger.seal(); // 在第一个Native拆图前固定全部Find字段、按钮和实际parent角色。
    let mut cohort =
        crate::native_surface::deallocation::NativeMarkdownDeallocationCohort::new(rig.panel.mtm());
    // 查找真实输入会按需创建会话编辑器；像素归还不代表它的系统回调也已结束。
    // 登记同一显式角色账本的销毁事件，不允许靠关闭后的队列轮次猜测它已释放。
    ledger.track_remaining(&mut cohort, rig.panel.mtm());
    let accepted_close = if failed {
        if rig.backing.first_errno() != Some(libc::EIO) {
            return Err("故障关闭缺少原Source的真实EIO首错".into());
        }
        // 仅已验证的不可用Source控制先封闭原Input；否则正常辞任会再次读坏Source而拒绝。
        // 原关闭收据仍保留同一EIO及全部释放义务，不把它伪装成正常编辑/保存成功。
        Some(rig.body.begin_close())
    } else if rig.cell.as_ref().is_some_and(cell::Original::has_ended) {
        // 原Cell已提交/取消，Input不再是可编辑会话；消费原关闭收据后再断firstResponder。
        // 此处不推进队列，也不把仍活草稿或普通正文的辞任失败解释为已接受关闭。
        Some(rig.body.begin_close())
    } else {
        None // 正常笔记继续先允许真实输入辞任；拒绝时不得提前销毁可编辑会话。
    };
    if !rig.panel.makeFirstResponder(None) {
        return Err("原Find字段没有正常辞任，不能开始关闭".into());
    }
    let borrowed = rig.controller.ivars().borrowed.borrow().clone();
    let fields = rig.bar.fields();
    borrowed.finish_accepted(
        &rig.panel,
        &[&fields[0], &fields[1]],
        &rig.controller.ivars().undo,
        &rig.controller.ivars().field_editor_provider,
        true,
    );
    drop(fields); // 字段临时claim在后端释放和最终窗口销毁前归还，不能使严格Native弱图保活。
    rig.bar.finish_accepted();
    let closed = match accepted_close {
        Some(closed) => closed, // 故障或已结束Cell只消费先前同一关闭操作，不另领收据或刷新首错。
        None => rig.body.begin_close(),
    };
    let released = match &closed {
        Ok(receipt) => wait_close(receipt).and_then(|()| {
            if !failed {
                return receipt.verify_released();
            }
            if rig.backing.first_errno() != Some(libc::EIO) {
                return Err("原Source EIO关闭夹具没有保留同一原始EIO首错".into());
            }
            // 原Find取消和真实字体退休可重复观察同一原Source；只消费精确EIO叶组成的链。
            // 每条实际资源归还仍由同一收据和后续原observer严格核验，其他typed清理次因不能通过。
            match receipt.cleanup_failure() {
                None => Ok(()), // 没有在途Find时可没有额外清理错误，原Source失败仍由下方完整核验。
                Some(NativeMarkdownPagedCloseFailure::Body(cause))
                    if injected_source_eio_only(cause) =>
                {
                    Ok(())
                }
                failure => Err(format!("原Source EIO关闭出现非预期清理错误：{failure:?}")),
            }
        }),
        Err(error) => Err(error.to_string()),
    }; // 已接受只代表同步拆图；同份receipt事件和严格zero通过后才能报告关闭完成。
    if let Some(cell) = &rig.cell
        && let Err(cause) = cell.verify_closed()
    {
        close_error = Some(cause); // 此时显式parent Doc仍在，不能靠整个Rig drop掩盖Cell读取者残留。
    }
    rig.scroll.setDocumentView(None);
    rig.scroll.removeFromSuperview();
    rig.panel.setContentView(None);
    rig.panel.orderOut(None);
    // 原controller仍由Rig持有，输入已沿正式accepted边界清空；结束本夹具的公开窗口借用边。
    rig.panel.setDelegate(None);
    rig.panel.close();
    let all_terminal = rig.requests.iter().all(|request| !request.pending());
    let Rig {
        body,
        view,
        panel,
        parent,
        scroll,
        bar,
        controller,
        lease,
        ledger: owner_ledger,
        document,
        backing,
        cell,
        _file: file,
        requests,
        errors,
        receipts,
        retired,
    } = rig;
    drop((
        view,
        body,
        scroll,
        parent,
        bar,
        controller,
        panel,
        lease,
        owner_ledger,
        cell,
        file,
    ));
    CATransaction::flush();
    cohort.seal(); // 外层autoreleasepool退场后才等待最后一个真实Native销毁栈。
    if !all_terminal || released.is_err() {
        close_error = Some(format!("真实Find关闭未完成原意图撤权/释放：{released:?}"));
    }
    Ok(Retirement {
        cohort,
        close_started,
        native,
        ledger,
        borrowed,
        document,
        input,
        bar: weak_bar,
        backing,
        pixels,
        requests,
        errors,
        receipts,
        retired,
        close_error,
    })
}

impl Retirement {
    pub(super) fn verify(self, failed: bool) -> Result<(), String> {
        let arrived = Rc::new(Cell::new(false));
        let observed = Rc::clone(&arrived);
        let run_loop = CFRunLoop::current().ok_or("原Native关闭缺少主runloop")?;
        self.cohort.when_released(move || {
            observed.set(true);
            run_loop.stop(); // 只响应原账本的最后实际销毁事件，不携带字段或文档强引用。
        });
        if let Some(remaining) = Duration::from_secs(3).checked_sub(self.close_started.elapsed()) {
            // SAFETY: 原关闭共用一次三秒预算；只在外层pool退场后交付原cohort事件，不轮询Weak。
            unsafe {
                CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), false)
            };
        }
        if !arrived.get() || !self.cohort.has_completed() {
            return Err("原Find Native角色在关闭三秒内没有真实销毁事件".into());
        }
        metrics::sample("after_close")?; // 原autoreleasepool已退出，分类只能描述当前实际内核状态。
        if !self.borrowed.is_detached() {
            return Err("原Find字段的实际输入数据或历史没有清空".into());
        }
        if self.document.upgrade().is_some()
            || self.input.upgrade().is_some()
            || self.bar.upgrade().is_some()
            || !self.backing.unmap_succeeded()
            || self.backing.mapped_bytes() != 0
            || !self.pixels.is_released()
            || self.requests.iter().any(|request| request.pending())
            || self.backing.failed() != failed
            || self.backing.first_errno() != failed.then_some(libc::EIO)
        {
            return Err("原Find caller仍持票时Doc/Input/Bar/源映射或像素未实际释放".into());
        }
        for (role, weak) in &self.native {
            if weak.load().is_some() {
                return Err(format!("原Find Native角色未释放：{role}"));
            }
        }
        self.ledger.verify_released()?;
        for receipt in &self.receipts {
            if !receipt.source.payload_released() {
                return Err("原Find Source仍被请求或提交错误保活".into());
            }
            // 本组原Find动作均已交付完整帧，不能把缺少Reader解释成提前释放成功。
            if receipt.reader.is_none() {
                return Err("原Find完整帧缺少实际Reader释放收据".into());
            }
            for observer in receipt
                .reader
                .iter()
                .chain(std::iter::once(&receipt.writer))
            {
                let actual = observer.snapshot();
                if actual.file_open
                    || actual.mapped_bytes != 0
                    || actual.cleanup_failed
                    || actual.reader_failed
                    || !actual.source_payload_released
                    || actual.first_source_errno != failed.then_some(libc::EIO)
                {
                    return Err(format!("原Find Reader/Writer资源未归还：{actual:?}"));
                }
            }
            let worker = receipt.worker.snapshot().ok_or("原worker观察者已poison")?;
            if worker.first_source_errno != failed.then_some(libc::EIO) {
                return Err(format!("原Find worker没有保持原Source首errno：{worker:?}"));
            }
            // 仅已精确认证Some(EIO)的本次注入允许区分资源归还与Source健康；不改原快照。
            let released = if failed {
                worker.actual_resources_released()
            } else {
                worker.is_released()
            };
            if !released {
                return Err(format!("原Find完整worker原收据未通过：{worker:?}"));
            }
            for (bytes, payload) in &receipt.fonts {
                if !payload.payload_released()
                    || !bytes.unmap_succeeded()
                    || bytes.mapped_bytes() != 0
                    || bytes.failed()
                {
                    return Err("原Find实际字体/字节映射未归还".into());
                }
            }
            // 同次正文收据也保原可见/在途格的真实弱观察，不能只核主Reader而漏过派生显示目录。
            // Cell仍用其实际原worker与FD事实，没有额外cache计数、资源账本或释放轮询。
            for cell in &receipt.cells {
                cell.verify_released()?;
            }
        }
        for retired in &self.retired {
            let writer = retired.writer.snapshot();
            let worker = retired
                .worker
                .snapshot()
                .ok_or("原撤权worker观察者poison")?;
            if worker.first_source_errno != failed.then_some(libc::EIO) {
                return Err(format!("原撤权worker没有保持原Source首errno：{worker:?}"));
            }
            let worker_released = if failed {
                worker.actual_resources_released()
            } else {
                worker.is_released()
            }; // 同一原Source的撤权worker也必须逐角色真实归零，不能凭取消状态过门。
            if !retired.source.payload_released()
                || writer.file_open
                || writer.mapped_bytes != 0
                || writer.cleanup_failed
                || writer.reader_failed
                || !writer.source_payload_released
                || writer.first_source_errno != failed.then_some(libc::EIO)
                || !worker_released
            {
                return Err(format!(
                    "原撤权Source/Writer/worker未完整释放：writer={writer:?}, worker={worker:?}"
                ));
            }
        }
        println!(
            "native_body_find release_actual_native_roles={} held_original_requests={} held_committed_errors={} actual_doc_input_bar_nil=true actual_native_weak_nil=true actual_source_fd_worker_font_zero=true actual_mmap_zero=true actual_pixels_zero=true expected_injected_errno={:?} formal=false whole_product=false",
            self.native.len(),
            self.requests.len(),
            self.errors.len(),
            failed.then_some(libc::EIO)
        );
        if let Some(cause) = &self.close_error {
            return Err(cause.clone());
        }
        drop(self); // 所有真正资源门通过后才归还调用方的原请求/错误，不靠先drop弱票洗结果。
        Ok(())
    }
}
