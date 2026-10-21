//! Cell-local inline formatting appends only the existing domain markers. Publish
//! the draft root and local history once, after its private candidate is complete.

use super::edit::{DraftSource, validate_positions};
use super::*;

impl PendingCellDraft {
    fn insert_format_marker(
        &mut self,
        document: &MarkdownDocumentBuffer,
        root: PieceTreeLink,
        byte: usize,
        marker: &'static str,
    ) -> Result<PieceTreeLink, MarkdownDocumentBufferError> {
        let added = self
            .edits
            .append(marker, MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES)?;
        let source = DraftSource {
            document,
            edits: &self.edits,
            scope: &self.scope,
            serial: self.serial,
        };
        // Split only the original endpoint path. Selected text stays shared in
        // its actual pieces, without scanning or copying the complete cell.
        let (prefix, suffix) = split_piece_tree(&source, root, byte)?;
        Ok(join_piece_trees(
            prefix,
            join_piece_trees(build_balanced_piece_tree(added), suffix),
        ))
    }

    fn format(
        &mut self,
        document: &MarkdownDocumentBuffer,
        action: MarkdownFormatAction,
    ) -> Result<(MarkdownCellDraftFacts, MarkdownHistoryTextEdit), MarkdownCellDraftError> {
        self.require(document)?;
        if self.marked_range.is_some() {
            return Err(MarkdownCellDraftError::CompositionActive);
        } // Marked text still belongs to its original IME; Core cannot guess confirmation or replace input authority.
        let (opening, closing, placeholder) = action
            .inline_markers()
            .ok_or(MarkdownCellDraftError::FormatRequiresDocument)?;
        let range = self.selection.anchor_utf16.min(self.selection.head_utf16)
            ..self.selection.anchor_utf16.max(self.selection.head_utf16);
        let placeholder = range.is_empty().then_some(placeholder);
        let opening_units = opening.encode_utf16().count();
        let growth = opening_units
            .checked_add(closing.encode_utf16().count())
            .and_then(|units| {
                units.checked_add(placeholder.map_or(0, |value| value.encode_utf16().count()))
            })
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        let after = MarkdownTextSelection {
            anchor_utf16: range
                .start
                .checked_add(opening_units)
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?,
            head_utf16: range
                .end
                .checked_add(opening_units)
                .and_then(|units| {
                    units.checked_add(placeholder.map_or(0, |value| value.encode_utf16().count()))
                })
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?,
        }; // Surround selects the forward inner range; Undo still retains the actual original directed selection.
        let replacement = MarkdownReplacementMetrics::new(
            range.start,
            range.len(),
            range
                .len()
                .checked_add(growth)
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?,
        )?;
        let next_serial = self
            .serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        let next_content_serial = self
            .content_serial
            .checked_add(1)
            .ok_or(MarkdownCellDraftError::Stale)?;
        let source = DraftSource {
            document,
            edits: &self.edits,
            scope: &self.scope,
            serial: self.serial,
        };
        let start = byte_offset_for_utf16_in_tree(&source, self.root.as_deref(), range.start, 0)?;
        let end = byte_offset_for_utf16_in_tree(&source, self.root.as_deref(), range.end, 0)?;
        let group = self.history.plan_group(history::grouping::GroupChange {
            kind: Some(MarkdownCellDraftInputKind::Isolated),
            serial: self.serial,
            next_serial,
            selection_before: self.selection,
            marked_before: &self.marked_range,
            selection_after: after,
            marked_after: &None,
            replacement,
        })?;
        self.history.reserve_group(&group)?; // Reserve local history first; formatting never merges into previous typing/marked input.
        let checkpoint = command_transaction::AppendCheckpoint::capture_segments(&self.edits);
        let result = (|| {
            // Insert backward to preserve original byte positions. Any failure
            // discards the candidate without one-sided markers or an intermediate serial.
            let mut root = self.insert_format_marker(document, self.root.clone(), end, closing)?;
            if let Some(placeholder) = placeholder {
                root = self.insert_format_marker(document, root, start, placeholder)?;
            } // Only an empty caret gets static placeholder text; nonempty cell selection keeps actual Unicode/EOL bytes.
            let root = self.insert_format_marker(document, root, start, opening)?;
            let maximum_descriptors = self
                .base_descriptors
                .saturating_add(
                    MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES.div_ceil(EDIT_SEGMENT_BYTES - 3),
                )
                .saturating_add(1);
            if self.edits.segments.len() > maximum_descriptors {
                return Err(MarkdownDocumentBufferError::EditBufferLimitExceeded);
            } // Use replacement's same cumulative delta/descriptor limits; deletion or formatting cannot renew allowance.
            let source = DraftSource {
                document,
                edits: &self.edits,
                scope: &self.scope,
                serial: self.serial,
            };
            validate_positions(&source, &root, after, &None)?;
            source.source_health()?;
            Ok(root)
        })();
        let root = match result {
            Ok(root) => root,
            Err(error) => {
                // The failed candidate has actually dropped before its marker
                // appends are rolled back. Keep root, selection, marked state,
                // history and existing read authority unchanged.
                checkpoint.rollback_segments(&mut self.edits);
                self.scope.record_source(error);
                return Err(error.into());
            }
        };
        self.history
            .record_group(group, self.root.take(), self.selection);
        self.edits.publish(); // Transfer new-segment Source ownership only at this transaction's infallible publication point.
        self.root = root;
        self.selection = after;
        self.serial = next_serial;
        self.content_serial = next_content_serial;
        self.scope.publish_delta(self.edits.used_bytes);
        self.scope
            .content_serial
            .store(next_content_serial, Ordering::Release);
        self.scope.serial.store(next_serial, Ordering::Release);
        // Publication from record_group onward cannot fail. A native caller must
        // not roll back this completed format operation using the now-old receipt.
        Ok((self.facts(), replacement.redo_text_edit(after)))
    }
}

impl MarkdownDocumentBuffer {
    /// Consume the original cell-edit capsule for one inline format action. Append
    /// static markers only, without materializing selection or editing canonical text.
    ///
    /// # Errors
    /// Source/authority failure, active marked text, a document-block action, bad
    /// endpoint or append-budget failure preserve the draft and its local history.
    pub fn format_cell_draft(
        &mut self,
        edit: MarkdownCellDraftEditCapsule,
        action: MarkdownFormatAction,
    ) -> Result<(MarkdownCellDraftFacts, MarkdownHistoryTextEdit), MarkdownCellDraftError> {
        self.require_cell_draft_edit(&edit)?;
        let Some(CellDraftSlot::Active(mut pending)) = self.cell_draft.take() else {
            unreachable!("已核原格式票的唯一草稿");
        };
        // This pure Rust candidate temporarily takes the original slot. No native
        // callback, second Source, or canonical transaction runs during the operation.
        let result = pending.format(self, action);
        self.cell_draft = Some(CellDraftSlot::Active(pending));
        result
    }
}
