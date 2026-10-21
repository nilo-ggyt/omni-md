//! Compute, publish and install standard selection actions under one original input ticket,
//! without refreshing a stale native callback from a successor document state.

use super::*;
use objc2_app_kit::{NSTextSelectionNavigationDestination, NSTextSelectionNavigationDirection};

/// Share authority/publication for system-interpreted semantic actions, without rebinding physical keys.
#[derive(Clone, Copy)]
enum SelectionAction {
    Motion(NativeMarkdownSelectionMotion),
    Expansion(NativeMarkdownSelectionExpansion),
}

/// Return marked-text navigation to the IME only while the same original action remains current;
/// destruction during return cannot renew stale authority.
pub(super) struct NativeMarkdownSelectionImeForward {
    capture: NativeMarkdownSelectionCapture,
    input_epoch: u64,
}

impl NativeMarkdownSelectionImeForward {
    /// Match the original capture, live graph and exact input epoch immediately before native forwarding.
    pub(super) fn current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        self.capture.current()
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().table_input_intent_epoch.get() == self.input_epoch
    }
}

impl PieceBackedMarkdownTextView {
    /// Standard motion changes selection only; the IME retains navigation inside marked text and candidate interaction.
    pub(super) fn run_semantic_selection_motion(
        &self,
        motion: NativeMarkdownSelectionMotion,
    ) -> Option<NativeMarkdownSelectionImeForward> {
        self.run_semantic_selection_action(SelectionAction::Motion(motion))
    }

    /// Run select-all/word/line through one state entry; explicit actions or the original graph supply complete endpoints.
    pub(super) fn run_semantic_selection_expansion(
        &self,
        expansion: NativeMarkdownSelectionExpansion,
    ) -> Option<NativeMarkdownSelectionImeForward> {
        self.run_semantic_selection_action(SelectionAction::Expansion(expansion))
    }

