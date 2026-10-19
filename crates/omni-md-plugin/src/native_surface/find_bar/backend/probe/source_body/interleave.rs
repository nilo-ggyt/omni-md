//! 原Font current回调中的真实输入/宽度/查询撤权；延后错误仍归原Find提交，不借新代次。

use super::*;

#[derive(Clone, Copy, Debug)]
enum Action {
    Query,
    Hide,
    Edit,
    Resize,
    #[cfg(feature = "native-source-reclaim-probe")]
    ReclaimFault,
}

struct Interleave {
    action: Action,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    calls: Cell<usize>,
    view: NativeWeak<NativeMarkdownPagedTextView>,
    panel: NativeWeak<NativeMarkdownPanel>,
    scroll: NativeWeak<NSScrollView>,
    controller: NativeWeak<NativeMarkdownController>,
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    #[cfg(feature = "native-source-reclaim-probe")]
    document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    retired: RefCell<Option<NativeMarkdownPagedBodyPreparationResources>>,
    original: RefCell<Option<NativeMarkdownPagedSceneRequest>>,
    failure: RefCell<Option<String>>,
    bound_request: RefCell<Option<std::rc::Weak<NativeMarkdownPagedFindRevealRequest>>>,
    #[cfg(feature = "native-source-reclaim-probe")]
    backing: omni_markdown_core::MarkdownOriginalBackingObserver,
}

/// 原查询通过真实会话编辑器输入；没有编辑器的合成通知不能冒充用户已经改变查询。
fn query_changed(
    bar: &NativeMarkdownFindBar,
    controller: &NativeMarkdownController,
) -> Result<(), String> {
    let intent = controller.find_input_intent();
    // SAFETY: 本同步控制持有原栏和字段；标准聚焦交给AppKit，不替换delegate或输入上下文。
    unsafe { bar.query.selectText(None) };
    let editor = bar
        .query
        .currentEditor()
        .ok_or("原查询没有实际字段编辑器")?
        .downcast::<objc2_app_kit::NSTextView>()
        .map_err(|_| "原查询输入不是NSTextView")?;
    let session = editor
        .downcast_ref::<crate::native_surface::NativeMarkdownSessionFieldEditor>()
        .ok_or("原查询没有使用会话字段编辑器")?;
    if !session.belongs_to(&controller.ivars().undo, &controller.ivars().owned)
        || editor.hasMarkedText()
        || controller.find_input_intent() != intent
    {
        return Err("真实查询输入前原会话或意图已经变化".into());
    }
    let length = bar.query.stringValue().length();
    if length > 32 {
        return Err("本回归的固定短查询已变化".into()); // 只接受既有合成查询，绝不借此读取正文或任意长字段。
    }
    let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
    // SAFETY: 同一实际字段编辑器和固定短NSString在输入栈存活；明确范围只替换原字段的短查询。
    unsafe {
        input.insertText_replacementRange(ns_string!("新查询"), NSRange::new(0, length));
    }
    if controller.find_input_intent() != intent.checked_add(1).ok_or("查询意图溢出")?
        || !bar
            .query
            .stringValue()
            .isEqualToString(ns_string!("新查询"))
        || editor.hasMarkedText()
    {
        return Err("真实查询输入没有产生一次准确的原意图变化".into());
    }
    Ok(())
}

