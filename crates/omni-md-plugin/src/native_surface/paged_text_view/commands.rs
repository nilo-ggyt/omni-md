//! Execute Cocoa's interpreted text actions under their original permits, without global keys or new shortcuts.

use objc2::{ClassType, DefinedClass, runtime::Sel, sel};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSPasteboard, NSPasteboardTypeString};
use omni_markdown_core::{MarkdownEditingCommand, MarkdownTextSelection};

use super::{
    NativeMarkdownPagedEditCommand as Edit, NativeMarkdownPagedHeadAffinity,
    NativeMarkdownPagedInputHistory, NativeMarkdownPagedInputOutcome,
    NativeMarkdownPagedNavigation as Nav, NativeMarkdownPagedNavigationContext,
    NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError,
    NativeMarkdownPagedWordAnchor as WordAnchor, NativeMarkdownPagedWordMotion,
    NativeMarkdownPagedWordSelection as WordSelection, Result, not_found,
};
use crate::payload_use::PayloadUseGuard;

#[derive(Clone, Copy)]
/// The existing gesture slot keeps either a caret or the first word/Shift seed; it never stores source text.
pub(super) enum DragAnchor {
    // A caret drag keeps its original logical anchor; reflow cannot replace it with a new hit.
    Caret {
        anchor_utf16: usize,
        generation: u64,
    },
    // Word selection retains the certified initial group/Shift role instead of expanding
    // each endpoint independently.
    Word {
        anchor: WordAnchor,
        generation: u64,
    },
}

/// Finite physical facts from one down event move with its gesture, without retaining events or native objects.
#[derive(Clone, Copy)]
pub(super) struct PointerMotion {
    pub down: objc2_foundation::NSPoint,
    pub dragged: bool,
    pub clicks: usize,
}

impl DragAnchor {
    /// Use the actual published generation/coordinates and a word seed mapped by the same sanitizer.
    /// Handoff and selection adoption share this pure conversion; neither reads Source nor recaptures Input authority.
    fn from_published_selection(
        state: super::NativeMarkdownPagedInputState,
        word_anchor: Option<WordAnchor>,
    ) -> Result<Self> {
        if !state.selection_is_directed {
            // Multiple unordered ranges do not establish the one anchor/head pair needed by this drag.
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        match word_anchor {
            // The producer already mapped this word seed into the published Input coordinate domain.
            Some(anchor) => {
                let position = match anchor {
                    WordAnchor::Selection(position) => position,
                    WordAnchor::Word { head_utf16, .. } => head_utf16,
                    WordAnchor::Group {
                        from_utf16,
                        to_utf16,
                        origin_utf16,
                    } => {
                        // A mapped word group must remain inside this exact published coordinate domain.
                        if from_utf16 > to_utf16
                            || to_utf16 > state.utf16_len
                            || origin_utf16 < from_utf16
                            || origin_utf16 > to_utf16
                        {
                            return Err(NativeMarkdownPagedTextViewError::InvalidRange);
                        }
                        from_utf16
                    }
                };
                if position > state.utf16_len {
                    // Reusing the old numeric offset after an edit would attach the gesture to unrelated text.
                    return Err(NativeMarkdownPagedTextViewError::InvalidRange);
                }
                Ok(Self::Word {
                    anchor,
                    generation: state.generation,
                })
            }
            // No word proof means a normal caret seed, derived only from the actual published selection.
            None => Ok(Self::Caret {
                anchor_utf16: state.selection.anchor_utf16,
                generation: state.generation,
            }),
        }
    }
}

#[derive(Clone)]
/// After child commit, forward later Cocoa drag/up callbacks weakly; never reopen its sealed draft or replay an event.
pub(super) struct CellPointerForward {
    pub(super) parent: std::rc::Weak<objc2::rc::Retained<NativeMarkdownPagedTextView>>,
    pub(super) intent: std::rc::Weak<()>,
}

/// Mouse-down records a certified hit; dragging remains selection, and only mouse-up may admit one link effect.
pub(super) struct PressedLink {
    key: super::NativeMarkdownPagedSceneKey,
    part: std::ops::Range<usize>,
    index: usize,
}

/// Only a Command-only click activates links; ordinary clicks and selection modifiers keep text-editing semantics.
/// Ignore non-gesture flags such as Caps Lock, but never reinterpret Shift, Option, or Control as link activation.
fn link_activation_modifiers(flags: NSEventModifierFlags) -> bool {
    flags
        & (NSEventModifierFlags::Command
            | NSEventModifierFlags::Shift
            | NSEventModifierFlags::Option
            | NSEventModifierFlags::Control)
        == NSEventModifierFlags::Command
}

/// Decode existing Cocoa motion semantics; the boolean distinguishes selection extension from a new caret.
fn navigation(selector: Sel) -> Option<(Nav, bool)> {
    // Decoding does not calculate a position: the original Source/Bidi geometry must prove direction and side.
    for (normal, extending, action) in [
        (
            sel!(moveBackward:),
            sel!(moveBackwardAndModifySelection:),
            Nav::Backward,
        ),
        (
            sel!(moveForward:),
            sel!(moveForwardAndModifySelection:),
            Nav::Forward,
        ),
        (
            sel!(moveLeft:),
            sel!(moveLeftAndModifySelection:),
            Nav::Left,
        ),
        (
            sel!(moveRight:),
            sel!(moveRightAndModifySelection:),
            Nav::Right,
        ),
        (sel!(moveUp:), sel!(moveUpAndModifySelection:), Nav::Up),
        (
            sel!(moveDown:),
            sel!(moveDownAndModifySelection:),
            Nav::Down,
        ),
        (
            sel!(moveWordBackward:),
            sel!(moveWordBackwardAndModifySelection:),
            Nav::WordBackward,
        ),
        (
            sel!(moveWordForward:),
            sel!(moveWordForwardAndModifySelection:),
            Nav::WordForward,
        ),
        (
            sel!(moveWordLeft:),
            sel!(moveWordLeftAndModifySelection:),
            Nav::WordLeft,
        ),
        (
            sel!(moveWordRight:),
            sel!(moveWordRightAndModifySelection:),
            Nav::WordRight,
        ),
        (
            sel!(moveToBeginningOfLine:),
            sel!(moveToBeginningOfLineAndModifySelection:),
            Nav::LineStart,
        ),
        (
            sel!(moveToEndOfLine:),
            sel!(moveToEndOfLineAndModifySelection:),
            Nav::LineEnd,
        ),
        (
            sel!(moveToLeftEndOfLine:),
            sel!(moveToLeftEndOfLineAndModifySelection:),
            Nav::LineLeft,
        ),
        (
            sel!(moveToRightEndOfLine:),
            sel!(moveToRightEndOfLineAndModifySelection:),
            Nav::LineRight,
        ),
        (
            sel!(moveToBeginningOfParagraph:),
            sel!(moveToBeginningOfParagraphAndModifySelection:),
            Nav::ParagraphStart,
        ),
        (
            sel!(moveToEndOfParagraph:),
            sel!(moveToEndOfParagraphAndModifySelection:),
            Nav::ParagraphEnd,
        ),
        // These Cocoa paragraph actions use the same established direction, without a second navigation algorithm.
        (
            sel!(moveParagraphBackward:),
            sel!(moveParagraphBackwardAndModifySelection:),
            Nav::ParagraphStart,
        ),
        (
            sel!(moveParagraphForward:),
            sel!(moveParagraphForwardAndModifySelection:),
            Nav::ParagraphEnd,
        ),
        (
            sel!(moveToBeginningOfDocument:),
            sel!(moveToBeginningOfDocumentAndModifySelection:),
            Nav::DocumentStart,
        ),
        (
            sel!(moveToEndOfDocument:),
            sel!(moveToEndOfDocumentAndModifySelection:),
            Nav::DocumentEnd,
        ),
        (sel!(pageUp:), sel!(pageUpAndModifySelection:), Nav::PageUp),
        (
            sel!(pageDown:),
            sel!(pageDownAndModifySelection:),
            Nav::PageDown,
        ),
    ] {
        if selector == normal {
            // An ordinary motion collapses or moves selection under the action's existing rules.
            return Some((action, false));
        }
        if selector == extending {
            // Extension keeps the original anchor; it is not a second independent caret movement.
            return Some((action, true));
        }
    }
    // Whole-word/line/paragraph selection uses its own Scene operation, not an extending motion alias.
    for (selector_value, action) in [
        (sel!(selectWord:), Nav::SelectWord),
        (sel!(selectLine:), Nav::SelectLine),
        (sel!(selectParagraph:), Nav::SelectParagraph),
    ] {
        if selector == selector_value {
            return Some((action, false));
        }
    }
    None
}

/// Translate only supported text-edit selectors; native dispatch never gains authority from an arbitrary Sel.
fn edit(selector: Sel) -> Option<Edit> {
    for (selector_value, action) in [
        (sel!(deleteBackward:), Edit::DeleteBackward),
        (sel!(deleteForward:), Edit::DeleteForward),
        (sel!(deleteWordBackward:), Edit::DeleteWordBackward),
        (sel!(deleteWordForward:), Edit::DeleteWordForward),
        (sel!(deleteToBeginningOfLine:), Edit::DeleteToLineStart),
        (sel!(deleteToEndOfLine:), Edit::DeleteToLineEnd),
        (sel!(insertNewline:), Edit::InsertNewline),
        (sel!(insertNewlineIgnoringFieldEditor:), Edit::InsertNewline),
        (sel!(insertTab:), Edit::InsertTab),
        (sel!(insertTabIgnoringFieldEditor:), Edit::InsertTab),
        (sel!(insertBacktab:), Edit::InsertBacktab),
    ] {
        if selector == selector_value {
            return Some(action);
        }
    }
    None
}

/// Admit only the two existing page-scroll actions; never reinterpret an arbitrary Sel as a native sender ABI.
fn page_scroll(selector: Sel) -> Option<bool> {
    if selector == sel!(scrollPageDown:) {
        Some(true)
    } else if selector == sel!(scrollPageUp:) {
        Some(false)
    } else {
        None
    }
}

/// Typed and native dispatch share recognition; a recognized action's runtime failure is not an unknown selector.
pub(super) fn recognizes_selector(selector: Sel) -> bool {
    navigation(selector).is_some()
        || edit(selector).is_some()
        || page_scroll(selector).is_some()
        || [
            sel!(undo:),
            sel!(redo:),
            sel!(selectAll:),
            sel!(copy:),
            sel!(cut:),
            sel!(paste:),
            sel!(pasteAsPlainText:),
            sel!(cancelOperation:),
            sel!(delete:),
        ]
        .contains(&selector)
}

impl NativeMarkdownPagedTextView {
    /// Reuse the original menu/controller action without granting the native forwarding ABI to safe Rust callers.
    pub fn command(&self, selector: Sel) -> Result<()> {
        self.command_internal(selector, false)
    }

