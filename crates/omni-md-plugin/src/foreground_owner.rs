//! Temporarily lends the unique foreground owner without holding its RefCell across native callbacks.
//!
//! A Copy identity gates reentry while the native owner is local. Restoration occurs on normal return,
//! operation failure and Rust unwinding; this mechanism never announces a host lifecycle transition.

use std::cell::{Cell, RefCell};

/// Acquisition failure occurs before native effects or ownership transfer to the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForegroundOwnerError {
    Busy,
    Missing,
}

/// Restores the exact temporarily extracted owner before releasing its scalar reentry gate.
struct ForegroundOwnerLease<'a, T, F: FnOnce()> {
    slot: &'a RefCell<Option<T>>,
    busy_identity: &'a Cell<Option<usize>>,
    owner: Option<T>,
    on_restored: Option<F>,
}

impl<T, F: FnOnce()> Drop for ForegroundOwnerLease<'_, T, F> {
    fn drop(&mut self) {
        // Acquisition/installation paths reject the busy gate, so this slot cannot contain a successor.
        // Restore ownership before callbacks can once again acquire it; no native value is dropped here.
        let mut slot = self.slot.borrow_mut();
        assert!(
            slot.is_none(),
            "a foreground successor bypassed its ownership gate"
        );
        *slot = self.owner.take();
        drop(slot);
        self.busy_identity.set(None);
        if let Some(on_restored) = self.on_restored.take() {
            // Accepted native completion can become ready during reentry; release its continuation
            // only after ownership and the scalar gate are restored, including the unwind path.
            on_restored();
        }
    }
}

/// Execute one operation with local unique ownership and no outstanding global-state borrow.
pub(crate) fn with_foreground_owner<T, R>(
    slot: &RefCell<Option<T>>,
    busy_identity: &Cell<Option<usize>>,
    identify: impl FnOnce(&T) -> usize,
    operation: impl FnOnce(&mut T) -> R,
    on_restored: impl FnOnce(),
) -> Result<R, ForegroundOwnerError> {
    if busy_identity.get().is_some() {
        // Reentry must fail before consulting the temporarily empty slot or touching native state.
        return Err(ForegroundOwnerError::Busy);
    }
    let owner = slot
        .borrow_mut()
        .take()
        .ok_or(ForegroundOwnerError::Missing)?;
    // Install the restoration guard before invoking even the pure identity callback, so unwinding
    // there cannot drop the unique owner or strand a busy gate. Zero is an acquisition-only sentinel.
    busy_identity.set(Some(0));
    let mut lease = ForegroundOwnerLease {
        slot,
        busy_identity,
        owner: Some(owner),
        on_restored: Some(on_restored),
    };
    // Identity is metadata only; the local lease remains the sole strong foreground owner.
    busy_identity.set(Some(identify(
        lease.owner.as_ref().expect("the acquired owner is local"),
    )));
    Ok(operation(
        lease.owner.as_mut().expect("the acquired owner is local"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    /// Reject nested acquisition without effects and restore the exact mutated owner on normal return.
    #[test]
    fn local_owner_has_no_slot_borrow_and_reentry_is_rejected() {
        let slot = RefCell::new(Some((31_usize, 0_usize)));
        let busy = Cell::new(None);
        let restored_count = Cell::new(0);
        let result = with_foreground_owner(
            &slot,
            &busy,
            |owner| owner.0,
            |owner| {
                // A native callback can inspect state without a RefCell panic, but it cannot acquire another owner.
                assert!(slot.borrow().is_none());
                assert_eq!(busy.get(), Some(31));
                assert_eq!(
                    with_foreground_owner(
                        &slot,
                        &busy,
                        |_| 0,
                        |_| panic!("reentry ran"),
                        || panic!("rejected restoration ran")
                    ),
                    Err(ForegroundOwnerError::Busy)
                );
                owner.1 += 1;
                Err::<(), _>("operation vetoed")
            },
            || {
                // Rejected operations still return the exact owner before accepted work may resume.
                assert_eq!(*slot.borrow(), Some((31, 1)));
                assert_eq!(busy.get(), None);
                restored_count.set(restored_count.get() + 1);
            },
        );
        assert_eq!(restored_count.get(), 1);
        assert_eq!(result, Ok(Err("operation vetoed")));
        assert_eq!(*slot.borrow(), Some((31, 1)));
        assert_eq!(busy.get(), None);
    }

    /// Unwinding restores unique ownership instead of prematurely dropping the canonical resource graph.
    #[test]
    fn unwind_restores_owner_before_clearing_busy_identity() {
        let canonical = Rc::new(());
        let weak = Rc::downgrade(&canonical);
        let slot = RefCell::new(Some(canonical));
        let busy = Cell::new(None);
        let restored_count = Cell::new(0);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = with_foreground_owner(
                &slot,
                &busy,
                |_| 17,
                |_| {
                    // Simulate failure after a synchronous native callback, while ownership is still local.
                    assert!(slot.borrow().is_none());
                    panic!("synthetic native operation panic");
                },
                || {
                    // Unwinding must not strand a completion that arrived while this lease was busy.
                    assert!(slot.borrow().is_some());
                    assert_eq!(busy.get(), None);
                    restored_count.set(restored_count.get() + 1);
                },
            );
        }));
        assert!(unwind.is_err());
        assert_eq!(restored_count.get(), 1);
        assert!(slot.borrow().is_some());
        assert_eq!(busy.get(), None);
        assert_eq!(weak.strong_count(), 1);
        // Actual resource destruction belongs to explicit owner removal rather than temporary extraction.
        slot.borrow_mut().take();
        assert!(weak.upgrade().is_none());
    }

    /// A missing foreground owner neither executes work nor leaves a spurious busy gate behind.
    #[test]
    fn missing_owner_preserves_empty_slot_and_gate() {
        let slot = RefCell::<Option<()>>::new(None);
        let busy = Cell::new(None);
        assert_eq!(
            with_foreground_owner(
                &slot,
                &busy,
                |_| 0,
                |_| panic!("missing owner ran"),
                || panic!("missing restoration ran")
            ),
            Err(ForegroundOwnerError::Missing)
        );
        assert_eq!(busy.get(), None);
        assert!(slot.borrow().is_none());
    }
}