impl NativeMarkdownPagedBodyPreparationProbe for Interleave {
    fn native_check(
        &self,
        original: &NativeMarkdownPagedSceneRequest,
        resources: NativeMarkdownPagedBodyPreparationResources,
    ) {
        if self.calls.replace(self.calls.get() + 1) != 0 {
            *self.failure.borrow_mut() = Some("原Find重入钩子交付多次".into());
            return;
        }
        *self.original.borrow_mut() = Some(original.clone());
        *self.retired.borrow_mut() = Some(resources);
        let result = (|| -> Result<(), String> {
            let (request, source) = self
                .body
                .upgrade()
                .ok_or("原Body已释放")?
                .probe_bound_find_source()
                .ok_or("Font回调前原Find尚未绑定唯一Source")?;
            if self
                .retired
                .borrow()
                .as_ref()
                .is_none_or(|retired| retired.source_authority != source)
            {
                return Err("Find目标绑定的Source不是原Font回调实际Source".into());
            }
            *self.bound_request.borrow_mut() = Some(request);
            if original.input.generation != 1
                || original.input.utf16_len != fixture::UTF16 + 1
                || original.input.primary_range != NSRange::new(fixture::LOCAL_SECOND + 1, 2)
            {
                return Err("重入没有发生在原Replace已提交后的一次绑定Source中".into());
            }
            let view = self.view.load().ok_or("原Find View已释放")?;
            match self.action {
                Action::Edit => {
                    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*view);
                    // SAFETY: 真实原client消费固定合成字节和显式插入点，仍走原canonical事务。
                    unsafe {
                        client.insertText_replacementRange(
                            &NSString::from_str("R"),
                            NSRange::new(0, 0),
                        );
                    }
                }
                Action::Resize => self
                    .panel
                    .load()
                    .ok_or("原Find panel已释放")?
                    .setContentSize(NSSize::new(680.0, 348.0)),
                Action::Query => {
                    let bar = self.bar.upgrade().ok_or("原Find栏已释放")?;
                    let controller = self.controller.load().ok_or("原Find控制器已释放")?;
                    query_changed(&bar, &controller)?;
                }
                Action::Hide => {
                    let bar = self.bar.upgrade().ok_or("原Find栏已释放")?;
                    let controller = self.controller.load().ok_or("原Find控制器已释放")?;
                    let scroll = self.scroll.load().ok_or("原scroll已释放")?;
                    if !bar.hide(&scroll, &view, &controller)? {
                        return Err("真实Find栏未隐藏".into());
                    }
                }
                #[cfg(feature = "native-source-reclaim-probe")]
                Action::ReclaimFault => {
                    let document = self.document.upgrade().ok_or("原Find文档已释放")?;
                    let mut prefix = [0; 5];
                    document
                        .borrow()
                        .copy_utf16_code_units(0, &mut prefix)
                        .map_err(|cause| cause.to_string())?;
                    if prefix != [0x4E2D, 0x4E2D, 0x66FF, 0x6362, 0x6587] {
                        return Err("注入前真实Replace数据不是一次提交".into());
                    }
                    let before = (
                        self.backing.reclaim_attempts(),
                        self.backing.reclaim_successes(),
                    );
                    self.backing.fail_next_reclaim_for_test();
                    let mut scalar = [0];
                    let error = document
                        .borrow()
                        .copy_utf16_code_units(500_001, &mut scalar);
                    if !matches!(error, Err(omni_markdown_core::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno)) if errno == libc::EIO)
                        || self.backing.first_errno() != Some(libc::EIO)
                        || self.backing.reclaim_attempts() <= before.0
                        || self.backing.reclaim_successes() <= before.1
                    {
                        return Err("原实际Source lease没有在真实msync成功后触发一次合成EIO".into());
                    }
                    println!(
                        "native_body_find injected_reclaim_errno={} actual_msync_attempts_before={} actual_msync_successes_before={} actual_msync_attempts_after={} actual_msync_successes_after={} kernel_eio_claim=false committed_bytes_verified_before_fault=true formal=false",
                        libc::EIO,
                        before.0,
                        before.1,
                        self.backing.reclaim_attempts(),
                        self.backing.reclaim_successes()
                    );
                    CFRunLoop::current().ok_or("原runloop已释放")?.stop();
                }
            }
            Ok(())
        })();
        if let Err(cause) = result {
            *self.failure.borrow_mut() = Some(cause);
        }
    }
}

