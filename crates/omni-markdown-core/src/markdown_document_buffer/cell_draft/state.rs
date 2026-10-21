//! Selection/marked state changes advance input-intent identity without revoking
//! the unchanged text's layout Source or appending history.

use super::edit::{DraftSource, MarkdownCellDraftEditCapsule, validate_positions};
use super::*;

impl PendingCellDraft {
    pub(super) fn set_state(
        &mut self,
        document: &MarkdownDocumentBuffer,
        selection: MarkdownTextSelection,
        marked: Option<Range<usize>>,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        self.require(document)?;
        let source = DraftSource {
            document,
            edits: &self.edits,
            scope: &self.scope,
            serial: self.serial,
        };
        validate_positions(&source, &self.root, selection, &marked)?;
        if self.selection == selection && self.marked_range == marked {
            // A true no-op keeps existing input capsules valid. Positions were
            // still checked against actual Source before returning those facts.
            return Ok(self.facts());
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        source.source_health()?;
        self.selection = selection;
        self.marked_range = marked;
        self.serial = serial;
        // Revoke old input receipts while retaining root/content_serial. Existing
        // GCB/UBA/glyph output remains bound to precisely the same text content.
        self.scope.serial.store(serial, Ordering::Release);
        Ok(self.facts())
    }
}

impl MarkdownDocumentBuffer {
    /// Apply the local breakUndoCoalescing intent: revoke old input serial/group
    /// only, without editing text or manufacturing a history entry.
    pub fn break_cell_draft_undo_coalescing(
        &mut self,
        edit: MarkdownCellDraftEditCapsule,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(&edit)?;
        if pending.marked_range.is_some() {
            return Err(MarkdownCellDraftError::CompositionActive); // An active IME composition is not an ordinary typing break.
        }
        let serial = pending
            .serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        let Some(CellDraftSlot::Active(pending)) = &mut self.cell_draft else {
            unreachable!("已核原断点的唯一草稿");
        };
        pending.history.break_group();
        pending.serial = serial;
        pending.scope.serial.store(serial, Ordering::Release);
        Ok(pending.facts())
    }

    /// Publish a cell-local directed selection. While IME owns marked text, the
    /// original input context must explicitly unmark it first.
    pub fn set_cell_draft_selection(
        &mut self,
        edit: MarkdownCellDraftEditCapsule,
        selection: MarkdownTextSelection,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(&edit)?;
        if pending.marked_range.is_some() {
            return Err(MarkdownCellDraftError::CompositionActive);
        }
        let Some(CellDraftSlot::Active(mut pending)) = self.cell_draft.take() else {
            unreachable!("已核活动草稿");
        };
        // Temporarily take the unique branch for this synchronous Rust operation.
        // Restore the same owner on both success and failure; no native callback runs.
        let result = pending.set_state(self, selection, None);
        self.cell_draft = Some(CellDraftSlot::Active(pending));
        result
    }

    /// End only the original marked-state fact, without changing raw characters,
    /// creating Undo, or forcing the unchanged text Source to scan again.
    pub fn unmark_cell_draft(
        &mut self,
        edit: MarkdownCellDraftEditCapsule,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        self.require_cell_draft_edit(&edit)?;
        let Some(CellDraftSlot::Active(mut pending)) = self.cell_draft.take() else {
            unreachable!("已核活动草稿");
        };
        let result = pending.set_state(self, pending.selection, None);
        self.cell_draft = Some(CellDraftSlot::Active(pending));
        result
    }
}
