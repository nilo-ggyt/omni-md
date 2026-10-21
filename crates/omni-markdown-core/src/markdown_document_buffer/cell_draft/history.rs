//! The raw draft's bounded root history. Canonical history stays separate, and
//! large input is not retained as another materialized String.

use super::edit::DraftSource;
use super::*;

pub(super) mod grouping;
pub use grouping::{
    MarkdownCellDraftHistoryFacts, MarkdownCellDraftInputIntent, MarkdownCellDraftInputKind,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Restore draft-local history only; this action cannot invoke the canonical
/// document's Undo/Redo.
pub enum MarkdownCellDraftHistoryDirection {
    Undo,
    Redo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Change geometry belongs to the cell-local UTF-16 domain before this restore.
/// Canonical selection, generation and history remain unchanged.
pub struct MarkdownCellDraftHistoryEdit {
    pub replaced_utf16_range: Range<usize>,
    pub change_in_utf16_length: isize,
    pub facts: MarkdownCellDraftFacts,
}

struct Memento {
    root: PieceTreeLink,
    selection: MarkdownTextSelection,
    // Grouped entries retain the transaction's opposite selection too: moving the
    // caret later must not replace the original group's Redo target. Ungrouped
    // entries keep None and use their public entry's current-selection swap rule.
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
        self.group = None; // Fixed metadata only; keep Undo/Redo and text still held by actual shared owners.
    }

    pub(super) fn reserve_edit(&mut self) -> Result<(), MarkdownDocumentBufferError> {
        if self.undo.len() < MAX_EDIT_HISTORY_ENTRIES {
            // Reserve one metadata slot within the fixed history limit before
            // appending text, leaving no fallible history allocation after preparation.
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
            self.undo.pop_front(); // Drop the oldest shared root; this does not renew cumulative delta allowance.
        }
        self.undo.push_back(Memento {
            root,
            selection,
            counterpart_selection: None,
            replacement,
        });
        self.redo.clear(); // After branching, old Redo cannot resurrect the pre-input successor draft.
        self.group = None; // An ungrouped edit/new independent group cannot inherit an earlier input's merge authority.
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
            // Core cannot guess that IME has ended. The adapter must unmark
            // through the original input context before capturing restore authority.
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
            // Validate the stored selection's actual bounded endpoints against
            // its original segment table; an old length alone is not sufficient.
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
            // All storage/coordinate checks and possible destination allocation
            // precede removing a source memento or publishing the restored root.
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
        self.history.group = None; // Successful history restoration breaks input coalescing; new input cannot merge into the old root.
        self.root = previous.root;
        self.selection = previous.selection;
        self.serial = serial;
        self.content_serial = content_serial;
        // History never resurrects a marked-input session. Restored text retains
        // only its original validated scalar selection.
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
    /// Consume the original-serial edit capsule to restore local history. Empty
    /// history returns None, without publishing canonical text or renewing delta budget.
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
