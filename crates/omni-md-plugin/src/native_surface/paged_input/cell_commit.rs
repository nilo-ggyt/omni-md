//! Prepare bounded admission, then let the original controller revoke Scenes and consume that same ticket without recapture or repeat sanitization.

use super::*;
use omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableRowSource;
use omni_markdown_core::{
    MarkdownCellDraftCommitError, MarkdownCellDraftCommitReceipt, MarkdownCellDraftPreparedCommit,
    MarkdownCellDraftWordAnchor, MarkdownViewportSourceAuthority,
};

#[cfg(test)]
#[path = "cell_commit/body_tests.rs"]
mod body_tests;

/// Core admission owns the sole result: bounded small strings stay on the heap, while long output is sealed anonymous storage.
/// This capsule adds only weak original Input/session identities and epoch facts, without copying output or retaining the draft.
pub struct NativeMarkdownPagedCellPreparedCommit {
    owner: Weak<NativeMarkdownPagedInput>,
    session: Weak<NativeMarkdownPagedCellSession>,
    epoch: u64,
    before: NativeMarkdownPagedInputState,
    prepared: MarkdownCellDraftPreparedCommit,
}

impl NativeMarkdownPagedCellPreparedCommit {
    #[must_use]
    /// Use only to query the next cell after actual commit; preparation authorizes neither caret/Source access nor early activation.
    pub fn next_cell(&self) -> Option<(usize, usize)> {
        self.prepared.next_cell()
    }
}

/// Deliver the original Body commit and same-transaction draft endpoints; consume coordinates only while original Body authority remains current.
/// The receipt itself changes no Body selection and cannot replace next-cell Source or successor input admission.
pub struct NativeMarkdownPagedCellCommit {
    pub outcome: NativeMarkdownPagedInputOutcome,
    pub receipt: MarkdownCellDraftCommitReceipt,
}

#[derive(Debug)]
pub enum NativeMarkdownPagedCellCommitError<E> {
    Input(NativeMarkdownPagedInputError),
    Commit(MarkdownCellDraftCommitError<E>),
}

impl<E: std::fmt::Display> std::fmt::Display for NativeMarkdownPagedCellCommitError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(error) => error.fmt(formatter),
            Self::Commit(error) => error.fmt(formatter),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for NativeMarkdownPagedCellCommitError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Input(error) => Some(error),
            Self::Commit(error) => Some(error),
        }
    }
}

impl<E> From<NativeMarkdownPagedInputError> for NativeMarkdownPagedCellCommitError<E> {
    fn from(error: NativeMarkdownPagedInputError) -> Self {
        Self::Input(error)
    }
}