    /// Preserve one original action from initial capture through bounded target calculation and receipt installation.
    fn run_semantic_selection_action(
        &self,
        action: SelectionAction,
    ) -> Option<NativeMarkdownSelectionImeForward> {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // An empty framework-retained shell cannot reconstruct selection or deferred work after close.
            return None;
        }
        self.ivars().folds.invalidate_native_install();
        let initial_capture = self.ivars().folds.capture_selection().ok()?;
        let input_epoch = self.ivars().table_input_intent_epoch.get().checked_add(1)?;
        // A new semantic action cancels old table continuity without adopting its queued input authority.
        self.cancel_table_continuity_for_new_input();
        let initial = || {
            initial_capture.current()
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == input_epoch
        };
        let Ok(Some(storage)) =
            native_markdown_table_intent_step(&initial, || self.ivars().text_storage.load())
        else {
            return None;
        };
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super());
        let Ok(marked) = native_markdown_table_intent_step(&initial, || input.hasMarkedText())
        else {
            return None;
        };
        if marked {
            // Inspect neither the marked range nor its text. Super may run once only if this
            // original ticket remains current after the forwarding payload returns to its caller.
            return Some(NativeMarkdownSelectionImeForward {
                capture: initial_capture,
                input_epoch,
            });
        }
        // Document boundaries are absolute source coordinates, independent of old soft-wrap
        // affinity. Plain boundary motion and select-all specify a new direction; extension
        // still preserves the original anchor and cannot infer an unknown active endpoint.
        let document_end = match action {
            SelectionAction::Motion(motion)
                if motion.destination == NSTextSelectionNavigationDestination::Document =>
            {
                match motion.direction {
                    NSTextSelectionNavigationDirection::Backward => Some(false),
                    NSTextSelectionNavigationDirection::Forward => Some(true),
                    // Visual directions are not logical source boundaries; the original graph still resolves them.
                    _ => None,
                }
            }
            // Word, line, paragraph and relative movements still require actual layout.
            _ => None,
        };
        let replaces_direction = match action {
            SelectionAction::Expansion(expansion) => !expansion.requires_known_head(),
            SelectionAction::Motion(motion) => document_end.is_some() && !motion.extending,
        };
        let captured = if replaces_direction {
            // An explicit new direction may replace unknown or multiple ranges; an old native
            // multi-selection cannot be presented as a single before-range.
            let Ok(Ok(native_before)) = native_markdown_table_intent_step(&initial, || {
                self.native_ordered_selection_state_for_capture(&initial_capture)
            }) else {
                return None;
            };
            let document = storage.document()?;
            let before = document.borrow().selection();
            let known = self.ivars().folds.known_selection() == Some(before);
            let captured = initial_capture.clone().finish(before, known).ok()?;
            drop(document);
            match native_before {
                NativeMarkdownOrderedSelection::Single(range) => {
                    captured.with_observed_native_before(range).ok()?
                }
                NativeMarkdownOrderedSelection::Multiple => captured,
            }
        } else {
            // Relative actions require the proven original active endpoint. Unknown nonempty
            // direction cannot become an invented endpoint in folding or history state.
            let Ok(Ok(captured)) =
                native_markdown_table_intent_step(&initial, || self.primary_text_selection())
            else {
                return None;
            };
            captured
        };
        let document = storage.document()?;
        if !captured.belongs_to(&document) || !captured.current() {
            return None;
        }
        let identity = Rc::downgrade(&document);
        let (generation, length) = {
            let document = document.borrow();
            (document.generation(), document.utf16_len())
        };
        // Preserve Weak identity and scalars only; native navigation retains no extra document root.
        drop(document);
        let width = storage.ivars().table_projection_width_key.get();
        let geometry_epoch = storage.ivars().table_geometry_epoch.get();
        let mode = storage.presentation_mode();
        let current = || {
            captured.current()
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == input_epoch
                && storage.ivars().accepts_edits.get()
                && storage.presentation_mode() == mode
                && storage.ivars().table_projection_width_key.get() == width
                && storage.ivars().table_geometry_epoch.get() == geometry_epoch
                && std::rc::Weak::ptr_eq(&identity, &storage.ivars().document.borrow())
                && identity.upgrade().is_some_and(|document| {
                    let document = document.borrow();
                    document.generation() == generation && document.utf16_len() == length
                })
        };
        let target = (|| -> Result<_, String> {
            if let SelectionAction::Expansion(expansion) = action {
                // Select-all uses finite source length alone; word/visual-line/paragraph
                // expansion temporarily acquires the original native navigation graph.
                let graph = expansion
                    .requires_known_head()
                    .then(|| NativeMarkdownSelectionGraph::load(self, &current))
                    .transpose()?;
                return selection_expansion_target(
                    expansion,
                    graph.as_ref(),
                    Some(&captured),
                    length,
                    &current,
                )
                .map(|target| (target, None));
            }
            let SelectionAction::Motion(motion) = action else {
                unreachable!("expansion handled above");
            };
            if let Some(at_end) = document_end {
                // Actual source endpoints require no body layout or inherited wrap side.
                // EOF is upstream and BOF downstream; an empty source has only BOF and
                // establishes neither a fictitious upstream affinity nor a vertical target.
                let head = if at_end { length } else { 0 };
                let selection = if motion.extending {
                    MarkdownTextSelection {
                        anchor_utf16: captured.selection.anchor_utf16,
                        head_utf16: head,
                    }
                } else {
                    MarkdownTextSelection::caret(head)
                };
                return Ok((
                    NativeMarkdownSelectionTarget {
                        selection,
                        head_upstream: at_end && length != 0,
                        line_offset: 0.0,
                    },
                    None,
                ));
            }
            let graph = NativeMarkdownSelectionGraph::load(self, &current)?;
            let vertical = motion.destination == NSTextSelectionNavigationDestination::Character
                && matches!(
                    motion.direction,
                    NSTextSelectionNavigationDirection::Up
                        | NSTextSelectionNavigationDirection::Down
                );
            let head_upstream = captured
                .head_is_upstream()
                .ok_or("当前插入位置的换行侧尚未确定")?;
            let column = if vertical {
                // Reuse a vertical goal only in its original width and layout epoch; otherwise
                // derive a fresh column from the actual original caret before moving.
                if let Some(goal) = captured
                    .vertical_goal()
                    .filter(|goal| goal.width_key == width && goal.geometry_epoch == geometry_epoch)
                {
                    Some(NativeMarkdownSelectionColumn {
                        line_offset: goal.line_offset,
                        container_x: goal.container_x,
                    })
                } else {
                    Some(graph.column_for_head(
                        self,
                        captured.selection.head_utf16,
                        head_upstream,
                        length,
                        &current,
                    )?)
                }
            } else {
                // Horizontal, word and boundary actions end the previous vertical-column sequence.
                None
            };
            let target = graph.move_selection(
                captured.selection,
                motion,
                head_upstream,
                column.map(|column| column.line_offset),
                length,
                &current,
            )?;
            // A short line's actual endpoint cannot replace the intended target column;
            // the next vertical action still attempts the original horizontal position.
            let goal = column.and_then(|column| {
                NativeMarkdownVerticalGoal::new(
                    column.line_offset,
                    column.container_x,
                    width,
                    geometry_epoch,
                )
            });
            Ok((target, goal))
        })();
        // Release the Graph and temporary native results before checking publication authority.
        if !current() {
            return None;
        }
        let Ok((target, goal)) = target else {
            // Missing layout or unproven coordinates preserve selection, without replaying the action.
            return None;
        };
        let valid_boundaries = identity.upgrade().is_some_and(|document| {
            let document = document.borrow();
            document
                .byte_offset_for_utf16(target.selection.anchor_utf16)
                .is_ok()
                && document
                    .byte_offset_for_utf16(target.selection.head_utf16)
                    .is_ok()
        });
        if !valid_boundaries || !current() {
            // Numerically valid UTF-16 extents still require canonical Unicode boundaries at both endpoints.
            return None;
        }
        let Ok(captured) = captured.with_selection(target.selection, true) else {
            return None;
        };
        let captured = captured
            .with_head_side(Some(target.head_upstream))
            .with_vertical_goal(goal);
        if let Some(committed) = storage.update_active_selection(captured) {
            // Pure publication created this receipt. Attribute notifications may reenter,
            // so installation consumes only it, never recapturing current fold/generation state.
            let is_current = || {
                self.ivars().table_input_intent_epoch.get() == input_epoch
                    && storage.ivars().table_projection_width_key.get() == width
                    && storage.presentation_mode() == mode
            };
            if self.apply_committed_selection_if(&committed, is_current)
                && matches!(action, SelectionAction::Motion(_))
            {
                // The setter installs selection only; standard motion also follows its new
                // active endpoint using a zero-length caret request, never the whole extended
                // range. Expansion such as select-all preserves the user's reading position.
                let current = || committed.current() && is_current();
                let _ = native_markdown_table_intent_step(&current, || {
                    self.scrollRangeToVisible(NSRange::new(target.selection.head_utf16, 0))
                });
            }
        }
        None
    }
}
