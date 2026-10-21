//! Retain callers' original requests across closure.
//! Drop these weak tickets only after every actual business resource is released.

use super::*;
use crate::native_surface::{
    paged_input::NativeMarkdownPagedInput, paged_text_view::NativeMarkdownPagedCloseReceipt,
    viewport_pixels::NativeMarkdownViewportPixelObserver,
};
use objc2_quartz_core::CATransaction;

/// One original release event completes the wait. Verify pixels while the parent
/// window is alive, without relying on later window closure or outer-pool drainage.
fn wait_close(receipt: &Rc<NativeMarkdownPagedCloseReceipt>) -> Result<(), String> {
    let started = Instant::now();
    let arrived = Rc::new(Cell::new(false));
    let observed = Rc::clone(&arrived);
    let run_loop = CFRunLoop::current().ok_or("关闭资源缺少原主runloop")?;
    receipt.when_released(move || {
        observed.set(true);
        run_loop.stop(); // Respond only to this actual final-resource relay; retain no Rig/view/document/frame.
    });
    while !arrived.get() {
        let Some(remaining) = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .filter(|remaining| !remaining.is_zero())
        else {
            break;
        };
        // The original receipt remains the sole completion authority. Other
        // callbacks stopping the main loop prove neither pixel release nor timeout.
        // Deliver sources only within the original remaining deadline; never
        // resubmit close, poll resources, or extend the allowance.
        let progress = autoreleasepool(|_| {
            // SAFETY: Use the original AppKit main-thread default mode, with no
            // temporary Native claim retained across event-loop iterations.
            unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), false) }
        });
        if progress == objc2_core_foundation::CFRunLoopRunResult::Finished && !arrived.get() {
            return Err("原正文关闭runloop已无可交付source，真实像素归还事件仍未完成".into());
        }
    }
    if !arrived.get() {
        return Err("原正文关闭在3秒内没有真实像素归还事件".into());
    }
    receipt.verify_resources_released()
}

/// Keep immutable/weak release observations alongside original caller-held tickets.
/// These tickets remain alive during verification to expose unintended retention.
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
    receipts: Vec<NativeMarkdownPagedBodyResourceReceipts>,
    close_error: Option<String>,
}

fn native_weak(value: &AnyObject) -> NativeWeak<AnyObject> {
    NativeWeak::from(value)
}

/// Only long-text/navigation fixtures containing the fixed target call this helper.
/// Ordinary close neither guesses source content nor invents RootLeaf/cell Find actions.
pub(super) fn hold_target_fixture_pending(rig: &mut Rig) -> Result<(), String> {
    if rig.view.session_is_current() {
        // Do not run the queue. The Rig retains this unbound pending ticket through
        // Retirement verification, so actual close must revoke it despite caller retention.
        rig.bar.query.setStringValue(ns_string!("目标"));
        let request = cases::pending(rig, MarkdownFindAction::Next)?;
        if !request.pending() || rig.body.probe_bound_find_source().is_some() {
            return Err("关闭控制没有真实未绑定的在途Find".into());
        }
    }
    Ok(())
}

/// Observe original owners before dismantling the actual graph. Accepted close
/// begins resource retirement; its receipt and destruction cohort separately prove completion.
pub(super) fn close(rig: Rig) -> Result<Retirement, String> {
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
    ledger.seal(); // Fix all actual Find field/button/parent roles before the first Native detach operation.
    let mut cohort =
        crate::native_surface::deallocation::NativeMarkdownDeallocationCohort::new(rig.panel.mtm());
    // Actual Find input may create a session editor lazily. Pixel release does not
    // prove its system callbacks ended; track destruction through the same role
    // ledger rather than inferring it from a number of post-close queue turns.
    ledger.track_remaining(&mut cohort, rig.panel.mtm());
    let accepted_close = if rig.cell.as_ref().is_some_and(cell::Original::has_ended) {
        // The committed/cancelled cell has ended its editable Input session.
        // Consume that close receipt before detaching firstResponder, without
        // advancing the queue or treating an active draft's resignation failure as accepted close.
        Some(rig.body.begin_close())
    } else {
        None // Ordinary notes must resign actual input first; refusal cannot destroy an editable session early.
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
    drop(fields); // Release temporary field claims before backend/window destruction so they cannot mask weak-role release.
    rig.bar.finish_accepted();
    let closed = match accepted_close {
        Some(closed) => closed, // Consume the same earlier close for ended cells without recapturing a receipt.
        None => rig.body.begin_close(),
    };
    let released = match &closed {
        Ok(receipt) => wait_close(receipt).and_then(|()| receipt.verify_released()),
        Err(error) => Err(error.to_string()),
    }; // Accepted means synchronous detach only; the same receipt event and actual resource-zero checks prove completion.
    if let Some(cell) = &rig.cell
        && let Err(cause) = cell.verify_closed()
    {
        close_error = Some(cause); // The parent document remains alive here; dropping the whole Rig must not conceal retained cell readers.
    }
    rig.scroll.setDocumentView(None);
    rig.scroll.removeFromSuperview();
    rig.panel.setContentView(None);
    rig.panel.orderOut(None);
    // The Rig still owns the controller. Input has cleared through the accepted
    // boundary, so now end this fixture's public window-delegate borrowing edge.
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
        receipts,
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
    cohort.seal(); // Wait for final actual Native destruction only after the outer autorelease pool drains.
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
        receipts,
        close_error,
    })
}

