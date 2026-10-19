//! 原始草稿自己的有限root历史；不触碰canonical历史，也不把大输入保存为另一个String。

use super::edit::DraftSource;
use super::*;

pub(super) mod grouping;
pub use grouping::{
    MarkdownCellDraftHistoryFacts, MarkdownCellDraftInputIntent, MarkdownCellDraftInputKind,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 恢复草稿局部历史；不能用此动作调用正文Doc的Undo/Redo。
pub enum MarkdownCellDraftHistoryDirection {
    Undo,
    Redo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 变化范围属于本次恢复前的cell局部UTF16域，canonical选区/代次与历史保持不变。
pub struct MarkdownCellDraftHistoryEdit {
    pub replaced_utf16_range: Range<usize>,
    pub change_in_utf16_length: isize,
    pub facts: MarkdownCellDraftFacts,
}

struct Memento {
    root: PieceTreeLink,
    selection: MarkdownTextSelection,
    // grouped项同时记原事务另一端的选区；移动选区后Undo不能把临时caret写成原组Redo目标。
    // legacy项保持None，仍按原入口的当前选区交换合同恢复，不改变已有调用者行为。
    counterpart_selection: Option<MarkdownTextSelection>,
    replacement: MarkdownReplacementMetrics,
}

#[derive(Default)]
pub(super) struct LocalHistory {
    undo: VecDeque<Memento>,
    redo: VecDeque<Memento>,
    group: Option<grouping::Group>,
}

impl LocalHistory {
    pub(super) fn break_group(&mut self) {
        self.group = None; // 只有固定metadata，不清原Undo/Redo，也不释放仍有owner的共享正文。
    }

    pub(super) fn reserve_edit(&mut self) -> Result<(), MarkdownDocumentBufferError> {
        if self.undo.len() < MAX_EDIT_HISTORY_ENTRIES {
            // 先预留一个固定上限内的metadata槽；追加正文后不再有可失败的历史分配。
            self.undo
                .try_reserve(1)
                .map_err(|_| MarkdownDocumentBufferError::EditHistoryUnavailable)?;
        }
        Ok(())
    }

    pub(super) fn record(
        &mut self,
        root: PieceTreeLink,
        selection: MarkdownTextSelection,
        replacement: MarkdownReplacementMetrics,
    ) {
        if self.undo.len() == MAX_EDIT_HISTORY_ENTRIES {
            self.undo.pop_front(); // 丢最旧共享树metadata，累计delta预算不因此获得新额度。
        }
        self.undo.push_back(Memento {
            root,
            selection,
            counterpart_selection: None,
            replacement,
        });
        self.redo.clear(); // 分叉后的旧redo不能重现新输入前的草稿。
        self.group = None; // legacy或新独立组不能继续较早一次输入的合并权限。
    }

    fn stacks(
        &mut self,
        direction: MarkdownCellDraftHistoryDirection,
    ) -> (&mut VecDeque<Memento>, &mut VecDeque<Memento>) {
        match direction {
            MarkdownCellDraftHistoryDirection::Undo => (&mut self.undo, &mut self.redo),
            MarkdownCellDraftHistoryDirection::Redo => (&mut self.redo, &mut self.undo),
        }
    }
}

impl PendingCellDraft {
    fn restore_history(
        &mut self,
        document: &MarkdownDocumentBuffer,
        direction: MarkdownCellDraftHistoryDirection,
    ) -> Result<Option<MarkdownCellDraftHistoryEdit>, MarkdownCellDraftError> {
        self.require(document)?;
        if self.marked_range.is_some() {
            // 不能在Core内猜输入法已经结束；适配器须先沿原context完成unmark，再捕获恢复票。
            return Err(MarkdownCellDraftError::CompositionActive);
        }
        let source = DraftSource {
            document,
            edits: &self.edits,
            scope: &self.scope,
            serial: self.serial,
        };
        let (from, to) = self.history.stacks(direction);
        let Some(previous) = from.back() else {
            return Ok(None);
        };
        for at in [
            previous.selection.anchor_utf16,
            previous.selection.head_utf16,
        ] {
            // 栈项仍须在原段表上通过真实有限端点验证，不能只根据旧长度恢复选区。
            byte_offset_for_utf16_in_tree(&source, previous.root.as_deref(), at, 0)?;
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        let content_serial = self
            .content_serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        if to.len() < MAX_EDIT_HISTORY_ENTRIES {
            to.try_reserve(1)
                .map_err(|_| MarkdownDocumentBufferError::EditHistoryUnavailable)?;
        }
        source.source_health()?;
        let previous = from.pop_back().expect("已验证原历史末项");
        if to.len() == MAX_EDIT_HISTORY_ENTRIES {
            to.pop_front();
        }
        to.push_back(Memento {
            root: self.root.take(),
            selection: previous.counterpart_selection.unwrap_or(self.selection),
            counterpart_selection: previous.counterpart_selection.map(|_| previous.selection),
            replacement: previous.replacement,
        });
        self.history.group = None; // 成功历史恢复是输入断点；不能把redo后的新输入并回旧root。
        self.root = previous.root;
        self.selection = previous.selection;
        self.serial = serial;
        self.content_serial = content_serial;
        // 原marked会话绝不随历史复活；恢复正文只保原scalar选区。
        self.marked_range = None;
        self.scope
            .content_serial
            .store(content_serial, Ordering::Release);
        self.scope.serial.store(serial, Ordering::Release);
        let geometry = match direction {
            MarkdownCellDraftHistoryDirection::Undo => {
                previous.replacement.undo_text_edit(previous.selection)
            }
            MarkdownCellDraftHistoryDirection::Redo => {
                previous.replacement.redo_text_edit(previous.selection)
            }
        };
        Ok(Some(MarkdownCellDraftHistoryEdit {
            replaced_utf16_range: geometry.replaced_utf16_range,
            change_in_utf16_length: geometry.change_in_utf16_length,
            facts: self.facts(),
        }))
    }
}

impl MarkdownDocumentBuffer {
    /// 消费原serial编辑胶囊恢复局部历史；空栈明确None，不发布正文或改变会话delta库存。
    pub fn restore_cell_draft_history(
        &mut self,
        edit: edit::MarkdownCellDraftEditCapsule,
        direction: MarkdownCellDraftHistoryDirection,
    ) -> Result<Option<MarkdownCellDraftHistoryEdit>, MarkdownCellDraftError> {
        self.require_cell_draft_edit(&edit)?;
        let Some(CellDraftSlot::Active(mut pending)) = self.cell_draft.take() else {
            unreachable!("已核活动草稿");
        };
        let result = pending.restore_history(self, direction);
        self.cell_draft = Some(CellDraftSlot::Active(pending));
        result
    }
}
