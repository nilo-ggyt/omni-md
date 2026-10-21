//! Formatting consumes the original domain enum and permit; Core's shared-piece plan owns text, history, and actual selection.

use super::*;
use omni_markdown_core::MarkdownFormatAction;

impl NativeMarkdownPagedInput {
    /// Apply one format transaction to the original directed Body/Cell selection, retaining Core block/inline-inner semantics.
    ///
    /// Finish the format plan, actual-change observer, and history together; afterward publish bounded Input metadata only.
    /// Window/input revocation after the receipt can stop presentation only; it never authorizes replay of this format action.
    ///
    /// # Errors
    /// Marked input, unknown multi-range direction, stale permit, or Core budget/observer failure preserves text/history.
    /// Cell accepts its eight inline actions; Body block actions fail explicitly without committing the draft or editing canonical text.
    pub fn apply_format(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        action: MarkdownFormatAction,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        let _mutation = self.begin(permit, current)?;
        self.check_permit(permit, current)?;
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        } // The IME owns composition text; formatting cannot force confirmation/cancellation and recapture a newer selection.
        if !permit.before.selection_is_directed {
            return Err(NativeMarkdownPagedInputError::MultipleSelectionInput);
        } // Core formatting requires one directed selection; a multi-range memento cannot stand in for the primary and discard others.
        if let Some(cell) = &permit.cell {
            // This permit's Cell capsule belongs to the actual target; use its original session without recapturing local input from canonical coordinates.
            let session = cell.session()?;
            let original = session.document()?;
            let mut document = original
                .try_borrow_mut()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            let (facts, edit) = document.format_cell_draft(cell.take()?, action)?;
            let commit = self.publish_cell(
                permit,
                facts,
                Some((edit.replaced_utf16_range, edit.change_in_utf16_length)),
                None,
            ); // Core published local root/history once; reuse metadata publication without running Body observation or formatting again.
            drop(document);
            drop(original);
            drop(session);
            return Ok(commit); // After success neither call current nor read Source; the original receipt checks revoked display effects.
        }
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let observer = self.observer(permit.before)?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut folds = self
            .folds
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        // Only Core generates markers, shares selected pieces, and prepares actual changes; Native keeps no duplicate format/tag table.
        let (edit, prepared) =
            document.apply_format_observed(action, permit.before.selection, observer)?;
        let commit = self.publish(permit, &document, prepared, None, Some(edit), &mut folds); // Core published; no Native/current/fallible postcommit I/O occurs before the receipt, so success cannot become rejection.
        drop(folds);
        drop(document);
        drop(original);
        Ok(commit)
    }
}