pub(super) fn before_binding(rig: &mut Rig) -> Result<(), String> {
    for action in [Action::Query, Action::Hide] {
        rig.bar.query.setStringValue(ns_string!("目标"));
        let frame = observe(&rig.body)?;
        let request = cases::pending(rig, MarkdownFindAction::Next)?;
        if rig.body.probe_bound_find_source().is_some() {
            return Err("begin尚未返回主队列就已偷捕Source或提前绑定".into());
        }
        match action {
            Action::Query => {
                query_changed(&rig.bar, &rig.controller)?;
            }
            Action::Hide => {
                rig.bar.hide(&rig.scroll, &rig.view, &rig.controller)?;
            }
            _ => unreachable!("此控制只测begin后尚未入队的查询/隐藏"),
        }
        wait_frame("cancel_before_binding", &rig.body, frame)?;
        if !request.cancelled()
            || request.pending()
            || request.completed_visible()
            || request.take_failure().is_some()
        {
            return Err("未绑定Find没有准确取消为原终态".into());
        }
        if matches!(action, Action::Hide) {
            let shown = observe(&rig.body)?;
            rig.bar.show(&rig.scroll, true, &rig.controller)?;
            rig.panel.makeFirstResponder(Some(&**rig.view));
            wait_frame("show_after_cancel", &rig.body, shown)?;
        }
    }
    rig.bar.query.setStringValue(ns_string!("目标"));
    let frame = observe(&rig.body)?;
    let old = cases::pending(rig, MarkdownFindAction::Next)?;
    rig.bar.query.setStringValue(ns_string!("局部"));
    let current = cases::pending(rig, MarkdownFindAction::Next)?;
    wait_frame("new_action_keeps_successor", &rig.body, frame)?;
    if !old.cancelled()
        || old.completed_visible()
        || !current.completed_visible()
        || current.cancelled()
    {
        return Err("旧Find取消夺走了后继动作或借后继Visible补签".into());
    }
    Ok(())
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let actions = [
        Action::Query,
        Action::Hide,
        Action::Edit,
        Action::Resize,
        #[cfg(feature = "native-source-reclaim-probe")]
        Action::ReclaimFault,
    ];
    for action in actions {
        let failed = {
            #[cfg(feature = "native-source-reclaim-probe")]
            {
                matches!(action, Action::ReclaimFault)
            }
            #[cfg(not(feature = "native-source-reclaim-probe"))]
            {
                false
            }
        };
        let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
            let mut rig = Rig::new(mtm)?;
            let probe = Rc::new(Interleave {
                action,
                body: Rc::downgrade(&rig.body),
                calls: Cell::new(0),
                view: NativeWeak::from(&**rig.view),
                panel: NativeWeak::from(&*rig.panel),
                controller: NativeWeak::from(&*rig.controller),
                scroll: NativeWeak::from(&*rig.scroll),
                bar: Rc::downgrade(&rig.bar),
                retired: RefCell::new(None),
                original: RefCell::new(None),
                failure: RefCell::new(None),
                bound_request: RefCell::new(None),
                #[cfg(feature = "native-source-reclaim-probe")]
                document: rig.document.clone(),
                #[cfg(feature = "native-source-reclaim-probe")]
                backing: rig.backing.clone(),
            });
            let erased: Rc<dyn NativeMarkdownPagedBodyPreparationProbe> = probe.clone();
            let result = (|| {
                cases::local_first(&mut rig)?;
                rig.bar.replacement.setStringValue(ns_string!("替换文"));
                rig.body
                    .probe_next_native_preparation(Rc::downgrade(&erased))
                    .map_err(|cause| cause.to_string())?;
                run_one(&mut rig, &probe, failed)
            })();
            drop((probe, erased));
            let pending = if failed {
                Ok(()) // Source真实失败后不再发新Find；保留原已失败请求到严格释放门。
            } else {
                release::hold_target_fixture_pending(&mut rig)
            };
            let retirement = release::close(rig, failed)?;
            Ok((retirement, pending.and(result)))
        })?;
        retirement.verify(failed)?;
        result?;
    }
    Ok(())
}