    /// Only the original input-context command or admitted queue-head action uses this interpreted entry.
    /// Public commands still enter through the pending-input barrier before changing the canonical document.
    pub(super) fn command_from_input_context(&self, selector: Sel) -> Result<()> {
        self.command_internal(selector, true)
    }

    /// Route one action through its original input owner, then publish its exact commit or explicit refusal.
    fn command_internal(&self, selector: Sel, from_input_context: bool) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.cancel_cell_pointer_intent();
        self.cancel_link_intent(); // New keyboard/menu work supersedes an older click before it can open an external target.
        if !recognizes_selector(selector) {
            return Err(NativeMarkdownPagedTextViewError::UnsupportedCommand);
        }
        // Only the outer callback of the key currently being interpreted may use that key's completion chain.
        let mut origin = self.origin()?;
        origin.interpreted = from_input_context
            && self.ivars().interpreting_key.get()
            && self.ivars().input_callback_depth.get() == 1;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        if !from_input_context && origin.delegate.has_native_input_barrier() {
            // Queue behind already admitted input using this permit; never execute against a stale caret.
            let permit = origin.input.capture(origin.epoch, &current)?;
            if !origin.delegate.defer_native_command(selector, &permit)? {
                return Err(NativeMarkdownPagedTextViewError::Revoked); // A rejected original barrier cannot borrow successor input.
            }
            return Ok(());
        }
        if let Some(down) = page_scroll(selector) {
            let request = self.scene_request(&origin)?;
            return origin.delegate.scroll_page(&request, down); // Scrolling admits this intent without editing Input or a goal column.
        }
        if selector == sel!(undo:) || selector == sel!(redo:) {
            // History is a canonical transaction, not native text replacement or replay of a previous key.
            let permit = origin.input.capture(origin.epoch, &current)?;
            let direction = if selector == sel!(undo:) {
                NativeMarkdownPagedInputHistory::Undo
            } else {
                NativeMarkdownPagedInputHistory::Redo
            };
            let outcome = origin.input.history(&permit, direction, &current)?;
            return self.finish_edit_outcome(&origin, outcome);
        }
        if selector == sel!(selectAll:) {
            // Canonical length authorizes the selection; selecting everything does not require laying it all out.
            let permit = origin.input.capture(origin.epoch, &current)?;
            let outcome = origin.input.set_selection_intent(
                &permit,
                MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: origin.state.utf16_len,
                },
                None,
                None,
                &current,
            )?;
            return self.finish_outcome(&origin, outcome); // Publish selection only; no whole-document layout or text copy.
        }
        if selector == sel!(copy:) || selector == sel!(cut:) {
            // The bounded output producer must finish first. Failed or reentrant clipboard work cannot delete text.
            let permit = origin.input.capture(origin.epoch, &current)?;
            let request = self.scene_request(&origin)?;
            origin.delegate.copy_selection(&request)?;
            if !origin.current(self) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if selector == sel!(cut:) {
                // Only a complete clipboard result permits deleting all selected ranges, never just the primary range.
                let outcome = origin.input.delete_selected_ranges(&permit, &current)?;
                return self.finish_edit_outcome(&origin, outcome);
            }
            return Ok(()); // Copy alone creates no Core edit; the actual clipboard producer already completed.
        }
        if selector == sel!(paste:) || selector == sel!(pasteAsPlainText:) {
            // Capture the original selection before any pasteboard getter can synchronously
            // change focus or Input.
            let permit = origin.input.capture(origin.epoch, &current)?;
            let pasteboard = origin.step(self, NSPasteboard::generalPasteboard)?;
            if selector == sel!(paste:) && !origin.input.is_cell() {
                // Body paste keeps file precedence; a cell accepts text and must not accidentally import attachments.
                // File IO waits in the existing input queue instead of blocking this native paste callback.
                let paths = origin.step(self, || {
                    crate::native_surface::workspace_actions::native_markdown_pasteboard_file_paths(&pasteboard)
                })?.map_err(NativeMarkdownPagedTextViewError::AttachmentImport)?;
                if !paths.is_empty() {
                    return self.queue_attachment_paths(paths, None, origin.state);
                }
            }
            // Read one actual pasteboard value at arrival. The Native conversion below still owns
            // its original permit; it cannot replace a later selection after getter reentry.
            let text = origin
                .step(self, || {
                    // SAFETY: the public framework type constant lives for the process; reading it accesses no document.
                    pasteboard.stringForType(unsafe { NSPasteboardTypeString })
                })?
                .ok_or(NativeMarkdownPagedTextViewError::ClipboardUnavailable)?;
            let commit =
                origin
                    .input
                    .insert_native_isolated(&permit, &text, not_found(), &current)?;
            return self.finish_edit_commit(&origin, commit); // Pasted text uses the existing bounded input conversion and transaction limit.
        }
        if selector == sel!(cancelOperation:) && origin.state.marked.is_some() {
            // Clone only the Rust Rc while borrowing the slot; this sends no Objective-C message.
            // End that borrow before composition completion may reenter the original context.
            let context = self
                .ivars()
                .context
                .borrow()
                .clone()
                .ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
            // Preserve Escape's existing composition-acceptance behavior; it neither cancels the document nor exits a cell.
            // This explicit command finishes the original system context, unlike protocol-only local unmarkText.
            return self.finish_composition_in_context(
                &origin.input,
                &origin.delegate,
                &context,
                origin.epoch,
            );
        }
        if origin.input.is_cell() {
            // These existing cell actions own focus/commit handoff; do not insert body tabs or newlines afterward.
            use super::NativeMarkdownPagedCellAction as CellAction;
            let action = if selector == sel!(cancelOperation:) {
                Some(CellAction::Cancel)
            } else {
                match edit(selector) {
                    Some(Edit::InsertTab) => Some(CellAction::Next),
                    Some(Edit::InsertBacktab) => Some(CellAction::Previous),
                    Some(Edit::InsertNewline) => Some(CellAction::NextRow),
                    _ => None,
                }
            };
            if let Some(action) = action {
                let permit = origin.input.capture(origin.epoch, &current)?;
                // A valid cell action may end this Input; do not restore its old origin or replay it as text.
                return origin.delegate.cell_action(action, permit);
            }
        }

        if (selector == sel!(deleteBackward:) || selector == sel!(deleteForward:))
            && origin.state.marked.is_none()
        {
            let permit = origin.input.capture(origin.epoch, &current)?;
            if origin
                .delegate
                .defer_grapheme_delete(selector == sel!(deleteForward:), &permit)?
            {
                return Ok(()); // Pending movement must establish its caret before deletion, preserving admitted input order.
            }
        } // Marked input stays in the synchronous NSTextInputClient protocol, outside the ordinary character queue.

        if selector == sel!(delete:) {
            // Cocoa delete: applies only to explicit selected ranges; an empty caret is
            // not implicit backspace.
            let permit = origin.input.capture(origin.epoch, &current)?;
            let outcome = origin.input.delete_selected_ranges(&permit, &current)?;
            return self.finish_edit_outcome(&origin, outcome);
        }
        if let Some((action @ (Nav::Left | Nav::Right), extend)) = navigation(selector)
            && origin.state.marked.is_none()
        {
            let permit = origin.input.capture(origin.epoch, &current)?;
            let request = self.scene_request(&origin)?;
            if origin
                .delegate
                .move_visual(&request, action == Nav::Right, extend, permit)?
            {
                return Ok(());
            }
        } // Accepted motions retain intent without requiring a Scene; subsequent ordinary input enters this same queue.
        if let Some((
            action @ (Nav::WordBackward | Nav::WordForward | Nav::WordLeft | Nav::WordRight),
            extend,
        )) = navigation(selector)
            && origin.state.marked.is_none()
        {
            let permit = origin.input.capture(origin.epoch, &current)?;
            let request = self.scene_request(&origin)?;
            let motion = match action {
                Nav::WordBackward => NativeMarkdownPagedWordMotion::Backward,
                Nav::WordForward => NativeMarkdownPagedWordMotion::Forward,
                Nav::WordLeft => NativeMarkdownPagedWordMotion::Left,
                Nav::WordRight => NativeMarkdownPagedWordMotion::Right,
                _ => unreachable!("这个分支只解释原四种 Word selector"),
            }; // Preserve all four directions; Core cannot reconstruct visual RTL motion after the transport discards it.
            return origin.delegate.move_word(&request, motion, extend, permit);
        }
        if navigation(selector) == Some((Nav::SelectWord, false)) && origin.state.marked.is_none() {
            let selection = origin
                .state
                .directed_selection()
                .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
            let permit = origin.input.capture(origin.epoch, &current)?;
            let request = self.scene_request(&origin)?;
            return origin.delegate.select_word(
                &request,
                WordSelection {
                    head_utf16: selection.head_utf16,
                    backward_bias: false,
                    anchor: None,
                    cell: None,
                },
                None,
                permit,
            ); // Match the existing right-biased word lookup; Core GCBs prove BOF and hard-line boundaries.
        } // Marked/provisional text remains with the system IME, not an ordinary source-word lookup.
        if matches!(
            edit(selector),
            Some(Edit::DeleteWordBackward | Edit::DeleteWordForward)
        ) && origin.state.marked.is_none()
        {
            let permit = origin.input.capture(origin.epoch, &current)?;
            let ranges = origin.input.selected_ranges(&permit, &current)?;
            if ranges.len() > 1 && ranges.iter().all(|range| !range.is_empty()) {
                // A complete nonempty selection is already explicit: delete it without expanding it to word boundaries.
                let outcome = origin.input.delete_selected_ranges(&permit, &current)?;
                return self.finish_edit_outcome(&origin, outcome);
            }
            if ranges.len() > 1 {
                return Err(super::NativeMarkdownPagedInputError::SelectionGeometryRequired.into());
            } // Mixed carets need one complete word plan; deleting only the primary range would lose user intent.
            let request = self.scene_request(&origin)?;
            return origin.delegate.delete_word(
                &request,
                edit(selector) == Some(Edit::DeleteWordForward),
                permit,
            ); // Body and cell use their existing producer, without requiring a ready line at admission.
        } // Marked input retains its own composition path; this branch cannot read provisional text or finish IME.
        if (selector == sel!(deleteBackward:) || selector == sel!(deleteForward:))
            && (origin.state.selection_range_count > 1 || origin.state.primary_range.length != 0)
        {
            // A single empty caret needs no selected-range read; only an actual selection enters this path.
            let permit = origin.input.capture(origin.epoch, &current)?;
            let ranges = origin.input.selected_ranges(&permit, &current)?;
            if ranges.iter().all(|range| !range.is_empty()) {
                // Explicit selected ranges need no hit/line geometry, so pending layout cannot discard this deletion.
                let outcome = origin.input.delete_selected_ranges(&permit, &current)?;
                return self.finish_edit_outcome(&origin, outcome);
            }
            if ranges.len() > 1 {
                // Mixed carets require every GCB and one complete plan; one replacement cannot stand in for all targets.
                return Err(super::NativeMarkdownPagedInputError::SelectionGeometryRequired.into());
            }
        }

