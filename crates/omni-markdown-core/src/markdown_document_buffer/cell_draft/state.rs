//! 选区/marked只改变输入意图版本，不撤销相同正文的布局Source或追加历史。

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
        // 输入旧票失效，正文root与content_serial保持原样；已有GCB/UBA/glyph输出仍绑定相同内容。
        self.scope.serial.store(serial, Ordering::Release);
        Ok(self.facts())
    }
}

impl MarkdownDocumentBuffer {
    /// 原breakUndoCoalescing的局部意图；只撤旧输入serial和固定group，不修改正文或制造历史。
    pub fn break_cell_draft_undo_coalescing(
        &mut self,
        edit: MarkdownCellDraftEditCapsule,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(&edit)?;
        if pending.marked_range.is_some() {
            return Err(MarkdownCellDraftError::CompositionActive); // 未测的IME中断不能被当作普通typing断点。
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

    /// 发布cell局部有向选区；输入法仍拥有marked时，先由原context完成明确unmark。
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
        let result = pending.set_state(self, selection, None);
        self.cell_draft = Some(CellDraftSlot::Active(pending));
        result
    }

    /// 只结束原marked事实；不改raw字符、不造Undo，也不让相同内容的Source重新扫描。
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
