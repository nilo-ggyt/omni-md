//! Body composition merges only real replacements inside the same current marked
//! range. History retains its first root; notifications describe this exact edit.

use super::*;

/// Translates the same input owner's original marked state. A new client's start
/// cannot inherit a historical composition marker left by an earlier client.
pub enum MarkdownTextInputComposition {
    Start,
    Update { marked_before: Range<usize> },
    Commit { marked_before: Range<usize> },
}

pub(in crate::markdown_document_buffer) struct CompositionGroup {
    generation: u64,
    marked: Range<usize>,
    // The actual append establishes file exclusivity or heap-prefix bounds.
    // Unreplaced sides of a subrange remain owned by the canonical document.
    allocation: Option<piece_tree::CompositionAllocation>,
}

struct MergedHistory {
    replacement: MarkdownReplacementMetrics,
    changes: change_journal::Entry,
}

/// The net journal remains inline. Heap-rebase preparation owns at most two
/// 64-KiB segments, and cancellation changes neither old text nor history.
pub(super) struct CompositionPlan {
    merged: Option<MergedHistory>,
    marked: Option<Range<usize>>,
    allocation: Option<piece_tree::CompositionAllocation>,
    retire_previous: bool,
    heap_before: Option<piece_tree::HeapAllocation>,
    heap_cow: Option<piece_tree::CompositionHeapCow>,
    rebased_root: Option<PieceTreeLink>,
}

impl MarkdownDocumentBuffer {
    /// Synchronous composition start, update, and commit reuse the sole prepared
    /// candidate. An empty candidate also terminates the composition.
    ///
    /// # Errors
    /// Range, Unicode, admission, observer, original-source, and candidate-cleanup
    /// failures preserve the original root, composition group, undo, and redo.
    pub fn prepare_composition_text_input_observed<O: MarkdownTextChangeObserver>(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        selection_before: MarkdownTextSelection,
        selection_after: MarkdownTextSelection,
        composition: MarkdownTextInputComposition,
        observer: O,
    ) -> Result<(MarkdownPreparedTextInput, O::Output), MarkdownObservedEditError<O::Error>> {
        self.prepare_text_input_replacement_composition_observed(
            range,
            TextInputReplacement::Text(replacement),
            selection_before,
            selection_after,
            Some(composition),
            observer,
        )
    }

    /// Unmark ends only this composition's history authority. It changes no text,
    /// generation, selection, undo/redo entry, or journal.
    ///
    /// # Errors
    /// Source failure or a pending candidate preserves the group. The unique
    /// candidate cannot be bypassed to end a later composition prematurely.
    pub fn finish_text_input_composition(&mut self) -> Result<(), MarkdownDocumentBufferError> {
        self.ensure_text_input_idle()?;
        self.text_input_composition = None;
        Ok(())
    }

    /// The sole prepare path already validated range, scalar endpoints, and final
    /// length. This phase prepares only the current group's net history.
    pub(super) fn plan_composition<E>(
        &self,
        range: &Range<usize>,
        inserted: usize,
        composition: MarkdownTextInputComposition,
    ) -> Result<CompositionPlan, MarkdownObservedEditError<E>> {
        let (marked_before, marked) = match composition {
            MarkdownTextInputComposition::Start => (None, true), // Input had no marked range, so always start a separate group.
            MarkdownTextInputComposition::Update { marked_before } => (Some(marked_before), true),
            MarkdownTextInputComposition::Commit { marked_before } => (Some(marked_before), false),
        };
        let end = range.start + inserted;
        let next_marked = (marked && inserted != 0).then_some(range.start..end);
        let previous = self.text_input_composition.as_ref().filter(|group| {
            group.generation == self.generation
                && marked_before.as_ref() == Some(&group.marked)
                && self.redo.is_empty()
                && range.start >= group.marked.start
                && range.end <= group.marked.end
        });
        let retire_previous = previous.is_some_and(|group| *range == group.marked);
        let heap_before = previous
            .filter(|_| retire_previous)
            .and_then(|group| group.allocation.as_ref())
            .and_then(piece_tree::CompositionAllocation::heap);
        let merged = if previous.is_some() {
            let previous = self.undo.back().expect("活动组合已经发布首个历史项");
            let net = previous
                .replacement
                .inserted_utf16_length
                .checked_sub(range.len())
                .and_then(|length| length.checked_add(inserted))
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
            let replacement = MarkdownReplacementMetrics::new(
                previous.replacement.location,
                previous.replacement.removed_utf16_length,
                net,
            )?;
            // A subrange updates only part of the marked text. The net journal still
            // covers the first group replacement plus retained candidate pieces.
            // Read the baseline from original undo-root aggregates. This local edit
            // cannot stand in for the full first-root-to-final-root change.
            let changes = change_journal::Entry::single_replacement(
                piece_tree_metrics(&previous.root).utf16_len,
                change_journal::Record {
                    start: replacement.location,
                    removed: replacement.removed_utf16_length,
                    inserted: replacement.inserted_utf16_length,
                },
            )?;
            Some(MergedHistory {
                replacement,
                changes,
            })
        } else {
            None // Outside the original marked group, start a new item and preserve external-text history.
        };
        Ok(CompositionPlan {
            merged,
            marked: next_marked,
            allocation: None, // Register only after actual append; input length cannot predict a segment identity.
            retire_previous,
            heap_before,
            heap_cow: None,
            rebased_root: None,
        })
    }
}

