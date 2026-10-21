//! One actual complete CPU-frame notification drives the original three-second
//! deadline. Ready state or an executed queue callback is not image completion.

use super::*;

/// Observe the production paint endpoint with only its key, count, and duration.
/// This observer owns no Body/view/document and stops only its own finite wait.
pub(super) struct Frame {
    started: Instant,
    loop_: CFRetained<CFRunLoop>,
    waiting: Cell<bool>,
    published: RefCell<Option<(NativeMarkdownPagedSceneKey, usize, Duration)>>,
}

impl NativeMarkdownPagedBodyPaintObserver for Frame {
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        assert!(self.published.borrow().is_none(), "原帧只能交付一次");
        *self.published.borrow_mut() = Some((key.clone(), paints, self.started.elapsed()));
        if self.waiting.get() {
            self.loop_.stop(); // Stop only this finite test wait, without quitting the app or adding shortcuts.
        }
    }
}

/// Bind the same real frame to its clock/run loop. Special action-error observation
/// still registers only once for the original frame notification.
pub(super) fn pending() -> Result<Rc<Frame>, String> {
    Ok(Rc::new(Frame {
        started: Instant::now(),
        loop_: CFRunLoop::current().ok_or("没有原主runloop")?,
        waiting: Cell::new(false),
        published: RefCell::new(None),
    }))
}

/// Subscribe through the actual Body's weak paint-observer API. The returned Rc
/// keeps this observation alive without making the Body retain the observer strongly.
pub(super) fn observe(body: &NativeMarkdownPagedBody) -> Result<Rc<Frame>, String> {
    let frame = pending()?;
    let erased: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = frame.clone();
    body.observe_next_cpu_frame(Rc::downgrade(&erased))
        .map_err(|cause| cause.to_string())?;
    Ok(frame)
}

pub(super) fn wait_frame(
    stage: &str,
    body: &NativeMarkdownPagedBody,
    frame: Rc<Frame>,
) -> Result<NativeMarkdownPagedBodyProgress, String> {
    wait(stage, body, frame, false)
}

/// Actual EOF navigation requires the sole composer to complete. This changes
/// neither the prefix frame's acceptance gate nor its original deadline.
pub(super) fn wait_complete_frame(
    stage: &str,
    body: &NativeMarkdownPagedBody,
    frame: Rc<Frame>,
) -> Result<NativeMarkdownPagedBodyProgress, String> {
    wait(stage, body, frame, true)
}

fn wait(
    stage: &str,
    body: &NativeMarkdownPagedBody,
    frame: Rc<Frame>,
    expected_complete: bool,
) -> Result<NativeMarkdownPagedBodyProgress, String> {
    let mut finished_without_frame = false;
    if frame.published.borrow().is_none() {
        frame.waiting.set(true);
        while frame.published.borrow().is_none() {
            let Some(remaining) = Duration::from_secs(3)
                .checked_sub(frame.started.elapsed())
                .filter(|remaining| !remaining.is_zero())
            else {
                break;
            };
            // A prior window/release callback may stop the shared main run loop.
            // Stopped proves neither this frame nor deadline expiry. Reuse only
            // the original deadline's remaining time; frame_published alone completes the wait.
            let progress = autoreleasepool(|_| {
                // SAFETY: Run the original main-thread default mode with no
                // document/Scene/RefCell borrow across actual source delivery.
                unsafe {
                    CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), false)
                }
            });
            if progress == objc2_core_foundation::CFRunLoopRunResult::Finished {
                // With no runnable source, another run returns immediately. Keep
                // explicit failure rather than spinning or resubmitting observation/production work.
                finished_without_frame = frame.published.borrow().is_none();
                break;
            }
        }
        frame.waiting.set(false);
    }
    if let Some(cause) = body.take_failure() {
        // This synthetic acceptance boundary retains the actual semantic error
        // type without changing public messages or logging document text/links.
        if let crate::native_surface::paged_body::NativeMarkdownPagedBodyError::Semantics(
            semantic,
        ) = &cause
        {
            return Err(format!(
                "{stage}: 原Body失败：{cause}；semantic_error={semantic:?}"
            ));
        }
        if let crate::native_surface::paged_body::NativeMarkdownPagedBodyError::Compose(
            omni_markdown_core::MarkdownLineComposeError::Bidi(bidi),
        ) = &cause
        {
            // Preserve the actual UBA error kind to distinguish table-domain
            // misuse, revoked sources, and storage failures. BidiError contains
            // types/coordinates/system errors, not document text; retain the same deadline.
            return Err(format!("{stage}: 原Body失败：{cause}；bidi_error={bidi:?}"));
        }
        return Err(format!("{stage}: 原Body失败：{cause}"));
    }
    let view = body.view().map_err(|cause| cause.to_string())?;
    let prior_pending = match view.take_failure() {
        Some(ViewError::Pending) => true, // Pre-Ready notification cannot replace the required actual complete frame.
        Some(cause) => return Err(format!("{stage}: 原View失败：{cause}")),
        None => false,
    };
    let progress = body.progress().map_err(|cause| cause.to_string())?;
    println!(
        "native_body_find stage={stage} progress={progress:?} actual_elapsed_ms={:.3} original_frame_event={} consumed_prior_view_pending={prior_pending} deadline_seconds=3 formal=false whole_product=false",
        frame.started.elapsed().as_secs_f64() * 1000.0,
        frame.published.borrow().is_some()
    );
    let (key, paints, elapsed) = frame.published.borrow_mut().take().ok_or_else(|| {
        if finished_without_frame {
            format!("{stage}: 原runloop已无可交付source，真实完整帧通知仍未完成")
        } else {
            format!("{stage}: 原三秒内没有真实完整帧通知")
        }
    })?;
    // The callback's original Scene and paint completion must still match the
    // actual view. A timely notification alone cannot certify newer/stale pixels.
    let actual = view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if !actual.same(&key)
        || view.cpu_paints() != Some(paints)
        || view.cpu_paint_pending() != Some(false)
        || progress.maximum_step_bytes > 4096
        || progress.maximum_step_scalars > 256
        || progress.maximum_step_metadata_units > 256
        || progress.complete != expected_complete
    {
        return Err(format!("{stage}: 完整帧原票或有界工作量不符"));
    }
    println!(
        "native_body_find stage={stage} actual_action_to_frame_ms={:.3} actual_cpu_paints={paints} original_key_verified=true formal=false",
        elapsed.as_secs_f64() * 1000.0
    );
    metrics::sample(stage)?; // The original frame timestamp/key are fixed; sampling grants no additional waiting budget.
    if elapsed > Duration::from_secs(3) {
        return Err(format!(
            "{stage}: 真实动作到完整帧耗时{:.3}ms，超过原3000ms门",
            elapsed.as_secs_f64() * 1000.0
        ));
    }
    Ok(progress)
}
