//! Find replacement consumes original coordinates; one Core transaction publishes text, history, and fold anchors together.

use super::*;
use omni_markdown_core::{MarkdownReplacement, MarkdownReplacementPart};

impl NativeMarkdownPagedInput {
    /// Consume ordered, disjoint replacements in old coordinates, keeping complete original anchor/head as the sole history before.
    ///
    /// Search ranges belong to individual replacements, never an envelope substituted for original selection. Both iterator layers return
    /// plain data only: no Native/current or document reentry, because Core consumes them under one short mutable borrow.
    ///
    /// # Errors
    /// Active composition, lost permit, invalid ranges, stream failure, or resource exhaustion rolls back completely. Every error precedes
    /// Core publication; only the returned commit's check_current reports postcommit authority changes.
    pub fn apply_replacements_observed<'a, I, P>(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        replacements: I,
        after: MarkdownTextSelection,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome>
    where
        I: IntoIterator<
            Item = std::result::Result<MarkdownReplacement<P>, MarkdownDocumentBufferError>,
        >,
        P: IntoIterator<
            Item = std::result::Result<MarkdownReplacementPart<'a>, MarkdownDocumentBufferError>,
        >,
    {
        let _mutation = self.begin(permit, current)?;
        self.check_permit(permit, current)?;
        if self.is_cell() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        } // Bulk Find is a canonical protocol; Cell coordinates cannot acquire Body authority without an actual local atomic batch implementation.
        if permit.before.marked.is_some() {
            // Find replacement cannot take the IME's actual candidate or relative selection.
            return Err(NativeMarkdownPagedInputError::CompositionActive);
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
            // The final reentrant current check is over; inspect pure Rust facts in the original document, without authorizing a newer generation.
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        let (edit, prepared) = document.apply_replacements_observed(
            replacements,
            permit.before.selection,
            after,
            observer,
        )?;
        if edit.is_none() && *folds == prepared.folds {
            // Core's empty-stream contract keeps selection/Redo; do not adopt caller after-selection or create an empty history entry.
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        // After Core success exchange bounded metadata only. No Native/current or other fallible operation may report
        // published text as an ordinary error that could cause the caller to replay this batch.
        let commit = self.publish(permit, &document, prepared, None, edit, &mut folds);
        drop(folds);
        drop(document);
        drop(original);
        Ok(NativeMarkdownPagedInputOutcome::Committed(commit))
    }
}
