//! Lend the main-thread cache's unique payload owner. No state borrow crosses
//! Native construction, retain, or destruction that could synchronously reenter.

use std::cell::{Cell, RefCell};

/// Empty payloads allocate no capacity or Native resources. They restore an
/// invalidated slot without recreating an old semantic request.
pub(super) trait CachePayload: Sized {
    fn empty() -> Self;

    /// Invalidate a locally owned Ready payload. Concrete scratch owners may
    /// clear keys while retaining their active session's bounded capacity.
    fn invalidate(&mut self) {
        // Default invalidation drops resources after the slot borrow has ended,
        // allowing Native destructors to reenter the already Loaned owner.
        *self = Self::empty();
    }
}

/// Ready owns one payload, Loaned transfers it exclusively to the synchronous
/// stack, and Closed permanently rejects restoring that payload.
pub(super) struct CacheOwner<T: CachePayload> {
    payload: RefCell<Option<T>>,
    loaned: Cell<bool>,
    closed: Cell<bool>,
    epoch: Cell<u64>,
}

/// An existing loan changes this computation's admission only; it neither
/// poisons the outer owner nor authorizes a second shared cache.
pub(super) enum CacheAdmission<'a, T: CachePayload> {
    Ready(CacheLease<'a, T>),
    Loaned,
    Closed,
}

impl<T: CachePayload> CacheOwner<T> {
    /// Take the existing unique payload without duplicating Native claims or capacity.
    pub(super) fn new(payload: T) -> Self {
        Self {
            payload: RefCell::new(Some(payload)),
            loaned: Cell::new(false),
            closed: Cell::new(false),
            epoch: Cell::new(0),
        }
    }

    /// Move the payload out before the caller performs any reentrant Native operation.
    pub(super) fn begin(&self) -> CacheAdmission<'_, T> {
        if self.closed.get() {
            // An accepted release is terminal, even if AppKit still retains an empty shell.
            return CacheAdmission::Closed;
        }
        if self.loaned.get() {
            // The outer Native stack retains exclusive ownership; nested work
            // must use its existing uncached path rather than alias the payload.
            return CacheAdmission::Loaned;
        }
        let payload =
            { self.payload.borrow_mut().take() }.expect("ready cache owns exactly one payload");
        self.loaned.set(true);
        CacheAdmission::Ready(CacheLease {
            owner: self,
            payload: Some(payload),
            epoch: self.epoch.get(),
            restore: true,
        })
    }

    /// Advance epoch even during a loan, so its old payload cannot later return as current.
    pub(super) fn invalidate(&self) {
        self.invalidate_loans();
        if let CacheAdmission::Ready(mut lease) = self.begin() {
            // Invalidate locally and reuse permitted capacity; Native destructor
            // reentry still observes Loaned throughout this operation.
            lease.payload_mut().invalidate();
        }
    }

    /// Retain reusable Ready content while revoking an older semantic request's
    /// loan. An old owner cannot be restored under the new epoch.
    pub(super) fn invalidate_loans(&self) {
        self.advance_epoch();
    }

    /// Requests capture scalar epoch only; the original lease alone owns payload return.
    pub(super) fn epoch(&self) -> u64 {
        self.epoch.get()
    }

    /// Close admission first. An outstanding synchronous lease must actually
    /// destroy its payload before the closed owner can report release.
    pub(super) fn close(&self) {
        self.closed.set(true);
        self.advance_epoch();
        if !self.loaned.get() {
            // Move out the final owner before destruction; no RefMut remains
            // while Native fields release or synchronously call back.
            self.loaned.set(true);
            let previous = { self.payload.borrow_mut().take() };
            drop(previous);
            self.loaned.set(false);
        }
    }

    /// Only Closed with every loan actually returned proves that payload, keys,
    /// and retained capacity are absent.
    pub(super) fn is_released(&self) -> bool {
        self.closed.get() && !self.loaned.get() && self.payload.borrow().is_none()
    }

    fn advance_epoch(&self) {
        // This slot alone advances epoch. Exhaustion is a programming invariant
        // failure rather than permission to wrap and admit an old request.
        self.epoch.set(
            self.epoch
                .get()
                .checked_add(1)
                .expect("cache epoch overflow"),
        );
    }
}

/// Own the concrete payload synchronously. Keep Loaned until destruction ends,
/// so reentrant Native release cannot observe a false completed release.
pub(super) struct CacheLease<'a, T: CachePayload> {
    owner: &'a CacheOwner<T>,
    payload: Option<T>,
    epoch: u64,
    restore: bool,
}

impl<T: CachePayload> CacheLease<'_, T> {
    /// The payload resides on this stack, outside RefCell. Reentry sees Loaned
    /// and cannot acquire an alias to this mutable value.
    pub(super) fn payload_mut(&mut self) -> &mut T {
        self.payload.as_mut().expect("live cache lease payload")
    }

    /// Discard after the caller rejects document identity, generation, or close
    /// authority; an old request cannot write its payload back into the live slot.
    pub(super) fn discard(&mut self) {
        self.restore = false;
    }
}

impl<T: CachePayload> Drop for CacheLease<'_, T> {
    fn drop(&mut self) {
        let payload = self.payload.take();
        if self.restore && !self.owner.closed.get() && self.epoch == self.owner.epoch.get() {
            // Move the unique owner without Native retain; the slot remained
            // empty for this lease's entire exclusive loan.
            let previous = self.owner.payload.replace(payload);
            drop(previous);
        } else {
            // Destroy the old value first; its destructor may invalidate or
            // close again, so restoration must recheck Closed afterwards.
            drop(payload);
            if !self.owner.closed.get() {
                // Empty performs no allocation or Native work. A later real
                // query may populate this still-open slot on demand.
                let empty = T::empty();
                let previous = self.owner.payload.replace(Some(empty));
                drop(previous);
            }
        }
        self.owner.loaned.set(false);
    }
}
