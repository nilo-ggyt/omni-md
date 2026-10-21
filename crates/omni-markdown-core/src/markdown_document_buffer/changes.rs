//! Pure change observation before text publication. Native notifications retain
//! their separate TextKit envelope and cannot substitute for actual text changes.

use std::convert::Infallible;
use std::ops::Range;

use super::change_journal::{self, Record};
use super::*;

pub use change_journal::{MarkdownChangeJournalError, MarkdownChangeJournalIoFailure};

#[derive(Clone, Debug, Eq, PartialEq)]
/// One change in real pre-edit coordinates. Emission order preserves separate
/// boundaries for adjacent replacements and insertions at the same position.
pub struct MarkdownTextChange {
    pub old_utf16_range: Range<usize>,
    pub inserted_utf16_len: usize,
}

#[derive(Debug)]
/// Retains typed document, journal, and observer failures. A rollback failure
/// preserves the original cause without recursive derive requirements.
pub enum MarkdownObservedEditError<E> {
    Document(MarkdownDocumentBufferError),
    Journal(MarkdownChangeJournalError),
    Observer(E),
    RollbackUnconfirmed {
        cause: Box<MarkdownObservedEditError<E>>,
        cleanup: MarkdownChangeJournalError,
    },
}
impl<E> From<MarkdownDocumentBufferError> for MarkdownObservedEditError<E> {
    fn from(error: MarkdownDocumentBufferError) -> Self {
        Self::Document(error)
    }
}
impl<E> From<MarkdownChangeJournalError> for MarkdownObservedEditError<E> {
    fn from(error: MarkdownChangeJournalError) -> Self {
        Self::Journal(error)
    }
}
impl<E: std::fmt::Display> std::fmt::Display for MarkdownObservedEditError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document(error) => error.fmt(formatter),
            Self::Journal(error) => error.fmt(formatter),
            Self::Observer(error) => error.fmt(formatter),
            // This known implementation recursively displays the cause without
            // requiring an additional recursive `Box<Self>: Display` bound.
            Self::RollbackUnconfirmed { cause, cleanup } => write!(formatter, "{cause}；{cleanup}"),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for MarkdownObservedEditError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Document(error) => Some(error),
            Self::Journal(error) => Some(error),
            Self::Observer(error) => Some(error),
            Self::RollbackUnconfirmed { cause, .. } => Some(cause.as_ref()),
        }
    }
}

/// Undo/redo returns `None` for empty history. Otherwise it returns the native
/// notification envelope with derived output; failure moves no history receipts.
pub type MarkdownObservedHistoryResult<O> = Result<
    Option<(
        MarkdownHistoryTextEdit,
        <O as MarkdownTextChangeObserver>::Output,
    )>,
    MarkdownObservedEditError<<O as MarkdownTextChangeObserver>::Error>,
>;

/// An empty replacement stream has no notification envelope, but its observer
/// can still validate a pure candidate. These absent-result meanings differ.
pub type MarkdownObservedReplacementsResult<O> = Result<
    (
        Option<MarkdownHistoryTextEdit>,
        <O as MarkdownTextChangeObserver>::Output,
    ),
    MarkdownObservedEditError<<O as MarkdownTextChangeObserver>::Error>,
>;

/// Borrows the candidate root and existing canonical source only. This is not
/// another document, copied snapshot, or escaping native object.
pub struct MarkdownPreparedText<'a> {
    pub(super) document: &'a MarkdownDocumentBuffer,
    pub(super) root: &'a PieceTreeLink,
}

impl MarkdownPreparedText<'_> {
    /// Reads candidate aggregate length without touching mapped bytes or borrowing
    /// mutable document state.
    #[must_use]
    pub fn utf16_len(&self) -> usize {
        piece_tree_metrics(self.root).utf16_len
    }

    /// Validates a scalar boundary in the actual candidate tree and locates its line,
    /// allowing fold-header coordinates to be prepared before publication.
    ///
    /// # Errors
    /// Rejects out-of-range and interior-surrogate positions. Validation neither
    /// temporarily swaps the canonical root nor copies document text.
    pub fn line_for_utf16(&self, position: usize) -> Result<usize, MarkdownDocumentBufferError> {
        let byte = byte_offset_for_utf16_in_tree(self.document, self.root.as_deref(), position, 0)?;
        Ok(metrics_for_tree_prefix(self.document, self.root.as_deref(), byte)?.line_breaks)
    }
}

/// Callbacks must remain pure Rust: no native calls, document reentry, or early
/// publication of foreground folds.
///
/// `changed` can stream preparation for at most 256 folds. `finish` runs only
/// after complete input and candidate-selection validation and may still fail.
/// `Output` must own its values and cannot retain a `PreparedText` borrow. The
/// native caller still verifies its original owner, root, and generation authority.
pub trait MarkdownTextChangeObserver {
    type Output;
    type Error;

    fn changed(&mut self, change: MarkdownTextChange) -> Result<(), Self::Error>;
    fn finish(
        self,
        candidate: MarkdownPreparedText<'_>,
        selection_after: MarkdownTextSelection,
    ) -> Result<Self::Output, Self::Error>;
}

pub(super) struct NoopObserver;
impl MarkdownTextChangeObserver for NoopObserver {
    type Output = ();
    type Error = Infallible;

    fn changed(&mut self, _: MarkdownTextChange) -> Result<(), Infallible> {
        Ok(()) // Unobserved callers still write the real journal; only derived output is omitted.
    }
    fn finish(
        self,
        _: MarkdownPreparedText<'_>,
        _: MarkdownTextSelection,
    ) -> Result<(), Infallible> {
        Ok(())
    }
}

pub(super) fn emit<O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    observer: &mut O,
    range: Range<usize>,
    inserted: usize,
) -> Result<(), MarkdownObservedEditError<O::Error>> {
    let removed = range
        .end
        .checked_sub(range.start)
        .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
    if removed == 0 && inserted == 0 {
        // A true empty change has no ChangeSet segment. A delete user event cannot
        // invent a deletion merely to update internal fold state.
        return Ok(());
    }
    observer
        .changed(MarkdownTextChange {
            old_utf16_range: range.clone(),
            inserted_utf16_len: inserted,
        })
        .map_err(MarkdownObservedEditError::Observer)?;
    document.change_journal.push(Record {
        start: range.start,
        removed,
        inserted,
    })?;
    Ok(())
}

pub(super) fn legacy_error(
    error: MarkdownObservedEditError<Infallible>,
) -> MarkdownDocumentBufferError {
    match error {
        MarkdownObservedEditError::Document(error) => error,
        MarkdownObservedEditError::Observer(never) => match never {},
        MarkdownObservedEditError::Journal(MarkdownChangeJournalError::BudgetExceeded) => {
            MarkdownDocumentBufferError::ChangeJournalLimitExceeded
        }
        MarkdownObservedEditError::Journal(_) => {
            MarkdownDocumentBufferError::ChangeJournalUnavailable
        }
        MarkdownObservedEditError::RollbackUnconfirmed { .. } => {
            MarkdownDocumentBufferError::ChangeJournalCleanupUnconfirmed
        }
    }
}

#[cfg(test)]
mod tests;
