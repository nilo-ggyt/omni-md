//! This gated negative control holds actual PaintUse while an existing queued event encounters
//! contention; production creates no nested run loop.

use super::*;

pub(in crate::native_surface::paged_text_view) fn nested_delivery(
    view: &NativeMarkdownPagedTextView,
    delivery: impl FnOnce(),
) -> Result<()> {
    let _use = PayloadUseGuard::enter(&view.ivars().depth);
    let surface = view
        .ivars()
        .cpu
        .borrow()
        .clone()
        .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
    let scene = view.scene()?;
    let serial = surface
        .active_serial
        .get()
        .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
    if surface.scheduled.get() != Some(serial) || surface.candidate.borrow().is_none() {
        return Err(NativeMarkdownPagedTextViewError::Pending);
    } // Observe an actual Pending candidate only, without forcing owner authority or fabricating a session.
    let before = surface.steps.get();
    let painting = PaintUse::enter(&surface.painting)?;
    delivery(); // The harness delivers one actual CFRunLoop turn; this callback is neither product scheduling nor a saved continuation.
    // Enter the actual Native display method while the synchronous stack retains paint admission,
    // exercising updateLayer reentry.
    // Observe the public failure result rather than mutating state; the previous implementation
    // incorrectly stored display waiting as a primary Pending error.
    // SAFETY: The receiver is the retained original NSView; call its public no-argument updateLayer on
    // the current main thread only.
    unsafe {
        let _: () = msg_send![view, updateLayer];
    }
    let reentrant = view.take_failure().map_or(Ok(()), Err);
    let validation = scene.scene.validate_ready(&|| scene.current(view));
    let deferred = surface.active_serial.get() == Some(serial)
        && surface.scheduled.get().is_none()
        && surface.deferred.get() == Some(serial)
        && surface.candidate.borrow().is_some()
        && surface.steps.get() == before;
    drop(painting);
    if let Err(error) = reentrant {
        surface.cancel_candidate();
        return Err(error); // Propagate actual paint/Source failures rather than clearing them through a diagnostic read.
    }
    if let Err(error) = validation {
        surface.cancel_candidate();
        return Err(error);
    }
    surface.wake_deferred(view)?;
    if !deferred || surface.scheduled.get() != Some(serial) || surface.deferred.get().is_some() {
        return Err(NativeMarkdownPagedTextViewError::Raster(
            "真实嵌套交付没有保留同候选唯一延续".into(),
        ));
    }
    Ok(()) // The successor event has not run; the original harness independently consumes actual paint, cancellation, and close outcomes.
}
