//! Prepares the candidate root, real change journal, and derived output together
//! before one publication. Failure preserves canonical text and both history branches.

use super::changes::{MarkdownObservedEditError, MarkdownPreparedText, MarkdownTextChangeObserver};
use super::*;

/// Streaming commands learn complete replacement geometry only after consuming
/// their input. This payload is still an unpublished candidate.
pub(super) struct PreparedCommand {
    pub(super) root: PieceTreeLink,
    pub(super) after: MarkdownTextSelection,
    pub(super) metrics: MarkdownReplacementMetrics,
}

/// Records append-store boundaries. Failed candidates remove only unpublished
/// tail storage instead of copying the existing document.
pub(super) struct AppendCheckpoint {
    segment_count: usize,
    segment_capacity: usize,
    tail_bytes: usize,
    used_bytes: usize,
}

impl AppendCheckpoint {
    fn capture(document: &MarkdownDocumentBuffer) -> Self {
        Self::capture_segments(&document.edits)
    }

    /// Draft branches share the same rollback contract, recording only descriptors
    /// and bounded tail-segment offsets.
    pub(super) fn capture_segments(edits: &MarkdownEditSegments) -> Self {
        Self {
            segment_count: edits.segments.len(),
            segment_capacity: edits.segments.capacity(),
            tail_bytes: edits.segments.last().map_or(0, |tail| tail.len()),
            used_bytes: edits.used_bytes,
        }
    }

    pub(super) fn rollback(self, document: &mut MarkdownDocumentBuffer) {
        self.rollback_segments(&mut document.edits);
    }

    /// The caller must first drop the failed candidate root. Restore its append
    /// branch while existing snapshots retain their original `Arc` bytes.
    pub(super) fn rollback_segments(self, edits: &mut MarkdownEditSegments) {
        // Candidate references must be gone before removing their new segments
        // or unpublished bytes from the old tail.
        edits.segments.truncate(self.segment_count);
        if let Some(tail) = edits.segments.last_mut()
            && tail.len() != self.tail_bytes
        {
            // Truncate only an actual append. If preparation froze the same segment,
            // copy-on-write preserves its original bytes instead of writing through.
            // A failure without appended bytes requires no copy; published leaf IDs
            // and the original fixed segment capacity remain valid.
            MarkdownEditSegments::writable_segment(tail).truncate(self.tail_bytes);
        }
        if edits.segments.capacity() != self.segment_capacity {
            // `truncate` retains outer capacity. Move only old descriptors to restore
            // the pre-transaction allocation budget.
            let mut restored = Vec::with_capacity(self.segment_capacity);
            restored.append(&mut edits.segments);
            edits.segments = restored;
        }
        edits.used_bytes = self.used_bytes;
    }
}

/// Commands with a known result size check admission before entering the shared
/// fallible preparation path.
pub(super) fn commit_command_observed<O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    metrics: MarkdownReplacementMetrics,
    result_bytes: usize,
    before: MarkdownTextSelection,
    observer: O,
    prepare: impl FnOnce(
        &mut MarkdownDocumentBuffer,
        &mut O,
    ) -> Result<
        (PieceTreeLink, MarkdownTextSelection),
        MarkdownObservedEditError<O::Error>,
    >,
) -> Result<(MarkdownHistoryTextEdit, O::Output), MarkdownObservedEditError<O::Error>> {
    document.ensure_text_input_idle()?;
    if result_bytes > document.maximum_document_bytes {
        // Reject over-budget output before appending text, opening a change
        // journal transaction, or invoking a derived-state observer.
        return Err(MarkdownDocumentBufferError::DocumentTooLarge.into());
    }
    commit_prepared_command_observed(document, before, observer, |document, observer| {
        let (root, after) = prepare(document, observer)?;
        Ok(PreparedCommand {
            root,
            after,
            metrics,
        })
    })
}

/// Observers prepare owned pure-Rust output. Every fallible step precedes
/// publication of the root, history, and selection.
pub(super) fn commit_prepared_command_observed<O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    before: MarkdownTextSelection,
    observer: O,
    prepare: impl FnOnce(
        &mut MarkdownDocumentBuffer,
        &mut O,
    ) -> Result<PreparedCommand, MarkdownObservedEditError<O::Error>>,
) -> Result<(MarkdownHistoryTextEdit, O::Output), MarkdownObservedEditError<O::Error>> {
    let (prepared, output) = prepare_command_observed(document, before, observer, prepare)?;
    Ok((publish_command(document, prepared), output))
}

