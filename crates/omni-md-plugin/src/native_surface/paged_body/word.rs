//! Word deletion, selection, and later ordinary input share one ordered commit chain; each step consumes original Composer work or bounded new payload.

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputPermit;
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedCellWordTarget as CellWordTarget,
    NativeMarkdownPagedWordAnchor as WordAnchor, NativeMarkdownPagedWordMotion as WordMotion,
    NativeMarkdownPagedWordSelection as WordSelection,
};
use objc2_foundation::NSRange;
use omni_markdown_core::{
    MarkdownEditingCommand, MarkdownLayoutTextAuthority, MarkdownTextSelection,
    MarkdownWordDeleteAnswer, MarkdownWordGroupAction, MarkdownWordGroupAnswer,
    MarkdownWordMoveScope,
};
use std::ops::Range;

mod folding;
pub(super) mod native;
use native::NativeInput;

/// Adjacent identical directions retain counts only. Input slots reserve actual arrival order, then receive the sole converted insertion string.
enum WordInput {
    Delete {
        forward: bool,
        /// Grapheme and word deletion share ordering; original Composer determines their actual stopping rules.
        grapheme: bool,
        remaining: usize,
    },
    Move {
        motion: WordMotion,
        extend: bool,
        remaining: usize,
    },
    Visual {
        right: bool,
        extend: bool,
        remaining: usize,
    },
    Select {
        selection: WordSelection,
        /// Original Down identity authorizes first-group handoff only; keyboard selection owns no such gesture authority.
        gesture: Option<Weak<()>>,
        /// Actual start/side of the sole Core query; a newer Drag cannot restart the unfinished initial query.
        query: (usize, bool, Option<CellWordTarget>),
        /// The completed initial group retains two endpoints; later targets serially reuse the same Core slot.
        first: Option<Range<usize>>,
        /// Initial click position determines same-group direction only; changing the latest target neither changes this position nor restarts its query.
        origin_utf16: usize,
    },
    /// Existing document-endpoint intent, without old length, anchor, or text.
    Document {
        end: bool,
        extend: bool,
        remaining: usize,
    },
    Typing(Rc<RefCell<Option<String>>>),
    /// Original event/menu awaiting NSTextInputContext interpretation, without asynchronous marked-state duplication.
    Native(Rc<RefCell<Option<NativeInput>>>),
    /// Retain semantic intent only; original Core resolves list continuation/indentation after the preceding actual commit.
    Source {
        command: MarkdownEditingCommand,
        remaining: usize,
    },
}

/// The sole current transaction is converted insertion, certified deletion, or original Source command; successor queue entries cannot determine its semantics.
enum WordEdit<'a> {
    Typing(&'a str),
    Delete(NSRange),
    Source(MarkdownEditingCommand),
}

/// Capture facts only when this action reaches the front; later typing cannot inherit a literal old-Source head.
#[derive(Clone, Copy)]
pub(super) struct VisualIntent {
    pub selection: MarkdownTextSelection,
    pub upstream: bool,
    pub side: Option<super::super::paged_input::NativeMarkdownBidiCaretSide>,
    pub right: bool,
    pub extend: bool,
}
#[derive(Clone, Copy)]
pub(super) struct VisualSelection {
    pub selection: MarkdownTextSelection,
    pub upstream: Option<bool>,
    pub side: Option<super::super::paged_input::NativeMarkdownBidiCaretSide>,
}

/// The chain retains its current ticket, final receipt, and bounded uninterpreted events, without document caching or another Input.
pub(super) struct PendingWordDelete {
    request: RefCell<NativeMarkdownPagedSceneRequest>,
    steps: RefCell<VecDeque<WordInput>>,
    queued_bytes: Cell<usize>,
    receipt: RefCell<Option<Rc<NativeMarkdownPagedInputCommit>>>,
    retired_serial: Cell<Option<u64>>,
    permit: RefCell<Option<NativeMarkdownPagedInputPermit>>,
    bound: RefCell<Option<(u64, MarkdownLayoutTextAuthority)>>,
    ready: RefCell<Option<Range<usize>>>,
    /// Retain original Producer-certified opaque endpoints; expansion leaves the same queued action to be recomputed.
    pub(super) fold_expansion: RefCell<Option<Range<usize>>>,
    /// Display ordinary fold rejection after actual queue completion so feedback layout cannot revoke admitted successors.
    fold_failure: RefCell<Option<ViewError>>,
    move_prepared: Cell<bool>,
    /// If the level-zero fast path cannot determine screen order, this same queue head switches to actual visual querying.
    visual_word: Cell<bool>,
    pub(super) visual_seed: Cell<Option<VisualIntent>>,
    pub(super) visual_ready: Cell<Option<VisualSelection>>,
    /// Valid only on this queue head's Native interpretation stack; callback-derived actions precede later uninterpreted events.
    interpreting_native: Cell<bool>,
    native_insert_at: Cell<usize>,
    /// Defer bounded business feedback until this chain ends; status layout cannot revoke its still-running admitted input.
    attachment_failure: RefCell<Option<ViewError>>,
    waiting_vertical: RefCell<Option<Rc<super::navigation::PendingVertical>>>,
}

impl PendingWordDelete {
    /// Only the published receipt replaces this step's before-state; geometry and original Doc/Cell identity remain from the same request.
    fn request(&self) -> NativeMarkdownPagedSceneRequest {
        let mut request = self.request.borrow().clone();
        if let Some(receipt) = self.receipt.borrow().as_ref() {
            request.input = receipt.state;
        }
        request
    }

