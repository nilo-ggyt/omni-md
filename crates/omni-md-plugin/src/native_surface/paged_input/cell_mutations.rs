//! Cell transactions consume the original capsule; after Core succeeds only bounded metadata changes, with no fallible Native/Source read.

use super::cell::{CellPermit, NativeMarkdownPagedCellSession};
use super::target::InputTarget;
use super::*;
use omni_markdown_core::{
    MarkdownCellDraftFacts, MarkdownCellDraftHistoryDirection, MarkdownCellDraftInputIntent,
    MarkdownCellDraftInputKind,
};

pub(super) struct CellReplacement<'a> {
    pub text: &'a str,
    pub range: Range<usize>,
    pub selection: MarkdownTextSelection,
    pub marked: Option<NSRange>,
    pub intent: MarkdownCellDraftInputIntent,
}

pub(super) struct CellInputIntents {
    primary: MarkdownCellDraftInputIntent,
    empty_marked: Option<MarkdownCellDraftInputIntent>,
}

impl CellInputIntents {
    pub(super) fn into_intent(self, empty_marked: bool) -> MarkdownCellDraftInputIntent {
        if empty_marked {
            // Empty setMarkedText ends composition; both bounded intents were captured before the Native payload getter, not from a successor serial.
            self.empty_marked.unwrap_or(self.primary)
        } else {
            self.primary
        }
    }
}

impl NativeMarkdownPagedInput {
    /// Honor the existing explicit breakUndoCoalescing action without inferring grouping boundaries from Paste, key text, or timing.
    ///
    /// # Errors
    /// Keep the draft when marked input or Source/permit checks reject; Body retains its existing transaction history policy.
    pub fn break_undo_coalescing(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let _mutation = self.begin(permit, current)?;
        self.check_permit(permit, current)?;
        let Some(cell) = &permit.cell else {
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        };
        let session = cell.session()?;
        let original = session.document()?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let facts = document.break_cell_draft_undo_coalescing(cell.take()?)?;
        Ok(NativeMarkdownPagedInputOutcome::Committed(
            self.publish_cell(permit, facts, None, None),
        ))
    }