impl NativeMarkdownPagedInput {
    /// Consume the same Cell admission and publish the sole canonical transaction through the existing Body Input owner.
    /// The original controller must revoke/return both Scenes first; do not prepare again, rebuild Body, or recapture authority here.
    ///
    /// # Errors
    /// Reject foreign document/Cell/Body authority, marked input, borrow conflicts, and observer failure before publication, retaining the draft.
    /// The returned Body receipt reports later foreground changes separately; published text remains committed.
    pub fn commit_cell_to_body(
        self: &Rc<Self>,
        prepared: NativeMarkdownPagedCellPreparedCommit,
        body: &Rc<NativeMarkdownPagedInput>,
        body_permit: &NativeMarkdownPagedInputPermit,
        cell_current: &(impl Fn(u64) -> bool + ?Sized),
        body_current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> std::result::Result<
        NativeMarkdownPagedCellCommit,
        NativeMarkdownPagedCellCommitError<NativeMarkdownPagedInputObserverError>,
    > {
        // Reject a foreign prepared owner before reading its document; then preserve Source errors through this owner's own session.
        if !Weak::ptr_eq(&prepared.owner, &Rc::downgrade(self)) {
            return Err(NativeMarkdownPagedInputError::Revoked.into());
        }
        let session = prepared
            .session
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::Closed)?;
        session.facts()?;
        let own_session = self
            .target
            .cell()?
            .ok_or(NativeMarkdownPagedInputError::BodyTargetRequired)?;
        if !Rc::ptr_eq(&own_session, &session) || body.is_cell() || body_permit.cell.is_some() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired.into());
        }
        let original = session.document()?;
        if !Weak::ptr_eq(&body.document_identity(), &Rc::downgrade(&original)) {
            return Err(NativeMarkdownPagedInputError::Revoked.into());
        } // Equal bytes, generation, or local range cannot replace original canonical document identity.

        self.check_original(prepared.epoch, prepared.before, cell_current)?;
        if self.busy.replace(true) {
            return Err(NativeMarkdownPagedInputError::Busy.into());
        }
        let _cell_mutation = Mutation(&self.busy);
        let _body_mutation = body.begin(body_permit, body_current)?;
        if body_permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive.into());
        } // Cell commit cannot take Body's IME candidate; Core admission independently rejects marked Cell input.

        // Cross-owner current calls can reenter, but both busy admissions are held. Finish callbacks before borrowing document/folds.
        // Only pure Core preparation/publication follows; observers must not call Native or a newer current callback.
        session.facts()?;
        self.check_original(prepared.epoch, prepared.before, cell_current)?;
        body.check_permit(body_permit, body_current)?;
        self.check_original(prepared.epoch, prepared.before, cell_current)?;
        let observer = body.observer(body_permit.before)?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut folds = body
            .folds
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !body.matches_document(&document, body_permit.before)
            || !self.metadata_matches(prepared.before)
        {
            return Err(NativeMarkdownPagedInputError::StateChanged.into());
        }
        let result = document
            .commit_prepared_cell_draft_observed(prepared.prepared, observer)
            .map_err(NativeMarkdownPagedCellCommitError::Commit)?;
        let outcome = match result.edit {
            Some((edit, mapped)) => {
                // Canonical text is published. publish exchanges only bounded metadata/selection from the original FoldObserver.
                // No current, Native, or fallible I/O follows; the receipt belongs to original Body, never Cell-local coordinates.
                NativeMarkdownPagedInputOutcome::Committed(body.publish(
                    body_permit,
                    &document,
                    mapped,
                    None,
                    Some(edit),
                    &mut folds,
                ))
            }
            None => {
                // Core's unchanged path runs no observer, moves no canonical selection, and creates no empty Undo entry.
                // Do not invent PreparedFolds or advance Body serial; end only the original local session.
                NativeMarkdownPagedInputOutcome::Unchanged(body_permit.before)
            }
        };
        self.seal_committed_cell(&session);
        Ok(NativeMarkdownPagedCellCommit {
            outcome,
            receipt: result.receipt,
        })
    }

    /// Seal local metadata only after Core success, with no fallible close or subsequent Source read.
    fn seal_committed_cell(&self, session: &NativeMarkdownPagedCellSession) {
        self.closed.set(true);
        self.marked.set(None);
        if self.selection.serial() != u64::MAX {
            self.selection.seal();
        }
        session.mark_committed();
    }

    /// Prepare the sole admission output while both Scene readers remain live; bound small strings and seal long output in anonymous storage.
    ///
    /// # Errors
    /// Reject invalid permit/Source, marked input, document budget failure, candidate storage failure, or existing admission.
    /// Failure clears neither Scenes, draft/local history, nor canonical text.
    pub fn prepare_cell_commit(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        append: Option<(
            &VerifiedMarkdownTableRowSource,
            MarkdownViewportSourceAuthority,
        )>,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedCellPreparedCommit> {
        let _mutation = self.begin(permit, current)?;
        let cell = permit
            .cell
            .as_ref()
            .ok_or(NativeMarkdownPagedInputError::BodyTargetRequired)?;
        self.check_permit(permit, current)?;
        let session = cell.session()?;
        let original = session.document()?;
        let prepared = {
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            // Both actions share original input admission and the sole candidate slot; End requires a verified terminal from the same Source request.
            // Do not commit then append separately, or manufacture missing End authority from raw row/column/range values.
            let edit = cell.take()?;
            match append {
                Some((terminal, authority)) => document.prepare_cell_draft_row_append(
                    edit,
                    terminal,
                    authority,
                    word_anchor,
                )?,
                None => document.prepare_cell_draft_commit(edit, word_anchor)?,
            }
        };
        session.facts()?; // Check Source before reentrant foreground authority; admission changes neither original serial nor draft.
        self.check_original(permit.epoch, permit.before, current)?;
        Ok(NativeMarkdownPagedCellPreparedCommit {
            owner: Rc::downgrade(self),
            session: Rc::downgrade(&session),
            epoch: permit.epoch,
            before: permit.before,
            prepared,
        })
    }
}
