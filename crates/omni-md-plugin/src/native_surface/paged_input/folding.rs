//! Source folding changes bounded anchors in the sole Input owner; canonical text, save, and Undo remain in Core.

use super::*;
use omni_markdown_core::markdown_folding::{
    anchor_mapping::MAX_MARKDOWN_FOLD_ANCHORS, markdown_fold_region,
};

impl NativeMarkdownPagedInput {
    /// Find reveals hidden matches in the same canonical selection commit; subsequent requests bind the resulting fold revision.
    /// Unlike Select All, this intent must reveal the complete match rather than clamp endpoints to a header.
    pub(in crate::native_surface) fn select_find_result(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        selection: MarkdownTextSelection,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        if permit.cell.is_some() {
            // Cell Find uses draft-local selection; Body folding has no authority over Cell, so retain its independent transaction.
            return self.set_selection(permit, selection, current);
        }
        let _mutation = self.begin(permit, current)?;
        self.require_fold_action(permit)?;
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        document.byte_offset_for_utf16(selection.anchor_utf16)?;
        document.byte_offset_for_utf16(selection.head_utf16)?;
        let range = selection.anchor_utf16.min(selection.head_utf16)
            ..selection.anchor_utf16.max(selection.head_utf16);
        let old = self
            .folds
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut folds = Vec::with_capacity(old.len());
        for fold in old.iter() {
            let hidden = folded_layout_range(&document, fold)?;
            // A caret must be strictly hidden; a nonempty Find match reveals
            // every overlapping hidden span so no part of the result stays hidden.
            let contains = if range.is_empty() {
                hidden.start < range.start && range.start < hidden.end
            } else {
                range.start < hidden.end && hidden.start < range.end
            };
            if !contains {
                folds.push(fold.clone());
            }
        }
        drop(old);
        drop(document);
        drop(original);
        self.publish_fold_intent(permit, folds, Some(selection), current)
    }

    /// The existing menu/gutter action consumes this permit; publish folds and any required header caret only after a successful query.
    /// Unfold only the selected folded header, retaining nested user choices; a nonfoldable context is genuinely unchanged.
    pub fn toggle_fold(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        line: usize,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let _mutation = self.begin(permit, current)?;
        self.require_fold_action(permit)?;
        let mut folds = self
            .folds
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?
            .clone();
        let mut selection = None;
        if let Some(index) = folds.iter().position(|fold| fold.header_line == line) {
            // Remove only this header's anchor; retained nested choices remain
            // available if the parent is folded again later.
            folds.remove(index);
        } else {
            if permit.before.selection_range_count > 1 {
                return Err(NativeMarkdownPagedInputError::MultipleSelectionInput);
            }
            let original = self
                .document_identity()
                .upgrade()
                .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            if !self.matches_document(&document, permit.before) {
                return Err(NativeMarkdownPagedInputError::StateChanged);
            }
            let region = markdown_fold_region(&document, line)?;
            drop(document);
            drop(original);
            let Some(region) = region else {
                // The query released all borrows before reentry. A nonfoldable
                // line is unchanged only while the original permit still holds.
                self.check_permit(permit, current)?;
                return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
            };
            if folds.len() == MAX_MARKDOWN_FOLD_ANCHORS {
                return Err(MarkdownFoldMappingError::TooManyAnchors.into());
            }
            let hidden = &region.hidden_utf16_range;
            if permit.before.selection_is_directed
                && hidden.start < permit.before.selection.head_utf16
                && permit.before.selection.head_utf16 < hidden.end
            {
                selection = Some(MarkdownTextSelection::caret(hidden.start));
            } // Move only a known head inside the hidden region to its header; unknown direction retains original metadata/history memento.
            folds.push(region);
        }
        self.publish_fold_intent(permit, folds, selection, current)
    }

    /// Expand All removes current bounded anchors without a text transaction or Undo entry.
    pub fn expand_folds(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let _mutation = self.begin(permit, current)?;
        self.require_fold_action(permit)?;
        self.publish_fold_intent(permit, Vec::new(), None, current)
    }

    /// Find/accessibility reveals intersecting hidden targets before the caller acquires geometry under the committed fold revision.
    /// Neither clamp the match to a header nor read/copy surrounding document text.
    pub fn reveal_folded_range(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        range: Range<usize>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let _mutation = self.begin(permit, current)?;
        self.require_fold_action(permit)?;
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        if range.start > range.end || range.end > document.utf16_len() {
            return Err(NativeMarkdownPagedInputError::InvalidRange);
        }
        document.byte_offset_for_utf16(range.start)?;
        document.byte_offset_for_utf16(range.end)?;
        let original_folds = self
            .folds
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut retained = Vec::with_capacity(original_folds.len());
        for fold in original_folds.iter() {
            let hidden = folded_layout_range(&document, fold)?;
            let intersects = if range.is_empty() {
                hidden.start < range.start && range.start < hidden.end
            } else {
                range.start < hidden.end && hidden.start < range.end
            };
            if !intersects {
                retained.push(fold.clone());
            }
        }
        drop(original_folds);
        drop(document);
        drop(original);
        self.publish_fold_intent(permit, retained, None, current)
    }