fn run_one(rig: &mut Rig, probe: &Interleave, failed: bool) -> Result<(), String> {
    let started = Instant::now(); // 故障也计真实同步动作，不能在Replace返回后再额外领取三秒。
    let frame = if failed {
        None
    } else {
        Some(observe(&rig.body)?)
    };
    let request = cases::pending(rig, MarkdownFindAction::ReplaceNext)?;
    if let Some(frame) = frame {
        wait_frame("bound_find_reentrant_cancel", &rig.body, frame)?;
    } else {
        if let Some(remaining) = Duration::from_secs(3).checked_sub(started.elapsed()) {
            // SAFETY: 此处预期无成功帧；实际原Source lease故障停原loop，只用原三秒剩余配额。
            unsafe {
                CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), false)
            };
        }
    }
    if probe.calls.get() != 1 {
        return Err("原Find绑定后的真实Font current钩子未发生一次".into());
    }
    if probe
        .bound_request
        .borrow()
        .as_ref()
        .is_none_or(|bound| !std::rc::Weak::ptr_eq(bound, &Rc::downgrade(&request)))
    {
        return Err("Source绑定被另一个Find请求或后继代次冒领".into());
    }
    if let Some(cause) = probe.failure.borrow_mut().take() {
        return Err(cause);
    }
    let retired = probe
        .retired
        .borrow_mut()
        .take()
        .ok_or("没有原绑定Source资源事实")?;
    if failed {
        if started.elapsed() > Duration::from_secs(3) {
            return Err("原Replace到实际Source故障交付超过三秒".into());
        }
        let cause = rig
            .body
            .take_failure()
            .ok_or("原Source EIO未到Body失败出口")?;
        // 同一绑定Source的准备与退休可以各保留原EIO；所有typed叶都须是这个夹具的精确源错误。
        // 原请求仍必须失败而非取消/Visible，不能把资源次因或已提交状态改变当作预期源故障。
        if !release::injected_source_eio_only(&cause)
            || request.pending()
            || request.cancelled()
            || request.completed_visible()
        {
            return Err(format!(
                "原Source首errno被取消吞掉或错误发布Visible：cause={cause:?} pending={} cancelled={} visible={}",
                request.pending(),
                request.cancelled(),
                request.completed_visible()
            ));
        }
        let error = request.take_failure().ok_or("原Find异步错误收据未交付")?;
        if !matches!(&error, NativeMarkdownFindExecutionError::CommittedEffect { commit, .. } if commit.state.generation == 1 && commit.state.utf16_len == fixture::UTF16 + 1)
            || request.take_failure().is_some()
        {
            return Err("原异步EIO没有保持一次CommittedEffect".into());
        }
        rig.errors.push(error); // 原Commit也跨关闭保持，不能靠先drop错误对象释放业务资源。
    } else {
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let expected_generation = if matches!(probe.action, Action::Edit) {
            2
        } else {
            1
        };
        if !request.cancelled()
            || request.pending()
            || request.completed_visible()
            || request.take_failure().is_some()
            || key.request.input.generation != expected_generation
        {
            return Err("原bound Find未准确取消或后继没有真实显示".into());
        }
        match probe.action {
            Action::Edit => {
                if rig.copy::<6>(0)? != [b'R' as u16, 0x4E2D, 0x4E2D, 0x66FF, 0x6362, 0x6587]
                    || key.source == retired.source_authority
                {
                    return Err("真实重入编辑丢替换或旧Source补签".into());
                }
            }
            Action::Resize => {
                let original = probe.original.borrow();
                let original = original.as_ref().ok_or("缺少原宽度")?;
                if key.request.bounds_width >= original.bounds_width
                    || key.source == retired.source_authority
                {
                    return Err("真实resize未产生较窄后继Source".into());
                }
            }
            Action::Query => {
                if key.source != retired.source_authority {
                    return Err("仅查询撤权误丢了仍有效的原Producer/Source".into());
                }
            }
            Action::Hide => {}
            #[cfg(feature = "native-source-reclaim-probe")]
            Action::ReclaimFault => unreachable!("故障分支已独立处理"),
        }
        rig.view
            .verify_find_target_visible(&key, NSRange::new(0, 1))
            .map_err(|cause| cause.to_string())?;
        rig.remember_receipt()?;
    }
    rig.retired.push(retired);
    println!(
        "native_body_find bound_reentry={:?} original_request_cancelled={} original_failure_once={} successor_verified={} formal=false whole_product=false",
        probe.action,
        request.cancelled(),
        failed,
        !failed
    );
    Ok(())
}