    /// Coalesce adjacent identical directions only; preserve reversals in order. Reject overflow/full runs before losing any admitted action.
    fn append_delete(&self, forward: bool, grapheme: bool) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !self.interpreting_native.get()
            && let Some(WordInput::Delete {
                forward: last,
                grapheme: old_grapheme,
                remaining,
            }) = steps.back_mut()
            && *last == forward
            && *old_grapheme == grapheme
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() >= 32 {
            return Err(ViewError::Busy); // Use the existing bounded-run contract, not a 32-repeat limit on holding one direction.
        }
        let step = WordInput::Delete {
            forward,
            grapheme,
            remaining: 1,
        };
        if self.interpreting_native.get() {
            let at = self.native_insert_at.get();
            steps.insert(at, step);
            self.native_insert_at.set(at + 1);
        } else {
            steps.push_back(step);
        }
        Ok(())
    }

    /// Coalesce only identical direction/extension repeats; interleaved motions retain arrival order.
    fn append_move(&self, motion: WordMotion, extend: bool) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !self.interpreting_native.get()
            && let Some(WordInput::Move {
                motion: last,
                extend: extending,
                remaining,
            }) = steps.back_mut()
            && *last == motion
            && *extending == extend
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() >= 32 {
            return Err(ViewError::Busy);
        }
        let step = WordInput::Move {
            motion,
            extend,
            remaining: 1,
        };
        if self.interpreting_native.get() {
            let at = self.native_insert_at.get();
            steps.insert(at, step);
            self.native_insert_at.set(at + 1);
        } else {
            steps.push_back(step);
        }
        Ok(())
    }

    /// Endpoint actions retain direction/extension intent; execution reads actual length/anchor rather than old arrival-time coordinates.
    fn append_document(&self, end: bool, extend: bool) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !self.interpreting_native.get()
            && let Some(WordInput::Document {
                end: old_end,
                extend: old_extend,
                remaining,
            }) = steps.back_mut()
            && *old_end == end
            && *old_extend == extend
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() >= 32 {
            return Err(ViewError::Busy); // Preserve admitted actions instead of overwriting an old run to make room.
        }
        let step = WordInput::Document {
            end,
            extend,
            remaining: 1,
        };
        if self.interpreting_native.get() {
            let at = self.native_insert_at.get();
            steps.insert(at, step);
            self.native_insert_at.set(at + 1);
        } else {
            steps.push_back(step);
        }
        Ok(())
    }

    /// Read only the actual head endpoint; later endpoint intent cannot overtake current input.
    fn document_motion(&self) -> Option<(bool, bool)> {
        match self.steps.borrow().front()? {
            WordInput::Document { end, extend, .. } => Some((*end, *extend)),
            _ => None, // Only a head endpoint action owns this meaning; later input cannot change selection ahead of it.
        }
    }

    /// Consume after actual selection completion; endpoints add no text history but preserve selection-side/serial contracts.
    fn consume_document(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let Some(WordInput::Document { remaining, .. }) = steps.front_mut() else {
            return Err(ViewError::Revoked);
        };
        *remaining -= 1;
        if *remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    /// Coalesce adjacent identical commands only; different indentation/Return retains arrival order and independent transactions.
    fn append_source(&self, command: MarkdownEditingCommand) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !self.interpreting_native.get()
            && let Some(WordInput::Source {
                command: old,
                remaining,
            }) = steps.back_mut()
            && *old == command
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() >= 32 {
            return Err(ViewError::Busy); // Reject the new run first, retaining admitted commands and characters in their original chain.
        }
        let step = WordInput::Source {
            command,
            remaining: 1,
        };
        if self.interpreting_native.get() {
            let at = self.native_insert_at.get();
            steps.insert(at, step);
            self.native_insert_at.set(at + 1);
        } else {
            steps.push_back(step);
        }
        Ok(())
    }

    fn source_command(&self) -> Option<MarkdownEditingCommand> {
        match self.steps.borrow().front()? {
            WordInput::Source { command, .. } => Some(*command),
            _ => None, // Other steps retain their own GCB, selection, or insertion semantics.
        }
    }

    /// Consume one command only after Core commit or explicit Unchanged, never later Return/indentation in advance.
    fn consume_source(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let Some(WordInput::Source { remaining, .. }) = steps.front_mut() else {
            return Err(ViewError::Revoked);
        };
        *remaining -= 1;
        if *remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    /// Share the Word/Typing queue; coalesce only adjacent motions with the same direction and extension rule.
    fn append_visual(&self, right: bool, extend: bool) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !self.interpreting_native.get()
            && let Some(WordInput::Visual {
                right: old,
                extend: old_extend,
                remaining,
            }) = steps.back_mut()
            && *old == right
            && *old_extend == extend
        {
            *remaining = remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            return Ok(());
        }
        if steps.len() >= 32 {
            return Err(ViewError::Busy);
        }
        let step = WordInput::Visual {
            right,
            extend,
            remaining: 1,
        };
        if self.interpreting_native.get() {
            let at = self.native_insert_at.get();
            steps.insert(at, step);
            self.native_insert_at.set(at + 1);
        } else {
            steps.push_back(step);
        }
        Ok(())
    }
    pub(super) fn visual_motion(&self) -> Option<(bool, bool)> {
        match self.steps.borrow().front()? {
            WordInput::Visual { right, extend, .. } => Some((*right, *extend)),
            WordInput::Move { motion, extend, .. }
                if !*extend && self.request().input.primary_range.length != 0 =>
            {
                motion.visual_right().map(|right| (right, false))
            } // Nonempty visual selection collapses once using actual line.base_level, without classification or logical-endpoint side guesses.
            _ => None,
        }
    }
    /// Expose Word left/right only after this head selected the actual visual consumer; logical and ordinary character motion remain separate.
    pub(super) fn visual_word_motion(&self) -> Option<(bool, bool)> {
        if !self.visual_word.get() {
            return None;
        }
        self.moving()
            .and_then(|(motion, extend)| motion.visual_right().map(|right| (right, extend)))
    }
    fn consume_visual(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let Some(WordInput::Visual { remaining, .. }) = steps.front_mut() else {
            return Err(ViewError::Revoked);
        };
        *remaining -= 1;
        if *remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    fn moving(&self) -> Option<(WordMotion, bool)> {
        match self.steps.borrow().front()? {
            WordInput::Move { motion, extend, .. } => Some((*motion, *extend)),
            _ => None, // Other input owns no Word motion direction; current selection cannot supply a guessed direction.
        }
    }

    /// Consume one published or explicitly rejected system action, preserving all later admitted input.
    fn consume_move(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let Some(WordInput::Move { remaining, .. }) = steps.front_mut() else {
            return Err(ViewError::Revoked);
        };
        *remaining -= 1;
        if *remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    /// Only the actual head Word step computes ranges; input slots use original transactions rather than Word approximations.
    fn direction(&self) -> Option<bool> {
        match self.steps.borrow().front()? {
            WordInput::Delete { forward, .. } => Some(*forward),
            WordInput::Typing(_)
            | WordInput::Native(_)
            | WordInput::Document { .. }
            | WordInput::Source { .. }
            | WordInput::Select { .. }
            | WordInput::Move { .. }
            | WordInput::Visual { .. } => None,
        }
    }

    /// Whether this admitted action deletes one whole grapheme, never inferred from later selection/layout.
    fn deleting_grapheme(&self) -> bool {
        matches!(
            self.steps.borrow().front(),
            Some(WordInput::Delete { grapheme: true, .. })
        )
    }

    /// Body keeps its same-ticket Scene only for this non-text-changing query, preserving later actual pointer hits.
    pub(super) fn selecting(&self) -> bool {
        matches!(self.steps.borrow().front(), Some(WordInput::Select { .. }))
    }

    /// Advance the actual started query only; same-gesture target updates cannot repeatedly restart the initial group during fast dragging.
    fn selection_query(&self) -> Option<(usize, bool, Option<CellWordTarget>)> {
        let steps = self.steps.borrow();
        match steps.front()? {
            WordInput::Select { query, .. } => Some(*query),
            _ => None,
        }
    }

    /// Store the first group only after its query is Ready; later query only the latest target, never re-expanding the original seed.
    /// Complete at most one query per quantum; a new target advances in the next original scheduling quantum.
    fn selection_target_query(
        &self,
        range: Range<usize>,
    ) -> Option<(usize, bool, Option<CellWordTarget>)> {
        let mut steps = self.steps.borrow_mut();
        let WordInput::Select {
            selection,
            query,
            first,
            ..
        } = steps.front_mut()?
        else {
            return None;
        };
        if first.is_none() && !matches!(selection.anchor, Some(WordAnchor::Selection(_))) {
            *first = Some(range);
        }
        // Targets may change during the query; query records which position actually completed, rather than assigning old Ready to the latest point.
        // Once available, the first group stays until selection ends; reverse drag changes target query without rereading that group.
        let latest = (
            selection.head_utf16,
            selection.backward_bias,
            selection.cell,
        );
        if *query != latest {
            *query = latest;
            return Some(latest);
        }
        None
    }

    /// A new point replaces only this actual Down's latest endpoint; its original permit and initial query remain unique.
    /// Only a pure Select head may coalesce this way; committed keyboard/input obligations retain their order.
    fn replace_pointer_selection(
        &self,
        selection: WordSelection,
        gesture: &Option<Weak<()>>,
    ) -> bool {
        let mut steps = self.steps.borrow_mut();
        if steps.len() != 1 {
            return false;
        }
        let Some(WordInput::Select {
            selection: latest,
            gesture: original,
            ..
        }) = steps.front_mut()
        else {
            return false;
        };
        let same = match (original.as_ref(), gesture.as_ref()) {
            (Some(original), Some(gesture)) => Weak::ptr_eq(original, gesture),
            _ => false,
        };
        if !same {
            return false;
        }
        // Leave the original permit, Source binding, and advancing Core cursor unchanged; the original Body remains sole cancellation owner.
        // Latest endpoint retains scalar coordinates and original anchor role, without events or an unbounded queue of intermediate drags.
        *latest = selection;
        drop(steps);
        self.ready.borrow_mut().take(); // The target may have changed; old Ready cannot prematurely commit its successor position.
        true
    }

    /// Only this pure selection chain follows pointer retirement; actual text and other admitted input still complete in order.
    fn pointer_gesture(&self) -> Option<Weak<()>> {
        let steps = self.steps.borrow();
        if steps.len() != 1 {
            return None;
        }
        match steps.front() {
            Some(WordInput::Select { gesture, .. }) => gesture.clone(),
            _ => None,
        }
    }

    /// Transfer the actual first group with its original gesture; a final union or Shift scalar cannot impersonate that initial group.
    fn pointer_group(&self) -> Option<(WordAnchor, Weak<()>)> {
        let steps = self.steps.borrow();
        let WordInput::Select {
            selection,
            first,
            gesture,
            origin_utf16,
            ..
        } = steps.front()?
        else {
            return None;
        };
        let anchor = match selection.anchor {
            Some(WordAnchor::Selection(anchor)) => WordAnchor::Selection(anchor),
            _ => {
                let group = first.as_ref()?;
                WordAnchor::Group {
                    from_utf16: group.start,
                    to_utf16: group.end,
                    origin_utf16: *origin_utf16,
                }
            }
        };
        Some((anchor, gesture.clone()?))
    }

    /// Preserve existing CodeMirror rangeForClick/SelectionRange.extend behavior: dragging retains whole endpoint groups and Shift retains actual anchor.
    fn selection_target(
        &self,
        range: Range<usize>,
    ) -> std::result::Result<MarkdownTextSelection, ViewError> {
        if let Some((motion, extend)) = self.moving() {
            let forward = matches!(motion, WordMotion::Forward | WordMotion::Right);
            let request = self.request();
            let before = request
                .input
                .directed_selection()
                .ok_or(ViewError::Revoked)?;
            let from = before.anchor_utf16.min(before.head_utf16);
            let to = before.anchor_utf16.max(before.head_utf16);
            let head = if !extend && from != to {
                let end = if forward { to } else { from };
                if range != (end..end) {
                    return Err(ViewError::InvalidRange);
                }
                end // Existing cursorByGroup collapses nonempty selection to from/to once, without also crossing another group.
            } else {
                if (forward && range.start != before.head_utf16)
                    || (!forward && range.end != before.head_utf16)
                {
                    return Err(ViewError::InvalidRange);
                }
                if forward { range.end } else { range.start }
            };
            return Ok(MarkdownTextSelection {
                anchor_utf16: if extend { before.anchor_utf16 } else { head },
                head_utf16: head,
            }); // Extension changes actual head only, never guessing or reversing the original anchor from direction/range.
        }
        let steps = self.steps.borrow();
        let Some(WordInput::Select {
            selection,
            first,
            origin_utf16,
            ..
        }) = steps.front()
        else {
            return Err(ViewError::Revoked);
        };
        let (anchor_utf16, head_utf16) = match selection.anchor {
            None => (range.start, range.end),
            Some(WordAnchor::Selection(anchor)) => {
                if range.start <= anchor && anchor <= range.end {
                    (range.start, range.end) // An anchor inside the group expands to that whole group rather than leaving a partial word.
                } else {
                    let head = if range.start.abs_diff(anchor) > range.end.abs_diff(anchor) {
                        range.start
                    } else {
                        range.end
                    };
                    (anchor, head) // An anchor outside the group extends only the active endpoint to the farther group edge.
                }
            }
            Some(WordAnchor::Word { .. } | WordAnchor::Group { .. }) => {
                let first = first.as_ref().ok_or(ViewError::Revoked)?;
                if selection.head_utf16 == *origin_utf16 {
                    (range.start, range.end) // At the original click position, do not union the first group; affinity changes keep the actual current target group.
                } else if range.start > first.start {
                    (first.start, first.end.max(range.end))
                } else {
                    (first.end.max(range.end), first.start.min(range.start))
                } // Other same-group positions keep existing reverse-drag semantics without re-expanding the first word.
            }
        };
        Ok(MarkdownTextSelection {
            anchor_utf16,
            head_utf16,
        })
    }

    fn consume_selection(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        if !matches!(steps.front(), Some(WordInput::Select { .. })) {
            return Err(ViewError::Revoked);
        }
        steps.pop_front(); // Later ordinary input may consume this selection only after commit or actual Unchanged.
        Ok(())
    }

    /// Decrement only after actual commit or proven endpoint Unchanged, never before a deletion occurs.
    fn consume_word(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let Some(WordInput::Delete { remaining, .. }) = steps.front_mut() else {
            return Err(ViewError::Revoked);
        };
        *remaining -= 1;
        if *remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    /// While the Native payload getter is still on-stack, the continuation cannot pass its reserved input slot.
    fn copying_input(&self) -> bool {
        self.steps.borrow().iter().any(|step| match step {
            WordInput::Typing(text) => text.borrow().is_none(),
            WordInput::Native(input) => input.borrow().is_none(),
            WordInput::Delete { .. }
            | WordInput::Document { .. }
            | WordInput::Source { .. }
            | WordInput::Select { .. }
            | WordInput::Move { .. }
            | WordInput::Visual { .. } => false,
        })
    }

    /// Cancellation releases converted payloads first; reservations still on the Native stack return through their own failure exit.
    fn clear_steps(&self) {
        *self.fold_expansion.borrow_mut() = None;
        drop(self.fold_failure.borrow_mut().take());
        let attachment_failure = self.attachment_failure.borrow_mut().take();
        drop(attachment_failure); // Cancellation returns original error text; a closed old queue cannot display status in a successor window.
        let steps = std::mem::take(&mut *self.steps.borrow_mut());
        let bytes: usize = steps
            .iter()
            .map(|step| match step {
                WordInput::Typing(text) => text.borrow().as_ref().map_or(0, String::len),
                WordInput::Native(input) => input.borrow().as_ref().map_or(0, NativeInput::bytes),
                WordInput::Delete { .. }
                | WordInput::Document { .. }
                | WordInput::Source { .. }
                | WordInput::Select { .. }
                | WordInput::Move { .. }
                | WordInput::Visual { .. } => 0,
            })
            .sum();
        drop(steps);
        self.queued_bytes.set(self.queued_bytes.get() - bytes);
    }

    /// selection_serial, marked, folds, and Cell domain belong to original InputState; equal text cannot renew successor selection authority.
    fn current_at(&self, owner: &NativeMarkdownPagedBody, serial: u64) -> bool {
        let request = self.request();
        owner.current_body()
            && owner.serial.get() == serial
            && owner
                .pending_word_delete
                .borrow()
                .as_ref()
                .is_some_and(|actual| std::ptr::eq(&**actual, self))
            && Weak::ptr_eq(&request.document, &owner.input.document_identity())
            && owner
                .input
                .target_domain()
                .is_ok_and(|target| target.same(&request.target))
            && owner
                .input
                .state()
                .is_ok_and(|state| state == request.input)
    }

    /// Before binding, original Input awaits the sole Producer; after binding, retry cannot change Source or serial.
    fn current(&self, owner: &NativeMarkdownPagedBody) -> bool {
        let serial = self.retired_serial.get().unwrap_or_else(|| {
            self.bound
                .borrow()
                .as_ref()
                .map_or(owner.serial.get(), |bound| bound.0)
        });
        self.current_at(owner, serial)
    }

    /// The same Native request must survive every actual reentry; owned cleanup predeclares serial+1 only, without authorizing other changes.
    fn current_native_at(&self, owner: &NativeMarkdownPagedBody, serial: u64) -> bool {
        self.current_at(owner, serial)
            && owner.view().is_ok_and(|view| {
                view.capture_scene_request().is_ok_and(|actual| {
                    if let Some(vertical) = self.waiting_vertical.borrow().as_ref() {
                        vertical.matches(owner, &actual)
                    } else {
                        actual.same(&self.request())
                    }
                })
            })
            && self.current_at(owner, serial)
    }
}

impl NativeMarkdownPagedBody {
    /// Close may cancel presentation and captured attachments not yet copying; uninterpreted input, text, and started transactions retain canonical obligations.
    /// Inspect the original queue only; begin_close retires all continuations/resources after actual close admission, without early cancellation here.
    pub(in crate::native_surface) fn has_pending_canonical_input(&self) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|pending| {
                pending.steps.borrow().iter().any(|step| match step {
                    WordInput::Move { .. }
                    | WordInput::Visual { .. }
                    | WordInput::Select { .. }
                    | WordInput::Document { .. } => false,
                    WordInput::Native(slot) => {
                        !matches!(
                            &*slot.borrow(),
                            Some(NativeInput::Fold(_) | NativeInput::Attachments { .. })
                        ) // None means a getter/interpretation/copy stack owns the payload: remain Busy and do not cancel started attachment work.
                    }
                    WordInput::Delete { .. } | WordInput::Typing(_) | WordInput::Source { .. } => {
                        true
                    }
                })
            })
    }

    /// Only an existing input chain retains pre-enqueue URL/batch rejection; feedback creates no second work chain.
    pub(super) fn retain_attachment_rejection(&self, message: &str) -> bool {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return false;
        };
        if !pending.current_native_at(self, self.serial.get()) {
            return false;
        }
        let mut failure = pending.attachment_failure.borrow_mut();
        if failure.is_none() {
            *failure = Some(ViewError::AttachmentImport(message.to_owned()));
        } // Retain only the first business rejection; Core/resource errors never enter this slot, and cancellation returns its bounded string.
        true
    }

    /// Explicit word/pointer selection supersedes old positioning while keeping the same Input permit, never successor text or renewed old-command authority.
    pub(super) fn accept_word_selection(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        selection: WordSelection,
        gesture: Option<Weak<()>>,
        permit: NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        if self.preparing.get() {
            return Err(ViewError::Busy.into());
        }
        let current = |epoch| {
            self.current(epoch)
                && self.input.state().is_ok_and(|state| state == request.input)
                && self.view().is_ok_and(|view| {
                    view.capture_scene_request()
                        .is_ok_and(|actual| actual.same(request))
                })
        };
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        if request.input.marked.is_some()
            || request.input.directed_selection().is_none()
            || selection.head_utf16 > request.input.utf16_len
            || selection.anchor.is_some_and(|anchor| match anchor {
                WordAnchor::Selection(anchor) => anchor > request.input.utf16_len,
                WordAnchor::Word { head_utf16, .. } => head_utf16 > request.input.utf16_len,
                WordAnchor::Group {
                    from_utf16,
                    to_utf16,
                    origin_utf16,
                } => {
                    from_utf16 > to_utf16
                        || to_utf16 > request.input.utf16_len
                        || origin_utf16 < from_utf16
                        || origin_utf16 > to_utf16
                }
            })
            || !self
                .input
                .target_domain()
                .map_err(ViewError::from)?
                .same(&request.target)
            || !Weak::ptr_eq(&request.document, &self.input.document_identity())
        {
            return Err(ViewError::InvalidRange.into());
        } // Word inference cannot replace IME state, multiple-selection direction, or client-domain coordinates.
        self.refresh_word_delete(request)?;
        if self
            .pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|previous| {
                previous
                    .steps
                    .borrow()
                    .iter()
                    .any(|step| !matches!(step, WordInput::Select { .. }))
            })
        {
            // Old Scene click endpoints cannot become successor text coordinates; reject before admission while preserving committed character obligations.
            if let Some(producer) = self.producer.borrow().as_ref() {
                producer.target_source_health()?;
            }
            return Err(ViewError::Busy.into());
        }
        let previous = self.pending_word_delete.borrow().clone();
        if let Some(previous) = previous
            && previous.request().same(request)
            && previous.replace_pointer_selection(selection, &gesture)
        {
            self.input
                .check_permit(&permit, &current)
                .map_err(ViewError::from)?;
            return self.schedule(); // The same gesture updates its latest point only, without cancelling the initial group or acquiring another Source/Reader.
        }
        self.cancel_word_delete()?; // Cancel old word work only after the new selection ticket passes; original Source failure cannot become pointer takeover.
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        drop(self.selection_reveal.borrow_mut().take());
        self.revoke_source_target();
        *self.pending_word_delete.borrow_mut() = Some(Rc::new(PendingWordDelete {
            request: RefCell::new(request.clone()),
            steps: RefCell::new(VecDeque::from([WordInput::Select {
                selection,
                gesture,
                // Start an unready actual first-click seed once; a known Group queries the current target directly without reconstructing its origin.
                query: match selection.anchor {
                    Some(WordAnchor::Word {
                        head_utf16,
                        backward_bias,
                        cell,
                    }) => (head_utf16, backward_bias, cell),
                    _ => (
                        selection.head_utf16,
                        selection.backward_bias,
                        selection.cell,
                    ),
                },
                first: match selection.anchor {
                    Some(WordAnchor::Group {
                        from_utf16,
                        to_utf16,
                        ..
                    }) => Some(from_utf16..to_utf16),
                    _ => None,
                },
                origin_utf16: match selection.anchor {
                    Some(WordAnchor::Word { head_utf16, .. }) => head_utf16,
                    Some(WordAnchor::Group { origin_utf16, .. }) => origin_utf16,
                    _ => selection.head_utf16,
                },
            }])),
            queued_bytes: Cell::new(0),
            receipt: RefCell::new(None),
            retired_serial: Cell::new(None),
            permit: RefCell::new(Some(permit)),
            bound: RefCell::new(None),
            ready: RefCell::new(None),
            fold_expansion: RefCell::new(None),
            fold_failure: RefCell::new(None),
            move_prepared: Cell::new(false),
            visual_word: Cell::new(false),
            visual_seed: Cell::new(None),
            visual_ready: Cell::new(None),
            interpreting_native: Cell::new(false),
            native_insert_at: Cell::new(0),
            attachment_failure: RefCell::new(None),
            waiting_vertical: RefCell::new(None),
        }));
        self.schedule()
    }

    /// Standard Word motion uses this same admitted chain; original Producer preparation alone determines whole-Source UBA capability.
    pub(super) fn accept_word_move(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        motion: WordMotion,
        extend: bool,
        permit: NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        self.accept_queued_move(
            request,
            WordInput::Move {
                motion,
                extend,
                remaining: 1,
            },
            permit,
        )
    }

    /// Keep the original synchronous Ready interaction; only an incomplete query enters asynchronous continuation.
    pub(super) fn accept_visual_move(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        right: bool,
        extend: bool,
        permit: NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        self.refresh_word_delete(request)?;
        let queued = self.pending_word_delete.borrow().is_some();
        let key = self
            .scene
            .borrow()
            .as_ref()
            .map(|scene| scene.key().clone());
        let ready = !queued
            && key
                .as_ref()
                .is_some_and(|key| key.request.same(request) && self.scene_current(key));
        self.accept_queued_move(
            request,
            WordInput::Visual {
                right,
                extend,
                remaining: 1,
            },
            permit,
        )?;
        if !ready {
            return Ok(());
        } // Never bypass an admitted input or guess geometry before the first real Scene.
        let pending = self
            .pending_word_delete
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        let serial = self.serial.get();
        let result: Result<()> = (|| {
            let source = {
                let slot = self.producer.borrow();
                let Some(producer) = slot.as_ref() else {
                    return Ok(());
                };
                let key = key
                    .as_ref()
                    .expect("The original Ready key was captured before this bounded attempt");
                producer.target_source_health()?;
                if !producer.owns_target_source(key) {
                    return Ok(());
                }
                producer.target_source_authority(request, serial)?
            }; // Drop the Producer borrow before any input/current Native callback.
            *pending.bound.borrow_mut() = Some((serial, source.clone()));
            let seed_result = self.bind_visual_seed(&pending, request, serial, right, extend);
            {
                let slot = self.producer.borrow();
                if let Some(producer) = slot.as_ref() {
                    producer.target_source_health()?;
                }
            } // A Source error recorded by a callback must precede its ordinary permission failure.
            let seed = seed_result?;
            let step = {
                let mut slot = self.producer.borrow_mut();
                let producer = slot.as_mut().ok_or(ViewError::Revoked)?;
                if producer.target_source_authority(request, serial)? != source {
                    return Err(ViewError::Revoked.into());
                }
                producer.step_visual_intent(request, seed)?
            }; // The pure bounded query cannot reenter AppKit while the Producer slot is borrowed.
            if let super::producer::VisualAdvance::Ready(ready) = step {
                pending.visual_ready.set(Some(ready));
                self.finish_word_delete()?; // Publish through the same permit/receipt before the native command returns.
            }
            Ok(()) // Pending/NeedPrefix retain the same query and seed for the already scheduled original queue.
        })();
        self.word_input_result(&pending, serial, serial, result)
    }

    /// Read navigation facts from the actual front permit once, after prior character commits have really completed.
    fn bind_visual_seed(
        &self,
        pending: &PendingWordDelete,
        request: &NativeMarkdownPagedSceneRequest,
        serial: u64,
        right: bool,
        extend: bool,
    ) -> Result<VisualIntent> {
        if let Some(seed) = pending.visual_seed.get() {
            return Ok(seed);
        }
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let result: Result<VisualIntent> = (|| {
            let side = self
                .input
                .navigation_bidi_side(&permit, &current)
                .map_err(ViewError::from)?;
            let (upstream, _) = self
                .input
                .navigation_selection_facts(&permit, &current)
                .map_err(ViewError::from)?;
            let selection = request
                .input
                .directed_selection()
                .ok_or(ViewError::Revoked)?;
            self.input
                .check_permit(&permit, &current)
                .map_err(ViewError::from)?;
            Ok(VisualIntent {
                selection,
                upstream: upstream.unwrap_or(false),
                side,
                right,
                extend,
            })
        })();
        *pending.permit.borrow_mut() = Some(permit); // A failed/revoked getter cannot silently consume the queued permit.
        let seed = result?;
        pending.visual_seed.set(Some(seed));
        Ok(seed)
    }

    fn accept_queued_move(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        action: WordInput,
        permit: NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        self.refresh_word_delete(request)?;
        let previous = self.pending_word_delete.borrow().clone();
        if let Some(previous) = previous {
            if let Some(producer) = self.producer.borrow().as_ref() {
                producer.target_source_health()?;
            }
            let serial = self.serial.get();
            let current = |epoch| self.current(epoch) && previous.current_native_at(self, serial);
            self.input
                .check_permit(&permit, &current)
                .map_err(ViewError::from)?;
            match action {
                WordInput::Visual { right, extend, .. } => previous.append_visual(right, extend)?,
                WordInput::Move { motion, extend, .. } => previous.append_move(motion, extend)?,
                _ => unreachable!("原移动入口只创建实际 Visual/Move 动作"),
            }
            return self.schedule(); // Before selection commits, retain direction/extension only without guessing a future head.
        }
        if self.preparing.get() {
            return Err(ViewError::Busy.into());
        }
        let current = |epoch| {
            self.current(epoch)
                && self.input.state().is_ok_and(|state| state == request.input)
                && self.view().is_ok_and(|view| {
                    view.capture_scene_request()
                        .is_ok_and(|actual| actual.same(request))
                })
        };
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        if request.input.marked.is_some()
            || request.input.selection_range_count != 1
            || request.input.directed_selection().is_none()
            || !self
                .input
                .target_domain()
                .map_err(ViewError::from)?
                .same(&request.target)
            || !Weak::ptr_eq(&request.document, &self.input.document_identity())
        {
            return Err(ViewError::InvalidRange.into());
        } // Word commands cannot infer direction for multiple selection, marked input, or another Source.
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        drop(self.selection_reveal.borrow_mut().take());
        self.revoke_source_target();
        *self.pending_word_delete.borrow_mut() = Some(Rc::new(PendingWordDelete {
            request: RefCell::new(request.clone()),
            steps: RefCell::new(VecDeque::from([action])),
            queued_bytes: Cell::new(0),
            receipt: RefCell::new(None),
            retired_serial: Cell::new(None),
            permit: RefCell::new(Some(permit)),
            bound: RefCell::new(None),
            ready: RefCell::new(None),
            fold_expansion: RefCell::new(None),
            fold_failure: RefCell::new(None),
            move_prepared: Cell::new(false),
            visual_word: Cell::new(false),
            visual_seed: Cell::new(None),
            visual_ready: Cell::new(None),
            interpreting_native: Cell::new(false),
            native_insert_at: Cell::new(0),
            attachment_failure: RefCell::new(None),
            waiting_vertical: RefCell::new(None),
        }));
        self.schedule()
    }

    /// Single-selection system actions preserve order with original permits; without a pending chain, delete nonempty selection directly while View owns multiple ranges.
    pub(super) fn accept_word_delete(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        forward: bool,
        permit: NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        self.refresh_word_delete(request)?;
        let previous = self.pending_word_delete.borrow().clone();
        if let Some(previous) = previous {
            if let Some(producer) = self.producer.borrow().as_ref() {
                producer.target_source_health()?; // Appending cannot hide an existing original Source read failure.
            }
            let serial = self.serial.get();
            let current = |epoch| self.current(epoch) && previous.current_native_at(self, serial);
            self.input
                .check_permit(&permit, &current)
                .map_err(ViewError::from)?;
            previous.append_delete(forward, false)?;
            return self.schedule(); // Append to the original chain only; Native reentry neither commits nor uses this before-state for a future action.
        }
        if self.preparing.get() {
            return Err(ViewError::Busy.into()); // A preparation stack without an original input chain cannot start another synchronous edit.
        }
        let current = |epoch| {
            self.current(epoch)
                && self.input.state().is_ok_and(|state| state == request.input)
                && self.view().is_ok_and(|view| {
                    view.capture_scene_request()
                        .is_ok_and(|actual| actual.same(request))
                })
        };
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        if request.input.marked.is_some()
            || request.input.selection_range_count != 1
            || request.input.directed_selection().is_none()
            || !self
                .input
                .target_domain()
                .map_err(ViewError::from)?
                .same(&request.target)
            || !Weak::ptr_eq(&request.document, &self.input.document_identity())
        {
            return Err(ViewError::InvalidRange.into()); // This delegate boundary accepts one actual unmarked selection in the original Body/Cell domain only.
        }
        if request.input.primary_range.length != 0 {
            // Without an async chain, delete a clear single nonempty selection in this call. Later Word actions behind pending Select
            // were already enqueued above and cannot prematurely delete its old pre-selection range.
            let view = self.view()?;
            let outcome = self
                .input
                .delete_selected_ranges(&permit, &current)
                .map_err(ViewError::from)?;
            return match outcome {
                super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => {
                    view.finish_word_delete_commit(&commit).map_err(Into::into)
                }
                super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => Ok(()),
            };
        }
        // Later character intent supersedes original positioning; revoke display targets without early canonical mutation or new reads.
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        drop(self.selection_reveal.borrow_mut().take());
        self.revoke_source_target();
        let pending = Rc::new(PendingWordDelete {
            request: RefCell::new(request.clone()),
            steps: RefCell::new(VecDeque::from([WordInput::Delete {
                forward,
                grapheme: false,
                remaining: 1,
            }])),
            queued_bytes: Cell::new(0),
            receipt: RefCell::new(None),
            retired_serial: Cell::new(None),
            permit: RefCell::new(Some(permit)),
            bound: RefCell::new(None),
            ready: RefCell::new(None),
            fold_expansion: RefCell::new(None),
            fold_failure: RefCell::new(None),
            move_prepared: Cell::new(false),
            visual_word: Cell::new(false),
            visual_seed: Cell::new(None),
            visual_ready: Cell::new(None),
            interpreting_native: Cell::new(false),
            native_insert_at: Cell::new(0),
            attachment_failure: RefCell::new(None),
            waiting_vertical: RefCell::new(None),
        });
        *self.pending_word_delete.borrow_mut() = Some(pending);
        self.schedule()
    }

    /// Document endpoints follow the original chain; without pending work keep direct endpoint interaction, without another pipeline.
    pub(super) fn accept_document_motion(
        &self,
        end: bool,
        extend: bool,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        if self.input.is_cell() {
            return Ok(false); // Body endpoints use this ordering; original local selection/client interaction still handles Cell endpoints directly.
        }
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
        if pending.interpreting_native.get() {
            return Ok(false); // This original key callback reached its actual caret; it cannot defer behind later uninterpreted events again.
        }
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        } // Existing actual Source failure takes precedence and cannot become ordinary cancellation through later absolute navigation.
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(permit, &current)
            .map_err(ViewError::from)?;
        pending.append_document(end, extend)?;
        self.schedule()?;
        Ok(true)
    }

    /// Defer existing Body Return/Tab/Backtab only; original client controller owns Cell row/column navigation, never disguised as Body input.
    pub(super) fn accept_source_command(
        &self,
        command: MarkdownEditingCommand,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        if self.input.is_cell() || command == MarkdownEditingCommand::DeleteMarkupBackward {
            return Ok(false); // Actual GCB queue owns Backspace; canonical indentation cannot modify a Cell-local draft.
        }
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false); // Ordinary Ready input stays synchronous; do not create a chain merely to defer it or alter interaction.
        };
        if pending.interpreting_native.get() {
            return Ok(false); // This original key callback reached its actual caret; it cannot defer behind later uninterpreted events again.
        }
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        } // Queue quota or successor input cannot override original Source failure.
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(permit, &current)
            .map_err(ViewError::from)?;
        pending.append_source(command)?;
        self.schedule()?;
        Ok(true)
    }

    /// While actual motion is uncommitted, enqueue later Backspace/Delete in the same chain; otherwise keep synchronous interaction.
    pub(super) fn accept_grapheme_delete(
        &self,
        forward: bool,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false); // Ready deletion adds no async step, preserving original markup handling and IME path.
        };

        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?; // Queue admission cannot replace an existing original Source read failure.
        }
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(permit, &current)
            .map_err(ViewError::from)?;
        pending.append_delete(forward, true)?; // Record intent only; resolve its GCB range after preceding actual receipts complete.
        self.schedule()?;
        Ok(true)
    }

    /// Ordinary NSNotFound insertion reserves actual order before converting its sole payload; without Word chain keep direct input.
    pub(super) fn accept_word_typing(
        &self,
        text: &objc2::runtime::AnyObject,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false); // Without pending work add no Native getter or change to explicit large-input/Frozen admission.
        };
        if pending.interpreting_native.get() {
            return Ok(false); // This original key callback reached its actual caret; it cannot defer behind later uninterpreted events again.
        }
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?; // Queue quota rejection cannot override the original Source's recorded first read failure.
        }
        let serial = self.serial.get();
        let payload = Rc::new(RefCell::new(None));
        {
            let mut steps = pending.steps.borrow_mut();
            if steps.len() >= 32 {
                return Err(ViewError::Busy.into()); // Reject the new slot while retaining every admitted direction and character.
            }
            steps.push_back(WordInput::Typing(Rc::clone(&payload)));
        } // Reserve before the first Native read so nested input can follow only after this action.
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let charged = Cell::new(0);
        let converted = self
            .input
            .copy_permit_text_with_limit(permit, text, &current, &|bytes| {
                let remaining =
                    super::super::text_storage::NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES
                        .saturating_sub(pending.queued_bytes.get());
                if bytes > remaining {
                    return false; // Reject before copying, not after allocating another large payload to inspect its total.
                }
                pending.queued_bytes.set(pending.queued_bytes.get() + bytes);
                charged.set(bytes);
                true
            });
        match converted {
            Ok(text) => {
                // Conversion checked the original ticket after its final getter; only Rust moves follow, without acquiring successor authority.
                *payload.borrow_mut() = Some(text);
            }
            Err(cause) => {
                pending
                    .queued_bytes
                    .set(pending.queued_bytes.get() - charged.get());
                pending.steps.borrow_mut().retain(|step| {
                    !matches!(step, WordInput::Typing(actual) if Rc::ptr_eq(actual, &payload))
                }); // Remove this conversion slot only, preserving later successfully admitted characters and successor queues.
                if pending.current(self) {
                    self.schedule()?;
                }
                return Err(ViewError::from(cause).into());
            }
        }
        self.schedule()?; // This actual conversion completion resumes the continuation that original advance suspended.
        Ok(true)
    }

    /// Only this chain's own actual Commit notification bypasses cancellation; equal state or a successor receipt cannot impersonate it.
    pub(super) fn owns_word_commit(&self, commit: &NativeMarkdownPagedInputCommit) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|pending| {
                pending
                    .receipt
                    .borrow()
                    .as_ref()
                    .is_some_and(|receipt| std::ptr::eq(&**receipt, commit))
            })
    }

    /// Wait through existing deferred scheduling while payload getters remain on-stack, without another timer or added Native retention.
    pub(super) fn word_input_is_copying(&self) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.copying_input())
    }

    /// Insertion, Source commands, and document endpoints consume preceding actual receipts; execute one original transaction per turn.
    pub(super) fn advance_word_typing(&self) -> Result<bool> {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
        if pending.waiting_vertical.borrow().is_some() {
            return Ok(false); // The original vertical action lacks its final receipt; later IME events cannot be interpreted yet.
        }
        if pending.native_front() {
            let scene = self.scene.borrow().clone();
            let ready = scene.as_ref().is_some_and(|scene| {
                scene.key().request.same(&pending.request()) && self.scene_current(scene.key())
            });
            drop(scene); // Release the temporary Scene claim before Native interpretation, retaining no Reader across input callbacks.
            if !ready {
                return Ok(false);
            } // The event may request vertical/line endpoints; wait for actual Scene instead of interpreting, failing, and replaying IME.
            return self.advance_native_input(&pending);
        }
        if let Some((end, extend)) = pending.document_motion() {
            let serial = self.serial.get();
            let result = self.commit_document_motion(&pending, end, extend, serial);
            self.word_input_result(&pending, serial, serial, result)?;
            return Ok(true); // Complete this actual selection only, without reading EOF text or executing later typing early.
        }
        if let Some(command) = pending.source_command() {
            let serial = self.serial.get();
            let released_serial = serial
                .checked_add(1)
                .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?;
            let result = self.commit_word_input(
                &pending,
                WordEdit::Source(command),
                serial,
                released_serial,
            );
            self.word_input_result(&pending, serial, released_serial, result)?;
            return Ok(true); // This turn performs one actual Core semantic transaction, without later input or additional layout.
        }
        let (payload, serial, released_serial) = {
            let mut steps = pending.steps.borrow_mut();
            if !matches!(steps.front(), Some(WordInput::Typing(_))) {
                return Ok(false);
            }
            let serial = self.serial.get();
            let released_serial = serial
                .checked_add(1)
                .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?;
            match steps.pop_front() {
                Some(WordInput::Typing(payload)) => (payload, serial, released_serial),
                _ => unreachable!("已在同一无重入借用中确认普通输入槽"),
            }
        };
        let text = payload.borrow_mut().take().ok_or(ViewError::Busy)?;
        let result =
            self.commit_word_input(&pending, WordEdit::Typing(&text), serial, released_serial);
        let bytes = text.len();
        drop(text); // Return cumulative allowance after the actual insertion copy drops; Native reentry cannot consume it twice.
        pending.queued_bytes.set(pending.queued_bytes.get() - bytes);
        self.word_input_result(&pending, serial, released_serial, result)?;
        Ok(true)
    }

    /// Editing, selection, scroll, or mode geometry changes revoke original Word intent; cancellation still preserves existing Source failure.
    pub(super) fn refresh_word_delete(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let pending = self.pending_word_delete.borrow().clone();
        if pending.as_ref().is_some_and(|pending| {
            !pending.current(self)
                || if let Some(vertical) = pending.waiting_vertical.borrow().as_ref() {
                    !vertical.matches(self, request)
                } else {
                    !pending.request().same(request)
                }
        }) {
            let gesture = pending
                .as_ref()
                .and_then(|pending| pending.pointer_gesture());
            self.cancel_word_delete()?;
            if let Some(gesture) = gesture {
                // Clear waiting-group facts only after actual query return; Source failure remains failure, not successful empty selection.
                // Cleanup identifies this gesture alone, never a reentrant successor Down or actual Reopen Source.
                self.cancel_waiting_pointer_word(&gesture);
            }
        }
        Ok(())
    }

    /// Original Up may precede the first group; the sole pointer slot retains that endpoint, while new Down revokes the old query.
    /// Only pure selection of the same weak identity follows this rule; admitted text still owns commit obligations independent of gesture lifetime.
    pub(super) fn has_pending_pointer_word(&self, intent: &Weak<()>) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .and_then(|pending| pending.pointer_gesture())
            .is_some_and(|original| Weak::ptr_eq(&original, intent))
    }

    /// Pointer cancellation matches only this original weak pure-selection identity, never real text transactions in the chain.
    pub(super) fn cancel_pointer_word(&self, intent: &Weak<()>) -> Result<()> {
        if self.has_pending_pointer_word(intent) {
            self.cancel_word_delete()?;
        }
        Ok(())
    }

    /// Take the slot before returning Core continuation/permit; hold no RefCell borrow across resource destruction.
    pub(super) fn cancel_word_delete(&self) -> Result<()> {
        let pending = self.pending_word_delete.borrow_mut().take();
        let Some(pending) = pending else {
            return Ok(()); // Without an original action, leave successor Producer untouched and acquire no Source.
        };
        let bound_serial = pending.bound.borrow().as_ref().map(|bound| bound.0);
        let request = pending.request();
        let health = {
            let mut slot = self.producer.borrow_mut();
            if let Some(producer) = slot.as_mut() {
                let health = match producer.replaced_cell_source(&self.input, &request) {
                    Ok(true) => Ok(()), // The Cell actually changed; only this old-draft Source invalidation is ordinary retirement.
                    Ok(false) => producer.target_source_health(),
                    Err(cause) => Err(cause), // Actual Body/Cell I/O failures remain errors rather than cancellation.
                };
                if bound_serial == Some(self.serial.get()) {
                    producer.cancel_word_delete(); // Cancel only the bound original Producer, preserving successor work even with equal generation.
                    producer.cancel_word_group(); // This same slot may expand original word selection; no second cursor or reader owner exists.
                    producer.cancel_visual();
                }
                health
            } else {
                Ok(()) // The sole Producer is on its preparation stack or already returned; cancellation creates no replacement reader.
            }
        };
        pending.clear_steps(); // The slot is detached; a local observing Rc cannot retain cancelled insertion strings.
        drop(pending);
        health
    }

    /// Bind Source once after selecting the sole Producer. Each turn advances one Core quantum, with no deletion commit before Ready.
    pub(super) fn advance_word_delete(
        &self,
        producer: &mut Producer,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let pending = self
            .pending_word_delete
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        producer.target_source_health()?;
        if !pending.current(self) || !pending.request().same(request) {
            return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
        }
        let serial = self.serial.get();
        let source = producer.target_source_authority(request, serial)?;
        let selecting = pending.selecting();
        let bound = pending.bound.borrow().clone();
        match bound {
            None => {
                if let Some((head, backward_bias, cell)) = pending.selection_query() {
                    producer.begin_word_group(
                        MarkdownWordGroupAction::Select { backward_bias },
                        head,
                        cell,
                    )?;
                } else if pending.moving().is_none()
                    && pending.visual_motion().is_none()
                    && request.input.primary_range.length == 0
                {
                    let forward = pending.direction().ok_or(ViewError::Revoked)?;
                    if pending.deleting_grapheme() {
                        producer
                            .begin_grapheme_delete(forward, request.input.primary_range.location)?;
                    } else {
                        producer
                            .begin_word_delete(forward, request.input.primary_range.location)?;
                    } // The two deletion kinds change stopping conditions only, retaining the same read budget, Source identity, and retirement path.
                } // Deletion after selection consumes that complete actual range without expanding from its start; the same bound Source remains health-checked.
                *pending.bound.borrow_mut() = Some((serial, source));
            }
            Some((original_serial, original_source)) => {
                if original_serial != serial || original_source != source {
                    return Err(ViewError::Revoked.into()); // A successor Producer cannot recompute this original deletion.
                }
            }
        }
        if let Some((right, extend)) = pending
            .visual_motion()
            .or_else(|| pending.visual_word_motion())
        {
            self.bind_visual_seed(&pending, request, serial, right, extend)?;
            return producer.advance(self, request); // Original font/line work and bounded visual queries share turns; do not scroll before commit.
        }
        if let Some((motion, extend)) = pending.moving() {
            let forward = matches!(motion, WordMotion::Forward | WordMotion::Right);
            if !pending.move_prepared.get() {
                if let Some(forward) = motion.logical_forward() {
                    pending.move_prepared.set(true);
                    if !extend && request.input.primary_range.length != 0 {
                        let selection = request
                            .input
                            .directed_selection()
                            .ok_or(ViewError::Revoked)?;
                        let head = if forward {
                            selection.anchor_utf16.max(selection.head_utf16)
                        } else {
                            selection.anchor_utf16.min(selection.head_utf16)
                        };
                        *pending.ready.borrow_mut() = Some(head..head);
                        return Ok(());
                    } // Logical collapse keeps ordered from/to, requiring no visual-level proof or directed-head substitution.
                    producer.begin_word_group(
                        if forward {
                            MarkdownWordGroupAction::Forward
                        } else {
                            MarkdownWordGroupAction::Backward
                        },
                        request.input.selection.head_utf16,
                        None,
                    )?;
                    return self.schedule();
                } // Actual logical Forward/Backward boundaries require no whole-Source level-zero proof, preserving logical meaning in RTL.
                let proved = producer.prepare_word_move(self, request)?;
                if !pending.current(self) {
                    return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
                }
                match proved {
                    None => return self.schedule(), // This turn spends only the original Bidi quantum, without starting or advancing GCB work.
                    Some(MarkdownWordMoveScope::Unsupported) => {
                        pending.visual_word.set(true);
                        return self.schedule(); // Nonzero UBA still has actual visual order; query original Parts/GCB next turn rather than substituting logical groups.
                    }
                    Some(
                        MarkdownWordMoveScope::WholeUniformZero
                        | MarkdownWordMoveScope::VisibleParagraphScopes,
                    ) => {
                        pending.move_prepared.set(true);
                    }
                }
                producer.begin_visual_word_group(forward, request.input.selection.head_utf16)?;
                return self.schedule(); // Original Bidi may just have completed; GCB work advances in the next original quantum.
            }
            let answer = producer.step_word_group(self, request)?;
            if !pending.current(self) {
                return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
            }
            return match answer {
                MarkdownWordGroupAnswer::Yielded => self.schedule(),
                MarkdownWordGroupAnswer::UnsupportedVisualOrder => {
                    producer.cancel_word_group();
                    producer.target_source_health()?;
                    pending.visual_word.set(true);
                    self.schedule() // Original two-slot layout context is restored; switch this consumer only, without Source reconstruction or lost successors.
                }
                MarkdownWordGroupAnswer::NeedsFoldExpansion(hidden) => {
                    *pending.fold_expansion.borrow_mut() = Some(hidden);
                    Ok(()) // Actual step work is accounted in Producer; retire/expand after slot restoration, without reentering on-stack Composer.
                }
                MarkdownWordGroupAnswer::Ready(range) => {
                    *pending.ready.borrow_mut() = Some(range);
                    Ok(())
                }
            };
        }
        if !selecting && request.input.primary_range.length != 0 {
            let range = request.input.primary_range;
            let end = range
                .location
                .checked_add(range.length)
                .filter(|end| *end <= request.input.utf16_len)
                .ok_or(ViewError::InvalidRange)?;
            *pending.ready.borrow_mut() = Some(range.location..end);
            return Ok(()); // The preceding Commit authorized this actual single selection; no scan or additional Core deletion cursor is needed.
        }
        if selecting {
            let answer = producer.step_word_group(self, request)?;
            if !pending.current(self) {
                return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
            }
            return match answer {
                MarkdownWordGroupAnswer::Yielded => self.schedule(),
                MarkdownWordGroupAnswer::UnsupportedVisualOrder => {
                    Err(ViewError::UnsupportedCommand.into())
                } // Logical Select never requests visual certification; an incorrect consumer cannot become a selection result.
                MarkdownWordGroupAnswer::NeedsFoldExpansion(hidden) => {
                    *pending.fold_expansion.borrow_mut() = Some(hidden);
                    Ok(()) // Actual step work is accounted in Producer; retire/expand after slot restoration, without reentering on-stack Composer.
                }
                MarkdownWordGroupAnswer::Ready(range) => {
                    if let Some((head, backward_bias, cell)) =
                        pending.selection_target_query(range.clone())
                    {
                        producer.cancel_word_group();
                        producer.begin_word_group(
                            MarkdownWordGroupAction::Select { backward_bias },
                            head,
                            cell,
                        )?;
                        self.schedule() // Next turn queries only the latest target; the Ready initial group is not expanded again.
                    } else {
                        *pending.ready.borrow_mut() = Some(range);
                        Ok(()) // Both actual groups are available; commit merges by original anchor without changing document text/history.
                    }
                }
            };
        }
        let answer = producer.step_word_delete(self, request)?;
        // PreparationTicket checked this Native request/Source; verify pending-slot ownership here without repeating every getter.
        if !pending.current(self) {
            return Err(NativeMarkdownPagedBodyError::PreparationRevoked);
        }
        match answer {
            MarkdownWordDeleteAnswer::NeedsFoldExpansion(hidden) => {
                *pending.fold_expansion.borrow_mut() = Some(hidden);
                Ok(()) // Opaque visible endpoints are not deletion ranges; the original head waits for actual expansion before recomputing.
            }
            MarkdownWordDeleteAnswer::Yielded => self.schedule(), // Continue this original continuation, without a timer or separate worker.
            MarkdownWordDeleteAnswer::Ready(range) => {
                *pending.ready.borrow_mut() = Some(range); // The exact range survives only to this same permit's sole final commit.
                Ok(())
            }
        }
    }

    /// After Ready, return old reads before consuming the original permit; neither partial selection nor partial deletion counts as success.
    pub(super) fn finish_word_delete(&self) -> Result<()> {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        let folded = pending.fold_expansion.borrow_mut().take();
        if let Some(hidden) = folded {
            let serial = pending
                .bound
                .borrow()
                .as_ref()
                .map(|bound| bound.0)
                .ok_or(ViewError::Revoked)?;
            let released = serial
                .checked_add(1)
                .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?;
            let result = self.commit_fold_expansion(&pending, hidden, serial, released);
            return self.word_input_result(&pending, serial, released, result);
        }
        if let Some(ready) = pending.visual_ready.take() {
            let serial = pending
                .bound
                .borrow()
                .as_ref()
                .map(|bound| bound.0)
                .ok_or(ViewError::Revoked)?;
            let result = self.commit_visual_selection(&pending, ready, serial);
            return self.word_input_result(&pending, serial, serial, result);
        }
        let ready = pending.ready.borrow_mut().take();
        let Some(range) = ready else {
            return Ok(());
        }; // Original schedule already registered the normal Yielded continuation.
        let serial = pending
            .bound
            .borrow()
            .as_ref()
            .map(|bound| bound.0)
            .ok_or(ViewError::Revoked)?;
        if !pending.current_native_at(self, serial) {
            self.cancel_original_word_delete(&pending)?;
            return self.schedule(); // Original Input lost authority; prepare the actual successor without committing this old ticket.
        }
        if pending.selecting() || pending.moving().is_some() {
            let result = self.commit_word_selection(&pending, range, serial);
            return self.word_input_result(&pending, serial, serial, result);
        }
        let forward = pending.direction().ok_or(ViewError::Revoked)?;
        let before_range = pending.request().input.primary_range;
        let head = before_range.location;
        if (before_range.length == 0
            && ((forward && range.start != head) || (!forward && range.end != head)))
            || (before_range.length != 0
                && (range.start != head
                    || Some(range.end) != head.checked_add(before_range.length)))
        {
            return Err(ViewError::InvalidRange.into()); // The irreversible edit must retain its direction's original caret endpoint.
        }
        if range.is_empty() {
            pending.consume_word()?;
            if let Some(producer) = self.producer.borrow_mut().as_mut() {
                producer.cancel_word_delete(); // An actual endpoint ends this Core query only, preserving later reverse/ordinary input.
            }
            let result = self.arm_word_successor(&pending, None);
            return self.word_input_result(&pending, serial, serial, result); // Unchanged creates neither a Commit nor history.
        }
        let released_serial = serial
            .checked_add(1)
            .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?;
        let result = self.commit_word_input(
            &pending,
            WordEdit::Delete(NSRange::new(range.start, range.len())),
            serial,
            released_serial,
        );
        self.word_input_result(&pending, serial, released_serial, result)
    }

    /// Publish the actual side and selection together; side-only motion consumes intent without a text Undo record.
    fn commit_visual_selection(
        &self,
        pending: &Rc<PendingWordDelete>,
        ready: VisualSelection,
        serial: u64,
    ) -> Result<()> {
        let view = self.view()?;
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let permission = self.input.check_permit(&permit, &current);
        if let Some(producer) = self.producer.borrow_mut().as_mut() {
            producer.target_source_health()?;
            producer.cancel_visual();
        } // Preserve actual Source failure priority after reentrant Native permission checks.
        permission.map_err(ViewError::from)?;
        let outcome = if let Some(side) = ready.side {
            self.input.set_visual_selection_intent(
                &permit,
                ready.selection,
                ready.upstream.unwrap_or(false),
                side,
                &current,
            )
        } else {
            self.input.set_selection_intent(
                &permit,
                ready.selection,
                ready.upstream,
                None,
                &current,
            )
        }
        .map_err(ViewError::from)?;
        if pending.moving().is_some() {
            pending.consume_move()?;
        } else {
            pending.consume_visual()?;
        } // Nonempty visual Word collapse reuses original line/base querying and consumes this Word action only, preserving later directions.
        match outcome {
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => {
                let commit = Rc::new(commit);
                *pending.receipt.borrow_mut() = Some(Rc::clone(&commit));
                view.finish_navigation_commit(&commit)?;
                self.arm_word_successor(pending, Some(&commit))
            }
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => {
                *pending.permit.borrow_mut() = Some(permit);
                self.arm_word_successor(pending, None)
            }
        }
    }

    /// Endpoint selection consumes the head permit and preceding actual state, never old arrival-time length/anchor authority.
    fn commit_document_motion(
        &self,
        pending: &Rc<PendingWordDelete>,
        end: bool,
        extend: bool,
        serial: u64,
    ) -> Result<()> {
        let view = self.view()?;
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        } // Selection leaves original Source alive, but cannot ignore existing actual I/O failure.
        let state = pending.request().input;
        let head = if end { state.utf16_len } else { 0 };
        let anchor = if extend {
            state
                .directed_selection()
                .ok_or(ViewError::Revoked)?
                .anchor_utf16
        } else {
            head
        }; // Extension keeps the preceding actual anchor; collapse uses actual BOF/EOF without guessing from an old primary range.
        let outcome = self
            .input
            .set_selection_intent(
                &permit,
                MarkdownTextSelection {
                    anchor_utf16: anchor,
                    head_utf16: head,
                },
                Some(end),
                None,
                &current,
            )
            .map_err(ViewError::from)?;
        pending.consume_document()?;
        match outcome {
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => {
                let commit = Rc::new(commit);
                *pending.receipt.borrow_mut() = Some(Rc::clone(&commit));
                view.finish_navigation_commit(&commit)?;
                self.arm_word_successor(pending, Some(&commit))
            }
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => {
                *pending.permit.borrow_mut() = Some(permit);
                self.arm_word_successor(pending, None)
            } // Actual Unchanged adds no text Undo; successors inherit the originally certified selection.
        }
    }

    /// Hand back the first group only after original selection succeeds; capture actual Input/Native after-geometry without renewing old serial.
    /// When parent owns Down and child completes the Source query, handoff follows that sole parent owner, never writing child-local groups into its domain.
    fn publish_word_pointer_group(
        &self,
        view: &NativeMarkdownPagedTextView,
        before: &NativeMarkdownPagedSceneRequest,
        pointer: Option<(WordAnchor, Weak<()>)>,
        commit: Option<&NativeMarkdownPagedInputCommit>,
    ) -> Result<()> {
        let Some((anchor, intent)) = pointer else {
            return Ok(());
        };
        let effect = |cause| ViewError::CommittedEffect(Box::new(cause));
        let after = view.capture_scene_request().map_err(effect)?;
        if let WordAnchor::Group {
            from_utf16,
            to_utf16,
            ..
        } = anchor
        {
            view.accept_pointer_word_group(&after, from_utf16..to_utf16, &intent)
                .map_err(effect)?;
        }
        // The same actual result also returns the admitted Up endpoint; absent View Down does not mean that endpoint was cancelled.
        self.finish_pointer_word_group(before, &after, anchor, &intent, commit)?;
        Ok(())
    }

    /// Word selection updates actual selection in the same Input; keep original layout Source without treating the query as text editing or another pipeline.
    fn commit_word_selection(
        &self,
        pending: &Rc<PendingWordDelete>,
        range: Range<usize>,
        serial: u64,
    ) -> Result<()> {
        let selection = pending.selection_target(range)?;
        let pointer_before = pending.request();
        let pointer_group = pending.pointer_group(); // Extract the actual initial group before consuming its queue head, never reconstructing it from the final union.
        let view = self.view()?; // Acquire original View before selection publication; later MissingView cannot masquerade as an uncommitted ordinary failure.
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        {
            let mut producer = self.producer.borrow_mut();
            let producer = producer.as_mut().ok_or(ViewError::Revoked)?;
            producer.target_source_health()?;
            producer.cancel_word_group();
            producer.target_source_health()?; // Recheck Source after actually returning query nodes, preserving failures exposed during release.
        }
        let affinity = pending.moving().and_then(|(motion, extend)| {
            let forward = matches!(motion, WordMotion::Forward | WordMotion::Right);
            (extend || pending.request().input.primary_range.length == 0).then_some(forward)
        }); // Logical and certified level-zero visual direction preserve original assoc; collapse alone invents no paragraph caret side.
        let outcome = self
            .input
            .set_selection_intent(&permit, selection, affinity, None, &current)
            .map_err(ViewError::from)?;
        if pending.moving().is_some() {
            pending.consume_move()?;
        } else {
            pending.consume_selection()?;
        }
        match outcome {
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => {
                let commit = Rc::new(commit);
                *pending.receipt.borrow_mut() = Some(Rc::clone(&commit));
                // Return the same-content initial group before reentrant selection/scroll notifications, preventing new Drag from retaining an obsolete seed.
                self.publish_word_pointer_group(
                    &view,
                    &pointer_before,
                    pointer_group,
                    Some(commit.as_ref()),
                )?;
                view.finish_navigation_commit(commit.as_ref())?;
                self.arm_word_successor(pending, Some(commit.as_ref()))
            }
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => {
                *pending.permit.borrow_mut() = Some(permit);
                self.publish_word_pointer_group(&view, &pointer_before, pointer_group, None)?;
                self.arm_word_successor(pending, None) // Equal actual selection creates no synthetic Commit or Undo.
            }
        }
    }

    /// Cancel the revoked original chain only; late callbacks cannot remove a successor queue on this Body.
    fn cancel_original_word_delete(&self, pending: &Rc<PendingWordDelete>) -> Result<()> {
        let original = self
            .pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, pending));
        if original {
            self.cancel_word_delete()?;
        }
        Ok(())
    }

    /// Only actual original foreground revocation yields to a successor; I/O, resource, and committed-effect failures remain errors.
    fn word_input_result(
        &self,
        pending: &Rc<PendingWordDelete>,
        serial: u64,
        released_serial: u64,
        result: Result<()>,
    ) -> Result<()> {
        match result {
            Err(NativeMarkdownPagedBodyError::View(
                cause @ (ViewError::Revoked
                | ViewError::Input(
                    super::super::paged_input::NativeMarkdownPagedInputError::Revoked
                    | super::super::paged_input::NativeMarkdownPagedInputError::StateChanged,
                )),
            )) => {
                // Use only predeclared before/after-retirement serials to classify revocation; this branch neither commits nor renews a permit.
                let expected = if self.serial.get() == serial {
                    serial
                } else {
                    released_serial
                };
                if pending.current_native_at(self, expected) {
                    return Err(cause.into()); // With the original ticket still valid, actual rejection is not successor input.
                }
                self.cancel_original_word_delete(pending)?; // Preserve original Source failure without removing a successor queue.
                self.schedule()
            }
            other => other, // Worker, I/O, preparation, and CommittedEffect failures retain their original outcome without exemptions.
        }
    }

    /// Before/after cleanup consumes the same ticket; both Source and Cell return actual reads through the shared retirement boundary.
    fn commit_word_input(
        &self,
        pending: &Rc<PendingWordDelete>,
        edit: WordEdit<'_>,
        serial: u64,
        released_serial: u64,
    ) -> Result<()> {
        let (text, replacement, deleting, source_command) = match edit {
            WordEdit::Typing(text) => (
                text,
                NSRange::new(objc2_foundation::NSNotFound as usize, 0),
                false,
                None,
            ), // Converted text already left its reservation; a later Source command cannot become this transaction.
            WordEdit::Delete(range) => ("", range, true, None), // Deletion comes from this Source's actual GCB or complete selection.
            WordEdit::Source(command) => (
                "",
                NSRange::new(objc2_foundation::NSNotFound as usize, 0),
                false,
                Some(command),
            ), // Core reads this actual selection at execution, without prebuilt continuation/indentation text.
        };
        let permit = pending
            .permit
            .borrow_mut()
            .take()
            .ok_or(ViewError::Revoked)?;
        let view = self.view()?;
        self.retire_word_reading(pending, &permit, serial, released_serial, deleting)?;
        let current =
            |epoch| self.current(epoch) && pending.current_native_at(self, released_serial);
        // A final ordinary edit still reveals its actual caret; capture that semantic decision before consuming the current action.
        let reveal_body_edit = !deleting || pending.deleting_grapheme();
        let outcome = if let Some(command) = source_command {
            self.input
                .apply_source_editing_command(&permit, command, None, &current)
                .map_err(ViewError::from)?
                .expect("已接纳的Return和缩进命令由Core明确消费，不需要几何后备")
        } else if deleting
            && pending.deleting_grapheme()
            && pending.direction() == Some(false)
            && !self.input.is_cell()
        {
            // Backspace first follows original list/indentation markup rules; actual GCB range applies only when Core finds no markup edit.
            self.input
                .apply_source_editing_command(
                    &permit,
                    omni_markdown_core::MarkdownEditingCommand::DeleteMarkupBackward,
                    Some(replacement),
                    &current,
                )
                .map_err(ViewError::from)?
                .expect("已提供同源完整GCB范围，普通退格必有明确结果")
        } else {
            // Word deletion, forward deletion, and insertion consume this step's actual range without another permit or recaptured selection.
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(
                self.input
                    .insert_text(&permit, text, replacement, &current)
                    .map_err(ViewError::from)?,
            )
        };
        if source_command.is_some() {
            pending.consume_source()?; // Decrement after actual commit/Unchanged only, preserving per-step Undo and directed selection.
        } else if deleting {
            pending.consume_word()?; // Consume this step after the actual Core outcome; successors inherit that receipt or original selection.
        }
        let commit = match outcome {
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => commit,
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => {
                // No text history is needed; keep original authority for admitted successors and the preceding receipt's actual caret reveal.
                *pending.permit.borrow_mut() = Some(permit);
                return self.arm_word_successor(pending, None);
            }
        };
        let commit = Rc::new(commit);
        // Cell Source authority changes with this actual content version; only the original Input receipt may acquire its successor domain,
        // never continuing old Cell authority or reconstructing permission from equal Doc/text.
        let target = (|| {
            commit.check_current(&|epoch| self.current(epoch))?;
            let target = self.input.target_domain()?;
            commit.check_current(&|epoch| self.current(epoch))?;
            Ok::<_, super::super::paged_input::NativeMarkdownPagedInputError>(target)
        })()
        .map_err(|cause| ViewError::CommittedEffect(Box::new(ViewError::Input(cause))))?;
        pending.request.borrow_mut().target = target;
        *pending.receipt.borrow_mut() = Some(Rc::clone(&commit));
        let before_gutter = pending.request();
        let synchronized = self
            .synchronize_committed_gutter(commit.as_ref(), &before_gutter)
            .map_err(|cause| ViewError::CommittedEffect(Box::new(cause)))?;
        if !pending.current_at(self, released_serial) {
            return Err(ViewError::CommittedEffect(Box::new(ViewError::Revoked)).into());
        }
        *pending.request.borrow_mut() = synchronized;
        // Only this actual commit's gutter-width change may update the handoff request; later actions inherit no external resize.
        view.finish_word_delete_commit(commit.as_ref())?;
        if !self.input.is_cell()
            && ((reveal_body_edit && pending.steps.borrow().is_empty())
                || pending.source_command().is_some())
        {
            // For the final ordinary edit or a later possibly-Unchanged indentation command, retain this actual receipt's weak reveal intent.
            // If notification reentry admits more actions, original arm revokes this reveal rather than renewing old input under successor state.
            self.accept_selection_reveal(commit.as_ref())
                .map_err(|cause| ViewError::CommittedEffect(Box::new(cause)))?;
        }
        self.arm_word_successor(pending, Some(commit.as_ref()))
    }

    /// The next step inherits actual Commit only; endpoint Unchanged keeps certified before-state, without current-user-state renewal.
    fn arm_word_successor(
        &self,
        pending: &Rc<PendingWordDelete>,
        commit: Option<&NativeMarkdownPagedInputCommit>,
    ) -> Result<()> {
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        if let Some(commit) = commit {
            commit.check_current(&current).map_err(ViewError::from)?;
        } else {
            let permit = pending
                .permit
                .borrow_mut()
                .take()
                .ok_or(ViewError::Revoked)?;
            self.input
                .check_permit(&permit, &current)
                .map_err(ViewError::from)?;
        }
        if pending.steps.borrow().is_empty() && !pending.interpreting_native.get() {
            let fold_failure = pending.fold_failure.borrow_mut().take();
            let attachment_failure = pending.attachment_failure.borrow_mut().take();
            let removed = self.pending_word_delete.borrow_mut().take();
            drop(removed); // current verified this same Rc and no Native occurs before removal, so no successor queue is taken.
            if let Some(cause) = attachment_failure {
                self.view()?.reject_attachment_import(cause); // Actual successors finished; feedback may change viewport after old-chain authority ended.
            }
            if let Some(cause) = fold_failure {
                self.view()?.reject_source_fold(cause);
            }
            return self.schedule();
        }
        let permit = self
            .input
            .capture(self.configuration.epoch, &current)
            .map_err(ViewError::from)?;
        let expected = pending.request();
        let request = self.view()?.capture_scene_request()?;
        if let Some(commit) = commit {
            commit.check_current(&current).map_err(ViewError::from)?;
        }
        self.input
            .check_permit(&permit, &current)
            .map_err(ViewError::from)?;
        if !request.same(&expected) {
            return Err(ViewError::Revoked.into()); // Geometry/unrelated input changes revoke remaining work, without reauthorization from a successor request.
        }
        // No Native/reentrant call occurs through installation; old receipt and next permit transfer once only.
        *pending.request.borrow_mut() = request;
        *pending.permit.borrow_mut() = Some(permit);
        pending.bound.borrow_mut().take();
        pending.ready.borrow_mut().take();
        pending.visual_seed.set(None);
        pending.visual_ready.set(None);
        pending.move_prepared.set(false);
        pending.visual_word.set(false);
        pending.retired_serial.set(None);
        pending.receipt.borrow_mut().take();
        if pending.source_command().is_none()
            && pending.document_motion().is_none()
            && !pending.native_front()
            && !pending.interpreting_native.get()
        {
            drop(self.selection_reveal.borrow_mut().take());
        } // Actual Unchanged semantic commands/repeated endpoints retain preceding reveal intent, never restored through synthetic selection Commits.
        self.revoke_source_target(); // Next admitted input owns positioning; do not first force remote caret preparation for a preceding Cell receipt.
        self.schedule()
    }
}
