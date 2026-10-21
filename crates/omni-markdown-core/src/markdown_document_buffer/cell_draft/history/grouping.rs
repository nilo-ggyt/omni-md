//! Coalesce only actual replacements under the original input serial. Ungrouped
//! input, selection moves and explicit breaks cannot renew an earlier group.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Fixed by the adapter before reading native payload. Isolated does not infer
/// coalescing boundaries for paste or other input not represented as proven typing.
pub enum MarkdownCellDraftInputKind {
    Typing,
    Marked,
    Isolated,
}

/// One input intent bound to the original nonce/serial. It cannot be cloned; the
/// original edit capsule must still be consumed independently by value.
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
/// Actual local-stack availability, without querying/creating a native UndoManager
/// or granting canonical text-edit permission.
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
        _ => Ok(()), // Isolated keeps the entry's valid-range rule; text content does not reveal the user's input origin.
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
        // Typing extends only an adjacent insertion whose before/after carets
        // agree with the actual edit. A moved selection or redo branch breaks it.
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
            // Marked input merges only when it replaces the same complete active
            // composition span, including the final ordinary-typing commit.
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
                metrics.inserted_utf16_length // Marked input replaces the full span; old candidate lengths must not accumulate.
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
            _ => None, // After composition ends, do not guess a later IME/paste policy or reopen an earlier transaction.
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
            self.reserve_edit()?; // A merge updates the existing tail; a new group first reserves within the 128-entry history limit.
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
            // Preserve the group's first root, but update its opposite selection
            // from actual final input, never from a later unrelated caret move.
            previous.counterpart_selection = plan.counterpart_selection;
            drop(root); // Intermediate roots stay out of history; actual Frozen owners still retain their shared leaves as needed.
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
    /// Bind one input kind to the original edit capsule. Another nonce or successor
    /// serial cannot renew authority for an old native payload.
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

    /// Read this draft's history only. Marked input does not hide stack facts, but
    /// actual Undo still requires unmark through the original input context first.
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
