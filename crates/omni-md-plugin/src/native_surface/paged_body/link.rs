//! Resolve one weak click through the existing bounded RootLeaf producer before any external effect.

use super::*;
mod heading;

/// One continuation owns either a clicked target or a heading query, never a second document or slug index.
pub(super) struct PendingLink {
    task: LinkTask,
}

enum LinkTask {
    // Allocate once per admitted gesture, keeping the idle slot small. Pending source
    // quanta move this same Box back into the slot instead of reallocating its Scene key.
    Click(Box<PendingClick>),
    Heading(Box<heading::PendingHeading>),
}

impl PendingLink {
    /// Retire after removing the slot; explicitly return heading scratch-close errors instead of hiding them in Drop.
    pub(super) fn retire(mut self) -> std::result::Result<(), ViewError> {
        match &mut self.task {
            // A click retains only a weak gesture and immutable coordinates, so no external cleanup is needed.
            LinkTask::Click(_) => Ok(()),
            // The Core query owns scratch resources; cancellation must settle them even before a target is found.
            LinkTask::Heading(pending) => pending.query.cancel().map_err(ViewError::HeadingAnchor),
        }
    }
}

/// Retain no Scene, reader, document, or native object; dropping the View's gesture revokes this weak intent.
struct PendingClick {
    key: NativeMarkdownPagedSceneKey,
    index: usize,
    intent: Weak<()>,
}

impl NativeMarkdownPagedBody {
    /// Accept only a hit already confirmed by mouse-up on its original Scene.
    /// Parsing and URL materialization stay off the pointer callback, leaving ordinary dragging unchanged.
    pub(super) fn follow_link(
        &self,
        key: &NativeMarkdownPagedSceneKey,
        index: usize,
        intent: Weak<()>,
    ) -> std::result::Result<(), ViewError> {
        // A stale Scene or ended gesture cannot acquire another continuation using the same numeric position.
        if !self.scene_current(key) || intent.strong_count() == 0 {
            return Err(ViewError::Revoked);
        }
        // Replace the pure Rust slot before closing the previous query; no RefCell borrow crosses its cleanup.
        let previous = self.pending_link.replace(Some(PendingLink {
            task: LinkTask::Click(Box::new(PendingClick {
                key: key.clone(),
                index,
                intent,
            })),
        }));
        if let Some(previous) = previous {
            previous.retire()?;
        }
        self.schedule().map_err(|_| ViewError::Revoked)
    }

    /// Advance at most the existing 4096-unit quantum; an in-flight FontTrial wakes its own continuation.
    pub(super) fn advance_link(&self, request: &NativeMarkdownPagedSceneRequest) -> Result<bool> {
        // Take ownership for this turn; asynchronous completion must not keep a mutable slot borrow alive.
        let Some(pending) = self.pending_link.borrow_mut().take() else {
            return Ok(false);
        };
        let pending = match pending.task {
            LinkTask::Click(pending) => pending,
            LinkTask::Heading(pending) => return self.advance_heading(*pending, request),
        };
        let view = self.view()?;
        let current = || view.link_intent_current(&pending.key, &pending.intent);
        // Expected gesture retirement ends the click; real Source/font/resource errors still propagate.
        let live = match current() {
            Ok(live) => live,
            Err(ViewError::Revoked | ViewError::Closed | ViewError::Pending) => false,
            Err(cause) => return Err(cause.into()),
        };
        if !pending.key.request.same(request) || !live {
            view.retire_link_intent(&pending.intent);
            return Ok(false); // New input, focus, or width retires the old target without borrowing a successor Scene.
        }
        // Borrow the one producer by ownership, not a RefCell lease; restore it before native callbacks can reenter.
        let mut producer = self
            .producer
            .borrow_mut()
            .take()
            .expect("发布Scene的活动Body持唯一Producer");
        let result = producer.advance_link(self, &pending.key, pending.index);
        let waiting_font = producer.has_in_flight();
        *self.producer.borrow_mut() = Some(producer); // The original producer is back in its slot before any later native work.
        // The bounded source step may revoke its caller; recheck the same gesture, not newly captured input.
        let live = match current() {
            Ok(live) => live,
            Err(ViewError::Revoked | ViewError::Closed | ViewError::Pending) => false,
            Err(cause) => return Err(cause.into()),
        };
        match result {
            Err(cause) if cause.is_foreground_revocation() && !live => {
                view.retire_link_intent(&pending.intent);
                self.schedule()?; // Producer already checked Source health; the normal next turn retires stale layout.
            }
            Err(cause) => return Err(cause),
            Ok(_) if !live => {
                // Even a resolved URI has no authority to open after its original click has ended.
                view.retire_link_intent(&pending.intent);
                self.schedule()?;
            }
            Ok(None) => {
                // Native reentry may admit a newer click. Restore this continuation only into an untouched slot.
                if self.pending_link.borrow().is_none() {
                    *self.pending_link.borrow_mut() = Some(PendingLink {
                        task: LinkTask::Click(pending),
                    });
                }
                if !waiting_font {
                    // No worker owns the wake, so schedule the next bounded turn instead of spinning here.
                    self.schedule()?;
                }
            }
            Ok(Some(link)) => {
                if let Some(link) = link {
                    // This is the finite URI certified by the semantic page, not the source paragraph.
                    // Materialize it only at the effect boundary after restoring the producer and rechecking the owner.
                    let destination = link.destination.into_string();
                    view.finish_link_activation(&pending.key, &pending.intent, &destination);
                } else {
                    view.retire_link_intent(&pending.intent);
                } // A covered non-link is a completed lookup; unknown coverage must never masquerade as None.
            }
        }
        Ok(true)
    }
}