        if !origin.input.is_cell() && origin.state.marked.is_none() {
            let source_command = match edit(selector) {
                Some(Edit::InsertNewline) => Some(MarkdownEditingCommand::InsertNewline),
                Some(Edit::InsertTab) => Some(MarkdownEditingCommand::IndentMore),
                Some(Edit::InsertBacktab) => Some(MarkdownEditingCommand::IndentLess),
                Some(Edit::DeleteBackward) => Some(MarkdownEditingCommand::DeleteMarkupBackward),
                _ => None, // Other position-dependent commands continue through the original Scene below.
            };
            if let Some(command) = source_command {
                if command != MarkdownEditingCommand::DeleteMarkupBackward {
                    let permit = origin.input.capture(origin.epoch, &current)?;
                    if origin.delegate.defer_source_command(command, &permit)? {
                        return Ok(()); // Interpret after the earlier selection commits, not against the superseded caret.
                    }
                } // Backspace already uses the GCB queue above; cells and marked input never enter this source-edit path.
                // Newline/indentation need no layout. Backspace may borrow a same-key ordinary GCB plan;
                // missing geometry cannot suppress applicable markup deletion, but real Source/authority errors must survive.
                let backward_scene = if command == MarkdownEditingCommand::DeleteMarkupBackward {
                    match self.scene() {
                        Ok(scene) => Some(scene),
                        Err(NativeMarkdownPagedTextViewError::Pending) => None,
                        Err(cause) => return Err(cause),
                    }
                } else {
                    None
                };
                let ordinary = if let Some(scene) = &backward_scene {
                    if scene.origin.state != origin.state || !scene.current(self) {
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    }
                    let plan = scene.scene.edit_plan(Edit::DeleteBackward);
                    if !scene.current(self) {
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    }
                    match plan {
                        Ok(plan) => Ok(plan.replacement),
                        Err(cause @ NativeMarkdownPagedTextViewError::Pending)
                        | Err(cause @ NativeMarkdownPagedTextViewError::InvalidRange) => Err(cause),
                        Err(cause) => return Err(cause),
                    }
                } else {
                    Err(NativeMarkdownPagedTextViewError::Pending)
                };
                let current = |epoch| {
                    epoch == origin.epoch
                        && origin.current(self)
                        && backward_scene
                            .as_ref()
                            .is_none_or(|scene| scene.current(self))
                };
                let permit = origin.input.capture(origin.epoch, &current)?;
                let outcome = origin.input.apply_source_editing_command(
                    &permit,
                    command,
                    ordinary.as_ref().ok().copied(),
                    &current,
                )?;
                return match outcome {
                    Some(outcome) => self.finish_edit_outcome(&origin, outcome),
                    None => match ordinary {
                        Err(cause) => Err(cause), // No markup rule or GCB plan handled it: preserve Pending/range failure.
                        Ok(_) => Err(NativeMarkdownPagedTextViewError::InvalidRange),
                    },
                };
            }
        } // Cell and marked input keep their own actions; this path cannot finish composition or insert a body tab.

        if let Some((action @ (Nav::DocumentStart | Nav::DocumentEnd), extend)) =
            navigation(selector)
        {
            let permit = origin.input.capture(origin.epoch, &current)?;
            if origin.state.marked.is_none()
                && origin.delegate.defer_document_motion(
                    action == Nav::DocumentEnd,
                    extend,
                    &permit,
                )?
            {
                return Ok(()); // Previously admitted input must finish before this command uses its resulting anchor/length.
            }
            // The actual body/cell length already proves its endpoints; no visible Scene is needed.
            // A new caret is explicit even when Find ranges have no primary direction; extension still needs the real anchor.
            let head = if action == Nav::DocumentStart {
                0
            } else {
                origin.state.utf16_len
            };
            let anchor = if extend {
                origin
                    .state
                    .directed_selection()
                    .ok_or(NativeMarkdownPagedTextViewError::Pending)?
                    .anchor_utf16
            } else {
                head
            }; // Extension preserves the actual anchor, never an inferred multi-selection history endpoint.
            let outcome = origin.input.set_selection_intent(
                &permit,
                MarkdownTextSelection {
                    anchor_utf16: anchor,
                    head_utf16: head,
                },
                Some(action == Nav::DocumentEnd),
                None,
                &current,
            )?;
            return match outcome {
                NativeMarkdownPagedInputOutcome::Committed(commit) => {
                    self.finish_owned_commit(&origin, commit, true) // Reveal through this exact commit without cancelling later key work.
                }
                unchanged => self.finish_outcome(&origin, unchanged),
            };
        }

        if let Some((action @ (Nav::Up | Nav::Down | Nav::PageUp | Nav::PageDown), extend)) =
            navigation(selector)
            && !origin.input.is_cell()
        {
            let request = self.scene_request(&origin)?;
            if origin
                .delegate
                .navigate_vertical(&request, action, extend)?
            {
                return Ok(()); // The original owner accepted it; an offscreen pending result is not permission to replay the action.
            }
        }