impl Retirement {
    /// Wait for original destruction events within the original close deadline,
    /// then inspect actual source/worker/font/pixel resources while tickets remain held.
    pub(super) fn verify(self) -> Result<(), String> {
        let arrived = Rc::new(Cell::new(false));
        let observed = Rc::clone(&arrived);
        let run_loop = CFRunLoop::current().ok_or("原Native关闭缺少主runloop")?;
        self.cohort.when_released(move || {
            observed.set(true);
            run_loop.stop(); // Only the original ledger's final actual destruction completes this wait; retain no field/document.
        });
        let mut finished_without_release = false;
        while !arrived.get() {
            let Some(remaining) = Duration::from_secs(3)
                .checked_sub(self.close_started.elapsed())
                .filter(|remaining| !remaining.is_zero())
            else {
                break;
            };
            // Other components can stop this shared run loop. Stopped proves
            // neither this cohort's destruction nor expiry. Use only the original
            // close deadline's remainder, with no weak polling or sleeping.
            let progress = autoreleasepool(|_| {
                // SAFETY: Use the original main-thread default mode and at most
                // the actual remaining seconds of this same close deadline.
                unsafe {
                    CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), false)
                }
            });
            if progress == objc2_core_foundation::CFRunLoopRunResult::Finished {
                // With no runnable source, another call returns immediately;
                // report failure instead of spinning until the deadline.
                finished_without_release = !arrived.get();
                break;
            }
        }
        if !arrived.get() || !self.cohort.has_completed() {
            // Diagnose survivors only when the original cohort wait fails, using
            // the existing weak-role list instead of another resource graph.
            // Drain diagnostic load claims in this pool; diagnostics relax no release gate.
            autoreleasepool(|_| {
                for (role, weak) in &self.native {
                    let alive = weak.load().is_some();
                    eprintln!("native_body_find_close_role role={role} alive={alive} formal=false");
                }
                eprintln!(
                    "native_body_find_close_cohort {} arrived={} formal=false",
                    self.cohort.diagnostic(),
                    arrived.get(),
                );
                #[cfg(feature = "native-close-stage-diagnostic")]
                {
                    let (complete, roles) = self.ledger.diagnostic_alive_roles();
                    eprintln!(
                        "native_body_find_close_ledger complete={complete} roles={roles:?} formal=false"
                    );
                }
            });
            if finished_without_release {
                return Err("原Find Native关闭runloop已无可交付source，原销毁事件仍未完成".into());
            }
            return Err("原Find Native角色在关闭三秒内没有真实销毁事件".into());
        }
        metrics::sample("after_close")?; // The original pool drained; classifications describe actual current kernel state only.
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
            || self.backing.failed()
            || self.backing.first_errno().is_some()
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
            // These actual Find actions delivered complete frames; a missing
            // reader receipt cannot be explained as successful early release.
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
                    || actual.first_source_errno.is_some()
                {
                    return Err(format!("原Find Reader/Writer资源未归还：{actual:?}"));
                }
            }
            let worker = receipt.worker.snapshot().ok_or("原worker观察者已poison")?;
            if worker.first_source_errno.is_some() {
                return Err(format!("原Find worker没有保持原Source首errno：{worker:?}"));
            }
            if !worker.is_released() {
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
            // The same Body receipt includes weak observations of visible/pending
            // cells; checking only the main reader would miss derived display
            // directories. Use each cell's actual worker/FD facts without another
            // cache counter, resource ledger, or release-polling loop.
            for cell in &receipt.cells {
                cell.verify_released()?;
            }
        }
        println!(
            "native_body_find release_actual_native_roles={} held_original_requests={} actual_doc_input_bar_nil=true actual_native_weak_nil=true actual_source_fd_worker_font_zero=true actual_mmap_zero=true actual_pixels_zero=true formal=false whole_product=false",
            self.native.len(),
            self.requests.len(),
        );
        if let Some(cause) = &self.close_error {
            return Err(cause.clone());
        }
        // Return caller-held original requests only after all resource
        // checks pass; dropping weak tickets first would conceal retention defects.
        drop(self);
        Ok(())
    }
}
