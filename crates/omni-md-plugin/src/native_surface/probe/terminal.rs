//! One terminal notification from the original request. This observer owns no
//! document, Native object, or successor request.

use std::cell::{Cell, RefCell};

#[derive(Default)]
pub(in crate::native_surface) struct ProbeTerminalObserver {
    registered: Cell<bool>,
    pending: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl ProbeTerminalObserver {
    /// An already completed request grants scheduling authority immediately.
    /// Pending retains one notification without polling, retrying, or inventing success.
    pub(in crate::native_surface) fn observe(
        &self,
        completed: Option<bool>,
        observer: impl FnOnce() + 'static,
    ) -> Result<bool, String> {
        if self.registered.replace(true) {
            // One original request has one observer. A second registration must
            // not overwrite or duplicate an already-owned continuation.
            return Err("原请求终点不能重复注册观察".to_owned());
        }
        if completed.is_some() {
            // Both outcomes are actual terminals; the successor must independently
            // verify the operation result instead of treating notification as success.
            return Ok(true);
        }
        *self.pending.borrow_mut() = Some(Box::new(observer));
        Ok(false)
    }

    /// Called only by the original completion publication. Release the slot borrow
    /// before invoking a continuation that may reenter or queue a Host event.
    pub(in crate::native_surface) fn notify(&self) {
        let observer = self.pending.borrow_mut().take();
        if let Some(observer) = observer {
            observer(); // Reentry sees an empty slot and cannot retain or redeliver this callback.
        }
    }
}
