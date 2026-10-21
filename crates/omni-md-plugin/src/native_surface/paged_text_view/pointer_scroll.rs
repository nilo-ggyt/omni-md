//! Autoscroll after actual drag leaves the viewport retains one finite endpoint/weak original gesture, without saving or replaying NSEvent.

use super::*;
use dispatch2::MainThreadBound;
use objc2::rc::Weak as NativeWeak;
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};
use std::ptr::NonNull;

/// Each actual client owns at most one timer; its closure weakly borrows View without owning Source, document, font, or Scene.
pub(super) struct DragScroll {
    timer: Retained<NSTimer>,
    point: NSPoint,
    intent: Weak<()>,
    clicks: usize,
}

impl Drop for DragScroll {
    fn drop(&mut self) {
        // This value has left the original slot; Drop ends only the system timer it owns.
        // invalidate may reenter; do not revisit the View slot or remove a new timer installed during reentry.
        self.timer.invalidate();
    }
}

impl NativeMarkdownPagedTextView {
    /// After actual Drag selection interpretation, use its bounded endpoint only to start/update this gesture's autoscroll.
    /// Capture intent at the original event entry before Native calls; this method cannot acquire a newer Down.
    pub(super) fn update_drag_scroll(&self, event: &NSEvent, intent: &Weak<()>) {
        let forward = self.ivars().cell_pointer_forward.borrow().clone();
        if let Some(forward) = forward {
            if Weak::ptr_eq(&forward.intent, intent)
                && let Some(parent) = forward.parent.upgrade()
                && parent.cell_pointer_is_current(intent)
            {
                parent.update_drag_scroll(event, intent);
            }
            return; // The real callback is still on-stack and may forward to current parent, without extending a sealed old draft.
        }
        let result = (|| {
            if !self.cell_pointer_is_current(intent) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let origin = self.origin()?;
            let current = || {
                self.cell_pointer_is_current(intent)
                    && origin.current(self)
                    && self.cell_pointer_is_current(intent)
            };
            if !current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let point = event.locationInWindow();
            if !current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let clicks = self
                .ivars()
                .pointer_motion
                .get()
                .ok_or(NativeMarkdownPagedTextViewError::Revoked)?
                .clicks;
            self.start_drag_scroll(point, intent, clicks)
        })();
        // Actual handoff/close may revoke the original client; that ends this timer naturally rather than create an Input failure for old View.
        if !matches!(
            &result,
            Err(NativeMarkdownPagedTextViewError::Closed
                | NativeMarkdownPagedTextViewError::Revoked)
        ) {
            self.record(result);
        }
    }

