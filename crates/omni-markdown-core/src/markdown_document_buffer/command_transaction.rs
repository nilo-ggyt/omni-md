//! Shared atomic publication for formatting and keyboard commands over persistent document roots.

use super::*;

/// Roll back unpublished appended storage on failure and publish exactly one history/selection edit.
pub(super) fn commit_command(
    document: &mut MarkdownDocumentBuffer,
    metrics: MarkdownReplacementMetrics,
    result_bytes: usize,
    before: MarkdownTextSelection,
    prepare: impl FnOnce(
        &mut MarkdownDocumentBuffer,
    )
        -> Result<(PieceTreeLink, MarkdownTextSelection), MarkdownDocumentBufferError>,
) -> Result<MarkdownHistoryTextEdit, MarkdownDocumentBufferError> {
    if result_bytes > document.maximum_document_bytes {
        // A command cannot exceed the live byte cap even when it shares or normalizes source text.
        return Err(MarkdownDocumentBufferError::DocumentTooLarge);
    }
    // Candidate trees stay unpublished while every split, append and future boundary may still fail.
    let segment_count = document.edits.segments.len();
    let tail_bytes = document.edits.segments.last().map_or(0, Vec::len);
    let used_bytes = document.edits.used_bytes;
    let prepared = prepare(document).and_then(|(root, after)| {
        // Validate against the candidate tree; no prospective full-document string is required.
        byte_offset_for_utf16_in_tree(document, root.as_deref(), after.anchor_utf16, 0)?;
        byte_offset_for_utf16_in_tree(document, root.as_deref(), after.head_utf16, 0)?;
        Ok((root, after))
    });
    let (root, after) = match prepared {
        // All fallible preparation succeeded, so the caller may commit the one logical command.
        Ok(prepared) => prepared,
        Err(error) => {
            // The failed candidate is gone; only its private append tail must be restored.
            document.edits.segments.truncate(segment_count);
            if let Some(tail) = document.edits.segments.last_mut() {
                // The previously final segment alone may have gained bytes before failure.
                tail.truncate(tail_bytes);
            }
            document.edits.used_bytes = used_bytes;
            return Err(error);
        }
    };
    // Transfer the old root into one bounded memento before replacing authoritative state.
    let previous = MarkdownEditorMemento {
        root: document.root.clone(),
        selection: before,
        replacement: metrics,
    };
    document.root = root;
    document.selection = after;
    push_bounded_snapshot(&mut document.undo, previous);
    document.redo.clear();
    document.generation = document.generation.wrapping_add(1);
    document
        .block_checkpoints
        .get_mut()
        .invalidate_after(metrics.location);
    Ok(metrics.redo_text_edit(after))
}
