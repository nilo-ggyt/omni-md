//! A synchronous payload-use claim prevents accepted release while a Native
//! callback still has stack-local business values.

use std::cell::Cell;

/// Borrow only the caller's main-thread depth scalar, without retaining business
/// data or Native objects across the synchronous operation.
pub(crate) struct PayloadUseGuard<'a>(&'a Cell<usize>);

impl<'a> PayloadUseGuard<'a> {
    /// Enter a synchronous region that may retain temporary business values while
    /// calling reentrant Native code. The original caller owns the depth scalar.
    pub(crate) fn enter(depth: &'a Cell<usize>) -> Self {
        depth.set(
            depth
                .get()
                .checked_add(1)
                .expect("payload-use nesting overflow"),
        );
        Self(depth)
    }
}

impl Drop for PayloadUseGuard<'_> {
    fn drop(&mut self) {
        // Normal return and unwinding return the same claim. An unbalanced
        // programmer error must not be reported as successful release or left Busy.
        self.0.set(
            self.0
                .get()
                .checked_sub(1)
                .expect("payload-use nesting underflow"),
        );
    }
}
