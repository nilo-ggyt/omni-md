//! 连续输入只合并原输入serial下的实际替换；旧入口、选区移动和显式断点不能补回先前分组。

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 由适配器在读取Native载荷之前固定；Isolated不推断粘贴或其他尚未差分的合并边界。
pub enum MarkdownCellDraftInputKind {
    Typing,
    Marked,
    Isolated,
}

/// 原nonce/serial的一次输入意图；不可Clone，原编辑胶囊仍须独立按值消费。
pub struct MarkdownCellDraftInputIntent {
    scope: Weak<Scope>,
    serial: u64,
    kind: MarkdownCellDraftInputKind,
}

impl MarkdownCellDraftInputIntent {
    pub(in crate::markdown_document_buffer::cell_draft) fn require(
        &self,
        pending: &PendingCellDraft,
    ) -> Result<MarkdownCellDraftInputKind, MarkdownCellDraftError> {
        if !Weak::ptr_eq(&self.scope, &Arc::downgrade(&pending.scope))
            || self.serial != pending.serial
        {
            return Err(MarkdownCellDraftError::Stale);
        }
        Ok(self.kind)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 原局部栈的真实可用性；不查询或创建Native UndoManager，不签发正文编辑许可。
pub struct MarkdownCellDraftHistoryFacts {
    pub can_undo: bool,
    pub can_redo: bool,
}

#[derive(Clone, Copy)]
enum GroupKind {
    Typing,
    Marked,
}

pub(super) struct Group {
    serial: u64,
    kind: GroupKind,
    span: Range<usize>,
}

pub(in crate::markdown_document_buffer::cell_draft) struct GroupChange<'a> {
    pub kind: Option<MarkdownCellDraftInputKind>,
    pub serial: u64,
    pub next_serial: u64,
    pub selection_before: MarkdownTextSelection,
    pub marked_before: &'a Option<Range<usize>>,
    pub selection_after: MarkdownTextSelection,
    pub marked_after: &'a Option<Range<usize>>,
    pub replacement: MarkdownReplacementMetrics,
}

pub(in crate::markdown_document_buffer::cell_draft) struct GroupPlan {
    merge: bool,
    replacement: MarkdownReplacementMetrics,
    counterpart_selection: Option<MarkdownTextSelection>,
    next: Option<Group>,
}

fn caret(selection: MarkdownTextSelection, at: usize) -> bool {
    selection.anchor_utf16 == at && selection.head_utf16 == at
}

pub(in crate::markdown_document_buffer::cell_draft) fn validate_input_kind(
    kind: MarkdownCellDraftInputKind,
    location: usize,
    inserted: usize,
    marked_after: &Option<Range<usize>>,
) -> Result<(), MarkdownCellDraftError> {
    let end = location
        .checked_add(inserted)
        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
    match kind {
        MarkdownCellDraftInputKind::Marked if marked_after.as_ref() != Some(&(location..end)) => {
            Err(MarkdownCellDraftError::InvalidInputIntent)
        }
        MarkdownCellDraftInputKind::Typing if marked_after.is_some() => {
            Err(MarkdownCellDraftError::InvalidInputIntent)
        }
        _ => Ok(()), // Isolated保留旧合法范围规则，不从字符串内容猜用户的输入来源。
    }
}

impl LocalHistory {
    pub(in crate::markdown_document_buffer::cell_draft) fn plan_group(
        &self,
        change: GroupChange<'_>,
    ) -> Result<GroupPlan, MarkdownCellDraftError> {
        let metrics = change.replacement;
        let end = metrics
            .location
            .checked_add(metrics.inserted_utf16_length)
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        let span = metrics.location..end;
        if let Some(kind) = change.kind {
            validate_input_kind(
                kind,
                metrics.location,
                metrics.inserted_utf16_length,
                change.marked_after,
            )?;
        }
        let old = self.group.as_ref().filter(|group| {
            group.serial == change.serial && self.redo.is_empty() && !self.undo.is_empty()
        });
        let merge_typing = matches!(change.kind, Some(MarkdownCellDraftInputKind::Typing))
            && change.marked_before.is_none()
            && change.marked_after.is_none()
            && metrics.removed_utf16_length == 0
            && metrics.inserted_utf16_length != 0
            && caret(change.selection_before, metrics.location)
            && caret(change.selection_after, end)
            && old.is_some_and(|group| {
                matches!(group.kind, GroupKind::Typing) && group.span.end == metrics.location
            });
        let merge_marked = matches!(
            change.kind,
            Some(MarkdownCellDraftInputKind::Marked | MarkdownCellDraftInputKind::Typing)
        ) && old.is_some_and(|group| {
            matches!(group.kind, GroupKind::Marked)
                && change.marked_before.as_ref() == Some(&group.span)
                && metrics.location == group.span.start
                && metrics.removed_utf16_length == group.span.len()
        });
        let replacement = if merge_typing || merge_marked {
            let previous = self.undo.back().expect("已核同组历史非空").replacement;
            let inserted = if merge_typing {
                previous
                    .inserted_utf16_length
                    .checked_add(metrics.inserted_utf16_length)
                    .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?
            } else {
                metrics.inserted_utf16_length // marked始终替换原完整组合范围，不能累计旧候选长度。
            };
            MarkdownReplacementMetrics::new(
                previous.location,
                previous.removed_utf16_length,
                inserted,
            )?
        } else {
            metrics
        };
        let next = match change.kind {
            Some(MarkdownCellDraftInputKind::Marked) => Some(Group {
                serial: change.next_serial,
                kind: GroupKind::Marked,
                span,
            }),
            Some(MarkdownCellDraftInputKind::Typing)
                if change.marked_before.is_none()
                    && metrics.removed_utf16_length == 0
                    && metrics.inserted_utf16_length != 0
                    && caret(change.selection_before, metrics.location)
                    && caret(change.selection_after, end) =>
            {
                Some(Group {
                    serial: change.next_serial,
                    kind: GroupKind::Typing,
                    span: replacement.location..end,
                })
            }
            _ => None, // 完成组合后不猜后续IME/粘贴的合并策略；原四组事务已经完整保留。
        };
        Ok(GroupPlan {
            merge: merge_typing || merge_marked,
            replacement,
            counterpart_selection: change.kind.map(|_| change.selection_after),
            next,
        })
    }