    /// The Source composer receives bounded endpoint projections under this permit, not a second mutable fold owner.
    pub fn folded_layout_ranges(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<Vec<Range<usize>>> {
        self.check_permit(permit, current)?;
        if permit.cell.is_some() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        }
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        let folds = self
            .folds
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut ranges = Vec::with_capacity(folds.len());
        for fold in folds.iter() {
            ranges.push(folded_layout_range(&document, fold)?);
        }
        drop(folds);
        drop(document);
        drop(original);
        self.check_permit(permit, current)?;
        Ok(ranges)
    }

    /// Copy only bounded header line numbers and release borrows before Native geometry conversion; retain no second fold owner.
    pub(in crate::native_surface) fn folded_header_lines(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<Vec<usize>> {
        self.check_permit(permit, current)?;
        if permit.cell.is_some() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        }
        let headers = self
            .folds
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?
            .iter()
            .map(|fold| fold.header_line)
            .collect();
        self.check_permit(permit, current)?;
        Ok(headers)
    }

    /// Cell or marked input cannot modify Body folds; the controller must first complete its existing composition flow.
    fn require_fold_action(&self, permit: &NativeMarkdownPagedInputPermit) -> Result<()> {
        if permit.cell.is_some() {
            return Err(NativeMarkdownPagedInputError::BodyTargetRequired);
        }
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        }
        Ok(())
    }

    /// Finish the final reentrant current check outside Rust borrows; then publish only Core selection and bounded metadata.
    fn publish_fold_intent(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        folds: Vec<MarkdownFoldRegion>,
        selection: Option<MarkdownTextSelection>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        self.check_permit(permit, current)?;
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        let mut old_folds = self
            .folds
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let folds_changed = *old_folds != folds;
        if !folds_changed && selection.is_none() {
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        let revision = if folds_changed {
            // Preflight the revision before changing Core selection or replacing
            // anchors; exhaustion cannot leave a partially published fold action.
            permit
                .before
                .fold_revision
                .checked_add(1)
                .ok_or(NativeMarkdownPagedInputError::CounterExhausted)?
        } else {
            permit.before.fold_revision
        };
        let selection_changed = selection.is_some_and(|next| {
            next != permit.before.selection || !permit.before.selection_is_directed
        });
        let mut state = permit.before;
        if let Some(next) = selection {
            // Core checks scalar endpoints, Source health, and idle state first; failure publishes neither folds nor selection.
            document.set_selection(next)?;
            if !folds_changed && !selection_changed {
                return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
            }
            let serial = self
                .selection
                .publish(state.generation, next, true, None, None);
            drop(self.selection_ranges.replace(Vec::new()));
            state.selection = next;
            state.selection_serial = serial;
            state.selection_is_directed = true;
            state.selection_range_count = 1;
            state.primary_range = ordered_selection_range(next);
        }
        *old_folds = folds;
        self.fold_revision.set(revision);
        self.clear_navigation_geometry();
        state.fold_revision = revision;
        drop(old_folds);
        drop(document);
        drop(original);
        Ok(NativeMarkdownPagedInputOutcome::Committed(
            NativeMarkdownPagedInputCommit {
                owner: Rc::downgrade(self),
                epoch: permit.epoch,
                state,
                invalidation: NativeMarkdownPagedInputInvalidation {
                    replaced_utf16_range: None,
                    change_in_utf16_length: 0,
                    characters_changed: false,
                    selection_changed,
                    marked_changed: false,
                    folds_changed,
                },
            },
        ))
    }
}

/// Reuse the Source delegate's CRLF-aware omission contract, extending only over the actual last hidden line terminator.
/// Recheck the original document's Source after closure read failure, so EIO cannot become an ordinary missing fold.
pub(super) fn folded_layout_range(
    document: &MarkdownDocumentBuffer,
    fold: &MarkdownFoldRegion,
) -> Result<Range<usize>> {
    let hidden = &fold.hidden_utf16_range;
    document.byte_offset_for_utf16(hidden.start)?;
    document.byte_offset_for_utf16(hidden.end)?;
    let end = super::super::fold_state::native_fold_projection_end(
        hidden,
        document.utf16_len(),
        |offset| document.utf16_code_unit_at(offset).ok(),
        |offset| {
            let byte = document.byte_offset_for_utf16(offset).ok()?;
            let number = document.line_for_byte(byte).ok()?;
            let start = document
                .utf16_offset_for_byte(document.byte_offset_for_line(number).ok()?)
                .ok()?;
            let next = number.checked_add(1)?;
            let end = if next < document.line_count() {
                document
                    .utf16_offset_for_byte(document.byte_offset_for_line(next).ok()?)
                    .ok()?
            } else {
                document.utf16_len()
            };
            Some((start, end))
        },
    );
    // Projection callbacks intentionally express missing coordinates as None;
    // check original Source again before turning that into InvalidRange.
    document.byte_offset_for_utf16(hidden.start)?;
    let end = end.ok_or(NativeMarkdownPagedInputError::InvalidRange)?;
    Ok(hidden.start..end)
}
