//! Observe one actual complete-frame publication; Ready, queue activity, or a completed tile never
//! substitutes for pixel publication.

use std::rc::Weak;

use dispatch2::MainThreadBound;
use objc2_foundation::MainThreadMarker;

use super::{
    NativeMarkdownPagedBody, NativeMarkdownPagedBodyError, NativeMarkdownPagedSceneKey,
    NativeMarkdownPagedTextViewDelegate, Result,
};

/// Deliver only an actual complete frame still belonging to the original key; weak observation extends
/// no UI, document, or probe run-loop lifetime.
pub trait NativeMarkdownPagedBodyPaintObserver {
    /// Receive the actual complete publication after the original key passes current checks.
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize);
}

pub(super) struct PaintObservation {
    observer: Weak<dyn NativeMarkdownPagedBodyPaintObserver>,
    baseline: usize,
}

impl NativeMarkdownPagedBody {
    /// Register one observation before the Native action and capture its baseline from the real
    /// surface, never from a caller-supplied count.
    /// This neither invalidates nor draws the view and never reissues a completed frame; no new
    /// publication means genuinely unfinished work.
    pub fn observe_next_cpu_frame(
        &self,
        observer: Weak<dyn NativeMarkdownPagedBodyPaintObserver>,
    ) -> Result<()> {
        if !self.current_body() {
            return Err(NativeMarkdownPagedBodyError::Closed);
        }
        if self.paint_observation.borrow().is_some() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        let baseline = self
            .view()?
            .cpu_paints()
            .ok_or(NativeMarkdownPagedBodyError::Unsupported)?;
        *self.paint_observation.borrow_mut() = Some(PaintObservation { observer, baseline });
        Ok(())
    }

    /// Defer delivery until Native drawing and owner borrows have returned.
    pub(super) fn queue_cpu_publication(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        if self.paint_observation.borrow().is_none() {
            return; // Without an observer, add no product main-queue work and retain no published-frame history.
        }
        let weak = MainThreadBound::new(self.weak.clone(), self.mtm);
        // The original key contains main-thread Rc::Weak; carry that same weak ticket inside
        // MainThreadBound across the Send queue
        // without unsafe Send or additional strong Scene, View, Source, or pixel ownership.
        let key = MainThreadBound::new(key.clone(), self.mtm);
        super::super::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("完整帧观察只在原主队列交付");
            let key = key.get(mtm);
            let Some(owner) = weak.get(mtm).upgrade() else {
                return;
            };
            let Some(baseline) = owner
                .paint_observation
                .borrow()
                .as_ref()
                .map(|observation| observation.baseline)
            else {
                return;
            };
            if !owner.scene_current(key) {
                return; // Input or geometry revoked the original frame; a successor key cannot renew this notification.
            }
            let Ok(view) = owner.view() else {
                return;
            };
            if view.cpu_paint_pending() != Some(false)
                || view.cpu_paints() != Some(paints)
                || paints <= baseline
                || !owner.scene_current(key)
            {
                return; // Unfinished work, successor replacement, or an unchanged publication count is not this observation's success.
            }
            let observation = owner.paint_observation.borrow_mut().take();
            drop(view); // Release Native strong claims and all slot borrows before invoking a callback that may stop the run loop.
            if let Some(observer) =
                observation.and_then(|observation| observation.observer.upgrade())
            {
                observer.frame_published(key, paints);
            }
        });
    }
}
