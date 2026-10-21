//! One foreground operation owns one cancelable result slot. Queued
//! notifications retain only a weak reference to this mailbox.

use std::sync::{Mutex, MutexGuard};

/// Closing and payload publication share one lock, preventing a late worker
/// from publishing business data after its operation was cancelled.
struct MailboxState<T> {
    closed: bool,
    pending: Option<T>,
}

/// Each operation generation has an independent mailbox. Cancellation releases
/// its actual payload synchronously, without waiting for a queued UI notification.
pub(crate) struct CancelableMailbox<T> {
    state: Mutex<MailboxState<T>>,
}

impl<T> CancelableMailbox<T> {
    /// Start without a result; the actual operation owner determines the Arc/Weak lifetime.
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(MailboxState {
                closed: false,
                pending: None,
            }),
        }
    }

    /// Publish the current operation's latest result. Replaced or rejected
    /// payloads are destroyed only after releasing the state lock.
    pub(crate) fn publish(&self, value: T) -> bool {
        // Retain the incoming value locally until transfer, so rejection can
        // also execute arbitrary payload destruction outside the mutex.
        let mut incoming = Some(value);
        let (accepted, discarded) = {
            let (mut state, poisoned) = self.lock_state();
            if poisoned {
                // Interrupted state cannot authorize a result. Permanently close
                // the slot and extract its previous payload for cleanup.
                state.closed = true;
                (false, state.pending.take())
            } else if state.closed {
                // A late worker cannot revive a cancelled operation's result ownership.
                (false, None)
            } else {
                // Replace the result within this same operation. The locked
                // section moves ownership only and never calls T::drop.
                (true, std::mem::replace(&mut state.pending, incoming.take()))
            }
        };
        // Payload destruction may reenter cancel/publish; no MutexGuard remains.
        drop(discarded);
        drop(incoming);
        accepted
    }

    /// Transfer one published result. Closed or poisoned state cannot deliver
    /// a previously retained payload to a later UI operation.
    pub(crate) fn take(&self) -> Option<T> {
        let (result, discarded) = {
            let (mut state, poisoned) = self.lock_state();
            if poisoned {
                // Do not restore interrupted state to a usable result; drain its
                // original payload and release it outside the lock.
                state.closed = true;
                (None, state.pending.take())
            } else if state.closed {
                // Cancellation revoked consumption. Any residual value belongs
                // to cleanup rather than to this caller.
                (None, state.pending.take())
            } else {
                // Transfer an existing value without creating a query or successor task.
                (state.pending.take(), None)
            }
        };
        // Destruction after unlocking permits real resource cleanup to reenter safely.
        drop(discarded);
        result
    }

    /// Observe readiness without consuming a result. A notification arriving
    /// after cancellation/poison cannot reserve a foreground restoration slot.
    pub(crate) fn has_pending(&self) -> bool {
        let (pending, discarded) = {
            let (mut state, poisoned) = self.lock_state();
            if poisoned {
                // Keep the operation revoked; return its original payload for
                // destruction at the same unlocked failure boundary.
                state.closed = true;
                (false, state.pending.take())
            } else {
                (!state.closed && state.pending.is_some(), None)
            }
        };
        drop(discarded);
        pending
    }

    /// Permanently revoke publication and consumption, releasing the actual
    /// mailbox-owned result before returning to the operation owner.
    pub(crate) fn cancel(&self) {
        let discarded = {
            let (mut state, _) = self.lock_state();
            // Poison does not prevent idempotent close or extraction of already-owned resources.
            state.closed = true;
            state.pending.take()
        };
        // Close has linearized; payload destruction may reenter without deadlocking.
        drop(discarded);
    }

    /// Preserve poisoning so callers reject business use, recovering the guard
    /// only to extract and release original resources.
    fn lock_state(&self) -> (MutexGuard<'_, MailboxState<T>>, bool) {
        match self.state.lock() {
            // Healthy access still obeys the actual closed state.
            Ok(guard) => (guard, false),
            // Never clear poisoning: later accesses remain revoked and perform cleanup only.
            Err(error) => (error.into_inner(), true),
        }
    }
}