        // Remaining position actions require this exact ready Scene; scalar approximations cannot replace pending GCB/Bidi proof.
        if navigation(selector).is_some() || edit(selector).is_some() || selector == sel!(delete:) {
            let scene = self.scene()?;
            if scene.origin.state != origin.state || !origin.current(self) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if let Some((action, extend)) = navigation(selector) {
                if origin.state.directed_selection().is_none() {
                    return Err(NativeMarkdownPagedTextViewError::Pending);
                } // Unordered ranges have no unique active direction; history cannot invent this motion's origin.
                let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
                let permit = origin.input.capture(origin.epoch, &current)?;
                let geometry = scene.scene.navigation_geometry()?;
                if !scene.current(self) || !geometry.permits(&scene.key) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                let (head_upstream, goal) =
                    origin.input.navigation_selection_facts(&permit, &current)?;
                let stale_goal = goal.is_some_and(|goal| !geometry.permits_goal(goal));
                let context = NativeMarkdownPagedNavigationContext {
                    geometry: geometry.clone(),
                    // A layout change invalidates old line affinity; Source/style changes must clear the owner's same slot.
                    head_affinity: if stale_goal {
                        None
                    } else {
                        head_upstream.map(NativeMarkdownPagedHeadAffinity::from_upstream)
                    },
                    vertical_goal: goal.filter(|goal| {
                        action.preserves_vertical_goal() && geometry.permits_goal(*goal)
                    }),
                    bidi_side: origin.input.navigation_bidi_side(&permit, &current)?,
                };
                let target = scene.scene.navigate(action, extend, &context)?;
                if !scene.current(self) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                let after_geometry = scene.scene.navigation_geometry()?;
                if !scene.current(self) || !after_geometry.same(&geometry) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                } // Layout changed during calculation; new geometry cannot certify an already computed old target.
                // A vertical goal belongs to this exact layout and needs an explicit head side.
                // Reject contradictory Scene output before any selection transaction is prepared.
                if target.vertical_goal.is_some_and(|goal| {
                    !geometry.permits_goal(goal)
                        || target.head_affinity.is_none()
                        || !action.preserves_vertical_goal()
                }) {
                    return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
                }
                if target.head_affinity.is_none()
                    || (action.preserves_vertical_goal() && target.vertical_goal.is_none())
                {
                    return Err(NativeMarkdownPagedTextViewError::Pending);
                } // Without certified affinity/goal, keep Pending rather than committing an approximate motion.
                if action.preserves_vertical_goal()
                    && context.vertical_goal.is_some()
                    && target.vertical_goal != context.vertical_goal
                {
                    return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
                } // A short line's actual head cannot replace the user's established vertical goal column.
                if extend && target.selection.anchor_utf16 != origin.state.selection.anchor_utf16 {
                    return Err(NativeMarkdownPagedTextViewError::InvalidRange);
                } // Extension moves only the active head, never the user's established anchor.
                let outcome = if let Some(side) = target.bidi_side {
                    // Publish visual side and selection under one permit; equal offsets still receive a new intent serial.
                    origin.input.set_visual_selection_intent(
                        &permit,
                        target.selection,
                        target
                            .head_affinity
                            .ok_or(NativeMarkdownPagedTextViewError::Pending)?
                            .is_upstream(),
                        side,
                        &current,
                    )?
                } else {
                    // Logical/vertical motion and nonempty collapse have no right to inherit an obsolete Bidi side.
                    origin.input.set_selection_intent(
                        &permit,
                        target.selection,
                        target
                            .head_affinity
                            .map(NativeMarkdownPagedHeadAffinity::is_upstream),
                        target.vertical_goal,
                        &current,
                    )?
                };
                if matches!(action, Nav::Left | Nav::Right) {
                    return match outcome {
                        NativeMarkdownPagedInputOutcome::Committed(commit) => {
                            self.finish_owned_commit(&origin, commit, true)
                        }
                        unchanged => self.finish_outcome(&origin, unchanged),
                    };
                }
                return self.finish_outcome(&origin, outcome);
            }
            let permit = origin.input.capture(origin.epoch, &current)?;
            let action = edit(selector).ok_or(NativeMarkdownPagedTextViewError::Pending)?;
            // The Scene owns the full grapheme/line replacement plan. Revalidate after calculation
            // before the canonical transaction can use that range or its inserted text.
            let plan = scene.scene.edit_plan(action)?;
            if !scene.current(self) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let commit =
                origin
                    .input
                    .insert_text(&permit, &plan.inserted, plan.replacement, &current)?;
            return self.finish_edit_commit(&origin, commit);
        }

        Err(NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }

    /// An asynchronous word delete already consumed its permit; notify display using that commit, not a new before-state.
    pub(in crate::native_surface) fn finish_word_delete_commit(
        &self,
        commit: &super::NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        let origin = self
            .origin()
            .map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))?;
        commit
            .check_current(&|epoch| origin.session_current(self, epoch))
            .map_err(|cause| {
                NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause.into()))
            })?;
        self.finish_commit_ref(&origin, commit)?;
        if origin.input.is_cell() {
            origin
                .delegate
                .reveal_committed_selection(commit)
                .map_err(|cause| {
                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
                })?;
        } // The original queue continues validating this same postcommit after native notification returns.
        Ok(())
    }

    /// Forward only the system's original action selector; a safe Rust caller cannot confer an arbitrary Sel's ABI.
    ///
    /// # Safety
    /// `selector` must use Cocoa's one-optional-sender action signature, never a getter or setter with another ABI.
    pub(super) unsafe fn forward_native_action(&self, selector: Sel) -> Result<()> {
        let origin = self.origin()?;
        // SAFETY: this main-thread view is protected by the synchronous payload lease; no document borrow crosses Native.
        let next = origin.step(self, || unsafe { self.nextResponder() })?;
        if let Some(next) = next {
            // SAFETY: the system supplied this public action signature; try that action once on the existing responder chain.
            origin.step(self, || unsafe {
                next.tryToPerform_with(selector, Some(self))
            })?;
        } // Find and return-to-launcher still belong to their controllers, without registering or rewriting shortcuts here.
        Ok(())
    }

    /// Let the composing child consume this actual event before the parent changes focus or reads its Scene.
    /// The system input context remains the sole interpreter; the parent does not enqueue a second copy.
    pub(in crate::native_surface) fn handle_marked_mouse(&self, event: &NSEvent) -> Result<bool> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let origin = self.origin()?;
        if origin.state.marked.is_none() {
            return Ok(false);
        }
        let (_, context) = self.foreground_context()?;
        // Deliver this physical event once to the composing client. Its synchronous callbacks may
        // publish a new marked state, so only session identity is stable across the Native call.
        context.0.handleEvent(event);
        if !origin.session_current(self, origin.epoch) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(true) // IME may synchronously change marked text/selection; this delivered event must not be replayed on the parent.
    }

    /// Convert an interpreted parent cell hit through this exact raw range, without replaying the native event.
    /// False means its original Scene is pending; Source/input/resource failures remain errors, not retried word requests.
    pub(in crate::native_surface) fn apply_cell_pointer(
        &self,
        pointer: &super::NativeMarkdownPagedCellPointer,
        raw_range: std::ops::Range<usize>,
    ) -> Result<bool> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let origin = self.origin()?;
        if !origin.input.is_cell() || origin.state.marked.is_some() {
            return Err(NativeMarkdownPagedTextViewError::Pending);
        } // Composition belongs to its original context; parent clicks cannot bypass candidates or impersonate a cell.
        // Double-click uses the certified raw head/side, not glyphs, so the first word need not wait for a first frame.
        // Triple-click still needs the Scene's complete paragraph proof rather than just its visible suffix.
        let request = self.scene_request(&origin)?;
        // Parent endpoints belong to the canonical raw cell, while child input accepts local coordinates only.
        // The parent handles an out-of-cell Shift; never clamp it to the draft or copy offscreen text into a string.
        let local = |position: usize| {
            position
                .checked_sub(raw_range.start)
                .filter(|position| *position <= raw_range.end.saturating_sub(raw_range.start))
                .filter(|position| *position <= origin.state.utf16_len)
                .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)
        };
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let head = local(pointer.hit.head_utf16)?;
        let anchor = pointer.anchor.map(local).transpose()?;
        // The same Scene/Input generation constrains publication. Current checks may reenter, so hold no document/reader lease.
        // Either the word producer or selection consumes the original permit once; failure cannot recapture this event.
        let current = |epoch| {
            epoch == origin.epoch
                && origin.current(self)
                && self
                    .scene_request(&origin)
                    .is_ok_and(|actual| actual.same(&request))
        };
        if pointer.clicks == 2 {
            // A double click asks the existing source-word producer directly; no first rendered
            // child frame is required to certify the raw hit already supplied by the parent.
            let permit = origin.input.capture(origin.epoch, &current)?;
            // Word dragging keeps the first word seed; Shift keeps the directed anchor. Original UBA treats them distinctly.
            // Convert all endpoints through this raw range; the parent must already own any cross-cell endpoint.
            let anchor = pointer
                .word_anchor
                .map(|anchor| -> Result<WordAnchor> {
                    Ok(match anchor {
                        WordAnchor::Selection(position) => WordAnchor::Selection(local(position)?),
                        WordAnchor::Word {
                            head_utf16,
                            backward_bias,
                            ..
                        } => WordAnchor::Word {
                            head_utf16: local(head_utf16)?,
                            backward_bias,
                            cell: None,
                        },
                        WordAnchor::Group {
                            from_utf16,
                            to_utf16,
                            origin_utf16,
                        } => WordAnchor::Group {
                            from_utf16: local(from_utf16)?,
                            to_utf16: local(to_utf16)?,
                            origin_utf16: local(origin_utf16)?,
                        },
                    })
                })
                .transpose()?;
            origin.delegate.select_word(
                &request,
                WordSelection {
                    head_utf16: head,
                    backward_bias: pointer.hit.backward_bias,
                    anchor,
                    cell: None,
                },
                Some(pointer.intent.clone()),
                permit,
            )?;
            return Ok(true); // CellSource/UBA admitted this word request; waiting for it does not replay the click.
        }
        let scene = match self.scene() {
            Ok(scene) => scene,
            Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(false),
            Err(cause) => return Err(cause),
        };
        if scene.origin.state != origin.state || !scene.key.request.same(&request) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        // Single-click uses the actual hit; triple-click uses a source-certified paragraph, not a soft visual line.
        // A paragraph outside the prepared window remains pending, without treating the displayed suffix as the whole range.
        let target = match pointer.clicks {
            0 | 1 => MarkdownTextSelection {
                anchor_utf16: anchor.unwrap_or(head),
                head_utf16: head,
            },
            3 => match scene
                .scene
                .paragraph_selection(head, pointer.hit.head_affinity)
            {
                Ok(selection) => selection,
                Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(false),
                Err(cause) => return Err(cause),
            },
            _ => return Err(NativeMarkdownPagedTextViewError::UnsupportedCommand),
        };
        // Publish affinity with the selection so later preview reflow preserves its explicit head side.
        // This changes selection only: it creates no document history and makes no disk-save claim.
        let outcome = origin.input.set_selection_intent(
            &permit,
            target,
            Some(if pointer.clicks == 3 {
                true
            } else {
                pointer.hit.head_affinity.is_upstream()
            }),
            None,
            &current,
        )?;
        self.finish_outcome(&origin, outcome)?;
        // The parent still owns mouse-down; late readiness after up completes selection without reopening a child drag.
        Ok(true)
    }

    /// Hand one interpreted cell hit to its parent Body with source, selection permit, and down-lifetime checked together.
    /// Read word/Shift seeds from the existing slot; native mounting cannot overwrite a newer down or an accepted up.
    fn hand_cell_pointer(
        &self,
        scene: &super::SceneUse,
        hit: super::NativeMarkdownPagedCellHit,
        clicks: usize,
        extend: bool,
        dragging: bool,
    ) -> Result<bool> {
        let origin = &scene.origin;
        let pointer = self.ivars().drag_anchor.get();
        let word = clicks == 2;
        // Link handling already had its chance; this path forwards the real hit/seed, not a replayed down or invented point.
        let anchor = if dragging {
            match pointer {
                Some(DragAnchor::Caret {
                    anchor_utf16,
                    generation,
                }) if generation == origin.state.generation => Some(anchor_utf16),
                Some(DragAnchor::Caret { .. }) => {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                Some(DragAnchor::Word {
                    anchor: WordAnchor::Selection(anchor),
                    ..
                }) => Some(anchor),
                Some(DragAnchor::Word { .. }) => None,
                None => return Err(NativeMarkdownPagedTextViewError::Revoked),
            }
        } else if extend {
            Some(
                origin
                    .state
                    .directed_selection()
                    .ok_or(NativeMarkdownPagedTextViewError::Pending)?
                    .anchor_utf16,
            )
        } else {
            None
        };
        // An edit invalidates the old word seed even when its numeric offset is unchanged.
        // Keep Shift and word roles distinct until the child or cross-cell producer converts their coordinate domain.
        let word_anchor = if word && dragging {
            let Some(DragAnchor::Word { anchor, generation }) = pointer else {
                unreachable!("原Word手势已确认");
            };
            if generation != scene.origin.state.generation {
                self.cancel_cell_pointer_intent();
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            Some(anchor)
        } else {
            anchor.filter(|_| word).map(WordAnchor::Selection)
        };
        // Claim only this View's gesture; raw source travels with the call, without becoming retained View state.
        // Prepare the next anchor from the real hit but publish it only after delegate acceptance and original-intent recheck.
        let intent = self.claim_cell_pointer_intent();
        let next_anchor = if word {
            DragAnchor::Word {
                anchor: word_anchor.unwrap_or(WordAnchor::Word {
                    head_utf16: hit.head_utf16,
                    backward_bias: hit.backward_bias,
                    cell: hit.word_target,
                }),
                generation: scene.origin.state.generation,
            }
        } else {
            DragAnchor::Caret {
                anchor_utf16: anchor.unwrap_or(hit.head_utf16),
                generation: origin.state.generation,
            }
        };
        let cell_pointer = super::NativeMarkdownPagedCellPointer {
            hit,
            clicks: if word { 2 } else { clicks },
            extending: extend,
            anchor,
            word_anchor,
            dragging,
            intent: std::rc::Rc::downgrade(&intent),
        };
        if !scene.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // Input and Scene share one before-state. Body admission obtains actual Row/RequiredRow authority, not a View-made token.
        // True means ownership was accepted; only actual Source/CPU publication can establish that an editor is visible.
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        if scene
            .origin
            .delegate
            .cell_pointer(&scene.key.request, cell_pointer, permit)?
        {
            // Native handoff can reenter. Install a seed only if this down still lives and no newer gesture replaced it.
            let owns = self
                .ivars()
                .cell_pointer_intent
                .borrow()
                .as_ref()
                .is_some_and(|active| std::rc::Rc::ptr_eq(active, &intent));
            if !dragging && owns && origin.input.state()?.generation == origin.state.generation {
                self.ivars().drag_anchor.set(Some(next_anchor));
            } // Cell commit may change generation; Reopen must map its first query before any later endpoint can use it.
            return Ok(true);
        }
        if !dragging {
            self.cancel_cell_pointer_intent();
        } // A non-Body client can decline cell ownership; continue normal Source selection rather than report acceptance.
        Ok(false)
    }

    /// Route a real callback to the one live gesture owner before inspecting any coordinates or input state.
    pub(super) fn mouse(&self, event: &NSEvent, dragging: bool) {
        let forward = self.ivars().cell_pointer_forward.borrow().clone();
        if let Some(forward) = forward {
            // Cocoa delivered a later callback to the retired child. Forward it once before reading points/modifiers.
            // A new down cannot inherit that old handoff, and a closed child cannot read its sealed Input.
            if dragging
                && let Some(parent) = forward.parent.upgrade()
                && parent.cell_pointer_is_current(&forward.intent)
            {
                parent.handle_pointer(event, true);
            } else {
                self.ivars().cell_pointer_forward.borrow_mut().take();
            }
            return;
        }
        if self.ivars().closed.get() {
            return; // Up ended the weak route; a late callback to an empty view cannot revive released Input.
        }
        // Up or handoff already ended this gesture even if native detachment is still queued.
        // Stop before reading Origin/a sealed draft; later close cannot retroactively make such access safe.
        if dragging
            && self.ivars().drag_anchor.get().is_none()
            && self.ivars().cell_pointer_intent.borrow().is_none()
        {
            return;
        }
        self.handle_pointer(event, dragging);
    }

    /// Decode one finite endpoint while retaining the original down's logical seed, never a guessed word group.
    /// Body records an admitted up explicitly; a dead Weak cannot distinguish completed release from cancellation.
    fn pointer_location(
        &self,
        origin: &super::Origin,
        event: &NSEvent,
        dragging: bool,
        mouse_up: bool,
    ) -> Result<super::NativeMarkdownPagedPointerLocation> {
        use super::NativeMarkdownPagedCellDragAnchor as Anchor;
        let anchor = match self.ivars().drag_anchor.get() {
            Some(DragAnchor::Word { anchor, generation }) if dragging => {
                if generation != origin.state.generation {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                Some(Anchor::Word(anchor))
            }
            Some(DragAnchor::Caret {
                anchor_utf16,
                generation,
            }) if dragging => {
                if generation != origin.state.generation {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                Some(Anchor::Caret { anchor_utf16 })
            }
            _ => None,
        };
        let window_point = origin.step(self, || event.locationInWindow())?;
        let flags = origin.step(self, || event.modifierFlags())?;
        let clicks = origin.step(self, || event.clickCount())?;
        let clicks = usize::try_from(clicks)
            .map_err(|_| NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        if !window_point.x.is_finite() || !window_point.y.is_finite() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let extending = dragging || flags.contains(NSEventModifierFlags::Shift);
        // A fresh Shift-down uses the current directed anchor. A continuing drag instead keeps
        // the generation-bound seed captured at its own down, even after selection changes.
        let anchor = if !dragging && extending {
            let anchor = origin
                .state
                .directed_selection()
                .ok_or(NativeMarkdownPagedTextViewError::Pending)?
                .anchor_utf16;
            Some(if clicks == 2 {
                Anchor::Word(WordAnchor::Selection(anchor))
            } else {
                Anchor::Caret {
                    anchor_utf16: anchor,
                }
            })
        } else {
            anchor
        };
        // Drag/up may only borrow a live down. Only an actual new down may allocate gesture identity.
        let intent = if dragging || mouse_up {
            self.ivars()
                .cell_pointer_intent
                .borrow()
                .clone()
                .ok_or(NativeMarkdownPagedTextViewError::Revoked)?
        } else {
            self.claim_cell_pointer_intent()
        };
        Ok(super::NativeMarkdownPagedPointerLocation {
            window_point,
            clicks: if matches!(anchor, Some(Anchor::Word(_))) {
                2
            } else {
                clicks
            },
            extending,
            anchor,
            dragging,
            intent: std::rc::Rc::downgrade(&intent),
            mouse_up,
        })
    }

    /// Interpret finite pointer facts against one ready Scene for both immediate and deferred delivery.
    /// Capture the permit beside publication so cell admission and body selection cannot consume one action twice.
    fn apply_pointer(
        &self,
        scene: &super::SceneUse,
        pointer: &super::NativeMarkdownPagedPointerLocation,
    ) -> Result<()> {
        if pointer.mouse_up && !pointer.dragging {
            // A stationary up only ends its admitted down. Selection can expose quote/inline markup and change geometry;
            // re-hitting that screen point would incorrectly select a neighbor. The down's original continuation still finishes.
            return Ok(());
        }
        let local = scene.step(self, || {
            self.convertPoint_fromView(pointer.window_point, None)
        })?;
        if !local.x.is_finite() || !local.y.is_finite() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let local = if pointer.dragging {
            let point = scene.scene.drag_selection_point(local)?;
            if !scene.current(self) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            point
        } else {
            local
        }; // Dragging uses prepared lines, while ordinary clicks keep exact hits and the slot keeps the physical point.
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        use super::NativeMarkdownPagedCellDragAnchor as Anchor;
        if pointer.clicks == 2 || matches!(pointer.anchor, Some(Anchor::Word(_))) {
            return self.apply_word_pointer(scene, pointer, local);
        }
        let anchor = match pointer.anchor {
            Some(Anchor::Caret { anchor_utf16 }) => Some(anchor_utf16),
            None => None,
            Some(Anchor::Word(_)) => unreachable!("真实Word已交上方原选词消费者"),
        };
        if let Some(hit) = scene.scene.cell_hit(local, false)? {
            // Offer the certified raw Cell hit to its real parent before a body selection can
            // consume this endpoint; accepted ownership ends processing on the old Scene.
            let next_anchor = DragAnchor::Caret {
                anchor_utf16: anchor.unwrap_or(hit.head_utf16),
                generation: scene.origin.state.generation,
            };
            let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
            if scene.origin.delegate.cell_pointer(
                &scene.key.request,
                super::NativeMarkdownPagedCellPointer {
                    hit,
                    clicks: pointer.clicks,
                    extending: pointer.extending,
                    anchor,
                    word_anchor: None,
                    dragging: pointer.dragging,
                    intent: pointer.intent.clone(),
                },
                permit,
            )? {
                if !pointer.mouse_up
                    && !pointer.dragging
                    && self.cell_pointer_is_current(&pointer.intent)
                    && scene.origin.input.state()?.generation == scene.origin.state.generation
                {
                    self.ivars().drag_anchor.set(Some(next_anchor));
                }
                return Ok(()); // The same parent now owns mounting/cross-cell commit; never fall through to the old Scene.
            }
        }
        let (target, affinity) =
            scene
                .scene
                .mouse_selection(local, pointer.clicks, anchor, pointer.extending)?;
        if !scene.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        // An active child/cross-Cell gesture gets first refusal with its original permit.
        // A decline leaves this same Scene eligible for the ordinary body selection below.
        if scene.origin.delegate.extend_cell_pointer(
            &scene.key.request,
            super::NativeMarkdownPagedCellExtension::Selection {
                selection: target,
                head_affinity: affinity,
                intent: pointer.intent.clone(),
            },
            permit,
        )? {
            return Ok(());
        }
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        let outcome = scene.origin.input.set_selection_intent(
            &permit,
            target,
            Some(affinity.is_upstream()),
            None,
            &current,
        )?;
        self.finish_outcome(&scene.origin, outcome)?;
        if !pointer.mouse_up && !pointer.dragging && self.cell_pointer_is_current(&pointer.intent) {
            self.ivars().drag_anchor.set(Some(DragAnchor::Caret {
                anchor_utf16: target.anchor_utf16,
                generation: scene.origin.state.generation,
            }));
        } // An admitted up completes its endpoint; asynchronous readiness cannot reinstall an ended drag.
        Ok(())
    }

    /// Immediate and deferred word gestures share actual Scene hits; only the original producer establishes the first group.
    fn apply_word_pointer(
        &self,
        scene: &super::SceneUse,
        pointer: &super::NativeMarkdownPagedPointerLocation,
        local: objc2_foundation::NSPoint,
    ) -> Result<()> {
        use super::NativeMarkdownPagedCellDragAnchor as Anchor;
        let anchor = match pointer.anchor {
            Some(Anchor::Word(anchor)) => Some(anchor),
            None if !pointer.dragging => None,
            _ => return Err(NativeMarkdownPagedTextViewError::Pending),
        };
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        if let Some(hit) = scene.scene.cell_hit(local, true)? {
            // Cell word hits preserve their original source-side bias and first-word seed;
            // successful handoff must not fall through to a second body word query.
            let next = anchor.unwrap_or(WordAnchor::Word {
                head_utf16: hit.head_utf16,
                backward_bias: hit.backward_bias,
                cell: hit.word_target,
            });
            let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
            if scene.origin.delegate.cell_pointer(
                &scene.key.request,
                super::NativeMarkdownPagedCellPointer {
                    hit,
                    clicks: 2,
                    extending: pointer.extending,
                    anchor: match anchor {
                        Some(WordAnchor::Selection(anchor)) => Some(anchor),
                        _ => None,
                    },
                    word_anchor: anchor,
                    dragging: pointer.dragging,
                    intent: pointer.intent.clone(),
                },
                permit,
            )? {
                if !pointer.dragging
                    && !pointer.mouse_up
                    && self.cell_pointer_is_current(&pointer.intent)
                    && scene.origin.input.state()?.generation == scene.origin.state.generation
                {
                    self.ivars().drag_anchor.set(Some(DragAnchor::Word {
                        anchor: next,
                        generation: scene.origin.state.generation,
                    }));
                }
                return Ok(());
            }
        }
        let (head_utf16, backward_bias, cell) = scene.scene.word_hit(local)?;
        let selection = WordSelection {
            head_utf16,
            backward_bias,
            anchor,
            cell,
        };
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        if scene.origin.delegate.extend_cell_pointer(
            &scene.key.request,
            super::NativeMarkdownPagedCellExtension::Word {
                selection,
                intent: pointer.intent.clone(),
            },
            permit,
        )? {
            return Ok(());
        }
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        scene.origin.delegate.select_word(
            &scene.key.request,
            selection,
            Some(pointer.intent.clone()),
            permit,
        )?;
        if !pointer.dragging && !pointer.mouse_up && self.cell_pointer_is_current(&pointer.intent) {
            self.ivars().drag_anchor.set(Some(DragAnchor::Word {
                anchor: anchor.unwrap_or(WordAnchor::Word {
                    head_utf16,
                    backward_bias,
                    cell,
                }),
                generation: scene.origin.state.generation,
            }));
        }
        Ok(())
    }

    /// The existing Body continuation runs after real Scene publication, without retaining/reposting a native event.
    /// False means the original window is pending; changed Input or geometry revokes, never recertifies, an old endpoint.
    pub(in crate::native_surface) fn apply_deferred_pointer(
        &self,
        request: &super::NativeMarkdownPagedSceneRequest,
        pointer: &super::NativeMarkdownPagedPointerLocation,
        accepted_up: Option<&super::NativeMarkdownPagedPointerLocation>,
    ) -> Result<bool> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let original_up = accepted_up.is_some_and(|terminal| {
            terminal.mouse_up && std::rc::Weak::ptr_eq(&terminal.intent, &pointer.intent)
        }); // Only the Body's admitted terminal endpoint can complete a down; Weak death alone is not completion evidence.
        if !pointer.mouse_up && !original_up && !self.cell_pointer_is_current(&pointer.intent) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let scene = match self.scene() {
            Ok(scene) => scene,
            Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(false),
            Err(cause) => return Err(cause),
        };
        if !scene.key.request.same(request) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        self.apply_pointer(&scene, pointer)?;
        Ok(true)
    }

    /// Forward only this decoded out-of-cell endpoint; its child-local seed must not be mistaken for canonical coordinates.
    pub(super) fn hand_child_pointer(
        &self,
        origin: &super::Origin,
        pointer: &super::NativeMarkdownPagedPointerLocation,
    ) -> Result<bool> {
        if !origin.input.is_cell() {
            return Ok(false);
        }
        let request = self.scene_request(origin)?;
        let local = origin.step(self, || {
            self.convertPoint_fromView(pointer.window_point, None)
        })?;
        if !local.x.is_finite() || !local.y.is_finite() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let viewport = request.viewport;
        // Inside the child viewport, keep local editing ownership. Only an actual outside
        // endpoint may ask the existing parent to commit/extend into canonical coordinates.
        if local.x >= viewport.origin.x
            && local.y >= viewport.origin.y
            && local.x < viewport.origin.x + viewport.size.width
            && local.y < viewport.origin.y + viewport.size.height
        {
            return Ok(false);
        }
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        origin
            .delegate
            .drag_cell_pointer_outside(&request, pointer.clone(), permit)
    }

    /// Hand the original permit and decoded endpoint to the existing Body slot; refusal preserves genuine Pending.
    pub(super) fn defer_pointer_location(
        &self,
        origin: &super::Origin,
        pointer: super::NativeMarkdownPagedPointerLocation,
    ) -> Result<()> {
        let request = self.scene_request(origin)?;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if origin.delegate.defer_pointer(&request, pointer, permit)? {
            Ok(())
        } else {
            Err(NativeMarkdownPagedTextViewError::Pending)
        }
    }

    /// Interpret one real down/drag under the original gesture, with marked input taking precedence over text hit-testing.
    fn handle_pointer(&self, event: &NSEvent, dragging: bool) {
        let drag_intent = dragging
            .then(|| {
                self.ivars()
                    .cell_pointer_intent
                    .borrow()
                    .as_ref()
                    .map(std::rc::Rc::downgrade)
            })
            .flatten(); // Capture the down before Native getters can reenter; a newer down cannot authorize this callback.
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let draining_reopen = !dragging && self.cancel_cell_pointer_intent();
        self.cancel_link_intent(); // New down or dragging retires link activation, while the same pointer slot owns caret/word selection.
        let result = (|| {
            let origin = self.origin()?;
            if origin.delegate.handle_cell_marked_mouse(event)? {
                return Ok(()); // The composing cell consumed the event before parent focus, Scene lookup, or draft commit.
            }
            if !origin.current(self) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if origin.state.marked.is_some() {
                // IME owns mouse delivery during composition; a resulting commit cannot recertify this old click on a newer Scene.
                let (_, context) = self.foreground_context()?;
                context.0.handleEvent(event);
                if !origin.session_current(self, origin.epoch) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                return Ok(());
            }
            let point = origin.step(self, || event.locationInWindow())?;
            if !point.x.is_finite() || !point.y.is_finite() {
                return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
            }
            if dragging {
                if let Some(mut motion) = self.ivars().pointer_motion.get() {
                    motion.dragged = true; // Only a real drag callback proves movement; reflow or selection changes do not.
                    self.ivars().pointer_motion.set(Some(motion));
                }
            } else {
                self.ivars().pointer_motion.set(Some(PointerMotion {
                    down: point,
                    dragged: false,
                    clicks: usize::try_from(origin.step(self, || event.clickCount())?)
                        .map_err(|_| NativeMarkdownPagedTextViewError::InvalidGeometry)?,
                }));
            }
            if draining_reopen {
                // The previous source is still returning to its owner. Queue this down's own finite point, not a second request.
                let pointer = self.pointer_location(&origin, event, false, false)?;
                return self.defer_pointer_location(&origin, pointer);
            }
            if dragging {
                let pointer = self.pointer_location(&origin, event, true, false)?;
                if self.hand_child_pointer(&origin, &pointer)? {
                    return Ok(());
                }
                if matches!(
                    pointer.anchor,
                    Some(super::NativeMarkdownPagedCellDragAnchor::Word(_))
                ) || self.ivars().drag_anchor.get().is_none()
                {
                    return self.defer_pointer_location(&origin, pointer);
                } // The one slot keeps the original down and latest word endpoint without replacing its pending first group.
            }
            let scene = match self.scene() {
                Ok(scene) => scene,
                Err(NativeMarkdownPagedTextViewError::Pending) => {
                    let pointer = self.pointer_location(&origin, event, dragging, false)?;
                    return self.defer_pointer_location(&origin, pointer);
                }
                Err(cause) => return Err(cause),
            };
            if !origin.current(self) || scene.origin.state != origin.state {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let window = scene
                .step(self, || self.window())?
                .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
            let focused = scene.step(self, || window.makeFirstResponder(Some(self)))?;
            if !focused {
                // A refused focus transfer cannot authorize selection or link activation in this View.
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let point = scene.step(self, || event.locationInWindow())?;
            let local = scene.step(self, || self.convertPoint_fromView(point, None))?;
            let flags = scene.step(self, || event.modifierFlags())?;
            let clicks = scene.step(self, || event.clickCount())?;
            let clicks = usize::try_from(clicks)
                .map_err(|_| NativeMarkdownPagedTextViewError::InvalidGeometry)?;
            let extend = dragging || flags.contains(NSEventModifierFlags::Shift);
            if !dragging && !extend && clicks == 1 && link_activation_modifiers(flags) {
                let selected = origin.state.primary_range;
                let inside_selection = if selected.length == 0 {
                    false
                } else {
                    let position = scene.scene.hit(local)?;
                    position >= selected.location && position <= selected.location + selected.length
                }; // A click inside selected text keeps its selection/drag semantics instead of being stolen by a link.
                if !inside_selection && let Some((part, index)) = scene.scene.link_hit(local)? {
                    let anchor = scene.scene.hit(local)?;
                    if !scene.current(self) {
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    }
                    // Command-link down records identity without editing canonical selection or opening early.
                    // A later drag can cancel the link and continue from this down; it cannot invent the anchor's authority later.
                    let _intent = self.claim_cell_pointer_intent();
                    *self.ivars().link_intent.borrow_mut() = Some(std::rc::Rc::new(()));
                    *self.ivars().pressed_link.borrow_mut() = Some(PressedLink {
                        key: scene.key.clone(),
                        part,
                        index,
                    });
                    self.ivars().drag_anchor.set(Some(DragAnchor::Caret {
                        anchor_utf16: anchor,
                        generation: origin.state.generation,
                    }));
                    return Ok(()); // Keeping canonical selection unchanged preserves this publication; a drag still takes normal selection.
                }
            }
            let pointer = self.ivars().drag_anchor.get();
            if dragging && pointer.is_none() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            } // Up already cleared the slot; a late drag cannot construct a nonexistent down from current selection.
            let word = (!dragging && clicks == 2)
                || (dragging && matches!(pointer, Some(DragAnchor::Word { .. })));
            let local = if word && dragging {
                let point = scene.scene.drag_selection_point(local)?;
                if !scene.current(self) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                point
            } else {
                local
            };
            if word
                && let Some(hit) = scene.scene.cell_hit(local, true)?
                && self.hand_cell_pointer(
                    &scene,
                    hit,
                    if word { 2 } else { clicks },
                    extend,
                    dragging,
                )?
            {
                return Ok(());
            }
            if (!dragging && clicks == 2)
                || (dragging && matches!(pointer, Some(DragAnchor::Word { .. })))
            {
                let (head_utf16, backward_bias, cell) = scene.scene.word_hit(local)?;
                if !scene.current(self) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                let anchor = if dragging {
                    let Some(DragAnchor::Word { anchor, generation }) = pointer else {
                        unreachable!("本手势已确认原Word指针槽");
                    };
                    if generation != origin.state.generation {
                        self.ivars().drag_anchor.set(None);
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    } // An edit revokes the old click's coordinates instead of mapping them onto a different document state.
                    Some(anchor)
                } else {
                    let extend_anchor = if extend {
                        Some(
                            origin
                                .state
                                .directed_selection()
                                .ok_or(NativeMarkdownPagedTextViewError::Pending)?
                                .anchor_utf16,
                        )
                    } else {
                        None
                    };
                    // A Shift anchor and first-word seed are distinct roles; store only the role this gesture actually owns.
                    self.ivars().drag_anchor.set(Some(DragAnchor::Word {
                        anchor: extend_anchor.map_or(
                            WordAnchor::Word {
                                head_utf16,
                                backward_bias,
                                cell,
                            },
                            WordAnchor::Selection,
                        ),
                        generation: origin.state.generation,
                    }));
                    extend_anchor.map(WordAnchor::Selection)
                };
                let selection = WordSelection {
                    head_utf16,
                    backward_bias,
                    anchor,
                    cell,
                };
                let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
                let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
                let intent = self.claim_cell_pointer_intent();
                if scene.origin.delegate.extend_cell_pointer(
                    &scene.key.request,
                    super::NativeMarkdownPagedCellExtension::Word {
                        selection,
                        intent: std::rc::Rc::downgrade(&intent),
                    },
                    permit,
                )? {
                    return Ok(()); // The original owner admitted cross-cell word selection; this old Scene cannot consume its permit again.
                }
                let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
                return scene.origin.delegate.select_word(
                    &scene.key.request,
                    selection,
                    Some(std::rc::Rc::downgrade(&intent)),
                    permit,
                );
            }
            // Ordinary carets and deferred endpoints share one hit/selection consumer; word gestures use the path above.
            let pointer = self.pointer_location(&origin, event, dragging, false)?;
            self.apply_pointer(&scene, &pointer)?;
            Ok(())
        })();
        self.record(result);
        if let Some(intent) = drag_intent {
            self.update_drag_scroll(event, &intent); // Retain only this down's latest finite window point, never the NSEvent.
        }
    }

    /// One actual down/drag shares this View's single identity slot, without retaining raw permits, Source, or events.
    fn claim_cell_pointer_intent(&self) -> std::rc::Rc<()> {
        let active = self.ivars().cell_pointer_intent.borrow().clone();
        active.unwrap_or_else(|| {
            // Create only when there is no live down; subsequent drags keep the existing gesture identity.
            let intent = std::rc::Rc::new(());
            *self.ivars().cell_pointer_intent.borrow_mut() = Some(intent.clone());
            intent
        })
    }

    /// Require this View to own the down; a temporary stack Rc cannot keep its Weak authorized after real up.
    pub(in crate::native_surface) fn cell_pointer_is_current(
        &self,
        intent: &std::rc::Weak<()>,
    ) -> bool {
        !self.ivars().closed.get()
            && self
                .ivars()
                .cell_pointer_intent
                .borrow()
                .as_ref()
                .is_some_and(|active| {
                    std::rc::Weak::ptr_eq(&std::rc::Rc::downgrade(active), intent)
                })
    }

    /// Move the gesture Rc after actual Core publication but before child detachment; Cocoa may still target that old view.
    /// Never read sealed child Input: validate the parent's actual result and refuse transfer if up already ended ownership.
    pub(in crate::native_surface) fn transfer_cell_pointer_to(
        &self,
        parent: &std::rc::Rc<objc2::rc::Retained<Self>>,
        after: &NativeMarkdownPagedInputOutcome,
        intent: &std::rc::Weak<()>,
        mapped_word_anchor: Option<WordAnchor>,
    ) -> Result<bool> {
        if !self.cell_pointer_is_current(intent) {
            return Ok(false);
        }
        let origin = parent.origin()?;
        let state = match after {
            NativeMarkdownPagedInputOutcome::Committed(commit) => {
                commit.check_current(&|epoch| origin.session_current(parent, epoch))?;
                commit.state
            }
            NativeMarkdownPagedInputOutcome::Unchanged(state) => *state,
        };
        if origin.state != state || !origin.current(parent) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let next = DragAnchor::from_published_selection(state, mapped_word_anchor)?;
        // Do not replace a different down already owned by the parent or recertify this child token for it.
        // No Native/current callback occurs between the check and slot move; one main-thread transfer needs no lock/registry.
        if parent
            .ivars()
            .cell_pointer_intent
            .borrow()
            .as_ref()
            .is_some_and(|active| !std::rc::Weak::ptr_eq(&std::rc::Rc::downgrade(active), intent))
        {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // Move the original Rc rather than cloning gesture lifetime into both views. The
        // parent becomes the sole slot owner before any Native timer/deallocation callback.
        let token = {
            let mut slot = self.ivars().cell_pointer_intent.borrow_mut();
            if slot.as_ref().is_none_or(|active| {
                !std::rc::Weak::ptr_eq(&std::rc::Rc::downgrade(active), intent)
            }) {
                return Ok(false);
            }
            slot.take().expect("同一原Down已核存在")
        };
        *parent.ivars().cell_pointer_intent.borrow_mut() = Some(token);
        parent.ivars().drag_anchor.set(Some(next));
        parent
            .ivars()
            .pointer_motion
            .set(self.ivars().pointer_motion.take());
        self.ivars().drag_anchor.set(None);
        *self.ivars().cell_pointer_forward.borrow_mut() = Some(CellPointerForward {
            parent: std::rc::Rc::downgrade(parent),
            intent: intent.clone(),
        });
        self.transfer_drag_scroll_to(parent); // Process the native deadline only after the complete pure-state handoff.
        Ok(true) // The same mapped seed and gesture now have one owner, before Native can observe child detachment.
    }

    /// Accept the original producer's complete first group only into the same down's live Word slot.
    /// The request is the actual post-selection domain; an old serial or child-local integer cannot authorize the parent.
    /// Word selection may finish after up, but that does not recreate a commit or reopen an ended drag.
    pub(in crate::native_surface) fn accept_pointer_word_group(
        &self,
        request: &super::NativeMarkdownPagedSceneRequest,
        group: std::ops::Range<usize>,
        intent: &std::rc::Weak<()>,
    ) -> Result<bool> {
        if !self.cell_pointer_is_current(intent) {
            return Ok(false);
        }
        let origin = self.origin()?;
        if !self.scene_request(&origin)?.same(request) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        if group.start > group.end || group.end > origin.state.utf16_len {
            return Err(NativeMarkdownPagedTextViewError::InvalidRange);
        }
        let Some(DragAnchor::Word { anchor, generation }) = self.ivars().drag_anchor.get() else {
            return Ok(false); // A word result cannot turn a caret or ended gesture into a new Word drag.
        };
        if generation != origin.state.generation {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let origin_utf16 = match anchor {
            WordAnchor::Selection(_) => return Ok(true), // A real Shift scalar anchor must retain its original role.
            WordAnchor::Word { head_utf16, .. }
                if group.start <= head_utf16 && head_utf16 <= group.end =>
            {
                head_utf16
            }
            WordAnchor::Group {
                from_utf16,
                to_utf16,
                origin_utf16,
            } if group == (from_utf16..to_utf16) => origin_utf16,
            _ => return Err(NativeMarkdownPagedTextViewError::Revoked),
        }; // The first point preserves within-group direction; the actual group range comes only from this Core result.

        // Geometry checks may reenter; replace pure endpoints only while the original gesture and Input are still current.
        if self.cell_pointer_is_current(intent) && origin.current(self) {
            self.ivars().drag_anchor.set(Some(DragAnchor::Word {
                anchor: WordAnchor::Group {
                    from_utf16: group.start,
                    to_utf16: group.end,
                    origin_utf16,
                },
                generation,
            }));
            return Ok(true);
        }
        Ok(false)
    }

    /// Return a published draft selection to the same mouse gesture, updating only its already mapped drag seed.
    /// Up, a new down, or keyboard retirement ends this Weak; late readiness cannot resurrect the gesture.
    pub(in crate::native_surface) fn adopt_cell_pointer_selection(
        &self,
        commit: &super::NativeMarkdownPagedInputCommit,
        intent: &std::rc::Weak<()>,
        word_anchor: Option<WordAnchor>,
    ) -> Result<()> {
        let owns = || self.cell_pointer_is_current(intent);
        if !owns() {
            return Ok(());
        }
        // This View must still own the committed Input; a display consequence cannot roll back already published text.
        // Change pointer metadata only, without another Native coordinate notification or source-byte access.
        let origin = self.origin()?;
        commit.check_current(&|epoch| origin.session_current(self, epoch))?;
        if origin.state != commit.state || !commit.state.selection_is_directed {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let next = DragAnchor::from_published_selection(commit.state, word_anchor)?;
        if owns() && origin.current(self) {
            self.ivars().drag_anchor.set(Some(next));
        } // Current checks can reenter; only the original surviving gesture may update this slot.
        Ok(())
    }

    /// Retire this cell gesture lifetime; pending selection can finish but later readiness cannot reinstall its drag anchor.
    pub(super) fn cancel_cell_pointer_intent(&self) -> bool {
        let drag_scroll = self.take_drag_scroll(); // Remove the deadline before native invalidation can reenter.
        self.ivars().pointer_motion.set(None);
        drop(self.ivars().cell_pointer_intent.borrow_mut().take());
        self.ivars().drag_anchor.set(None); // The down ended; a later drag cannot reconstruct its seed from current selection.
        // The View's Rc may already be empty after admitted up; explicitly cancel any Body endpoint still waiting to publish.
        let delegate = self
            .ivars()
            .delegate
            .borrow()
            .as_ref()
            .and_then(std::rc::Weak::upgrade);
        let draining = delegate.is_some_and(|delegate| delegate.cancel_deferred_pointer());
        drop(drag_scroll); // Timer destruction can reenter; a new down already has independent state outside this cleanup.
        draining
    }

    /// End only this gesture; asynchronous work holds at most a Weak and cannot keep the View/Source/font graph alive.
    pub(super) fn cancel_link_intent(&self) {
        drop(self.ivars().pressed_link.borrow_mut().take());
        drop(self.ivars().link_intent.borrow_mut().take());
    }

    /// Only actual mouse-up can hand off a click; later drag, selection, geometry, or focus changes cannot recertify it.
    pub(super) fn finish_link_mouse(&self, event: &NSEvent) {
        let forward = self.ivars().cell_pointer_forward.borrow_mut().take();
        if let Some(forward) = forward {
            if let Some(parent) = forward.parent.upgrade()
                && parent.cell_pointer_is_current(&forward.intent)
            {
                parent.complete_pointer(event);
            }
            return; // Real up releases the child's weak route; late drag gains no new Input/down authority.
        }
        if self.ivars().closed.get() {
            return;
        }
        self.complete_pointer(event);
    }

    /// Complete up once on the actual owner; a handed-off child cannot open its own link or read its sealed draft.
    fn complete_pointer(&self, event: &NSEvent) {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let _drag_scroll = self.take_drag_scroll(); // Remove the deadline now; invalidate after up without confusing a reentrant down.
        // Up has a real final position even when layout is slower than the gesture; clearing a slot must not lose it.
        // Body admission occurs while this View owns the down; dropping its Rc later ends dragging, not that admitted endpoint.
        let pointer_intent = self
            .ivars()
            .cell_pointer_intent
            .borrow()
            .as_ref()
            .map(std::rc::Rc::downgrade);
        let pressed = self.ivars().pressed_link.borrow_mut().take();
        let link_intent = self
            .ivars()
            .link_intent
            .borrow()
            .as_ref()
            .map(std::rc::Rc::downgrade);
        let has_link = pressed.is_some();
        if let Some(pointer_intent) = pointer_intent {
            let result = (|| {
                if !self.cell_pointer_is_current(&pointer_intent) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                let origin = self.origin()?;
                if !has_link {
                    let point = origin.step(self, || event.locationInWindow())?;
                    let moved = self
                        .ivars()
                        .pointer_motion
                        .get()
                        .is_none_or(|motion| motion.dragged || motion.down != point);
                    let pointer = self.pointer_location(&origin, event, moved, true)?;
                    if !std::rc::Weak::ptr_eq(&pointer.intent, &pointer_intent)
                        || !self.cell_pointer_is_current(&pointer_intent)
                    {
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    }
                    if !moved {
                        // Preserve up for a still-preparing down/first word instead of re-hitting changed geometry.
                        // A standalone synchronous client may decline the async slot because its down already set selection.
                        let request = self.scene_request(&origin)?;
                        let permit = origin.input.capture(origin.epoch, &|epoch| {
                            epoch == origin.epoch && origin.current(self)
                        })?;
                        origin.delegate.defer_pointer(&request, pointer, permit)?;
                    } else if !self.hand_child_pointer(&origin, &pointer)? {
                        if matches!(
                            pointer.anchor,
                            Some(super::NativeMarkdownPagedCellDragAnchor::Word(_))
                        ) || self.ivars().drag_anchor.get().is_none()
                        {
                            self.defer_pointer_location(&origin, pointer)?;
                        } else {
                            match self.scene() {
                                Ok(scene) => self.apply_pointer(&scene, &pointer)?,
                                Err(NativeMarkdownPagedTextViewError::Pending) => {
                                    self.defer_pointer_location(&origin, pointer)?
                                }
                                Err(cause) => return Err(cause),
                            }
                        }
                    }
                }
                if !self.cell_pointer_is_current(&pointer_intent) {
                    // Child commit may already have moved this gesture and endpoint to the parent.
                    // Do not read a new origin from the sealed old Input or replay the event on that child.
                    return Ok(());
                }
                // Selection may have committed above; completion borrows its actual postcommit request, not the old InputState.
                let after = self.origin()?;
                let request = self.scene_request(&after)?;
                after.delegate.finish_cell_pointer(&request)
            })();
            let owned = self.end_pointer_metadata(&pointer_intent);
            if !owned {
                let forward = self.ivars().cell_pointer_forward.borrow().clone();
                if let Some(forward) = forward
                    && std::rc::Weak::ptr_eq(&forward.intent, &pointer_intent)
                {
                    self.ivars().cell_pointer_forward.borrow_mut().take();
                    if let Some(parent) = forward.parent.upgrade() {
                        parent.end_pointer_metadata(&pointer_intent);
                    }
                } // A handoff during up must also end the parent's same gesture; a surviving Rc cannot revive the released mouse.
            }
            if self.ivars().cell_pointer_intent.borrow().is_none() {
                self.record(result);
            } // A reentrant down owns its own feedback; this old up cannot clear or report into the newer operation.
        }
        let Some(pressed) = pressed else {
            return;
        };
        let Some(intent) = link_intent else {
            return;
        };
        let result = (|| {
            if !self.link_intent_current(&pressed.key, &intent)? {
                return Ok(());
            }
            let scene = self.scene()?;
            if !scene.key.same(&pressed.key) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            // Releasing Command cancels activation; mouse-up cannot rely on modifiers that were true only at down.
            let modifiers = scene.step(self, || event.modifierFlags())?;
            if !link_activation_modifiers(modifiers) {
                self.retire_link_intent(&intent);
                return Ok(());
            }
            let point = scene.step(self, || event.locationInWindow())?;
            let local = scene.step(self, || self.convertPoint_fromView(point, None))?;
            // Actual dragging already retired the token; leaving visible LINK ink on up also forbids opening the down target.
            if !scene
                .scene
                .link_hit(local)?
                .is_some_and(|(_, index)| pressed.part.contains(&index))
            {
                self.retire_link_intent(&intent);
                return Ok(());
            }
            if !self.link_intent_current(&pressed.key, &intent)? {
                self.retire_link_intent(&intent);
                return Ok(());
            }
            scene.scene.follow_link(pressed.index, intent.clone())
        })();
        if result.is_err() {
            self.retire_link_intent(&intent);
        }
        // Cleanup can deliver a reentrant down. Report this up only if its original link
        // slot still owns feedback, without clearing or overwriting a successor operation.
        let current_link = self
            .ivars()
            .link_intent
            .borrow()
            .as_ref()
            .map(std::rc::Rc::downgrade);
        if current_link
            .as_ref()
            .is_none_or(|current| std::rc::Weak::ptr_eq(current, &intent))
        {
            self.record(result); // Recheck feedback ownership after Native cleanup so an old click cannot report into a new down.
        }
    }

    /// End only the specified gesture: clear pure metadata before native deadline release can reenter with a new down.
    fn end_pointer_metadata(&self, intent: &std::rc::Weak<()>) -> bool {
        if !self.cell_pointer_is_current(intent) {
            return false;
        }
        let timer = self.take_drag_scroll();
        self.ivars().drag_anchor.set(None);
        self.ivars().pointer_motion.set(None);
        drop(self.ivars().cell_pointer_intent.borrow_mut().take());
        drop(timer);
        true
    }

    /// Validate the original weak gesture, Scene/Input/geometry, and actual first responder after each Native reentry point.
    pub(in crate::native_surface) fn link_intent_current(
        &self,
        key: &super::NativeMarkdownPagedSceneKey,
        intent: &std::rc::Weak<()>,
    ) -> Result<bool> {
        let owns = || {
            self.ivars()
                .link_intent
                .borrow()
                .as_ref()
                .is_some_and(|active| {
                    std::rc::Weak::ptr_eq(&std::rc::Rc::downgrade(active), intent)
                })
        };
        if !owns() || self.ivars().closed.get() {
            return Ok(false);
        }
        let origin = self.origin()?;
        let actual = self.scene_request(&origin)?;
        if !actual.same(&key.request) || !origin.delegate.scene_current(key) || !owns() {
            return Ok(false);
        }
        let window = origin.step(self, || self.window())?;
        let Some(window) = window else {
            return Ok(false);
        };
        if !origin.step(self, || window.isKeyWindow())? {
            return Ok(false); // An inactive window may remember its responder; that does not grant current keyboard focus.
        }
        let responder = origin.step(self, || window.firstResponder())?;
        let focused = responder.is_some_and(|responder| {
            std::ptr::eq::<objc2_app_kit::NSResponder>(&*responder, self.as_super().as_super())
        });
        drop(window); // Release native temporaries before the final same-ticket check, including destructor reentry.
        Ok(focused && owns() && origin.delegate.scene_current(key) && origin.current(self))
    }

    /// Consume only the original click's token; cleanup must not clear a newer gesture admitted by Native reentry.
    pub(in crate::native_surface) fn retire_link_intent(&self, intent: &std::rc::Weak<()>) {
        let matches = self
            .ivars()
            .link_intent
            .borrow()
            .as_ref()
            .is_some_and(|active| std::rc::Weak::ptr_eq(&std::rc::Rc::downgrade(active), intent));
        if matches {
            self.cancel_link_intent();
        }
    }

    /// Activate once after actual target resolution, consuming permission before an external effect that cannot be replayed.
    pub(in crate::native_surface) fn finish_link_activation(
        &self,
        key: &super::NativeMarkdownPagedSceneKey,
        intent: &std::rc::Weak<()>,
        destination: &str,
    ) {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let result = (|| {
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            let value = objc2_foundation::NSString::from_str(destination);
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            let url = objc2_foundation::NSURL::URLWithString(&value);
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            let Some(url) = url else {
                return Err(NativeMarkdownPagedTextViewError::LinkOpenFailed);
            };
            let scheme = url.scheme();
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            if scheme.is_none() {
                // Decode the encoded fragment after the original URI's first # exactly once.
                // Re-decoding Foundation's decoded value would turn %2520 into a space; this finite URI still belongs to one Source.
                let fragment = destination.split_once('#').map(|(_, value)| value);
                let fragment = fragment
                    .map(|value| {
                        objc2_foundation::NSString::from_str(value)
                            .stringByRemovingPercentEncoding()
                            .map(|value| value.to_string())
                            .ok_or(NativeMarkdownPagedTextViewError::LinkAnchorInvalid)
                    })
                    .transpose()?;
                if !self.link_intent_current(key, intent)? {
                    return Ok(false);
                }
                let query = url.query();
                if !self.link_intent_current(key, intent)? {
                    return Ok(false);
                }
                if query.is_some() {
                    return Err(NativeMarkdownPagedTextViewError::RelativeNoteLink(
                        "相对笔记链接不支持查询参数".into(),
                    ));
                }
                let host = url.host();
                if !self.link_intent_current(key, intent)? {
                    return Ok(false);
                }
                if host.is_some() {
                    return Err(NativeMarkdownPagedTextViewError::RelativeNoteLink(
                        "不能通过笔记链接访问网络文件路径".into(),
                    ));
                }
                let path = url.path().map(|path| path.to_string());
                if !self.link_intent_current(key, intent)? {
                    return Ok(false);
                }
                if path.as_ref().is_none_or(|path| path.is_empty())
                    && let Some(fragment) = fragment.as_deref()
                {
                    let scene = self.scene()?;
                    if !scene.key.same(key) || !self.link_intent_current(key, intent)? {
                        return Ok(false);
                    }
                    scene.scene.follow_heading(fragment, intent.clone())?;
                    return Ok(true); // The one heading task retains the click until actual resolution consumes it and selects the target.
                }
                let path = path.filter(|path| !path.is_empty()).ok_or_else(|| {
                    NativeMarkdownPagedTextViewError::RelativeNoteLink(
                        "笔记链接没有有效的相对路径".into(),
                    )
                })?;
                crate::native_surface::queue_native_markdown_relative_note_link(
                    self, key, intent, &path, fragment,
                )?;
                return Ok(true); // The queue borrows this Weak; save and old-graph barrier handoff wait until this stack returns.
            }
            let local_file = url.isFileURL();
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            if local_file {
                return Err(NativeMarkdownPagedTextViewError::RelativeNoteLink(
                    "本地笔记请使用笔记库内的相对路径".into(),
                ));
            } // File URLs must not bypass workspace confinement, unsaved-document commit, or teardown through another app.
            // Existing external-link policy delegates registered non-file schemes to macOS.
            // Relative notes and file URLs must still use the confined workspace routes above.
            let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            let scene = self.scene()?;
            scene.scene.validate_ready(&|| scene.current(self))?;
            if !self.link_intent_current(key, intent)? {
                return Ok(false);
            }
            // Consume this exact gesture before the sole system handoff. A false result or
            // later browser failure cannot authorize replaying an external side effect.
            self.retire_link_intent(intent);
            // True reports NSWorkspace acceptance only; page loading and browser cleanup
            // require separate external evidence and are never inferred from this return.
            if workspace.openURL(&url) {
                Ok(false)
            } else {
                Err(NativeMarkdownPagedTextViewError::LinkOpenFailed)
            }
        })();
        if !matches!(result, Ok(true)) {
            self.retire_link_intent(intent); // Queued relative/heading work consumes later; retiring it here would revoke the first click.
        }
        if let Err(cause) = result {
            self.report_link_failure(key, cause);
        }
    }

    /// Present failure only through the original workspace owner, retaining its typed cause rather than claiming unsupported success.
    pub(in crate::native_surface) fn report_link_failure(
        &self,
        key: &super::NativeMarkdownPagedSceneKey,
        cause: NativeMarkdownPagedTextViewError,
    ) {
        crate::native_surface::present_native_markdown_paged_link_error(self, &key.request, &cause);
        self.failure(cause); // Preserve the editable body/Source and the actual error; never replay the failed external effect.
    }

    /// Accept the heading result only under its original complete Input; selection preserves generation/undo.
    /// The commit chain unfolds as required, retires old CPU, and reveals through SourceTarget, never Find replay or raw scrolling.
    pub(in crate::native_surface) fn navigate_to_heading(
        &self,
        expected: super::NativeMarkdownPagedInputState,
        position: usize,
    ) -> Result<()> {
        let origin = self.origin()?;
        if origin.state != expected || origin.input.is_cell() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        let outcome = origin.input.set_selection_intent(
            &permit,
            MarkdownTextSelection {
                anchor_utf16: position,
                head_utf16: position,
            },
            Some(false),
            None,
            &current,
        )?;
        self.finish_edit_outcome(&origin, outcome)
    }

    /// Report a missing heading before a new note's first frame, but only to the same complete client without clearing its document.
    pub(in crate::native_surface) fn report_heading_failure(
        &self,
        request: &super::NativeMarkdownPagedSceneRequest,
        cause: NativeMarkdownPagedTextViewError,
    ) {
        if self
            .input_state()
            .is_ok_and(|actual| actual == request.input)
        {
            crate::native_surface::present_native_markdown_paged_link_error(self, request, &cause);
            self.failure(cause);
        }
    }
}