    /// Create a weak target from the sole session; local version/selection neither reads canonical selection nor creates another text owner.
    ///
    /// # Errors
    /// Reject invalid session, document borrow conflict, or Source failure without acquiring a layout reader.
    pub fn new_cell(session: Weak<NativeMarkdownPagedCellSession>) -> Result<Rc<Self>> {
        let original = session
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::Closed)?;
        let facts = original.facts()?;
        Ok(Rc::new(Self {
            target: InputTarget::Cell {
                session,
                serial: Cell::new(facts.serial),
            },
            selection: NativeMarkdownSelectionState::new(facts.content_serial, facts.selection),
            selection_ranges: RefCell::new(Vec::new()),
            marked: Cell::new(
                facts
                    .marked_range
                    .map(|range| NSRange::new(range.start, range.len())),
            ),
            folds: RefCell::new(Vec::new()),
            fold_revision: Cell::new(0),
            closed: Cell::new(false),
            busy: Cell::new(false),
            #[cfg(feature = "native-input-trace")]
            input_trace: RefCell::new(None),
        }))
    }

    pub(super) fn cell_state(&self) -> Result<Option<NativeMarkdownPagedInputState>> {
        let InputTarget::Cell { session, serial } = &self.target else {
            return Ok(None);
        };
        // Core facts check the original token/Source before local metadata. This
        // Input may describe only that serial, never an externally advanced draft.
        let original = session
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::Closed)?;
        let facts = original.facts()?;
        if facts.serial != serial.get()
            || self
                .selection
                .known_selection(facts.content_serial, facts.selection)
                != Some(facts.selection)
            || self.marked.get()
                != facts
                    .marked_range
                    .clone()
                    .map(|range| NSRange::new(range.start, range.len()))
        {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        Ok(Some(NativeMarkdownPagedInputState {
            generation: facts.content_serial,
            selection: facts.selection,
            selection_is_directed: true,
            selection_range_count: 1,
            primary_range: ordered_selection_range(facts.selection),
            marked: self.marked.get(),
            utf16_len: facts.utf16_len,
            selection_serial: self.selection.serial(),
            fold_revision: self.fold_revision.get(),
        }))
    }

    pub(super) fn cell_intent(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        kind: MarkdownCellDraftInputKind,
    ) -> Result<Option<CellInputIntents>> {
        // Capture both possible marked outcomes before reading Native payload:
        // nonempty text continues composition, while empty marked input isolates it.
        permit
            .cell
            .as_ref()
            .map(|cell| {
                Ok(CellInputIntents {
                    primary: cell.intent(kind)?,
                    empty_marked: (kind == MarkdownCellDraftInputKind::Marked)
                        .then(|| cell.intent(MarkdownCellDraftInputKind::Isolated))
                        .transpose()?,
                })
            })
            .transpose()
    }

    fn original_cell(permit: &NativeMarkdownPagedInputPermit) -> Result<&CellPermit> {
        permit
            .cell
            .as_ref()
            .ok_or(NativeMarkdownPagedInputError::StateChanged)
    }

    pub(super) fn replace_cell(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        replacement: CellReplacement<'_>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        self.check_permit(permit, current)?; // Finish reentrant checks before consuming the original capsule; never recapture a successor serial.
        let cell = Self::original_cell(permit)?;
        let session = cell.session()?;
        let original = session.document()?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let facts = document.replace_cell_draft_grouped(
            cell.take()?,
            replacement.intent,
            replacement.range.clone(),
            replacement.text,
            replacement.selection,
            replacement
                .marked
                .map(|range| range.location..range.location + range.length),
        )?;
        // Core published text, selection, and history together; do not read Source/call current afterward or report later revocation as an uncommitted edit.
        let change =
            replacement.text.encode_utf16().count() as isize - replacement.range.len() as isize;
        Ok(self.publish_cell(permit, facts, Some((replacement.range, change)), None))
    }

    pub(super) fn publish_cell(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        facts: MarkdownCellDraftFacts,
        edit: Option<(Range<usize>, isize)>,
        geometry: Option<(
            Option<bool>,
            Option<NativeMarkdownVerticalGoal>,
            Option<NativeMarkdownBidiCaretSide>,
        )>,
    ) -> NativeMarkdownPagedInputCommit {
        let InputTarget::Cell { serial, .. } = &self.target else {
            unreachable!("只有原cell事务能发布局部metadata");
        };
        serial.set(facts.serial);
        // Core facts are the committed local state. Replace bounded selection and
        // marked metadata without a fallible Source read or a callback after commit.
        let marked = facts
            .marked_range
            .map(|range| NSRange::new(range.start, range.len()));
        self.marked.set(marked);
        drop(self.selection_ranges.replace(Vec::new()));
        let (head, goal, side) = geometry.unwrap_or((None, None, None));
        let selection_serial = self.selection.publish_with_bidi_side(
            facts.content_serial,
            facts.selection,
            true,
            head,
            goal,
            side,
        );
        let state = NativeMarkdownPagedInputState {
            generation: facts.content_serial,
            selection: facts.selection,
            selection_is_directed: true,
            selection_range_count: 1,
            primary_range: ordered_selection_range(facts.selection),
            marked,
            utf16_len: facts.utf16_len,
            selection_serial,
            fold_revision: self.fold_revision.get(),
        };
        NativeMarkdownPagedInputCommit {
            owner: Rc::downgrade(self),
            epoch: permit.epoch,
            state,
            invalidation: NativeMarkdownPagedInputInvalidation {
                replaced_utf16_range: edit.as_ref().map(|(range, _)| range.clone()),
                change_in_utf16_length: edit.map_or(0, |(_, change)| change),
                characters_changed: state.generation != permit.before.generation,
                selection_changed: state.selection != permit.before.selection,
                marked_changed: state.marked != permit.before.marked,
                folds_changed: false, // Local coordinates never enter the Body fold mapper; canonical folds/history remain unchanged.
            },
        }
    }

    pub(super) fn unmark_cell(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let cell = Self::original_cell(permit)?;
        let session = cell.session()?;
        let original = session.document()?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let facts = document.unmark_cell_draft(cell.take()?)?;
        Ok(NativeMarkdownPagedInputOutcome::Committed(
            self.publish_cell(permit, facts, None, None),
        ))
    }

    pub(super) fn history_cell(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        direction: NativeMarkdownPagedInputHistory,
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let cell = Self::original_cell(permit)?;
        let session = cell.session()?;
        let original = session.document()?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let direction = match direction {
            NativeMarkdownPagedInputHistory::Undo => MarkdownCellDraftHistoryDirection::Undo,
            NativeMarkdownPagedInputHistory::Redo => MarkdownCellDraftHistoryDirection::Redo,
        };
        let Some(edit) = document.restore_cell_draft_history(cell.take()?, direction)? else {
            // No local entry is genuinely unchanged; canonical history is never
            // a fallback for an exhausted Cell stack.
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        };
        Ok(NativeMarkdownPagedInputOutcome::Committed(
            self.publish_cell(
                permit,
                edit.facts,
                Some((edit.replaced_utf16_range, edit.change_in_utf16_length)),
                None,
            ),
        ))
    }

    pub(super) fn selection_cell(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        selection: MarkdownTextSelection,
        geometry: Option<(
            Option<bool>,
            Option<NativeMarkdownVerticalGoal>,
            Option<NativeMarkdownBidiCaretSide>,
        )>,
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let cell = Self::original_cell(permit)?;
        let session = cell.session()?;
        let original = session.document()?;
        if geometry.is_some_and(|(head, goal, side)| {
            goal.is_some_and(|goal| !goal.is_finite() || head.is_none())
                || side.is_some_and(|side| side.level > 126 || head.is_none())
        }) {
            // Check Source even for invalid geometry, using an empty real range.
            // A backing failure must precede the later invalid-intent diagnosis.
            cell.with_edit(|document, edit| {
                Ok(document.validate_cell_draft_utf16_range(edit, 0..0)?)
            })?;
            return Err(NativeMarkdownPagedInputError::InvalidRange);
        }
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let facts = document.set_cell_draft_selection(cell.take()?, selection)?;
        if geometry.is_none() && selection == permit.before.selection {
            // A plain identical selection carries no new visual intent. Core
            // validated the capsule/Source without advancing local metadata.
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        Ok(NativeMarkdownPagedInputOutcome::Committed(
            self.publish_cell(permit, facts, None, geometry),
        ))
    }
}
