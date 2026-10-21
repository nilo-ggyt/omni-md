//! A finite foreground-restoration signal for workspace results. Only a weak
//! mailbox is retained, without document text, Native objects, or a second worker.

use crate::cancelable_mailbox::CancelableMailbox;
use omni_markdown_core::{
    MarkdownSearchPage, MarkdownWorkspaceError, MarkdownWorkspacePage, MarkdownWorkspacePager,
};
use std::sync::Weak;

pub(crate) type DirectoryResult =
    Result<(MarkdownWorkspacePager, MarkdownWorkspacePage), MarkdownWorkspaceError>;
pub(crate) type SearchResult = Result<MarkdownSearchPage, MarkdownWorkspaceError>;

/// Fix delivery authority when the actual worker starts. Foreground restoration
/// cannot acquire the generation of a later workspace query.
pub(crate) struct WorkspaceDelivery<T> {
    pub(crate) owner: usize,
    pub(crate) generation: u64,
    pub(crate) intent: u64,
    pub(crate) mailbox: Weak<CancelableMailbox<T>>,
}

impl<T> Clone for WorkspaceDelivery<T> {
    fn clone(&self) -> Self {
        // Cloning std Weak copies control-block metadata, without retaining or
        // loading an Objective-C object or a mailbox payload.
        Self {
            owner: self.owner,
            generation: self.generation,
            intent: self.intent,
            mailbox: Weak::clone(&self.mailbox),
        }
    }
}

/// Directory and search share the existing single worker, so separate variants
/// cannot each reserve an independently current result slot.
#[derive(Clone)]
pub(crate) enum WorkspaceReadySignal {
    Directory(WorkspaceDelivery<DirectoryResult>),
    Search(WorkspaceDelivery<SearchResult>),
}

impl WorkspaceReadySignal {
    pub(crate) fn owner(&self) -> usize {
        // This identity associates the original notification only; an integer
        // cannot establish current window liveness or authorize a new query.
        match self {
            Self::Directory(value) => value.owner,
            Self::Search(value) => value.owner,
        }
    }

    /// Readiness means an uncancelled payload exists. Actual delivery still
    /// requires the exact mailbox, owner, mode, generation, and intent.
    pub(crate) fn has_pending(&self) -> bool {
        match self {
            Self::Directory(value) => value
                .mailbox
                .upgrade()
                .is_some_and(|mailbox| mailbox.has_pending()),
            Self::Search(value) => value
                .mailbox
                .upgrade()
                .is_some_and(|mailbox| mailbox.has_pending()),
        }
    }

    pub(crate) fn cancel(&self) {
        // Upgrade only to cancel this original mailbox. Its payload is released
        // by that owner outside its mutex; this signal never starts replacement work.
        match self {
            Self::Directory(value) => {
                if let Some(mailbox) = value.mailbox.upgrade() {
                    mailbox.cancel();
                }
            }
            Self::Search(value) => {
                if let Some(mailbox) = value.mailbox.upgrade() {
                    mailbox.cancel();
                }
            }
        }
    }

    fn same(&self, other: &Self) -> bool {
        fn same<T>(a: &WorkspaceDelivery<T>, b: &WorkspaceDelivery<T>) -> bool {
            // Equal scalar inputs are insufficient if the operation has already
            // acquired another mailbox. Retain the original weak allocation identity.
            a.owner == b.owner
                && a.generation == b.generation
                && a.intent == b.intent
                && Weak::ptr_eq(&a.mailbox, &b.mailbox)
        }
        match (self, other) {
            (Self::Directory(a), Self::Directory(b)) => same(a, b),
            (Self::Search(a), Self::Search(b)) => same(a, b),
            _ => false, // Directory and search variants cannot share one delivery authority.
        }
    }
}

/// The slot moves weak signals only. Its caller performs mailbox observation,
/// cancellation, and extracted-value destruction outside the RefCell borrow.
#[derive(Default)]
pub(crate) struct WorkspaceReadySlot {
    pending: Option<WorkspaceReadySignal>,
}

impl WorkspaceReadySlot {
    pub(crate) fn snapshot(&self) -> Option<WorkspaceReadySignal> {
        // A copied weak signal authorizes only comparison after an unlocked
        // readiness check; it neither consumes nor retains the actual result.
        self.pending.clone()
    }

    /// Compare against the weak snapshot captured before the unlocked readiness
    /// check. Reentrant replacement revokes that observation rather than trusting it.
    pub(crate) fn park(
        &mut self,
        incoming: WorkspaceReadySignal,
        expected: Option<&WorkspaceReadySignal>,
        old_has_pending: bool,
    ) -> Result<Option<WorkspaceReadySignal>, WorkspaceReadySignal> {
        if let Some(old) = self.pending.as_ref() {
            if old.same(&incoming) {
                return Ok(Some(incoming)); // Coalesce this same notification without replacing its original signal.
            }
            if old_has_pending || expected.is_none_or(|snapshot| !old.same(snapshot)) {
                return Err(incoming); // Pending work or a stale comparison cannot authorize overwriting another signal.
            }
        }
        Ok(self.pending.replace(incoming))
    }

    pub(crate) fn take(&mut self) -> Option<WorkspaceReadySignal> {
        // Transfer notification ownership only; the receiving boundary separately
        // validates the original mailbox before taking its business result.
        self.pending.take()
    }

    pub(crate) fn cancel_owner(&mut self, owner: usize) -> Option<WorkspaceReadySignal> {
        if self
            .pending
            .as_ref()
            .is_some_and(|signal| signal.owner() == owner)
        {
            self.pending.take()
        } else {
            None
        } // A foreign cancellation cannot extract another window's signal.
    }
}