    /// Create a timer only outside actual bounded Body on either axis; return it when inside both axes, gesture ends, or composition starts.
    fn start_drag_scroll(&self, point: NSPoint, intent: &Weak<()>, clicks: usize) -> Result<()> {
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        if !self.cell_pointer_is_current(intent) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let origin = self.origin()?;
        let current = || {
            self.cell_pointer_is_current(intent)
                && origin.current(self)
                && self.cell_pointer_is_current(intent)
        };
        if !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        if origin.state.marked.is_some() {
            self.stop_drag_scroll_intent(intent);
            return Ok(()); // The IME owns pointer delivery during composition; old dragging cannot move the candidate anchor.
        }
        let local = self.convertPoint_fromView(point, None);
        if !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let bounds = self.bounds();
        if !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let right = bounds.origin.x + bounds.size.width;
        let top = bounds.origin.y + bounds.size.height;
        if !local.x.is_finite()
            || !local.y.is_finite()
            || !bounds.origin.x.is_finite()
            || !bounds.origin.y.is_finite()
            || !bounds.size.width.is_finite()
            || !bounds.size.height.is_finite()
            || bounds.size.width < 0.0
            || bounds.size.height < 0.0
            || !right.is_finite()
            || !top.is_finite()
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        if local.x >= bounds.origin.x
            && local.x <= right
            && local.y >= bounds.origin.y
            && local.y <= top
        {
            self.stop_drag_scroll_intent(intent);
            return Ok(());
        }
        let previous = {
            let mut slot = self.ivars().drag_scroll.borrow_mut();
            if let Some(active) = slot.as_mut()
                && Weak::ptr_eq(&active.intent, intent)
            {
                active.point = point;
                active.clicks = clicks;
                return Ok(()); // Frequent actual Drag updates one point instead of allocating a timer/queue item per event.
            }
            slot.take()
        };
        drop(previous); // Return only the extracted old timer; preserve a reentrantly installed successor slot before publication below.
        if !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let weak = MainThreadBound::new(NativeWeak::from(self), self.mtm());
        let block = block2::RcBlock::new(move |timer: NonNull<NSTimer>| {
            let mtm = MainThreadMarker::new().expect("drag timer runs on the main thread");
            // SAFETY: Foundation holds this parameter during the synchronous timer callback; the closure neither retains it nor transfers it across threads.
            let timer = unsafe { timer.as_ref() };
            if let Some(view) = weak.get(mtm).load() {
                view.drag_scroll_tick(timer);
            } else {
                timer.invalidate(); // End the system timer when its owner is gone; a weak closure must not become a permanent empty loop.
            }
        });
        // SAFETY: Create the repeating main-thread Native timer with a MainThreadBound weak closure, retaining no business/Input objects.
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(0.05, true, &block) };
        let mut created = Some(DragScroll {
            timer: timer.clone(),
            point,
            intent: intent.clone(),
            clicks,
        });
        if !current() {
            drop(created);
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let installed = {
            let mut slot = self.ivars().drag_scroll.borrow_mut();
            if slot.is_some() {
                false // A new action installed its owner during Native timer creation; the old stack cannot overwrite it.
            } else {
                *slot = created.take();
                true
            }
        };
        drop(created); // An unpublished timer releases itself only, and Native destruction runs outside slot borrows.
        if !installed {
            return Ok(());
        }
        if !current() || !self.owns_drag_scroll(&timer, intent) {
            self.stop_original_drag_scroll(&timer, intent);
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let run_loop = NSRunLoop::mainRunLoop();
        if !current() || !self.owns_drag_scroll(&timer, intent) {
            self.stop_original_drag_scroll(&timer, intent);
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // SAFETY: Register only this actual main-thread timer; CommonModes includes ordinary events and Native tracking.
        unsafe { run_loop.addTimer_forMode(&timer, NSRunLoopCommonModes) };
        if !current() || !self.owns_drag_scroll(&timer, intent) {
            self.stop_original_drag_scroll(&timer, intent);
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }

    /// Each tick scrolls at most one actual viewport and returns the same point to its bounded slot; await unfinished frames without forcing layout.
    fn drag_scroll_tick(&self, timer: &NSTimer) {
        // Foundation enters a separate actual callback; original close must wait for its Input/Scene borrows to return.
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let active = self
            .ivars()
            .drag_scroll
            .borrow()
            .as_ref()
            .and_then(|active| {
                std::ptr::eq::<NSTimer>(&*active.timer, timer)
                    .then(|| (active.point, active.intent.clone(), active.clicks))
            });
        let Some((point, intent, clicks)) = active else {
            timer.invalidate(); // A late old timer cannot revoke this View's newer timer.
            return;
        };
        if !self.cell_pointer_is_current(&intent) {
            self.stop_original_drag_scroll(timer, &intent);
            return;
        }
        let result = (|| {
            let origin = self.origin()?;
            let current = || {
                self.owns_drag_scroll(timer, &intent)
                    && self.drag_scroll_point_is_current(point, &intent)
                    && self.cell_pointer_is_current(&intent)
                    && origin.current(self)
                    && self.cell_pointer_is_current(&intent)
                    && self.drag_scroll_point_is_current(point, &intent)
                    && self.owns_drag_scroll(timer, &intent)
            };
            if !current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if origin.state.marked.is_some() {
                self.stop_original_drag_scroll(timer, &intent);
                return Ok(());
            }
            let pointer = self.drag_scroll_pointer(&origin, point, intent.clone(), clicks)?;
            if self.hand_child_pointer(&origin, &pointer)? {
                return Ok(()); // The active Cell must actually commit/handoff first; do not scroll its ended local input graph.
            }
            if !current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let request = match self.scene() {
                Ok(scene) if current() && scene.origin.state == origin.state => {
                    scene.key.request.clone()
                }
                Ok(_) => return Err(NativeMarkdownPagedTextViewError::Revoked),
                Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(()),
                Err(cause) => return Err(cause),
            }; // Return the actual Scene strong claim before scrolling; old publication must not span Native geometry changes.
            if !current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let changed = origin.delegate.drag_autoscroll(&request, point, &intent)?;
            if !changed {
                self.stop_original_drag_scroll(timer, &intent);
                return Ok(()); // The actual clip reached BOF/EOF or returned inside Body; a new actual Drag may restart.
            }
            // Native scrolling changed geometry; borrow current Input only while retaining the same original Down, never a successor gesture.
            let after = self.origin()?;
            if !self.cell_pointer_is_current(&intent)
                || !self.owns_drag_scroll(timer, &intent)
                || !self.drag_scroll_point_is_current(point, &intent)
                || !Rc::ptr_eq(&origin.input, &after.input)
                || origin.state != after.state
                || !after.input.target_domain()?.same(&request.target)
                || !after.current(self)
                || !self.cell_pointer_is_current(&intent)
                || !self.drag_scroll_point_is_current(point, &intent)
                || !self.owns_drag_scroll(timer, &intent)
            {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let pointer = self.drag_scroll_pointer(&after, point, intent.clone(), clicks)?;
            self.defer_pointer_location(&after, pointer)
        })();
        if let Err(cause) = result {
            if matches!(&cause, NativeMarkdownPagedTextViewError::Pending) {
                return; // Initial group/Scene still prepares; keep the same timer awaiting completion rather than report Input failure.
            }
            if self.drag_scroll_point_is_current(point, &intent) {
                self.stop_original_drag_scroll(timer, &intent);
            } // A newer Drag replaced this gesture's endpoint; the old tick exits without stopping the timer serving that latest point.
            if !matches!(
                &cause,
                NativeMarkdownPagedTextViewError::Closed
                    | NativeMarkdownPagedTextViewError::Revoked
            ) {
                self.record(Err(cause));
            }
        }
    }

    /// The timer inherits only original Down's mapped caret/word seed, never a gesture inferred from current selection or Source characters.
    fn drag_scroll_pointer(
        &self,
        origin: &Origin,
        point: NSPoint,
        intent: Weak<()>,
        clicks: usize,
    ) -> Result<NativeMarkdownPagedPointerLocation> {
        use commands::DragAnchor;
        let anchor = match self.ivars().drag_anchor.get() {
            Some(DragAnchor::Caret {
                anchor_utf16,
                generation,
            }) if generation == origin.state.generation => {
                Some(NativeMarkdownPagedCellDragAnchor::Caret { anchor_utf16 })
            }
            Some(DragAnchor::Word { anchor, generation })
                if generation == origin.state.generation =>
            {
                if matches!(anchor, NativeMarkdownPagedWordAnchor::Word { .. }) {
                    return Err(NativeMarkdownPagedTextViewError::Pending);
                } // Do not scroll before the actual initial group returns; geometry would revoke its query, and a bare seed cannot re-expand that word.
                Some(NativeMarkdownPagedCellDragAnchor::Word(anchor))
            }
            _ => return Err(NativeMarkdownPagedTextViewError::Pending),
        };
        Ok(NativeMarkdownPagedPointerLocation {
            window_point: point,
            clicks, // Retain actual Down click granularity for triple-click paragraphs/double-click words; Caret/Word slots cannot reconstruct click count.
            extending: true,
            anchor,
            dragging: true,
            intent,
            mouse_up: false,
        })
    }

    /// Up/cancel takes the timer first, then revokes gesture metadata before Drop; destruction cannot access a successor owner's slot.
    pub(super) fn take_drag_scroll(&self) -> Option<DragScroll> {
        self.ivars().drag_scroll.borrow_mut().take()
    }

    /// Focus/menu boundaries end the current timer; old ticks must not use this whole-slot removal entry.
    pub(super) fn stop_drag_scroll(&self) {
        drop(self.take_drag_scroll());
    }

    fn owns_drag_scroll(&self, timer: &NSTimer, intent: &Weak<()>) -> bool {
        self.ivars()
            .drag_scroll
            .borrow()
            .as_ref()
            .is_some_and(|active| {
                std::ptr::eq::<NSTimer>(&*active.timer, timer)
                    && Weak::ptr_eq(&active.intent, intent)
            })
    }

    /// Autoscroll checks the latest finite endpoint around effects; unchanged Input cannot let an old point overwrite reentrant Drag.
    pub(in crate::native_surface) fn drag_scroll_point_is_current(
        &self,
        point: NSPoint,
        intent: &Weak<()>,
    ) -> bool {
        self.ivars()
            .drag_scroll
            .borrow()
            .as_ref()
            .is_some_and(|active| active.point == point && Weak::ptr_eq(&active.intent, intent))
    }

    /// An event inside Body/composition ends its own timer, never a different Down's timer installed during Native reentry.
    fn stop_drag_scroll_intent(&self, intent: &Weak<()>) {
        let old = {
            let mut slot = self.ivars().drag_scroll.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|active| Weak::ptr_eq(&active.intent, intent))
            {
                slot.take()
            } else {
                None
            }
        };
        drop(old);
    }

    /// Each callback removes only its own timer/gesture; failure after Native effects does not authorize stopping a new timer.
    fn stop_original_drag_scroll(&self, timer: &NSTimer, intent: &Weak<()>) {
        let old = {
            let mut slot = self.ivars().drag_scroll.borrow_mut();
            if slot.as_ref().is_some_and(|active| {
                std::ptr::eq::<NSTimer>(&*active.timer, timer)
                    && Weak::ptr_eq(&active.intent, intent)
            }) {
                slot.take()
            } else {
                None
            }
        };
        drop(old);
    }

    /// After child commit transfer only its latest window point; the target checks the same gesture without old Input or a system-event handoff.
    pub(super) fn transfer_drag_scroll_to(&self, parent: &NativeMarkdownPagedTextView) {
        let original = self.take_drag_scroll();
        let target = original
            .as_ref()
            .map(|active| (active.point, active.intent.clone(), active.clicks));
        drop(original); // Child handed off its gesture Rc; invalidate may reenter, and only the same weak identity may continue afterward.
        if let Some((point, intent, clicks)) = target {
            let result = parent.start_drag_scroll(point, &intent, clicks);
            if !matches!(
                &result,
                Err(NativeMarkdownPagedTextViewError::Closed
                    | NativeMarkdownPagedTextViewError::Revoked)
            ) {
                parent.record(result);
            }
        }
    }

    /// Strict close observes the actual timer through weak identity without retaining timer, View, or gesture lifetime.
    pub(in crate::native_surface) fn drag_scroll_weak(&self) -> Option<NativeWeak<AnyObject>> {
        let timer = self
            .ivars()
            .drag_scroll
            .borrow()
            .as_ref()
            .map(|active| active.timer.clone());
        timer.map(|timer| NativeWeak::from(<NSTimer as AsRef<AnyObject>>::as_ref(&timer)))
    }
}