    pub(in crate::markdown_document_buffer::cell_draft) fn reserve_group(
        &mut self,
        plan: &GroupPlan,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if !plan.merge {
            self.reserve_edit()?; // 合并只改已有末项；新组仍先预留原128项预算内的历史槽。
        }
        Ok(())
    }

    pub(in crate::markdown_document_buffer::cell_draft) fn record_group(
        &mut self,
        plan: GroupPlan,
        root: PieceTreeLink,
        selection: MarkdownTextSelection,
    ) {
        if plan.merge {
            let previous = self.undo.back_mut().expect("已预检原组末项");
            previous.replacement = plan.replacement;
            // 连续输入的root前态不变，后态选区必须随真实最后一次输入更新，不能取后来移动的选区。
            previous.counterpart_selection = plan.counterpart_selection;
            drop(root); // 中间候选不进入历史；仍有Frozen owner的共享叶由其实际生命周期保留。
        } else {
            self.record(root, selection, plan.replacement);
            self.undo
                .back_mut()
                .expect("已记录本次原组")
                .counterpart_selection = plan.counterpart_selection;
        }
        self.group = plan.next;
    }
}

impl MarkdownDocumentBuffer {
    /// 与原编辑胶囊绑定一次输入类别；不同nonce或后继serial不能替旧Native载荷补签。
    pub fn capture_cell_draft_input_intent(
        &self,
        edit: &edit::MarkdownCellDraftEditCapsule,
        kind: MarkdownCellDraftInputKind,
    ) -> Result<MarkdownCellDraftInputIntent, MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(edit)?;
        Ok(MarkdownCellDraftInputIntent {
            scope: Arc::downgrade(&pending.scope),
            serial: pending.serial,
            kind,
        })
    }

    /// 只读本草稿历史；有marked时保留原栈事实，实际Undo仍须先沿原context完成unmark。
    pub fn cell_draft_history_facts(
        &self,
        token: &MarkdownCellDraft,
    ) -> Result<MarkdownCellDraftHistoryFacts, MarkdownCellDraftError> {
        let pending = self.active_cell_draft(token)?;
        Ok(MarkdownCellDraftHistoryFacts {
            can_undo: !pending.history.undo.is_empty(),
            can_redo: !pending.history.redo.is_empty(),
        })
    }
}
