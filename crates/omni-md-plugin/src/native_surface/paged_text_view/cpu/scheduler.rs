//! Each candidate has one main-queue continuation containing only weak View/surface identities and its
//! original serial.

use super::*;
use dispatch2::MainThreadBound;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) mod probe;

impl CpuSurface {
    /// Queue one turn for the original serial, retaining neither View nor surface while waiting.
    pub(super) fn schedule(
        self: &Rc<Self>,
        view: &NativeMarkdownPagedTextView,
        serial: u64,
    ) -> Result<()> {
        if self.active_serial.get() != Some(serial) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        if self.scheduled.get() == Some(serial) {
            return Ok(());
        }
        self.scheduled.set(Some(serial));
        let mtm = view.mtm();
        let weak_view = MainThreadBound::new(objc2::rc::Weak::from(view), mtm);
        let weak_surface = MainThreadBound::new(Rc::downgrade(self), mtm);
        crate::native_surface::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("CPU延续只在原AppKit主队列执行");
            let Some(surface) = weak_surface.get(mtm).upgrade() else {
                return;
            };
            let Some(view) = weak_view.get(mtm).load() else {
                return;
            };
            if surface.scheduled.get() != Some(serial)
                || surface.active_serial.get() != Some(serial)
            {
                return; // A stale queued event may neither clear a successor scheduled slot nor acquire a new Scene.
            }
            surface.scheduled.set(None);
            let _use = PayloadUseGuard::enter(&view.ivars().depth);
            let painting = match PaintUse::enter(&surface.painting) {
                Ok(painting) => painting,
                Err(_) => {
                    surface.deferred.set(Some(serial));
                    return; // The original synchronous paint exit owns the wake; avoid rescheduling loops inside nested run loops.
                }
            };
            let result = (|| {
                let candidate = surface
                    .candidate
                    .borrow_mut()
                    .take()
                    .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
                if candidate.serial != serial {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                surface.advance(&view, candidate)
            })();
            drop(painting);
            let wake = surface.wake_deferred(&view);
            let result = match result {
                Err(error) => Err(error),
                Ok(()) => wake,
            };
            view.record(result); // Record the actual error once per turn, without timers, nested run loops, or replaying failed work.
        });
        Ok(())
    }
    /// Wake deferred original work and independently honor a newer repaint, preserving the earlier
    /// error.
    pub(super) fn wake_deferred(self: &Rc<Self>, view: &NativeMarkdownPagedTextView) -> Result<()> {
        let continuation = if let Some(serial) = self.deferred.take()
            && self.active_serial.get() == Some(serial)
        {
            self.schedule(view, serial)
        } else {
            Ok(())
        };
        // The synchronous stack must release PaintUse before asking AppKit to deliver updateLayer
        // again.
        // Dirty only the actual current client, without acquiring a Scene or renewing old authority for
        // accepted new input.
        let repaint = if self.repaint_requested.replace(false) && !view.ivars().closed.get() {
            (|| {
                let origin = view.origin()?;
                self.layer_current(view, &|| origin.current(view))?;
                origin.step(view, || view.setNeedsDisplay(true))
            })()
        } else {
            Ok(())
        };
        // Run both operations so an original-serial scheduling error cannot discard a newer Scene
        // repaint request.
        // Preserve the continuation error first; the caller still gives an earlier paint/Source failure
        // priority.
        continuation.and(repaint)
    }
}