/// The document exclusively owns this sealed candidate. Cancellation drops its
/// root before append rollback; publication only moves already prepared values.
pub(super) struct SealedCommand {
    pub(super) command: PreparedCommand,
    pub(super) before: MarkdownTextSelection,
    pub(super) checkpoint: AppendCheckpoint,
    pub(super) changes: change_journal::Entry,
}

/// Completes all fallible text, observer, and journal work while the canonical
/// document still owns its original root and history.
pub(super) fn prepare_command_observed<O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    before: MarkdownTextSelection,
    mut observer: O,
    prepare: impl FnOnce(
        &mut MarkdownDocumentBuffer,
        &mut O,
    ) -> Result<PreparedCommand, MarkdownObservedEditError<O::Error>>,
) -> Result<(SealedCommand, O::Output), MarkdownObservedEditError<O::Error>> {
    document.ensure_text_input_idle()?;
    let checkpoint = AppendCheckpoint::capture(document);
    document.change_journal.begin(document.utf16_len())?;
    let prepared = (|| {
        let prepared = prepare(document, &mut observer)?;
        if piece_tree_metrics(&prepared.root).byte_len > document.maximum_document_bytes {
            // Streaming input reveals its final size here. Shared pieces still count
            // toward the live-document limit.
            return Err(MarkdownDocumentBufferError::DocumentTooLarge.into());
        }
        // Validate both endpoints against the actual candidate tree. Never swap
        // the canonical root temporarily or copy text merely to validate positions.
        byte_offset_for_utf16_in_tree(
            document,
            prepared.root.as_deref(),
            prepared.after.anchor_utf16,
            0,
        )?;
        byte_offset_for_utf16_in_tree(
            document,
            prepared.root.as_deref(),
            prepared.after.head_utf16,
            0,
        )?;
        let output = observer
            .finish(
                MarkdownPreparedText {
                    document,
                    root: &prepared.root,
                },
                prepared.after,
            )
            .map_err(MarkdownObservedEditError::Observer)?;
        document.original_backing_health()?; // Preserve a recorded original-source failure before candidate-storage failure.
        document.edits.pending_health()?; // Check unpublished mappings after the observer; the checkpoint still owns rollback.
        // Seal the journal only after derived output succeeds. A short write,
        // budget rejection, or failed validation cannot publish the candidate root.
        let changes = document
            .change_journal
            .finish(piece_tree_metrics(&prepared.root).utf16_len)?;
        Ok((prepared, output, changes))
    })();
    let (
        PreparedCommand {
            root,
            after,
            metrics,
        },
        output,
        changes,
    ) = match prepared {
        Ok(prepared) => prepared,
        Err(cause) => {
            // Local candidate roots and derived output have dropped. Restore append
            // storage before releasing this transaction's journal pages.
            checkpoint.rollback(document);
            return match document.change_journal.abort() {
                Ok(()) => Err(cause),
                Err(cleanup) => Err(MarkdownObservedEditError::RollbackUnconfirmed {
                    cause: Box::new(cause),
                    cleanup,
                }),
            };
        }
    };
    Ok((
        SealedCommand {
            command: PreparedCommand {
                root,
                after,
                metrics,
            },
            before,
            checkpoint,
            changes,
        },
        output,
    ))
}

/// The caller has verified exclusive authority. This phase performs no observer
/// callbacks, appends, journal writes, or fallible validation.
pub(super) fn publish_command(
    document: &mut MarkdownDocumentBuffer,
    prepared: SealedCommand,
) -> MarkdownHistoryTextEdit {
    let SealedCommand {
        command:
            PreparedCommand {
                root,
                after,
                metrics,
            },
        before,
        changes,
        ..
    } = prepared;
    // The remaining steps cannot fail. Move the old root and sole journal
    // receipt into one history entry, then revoke the redo branch.
    let previous = MarkdownEditorMemento {
        root: document.root.clone(),
        selection: before,
        counterpart_selection: None,
        replacement: metrics,
        changes,
    };
    document.edits.publish(); // Advance source ownership only at this infallible publication boundary.
    document.root = root;
    document.selection = after;
    document.text_input_composition = None; // A normal command starts its own history item; it cannot inherit an earlier IME group.
    if let Some(evicted) = push_bounded_snapshot(&mut document.undo, previous) {
        document.change_journal.release(evicted.changes);
    }
    for discarded in document.redo.drain(..) {
        document.change_journal.release(discarded.changes);
    }
    document.generation = document.generation.wrapping_add(1);
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    document
        .block_checkpoints
        .get_mut()
        .invalidate_after(metrics.location);
    metrics.redo_text_edit(after)
}
