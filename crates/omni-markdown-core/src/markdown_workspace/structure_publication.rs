//! Revoke workspace traversal at its first visible structural change and again
//! after rollback, closing scans that started while the operation was in progress.

use super::{MarkdownSessionAccess, MarkdownWorkspaceError};
use std::cell::Cell;

/// Declare before file/rollback guards so this owner drops last. It retains only
/// a borrowed session and scalar facts, with no I/O or native object of its own.
pub(super) struct WorkspaceStructurePublication<'session> {
    session: &'session MarkdownSessionAccess,
    invalidate_on_finish: Cell<bool>,
    rollback_unconfirmed: Cell<bool>,
}

impl<'session> WorkspaceStructurePublication<'session> {
    /// Start inactive: preflight rejection, collisions, and no-ops do not prove
    /// that the filesystem collection actually changed.
    pub(super) fn new(session: &'session MarkdownSessionAccess) -> Self {
        Self {
            session,
            invalidate_on_finish: Cell::new(false),
            rollback_unconfirmed: Cell::new(false),
        }
    }

    /// Lend the same confinement root to acquired-file validation without
    /// opening another directory or creating a second resource owner.
    pub(super) fn canonical_root(&self) -> &std::path::Path {
        &self.session.canonical_root
    }

    /// Revoke immediately after actual mkdir, exclusive open, rename, or unlink,
    /// before a later fallible stage can return without invalidating old pages.
    pub(super) fn changed(&self) {
        self.invalidate_on_finish.set(true);
        self.session.cancel_active_search();
    }

    /// Standard recursive removal can fail after partial deletion without reporting
    /// progress. Revoke conservatively after pure protection checks and before the
    /// attempt; do not rewrite its link-race handling merely to count exact mutations.
    pub(super) fn recursive_delete_attempt(&self) {
        self.invalidate_on_finish.set(true);
        self.session.cancel_active_search();
    }

    /// Retain files whose complete-write ownership cannot be proved. Record that
    /// fact without letting Drop replace the operation's earlier failure.
    pub(super) fn rollback_unconfirmed(&self) {
        self.rollback_unconfirmed.set(true);
        self.changed();
    }

    /// Combine cleanup uncertainty with the original result only after all file
    /// guards have finished inside the operation's private scope.
    pub(super) fn finish<T>(
        &self,
        result: Result<T, MarkdownWorkspaceError>,
    ) -> Result<T, MarkdownWorkspaceError> {
        if self.rollback_unconfirmed.get() {
            // Allocate only for unconfirmed cleanup. Preserve an existing operation
            // failure as the inspectable source; absent one, report storage failure.
            let cause = result
                .err()
                .unwrap_or(MarkdownWorkspaceError::StorageUnavailable);
            Err(MarkdownWorkspaceError::RollbackUnconfirmed {
                cause: Box::new(cause),
            })
        } else {
            // Successful operations and confirmed rollback retain their original result.
            result
        }
    }
}

impl Drop for WorkspaceStructurePublication<'_> {
    /// Close scans started during mutation after every inner rollback owner ended.
    fn drop(&mut self) {
        if self.invalidate_on_finish.get() {
            // A mid-operation worker may have observed a partial file or a name
            // later rolled back. File guards have already ended; neither successful
            // nor failed cleanup can restore that worker's obsolete authority.
            self.session.cancel_active_search();
        }
    }
}

#[cfg(test)]
mod tests;