impl CompositionPlan {
    /// The original append first supplies real candidate reads. Replacing an entire
    /// old heap allocation prepares equivalent copy-on-write storage separately;
    /// failure or cancellation never writes back into the old segment.
    pub(super) fn append(
        &mut self,
        edits: &mut MarkdownEditSegments,
        text: &str,
        maximum_session_edit_bytes: usize,
    ) -> Result<Vec<Arc<MarkdownPieceTreeNode>>, MarkdownDocumentBufferError> {
        let before_segments = edits.segments.len();
        let (pieces, allocation) = edits.append_composition(text, maximum_session_edit_bytes)?;
        self.allocation = allocation;
        if let Some(previous) = self.heap_before {
            let cow = edits.prepare_composition_heap_cow(previous, text, before_segments);
            if text.len() <= EDIT_SEGMENT_BYTES {
                self.allocation = cow.allocation(); // The candidate's heap-leaf offsets switch to this actual copy-on-write allocation.
            }
            self.heap_cow = Some(cow);
        }
        Ok(pieces)
    }

    /// Rebuilds only this insertion and the split prefix/suffix join paths. It scans
    /// neither the entire tree, undo history, nor earlier candidate text.
    pub(super) fn prepare_rebased_root(&mut self, prefix: &PieceTreeLink, suffix: &PieceTreeLink) {
        if let Some(cow) = &mut self.heap_cow
            && let Some(inserted) = cow.take_inserted()
        {
            self.rebased_root = Some(join_piece_trees(
                prefix.clone(),
                join_piece_trees(inserted, suffix.clone()),
            ));
        }
    }

    /// All endpoints and observer output were verified against the original append
    /// candidate. Further reads must wait until corresponding bytes are installed.
    pub(super) fn finish_prepared(&mut self, prepared: &mut SealedCommand) {
        if let Some(root) = self.rebased_root.take() {
            prepared.command.root = root;
        }
    }
}

/// Net journals and candidate text are fully prepared. Publication invokes no
/// external code; the original receipt retains failures from later payload drop.
pub(super) fn publish(
    document: &mut MarkdownDocumentBuffer,
    prepared: SealedCommand,
    mut plan: CompositionPlan,
) -> MarkdownHistoryTextEdit {
    let previous_allocation = document
        .text_input_composition
        .take()
        .and_then(|group| plan.retire_previous.then_some(group.allocation).flatten());
    if let Some(cow) = plan.heap_cow.take() {
        // No native callback, currentness callback, observer, or fallible I/O runs
        // here. Canonical identity and actual bytes change in one publication stack.
        cow.publish(&mut document.edits);
    }
    let edit = if let Some(merged) = plan.merged {
        let SealedCommand {
            command:
                PreparedCommand {
                    root,
                    after,
                    metrics,
                },
            changes,
            ..
        } = prepared;
        let previous = document.undo.back_mut().expect("原nonce仍独占活动组尾项");
        previous.replacement = merged.replacement;
        previous.counterpart_selection = Some(after);
        let old_changes = std::mem::replace(&mut previous.changes, merged.changes);
        document.change_journal.release(old_changes);
        document.change_journal.release(changes); // The real net journal replaces this local one; release its intermediate pages.
        document.edits.publish();
        document.root = root; // The tail entry keeps the first root/selection; no history owner retains intermediate roots.
        document.selection = after;
        document.generation = document.generation.wrapping_add(1);
        #[cfg(any(test, feature = "native-markdown-candidate"))]
        document
            .block_checkpoints
            .get_mut()
            .invalidate_after(metrics.location);
        metrics.redo_text_edit(after) // Native notification describes this actual replacement, not the cumulative composition envelope.
    } else {
        let edit = command_transaction::publish_command(document, prepared);
        document
            .undo
            .back_mut()
            .expect("首个真实组合字符事务已经入栈")
            .counterpart_selection = Some(document.selection);
        edit
    };
    document.text_input_composition = plan.marked.map(|marked| CompositionGroup {
        generation: document.generation,
        marked,
        allocation: plan.allocation,
    });
    if let Some(allocation) = previous_allocation {
        // Canonical text, net history, and the input post-state are now final.
        // Release the old payload's strong edge only now; cleanup cannot uncommit it.
        // A final frozen reader may legitimately delay physical release. The
        // original slot's observer retains its release result and first failure.
        allocation.retire(&mut document.edits);
    }
    edit
}

/// A true zero-character change can end marked text or move the group's final
/// selection, without creating an empty undo item or changing ordinary history.
pub(super) fn publish_selection(
    document: &mut MarkdownDocumentBuffer,
    plan: Option<CompositionPlan>,
    after: MarkdownTextSelection,
) {
    if plan.is_some_and(|plan| plan.merged.is_some()) {
        document
            .undo
            .back_mut()
            .expect("同一活动组保留首个历史项")
            .counterpart_selection = Some(after);
    }
    // Selection-only means empty range plus empty input, so it cannot establish
    // marked text. Both ordinary selection and empty input end the old group.
    document.text_input_composition = None;
}

#[cfg(test)]
mod tests;
