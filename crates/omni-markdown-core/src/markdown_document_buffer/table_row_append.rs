//! Commit the actual final cell and append its next row in one canonical
//! replacement. A visible window's end cannot stand in for the whole table's End.

use std::ops::Range;

use super::{
    MarkdownDocumentBuffer, MarkdownDocumentBufferError,
    table_cell_edit::{MarkdownTableCellEditPlan, TableCellEditContext},
    table_source::MarkdownTableSourceIdentity,
    table_window_request::{VerifiedMarkdownTableViewport, VerifiedMarkdownTableWindow},
    visit_piece_tree_range,
};
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableWindowError, TABLE_WINDOW_MAX_BYTES,
};

const MAX_APPEND_SOURCE_BYTES: usize = 64 * 1024;

/// Retain bounded replacement text, absolute next-cell coordinates, and weak
/// Source identity; no window, table AST, or strong original-text owner is kept.
pub struct MarkdownTableRowAppendPlan {
    source: MarkdownTableSourceIdentity,
    range: Range<usize>,
    replacement: String,
    next_row_ordinal: usize,
}

impl std::fmt::Debug for MarkdownTableRowAppendPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Diagnostics expose identity and bounded sizes only. Formatting a plan
        // must not put a user's uncommitted cell draft into logs.
        formatter
            .debug_struct("MarkdownTableRowAppendPlan")
            .field("source", &self.source)
            .field("range", &self.range)
            .field("replacement_bytes", &self.replacement.len())
            .field("next_cell", &self.next_cell())
            .finish()
    }
}

impl MarkdownTableRowAppendPlan {
    /// Revalidate the same root and generation before commit. The native caller
    /// must also admit its own current editing intent.
    ///
    /// # Errors
    /// Editing, Undo, note changes or source release reject the old plan instead
    /// of returning stale writable coordinates.
    pub fn replacement_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(&Range<usize>, &str), MarkdownTableWindowError> {
        if !self.source.is_current(document) {
            // Generation does not revive when the same root returns, preventing
            // a second delivery of one Tab intent from appending the row twice.
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        Ok((&self.range, &self.replacement))
    }

    /// The next position is a coordinate for another actual-window query, not an
    /// already-published cell or authority to activate a new window.
    pub fn next_cell(&self) -> (usize, usize) {
        (self.next_row_ordinal, 0)
    }
}

/// Tail output derived from the same verified End. Weak identity and original
/// ranges retain no document, materialized tail string, or new commit authority.
pub(super) struct VerifiedCellRowAppendTail {
    source: MarkdownTableSourceIdentity,
    range: Range<usize>,
    tail: Range<usize>,
    prefix: Range<usize>,
    eol: &'static str,
    columns: usize,
    extra_eol: bool,
    final_eol: bool,
    byte_len: usize,
    next_row_ordinal: usize,
}

impl VerifiedCellRowAppendTail {
    /// The original End determines range and next cell. Coordinates alone cannot
    /// authorize appending to a different document.
    pub(super) fn replacement_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&Range<usize>, MarkdownTableWindowError> {
        if !self.source.is_current(document) {
            // Any document/root/generation change revokes End. The old range
            // cannot be spliced onto successor content.
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        Ok(&self.range)
    }

    /// Supply only logical row/column for a post-success query. These coordinates
    /// neither retain a window nor grant its activation authority.
    pub(super) fn next_cell(&self) -> (usize, usize) {
        (self.next_row_ordinal, 0)
    }
    /// The original ranges and fixed new-row structure determine this suffix
    /// extent, allowing document-budget admission before bytes are written.
    pub(super) fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// Deliver original ranges through 4 KiB logical pieces to the same output.
    /// Giant padding and container prefixes never become one large String here.
    pub(super) fn write_to(
        &self,
        document: &MarkdownDocumentBuffer,
        write: &mut impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        document.original_backing_health()?;
        if !self.source.is_current(document) {
            return Err(MarkdownDocumentBufferError::PreparedTextInputStale);
        }
        visit_piece_tree_range(
            document,
            document.root.as_deref(),
            self.tail.clone(),
            0,
            write,
        )?;
        if self.extra_eol {
            write(self.eol.as_bytes())?;
        } // At original EOF without a terminator, separate the new row first.
        visit_piece_tree_range(
            document,
            document.root.as_deref(),
            self.prefix.clone(),
            0,
            write,
        )?;
        for _ in 0..self.columns {
            write(b"| ")?; // Keep one actual space per cell; even a one-column row cannot be a rejected bare pipe.
        }
        write(b"|")?;
        if self.final_eol {
            write(self.eol.as_bytes())?;
        } // An originally terminated file keeps the same actual EOL after its new row.
        document.original_backing_health()?;
        if !self.source.is_current(document) {
            return Err(MarkdownDocumentBufferError::PreparedTextInputStale);
        }
        Ok(())
    }
}

impl MarkdownDocumentBuffer {
    /// Combine the current cell draft with a new empty row only after this table's
    /// actual End proves the final row/column. This method does not mutate text.
    /// escaped_cell_source has already escaped pipes and normalized line breaks
    /// under the cell-input rule; an uncommitted multiline value is not accepted.
    ///
    /// # Errors
    /// Without actual End, return ContextPending so the original draft can await
    /// further structure work. Invalid identity, coordinates or budget reject the
    /// whole plan, without half a cell write, a whole-document/table String, or
    /// promoting a partial window's last row into the table's final row.
    pub fn plan_verified_table_row_append(
        &self,
        window: &VerifiedMarkdownTableWindow,
        row_ordinal: usize,
        column_index: usize,
        escaped_cell_source: &str,
    ) -> Result<MarkdownTableRowAppendPlan, MarkdownTableWindowError> {
        let end = window
            .table_end_for(self)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?
            .ok_or(MarkdownTableWindowError::ContextPending)?;
        self.plan_table_row_append_with_end(
            window,
            end,
            row_ordinal,
            column_index,
            escaped_cell_source,
        )
    }

    /// The same batch's final row and actual End share the single replacement
    /// plan. A different request's window cannot be combined into this authority.
    pub fn plan_verified_table_viewport_row_append(
        &self,
        batch: &VerifiedMarkdownTableViewport,
        window_index: usize,
        row_ordinal: usize,
        column_index: usize,
        escaped_cell_source: &str,
    ) -> Result<MarkdownTableRowAppendPlan, MarkdownTableWindowError> {
        let window = batch
            .window_at_for(self, window_index)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let end = batch
            .required_end_for_window(self, window_index)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?
            .ok_or(MarkdownTableWindowError::ContextPending)?;
        self.plan_table_row_append_with_end(
            window,
            end,
            row_ordinal,
            column_index,
            escaped_cell_source,
        )
    }

    fn plan_table_row_append_with_end(
        &self,
        window: &VerifiedMarkdownTableWindow,
        end: &pulldown_cmark::block_frontier::VerifiedTableEnd,
        row_ordinal: usize,
        column_index: usize,
        escaped_cell_source: &str,
    ) -> Result<MarkdownTableRowAppendPlan, MarkdownTableWindowError> {
        // The bounded public entry uses the original cell plan to fill missing
        // separators. Its candidate is still one bounded source replacement.
        let cell = self.plan_verified_table_cell_edit(
            window,
            row_ordinal,
            column_index,
            escaped_cell_source,
        )?;
        let (range, replacement) = cell.replacement_for(self)?;
        self.finish_table_row_append_with_end(
            &cell.cell.context,
            end,
            range.clone(),
            replacement.to_owned(),
        )
    }

    /// The original plan already verified row, column, and missing-cell prefix.
    /// Long cells and short windows derive their tail from the same actual End.
    pub(super) fn verified_cell_row_append_tail_for_plan(
        &self,
        plan: &MarkdownTableCellEditPlan,
    ) -> Result<VerifiedCellRowAppendTail, MarkdownTableWindowError> {
        let (range, _) = plan.replacement_for(self)?;
        let end = plan
            .cell
            .context
            .end
            .as_ref()
            .ok_or(MarkdownTableWindowError::ContextPending)?;
        self.table_row_append_tail_with_end(&plan.cell.context, end, range.clone())
    }

    /// The bounded public String plan keeps its copy allowance. It and long-cell
    /// file output share the same actual table-end proof and structural rules.
    fn finish_table_row_append_with_end(
        &self,
        context: &TableCellEditContext,
        end: &pulldown_cmark::block_frontier::VerifiedTableEnd,
        cell_range: Range<usize>,
        mut replacement: String,
    ) -> Result<MarkdownTableRowAppendPlan, MarkdownTableWindowError> {
        let tail = self.table_row_append_tail_with_end(context, end, cell_range)?;
        if tail.tail.len() > MAX_APPEND_SOURCE_BYTES || tail.prefix.len() > MAX_APPEND_SOURCE_BYTES
        {
            return Err(MarkdownTableWindowError::SourceLimit); // The explicit String entry cannot copy a giant tail.
        }
        let total = replacement
            .len()
            .checked_add(tail.byte_len)
            .filter(|count| *count <= TABLE_WINDOW_MAX_BYTES)
            .ok_or(MarkdownTableWindowError::SourceLimit)?;
        replacement.reserve_exact(total - replacement.len());
        tail.write_to(self, &mut |bytes| {
            let text = std::str::from_utf8(bytes)
                .map_err(|_| MarkdownDocumentBufferError::InvalidUtf8Document)?;
            replacement.push_str(text);
            Ok(())
        })
        .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        Ok(MarkdownTableRowAppendPlan {
            source: tail.source,
            range: tail.range,
            replacement,
            next_row_ordinal: tail.next_row_ordinal,
        })
    }

    /// Verify row/column, End, container and EOL together. Return only bounded
    /// descriptors of this original Source without reading the whole tail.
    fn table_row_append_tail_with_end(
        &self,
        context: &TableCellEditContext,
        end: &pulldown_cmark::block_frontier::VerifiedTableEnd,
        cell_range: Range<usize>,
    ) -> Result<VerifiedCellRowAppendTail, MarkdownTableWindowError> {
        let row_ordinal = context.row_ordinal;
        let column_index = context.column;
        let last_row = end.last_body_row_ordinal().unwrap_or(0);
        if end.source_revision() != self.generation()
            || end.header_byte_start() != context.header.start
            || end.columns() != context.columns
            || row_ordinal != last_row
            || column_index.checked_add(1) != Some(end.columns())
        {
            // Header identity, full column count and final body ordinal must all
            // agree. A Header/Delimiter-only table permits only its last header cell.
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let last_content_end = if row_ordinal == 0 {
            // With Header/Delimiter only, insertion follows the delimiter. Keep
            // the header cell's original tail and full delimiter unchanged together.
            context.delimiter.end
        } else {
            context.row_bytes.end
        };
        let terminal_bytes = end
            .byte_end()
            .checked_sub(last_content_end)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        if terminal_bytes > 2
            || end.byte_end() > self.byte_len()
            || self.byte_offset_for_utf16(end.utf16_end()).ok() != Some(end.byte_end())
            || (!end.ended_with_eol() && !end.ended_at_source_eof())
        {
            // End must immediately follow the final actual row's EOL. This
            // contiguous replacement cannot swallow another block or omitted row.
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let prefix_range = end.continuation_prefix_byte_range();
        if prefix_range.start != context.delimiter.start
            || prefix_range.start > prefix_range.end
            || prefix_range.end > context.delimiter.end
        {
            // Borrow continuation prefix only from the actual delimiter line.
            // Repeating a header's list/footnote opener would create another container.
            return Err(MarkdownTableWindowError::InvalidSource);
        }

        // cell_range comes from the original sealed cell plan. CellDraft admission
        // also verified this same row/column and implicit-cell prefix.
        if cell_range.start > cell_range.end || cell_range.end > end.utf16_end() {
            // The underlying plan still must end before this table's End, so an
            // invalid range cannot expand into deletion across another block.
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let tail_start = self
            .byte_offset_for_utf16(cell_range.end)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        let tail_bytes = end
            .byte_end()
            .checked_sub(tail_start)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let prefix_bytes = prefix_range.end - prefix_range.start;
        let terminal = self
            .copy_table_source_range(last_content_end..end.byte_end(), 2)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        let eol = if end.ended_with_eol() {
            match terminal.as_str() {
                "\n" => "\n",
                "\r\n" => "\r\n",
                "\r" => "\r",
                _ => {
                    // The typed End's terminating scalar must match actual Source
                    // bytes; do not silently rewrite a mismatched tail.
                    return Err(MarkdownTableWindowError::InvalidSource);
                }
            }
        } else {
            if !terminal.is_empty() {
                // Preserve the unterminated EOF form. An incorrectly counted user
                // character cannot be discarded as though it were an EOL.
                return Err(MarkdownTableWindowError::InvalidSource);
            }
            // For an unterminated final row, reuse the delimiter's actual EOL.
            // If the delimiter is itself final, the header still has a separating EOL.
            match self.table_append_eol_after(context.delimiter.end)? {
                Some(eol) => eol,
                None => {
                    // Borrow the header's separator only when the delimiter truly
                    // has no EOL; do not inspect another unrelated source position.
                    self.table_append_eol_after(context.header.end)?
                        .ok_or(MarkdownTableWindowError::InvalidSource)?
                }
            }
        };
        let next_row_ordinal = last_row
            .checked_add(1)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let new_row_bytes = end
            .columns()
            .checked_mul(2)
            .and_then(|count| count.checked_add(1))
            .ok_or(MarkdownTableWindowError::SourceLimit)?;
        let extra_eol = if end.ended_with_eol() { 0 } else { eol.len() };
        let final_eol = if end.ended_with_eol() { eol.len() } else { 0 };
        // Admit the full output extent, including original padding/container
        // bytes and both possible EOL placements, before any output allocation.
        let total_bytes = tail_bytes
            .checked_add(extra_eol)
            .and_then(|count| count.checked_add(prefix_bytes))
            .and_then(|count| count.checked_add(new_row_bytes))
            .and_then(|count| count.checked_add(final_eol))
            .ok_or(MarkdownTableWindowError::SourceLimit)?;
        Ok(VerifiedCellRowAppendTail {
            source: self
                .table_source_identity()
                .ok_or(MarkdownTableWindowError::InvalidSource)?,
            range: cell_range.start..end.utf16_end(),
            tail: tail_start..end.byte_end(),
            prefix: prefix_range,
            eol,
            columns: end.columns(),
            extra_eol: !end.ended_with_eol(),
            final_eol: end.ended_with_eol(),
            byte_len: total_bytes,
            next_row_ordinal,
        })
    }

    /// Read at most two bytes to recognize the actual EOL. Return a static choice
    /// without caching a line or scanning the document's preferred newline style.
    fn table_append_eol_after(
        &self,
        byte_end: usize,
    ) -> Result<Option<&'static str>, MarkdownTableWindowError> {
        if byte_end > self.byte_len() {
            // Reject an invalid coordinate instead of saturating/clamping it into
            // a false claim that this location has no EOL.
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let first = self
            .table_source_frame(byte_end, 1)
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        let first_byte = first.bytes.first().copied();
        first
            .finish()
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        match first_byte {
            Some(b'\n') => Ok(Some("\n")),
            Some(b'\r') => {
                // Across pieces, read only one additional actual byte. Both frames
                // must finish before the EOL fact can be returned.
                let next = self
                    .table_source_frame(byte_end + 1, 1)
                    .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
                let is_lf = next.bytes.first() == Some(&b'\n');
                next.finish()
                    .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
                Ok(Some(if is_lf { "\r\n" } else { "\r" }))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_document_buffer::{
        MarkdownTableWindowStep, MarkdownTableWindowTarget, MarkdownTextSelection,
    };
    use std::sync::Arc;

    fn fixture(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        // Obtain the actual snapshot/piece root from a privately owned test file.
        // Do not fabricate a window or End proof for the operation under test.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("S2-last-cell-append.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        (directory, document)
    }

    fn window(document: &MarkdownDocumentBuffer, row: usize) -> VerifiedMarkdownTableWindow {
        let mut request = document
            .table_window_request(
                MarkdownTableWindowTarget::RequiredRow {
                    header_byte_start: 0,
                    row_ordinal: row,
                },
                0..0,
                17,
                usize::MAX..usize::MAX,
            )
            .unwrap();
        for _ in 0..document
            .byte_len()
            .saturating_mul(64)
            .saturating_add(10_000)
        {
            // The driver has finite fuel and advances the actual piece Source.
            // A projection result is not used to infer whole-table membership.
            match request.advance(document, false, 4096).unwrap() {
                MarkdownTableWindowStep::Progress { .. } => {}
                MarkdownTableWindowStep::Ready { .. } => {
                    return request.take_ready_for(document).unwrap().unwrap();
                }
                other => panic!("真实目标row没有得到完整语义window: {other:?}"),
            }
        }
        panic!("有限append fixture请求没有结束");
    }

    fn assert_history(source: &str, row: usize, draft: &str, expected: &str) {
        let (_directory, mut document) = fixture(source);
        let verified = window(&document, row);
        let columns = verified.presentation_for(&document).unwrap().column_count;
        let owners = Arc::strong_count(document.root.as_ref().unwrap());
        let plan = document
            .plan_verified_table_row_append(&verified, row, columns - 1, draft)
            .unwrap();
        assert_eq!(plan.next_cell(), (row + 1, 0));
        assert_eq!(Arc::strong_count(document.root.as_ref().unwrap()), owners);
        let (range, replacement) = plan.replacement_for(&document).unwrap();
        let range = range.clone();
        let replacement = replacement.to_owned();
        document
            .replace_utf16_range(range, &replacement, MarkdownTextSelection::caret(0))
            .unwrap();
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 65536)
                .unwrap(),
            expected,
        );
        // Cell commit plus row append is one canonical transaction. One Undo
        // restores exact original bytes without reviving the old generation's plan.
        assert!(document.undo().is_some());
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 65536)
                .unwrap(),
            source,
        );
        assert!(!document.can_undo());
        assert!(plan.replacement_for(&document).is_err());
        assert!(document.redo().is_some());
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 65536)
                .unwrap(),
            expected,
        );
        // The actual producer and pinned Parser must recognize the new empty row.
        // Expected-string equality alone does not prove its table membership.
        let new_window = window(&document, row + 1);
        let presentation = new_window.presentation_for(&document).unwrap();
        assert_eq!(presentation.column_count, columns);
        assert_eq!(presentation.row(row + 1).unwrap().row_ordinal, row + 1);
        assert_eq!(
            new_window
                .table_end_for(&document)
                .unwrap()
                .unwrap()
                .last_body_row_ordinal(),
            Some(row + 1)
        );
    }

    #[test]
    fn final_cell_commit_and_new_row_share_one_undo_and_preserve_unicode_and_following_block() {
        for (source, expected) in [
            (
                "| H1 | H2 |\n| - | - |\n| a | b |\n",
                "| H1 | H2 |\n| - | - |\n| a | 新\\|😀 |\n| | |\n",
            ),
            (
                "| H1 | H2 |\r\n| - | - |\r\n| a | b |\r\n\r\n后继段落😀\r\n",
                "| H1 | H2 |\r\n| - | - |\r\n| a | 新\\|😀 |\r\n| | |\r\n\r\n后继段落😀\r\n",
            ),
        ] {
            // The same actual edit covers LF/CRLF, escaped pipes, surrogate pairs,
            // and following document text that does not belong to the table.
            assert_history(source, 1, "新\\|😀", expected);
        }
    }

    #[test]
    fn missing_last_cell_and_single_column_rows_append_valid_gfm_members() {
        assert_history(
            "| H1 | H2 | H3 | H4 |\n| - | - | - | - |\n|a|\n",
            1,
            "新😀",
            "| H1 | H2 | H3 | H4 |\n| - | - | - | - |\n|a| |  | 新😀\n| | | | |\n",
        );
        // A one-column empty row must be `| |`; a bare pipe has no actual body-row authority.
        assert_history("| H |\n| - |\n| a |", 1, "中", "| H |\n| - |\n| 中 |\n| |");
    }

    #[test]
    fn continuation_prefix_and_original_eof_eol_are_preserved_for_body_and_header_only_tables() {
        for (source, row, expected) in [
            (
                "> > | H1 | H2 |\r\n> > | - | - |\r\n> > | a | b |",
                1,
                "> > | H1 | H2 |\r\n> > | - | - |\r\n> > | a | 新😀 |\r\n> > | | |",
            ),
            (
                "3. | H1 | H2 |\r\n   | - | - |",
                0,
                "3. | H1 | 新😀 |\r\n   | - | - |\r\n   | | |",
            ),
            (
                "| H1 | H2 |\n| - | - |\n",
                0,
                "| H1 | 新😀 |\n| - | - |\n| | |\n",
            ),
        ] {
            // Delimiter continuation bytes preserve the original quote/list;
            // copying the header opener would incorrectly start another block.
            assert_history(source, row, "新😀", expected);
        }
    }

    #[test]
    fn partial_window_or_nonfinal_coordinate_preserves_source_and_draft() {
        // A partial actual window lacks End. Even a final window cannot append
        // from a nonfinal column; both rejections preserve real source and history.
        let source = "| H1 | H2 |\n| - | - |\n| one | x |\n| two | y |\n";
        let (_directory, document) = fixture(source);
        let partial = window(&document, 1);
        assert!(partial.table_end_for(&document).unwrap().is_none());
        assert!(matches!(
            document.plan_verified_table_row_append(&partial, 1, 1, "draft"),
            Err(MarkdownTableWindowError::ContextPending),
        ));
        let final_window = window(&document, 2);
        assert!(matches!(
            document.plan_verified_table_row_append(&final_window, 2, 0, "draft"),
            Err(MarkdownTableWindowError::InvalidSource),
        ));
        assert_eq!(
            document
                .copy_table_source_range(0..source.len(), 65536)
                .unwrap(),
            source
        );
        assert!(!document.can_undo());
    }

    #[test]
    fn plan_rejects_other_root_and_old_generation_and_does_not_keep_document_alive() {
        let source = "| H1 | H2 |\n| - | - |\n| a | b |\n";
        let (_first_directory, mut document) = fixture(source);
        let (_other_directory, other) = fixture(source);
        let verified = window(&document, 1);
        let plan = document
            .plan_verified_table_row_append(&verified, 1, 1, "draft")
            .unwrap();
        let weak = Arc::downgrade(document.root.as_ref().unwrap());
        assert!(plan.replacement_for(&other).is_err());
        document
            .insert_utf16(0, "改", MarkdownTextSelection::caret(1))
            .unwrap();
        assert!(plan.replacement_for(&document).is_err());
        document.undo().unwrap();
        assert!(plan.replacement_for(&document).is_err());
        assert!(
            document
                .plan_verified_table_row_append(&verified, 1, 1, "draft")
                .is_err()
        );
        drop(verified);
        drop(document);
        // The plan and old window have only weak identity. Restoring equal content
        // or dropping the document must not leave snapshot/history strong owners.
        assert_eq!(weak.strong_count(), 0);
    }

    #[test]
    fn five_thousand_rows_only_copy_last_cell_tail_and_reject_oversized_draft_before_mutation() {
        // Generate finite fixture data, then ask the actual producer for only the
        // final row. Planning must not copy preceding rows or mutate on a budget error.
        let mut source = String::from("| H1 | H2 |\n| - | - |\n");
        for row in 1..=5000 {
            source.push_str(&format!("| ROW{row} | 中😀 |\n"));
        }
        let (_directory, document) = fixture(&source);
        let verified = window(&document, 5000);
        assert_eq!(verified.presentation_for(&document).unwrap().rows.len(), 1);
        let plan = document
            .plan_verified_table_row_append(&verified, 5000, 1, "新😀")
            .unwrap();
        let (range, replacement) = plan.replacement_for(&document).unwrap();
        assert!(range.start > document.utf16_len() - 16);
        assert!(replacement.len() < 32);
        assert_eq!(plan.next_cell(), (5001, 0));
        let oversized = "x".repeat(TABLE_WINDOW_MAX_BYTES + 1);
        assert!(matches!(
            document.plan_verified_table_row_append(&verified, 5000, 1, &oversized),
            Err(MarkdownTableWindowError::SourceLimit),
        ));
        assert_eq!(document.byte_len(), source.len());
        assert!(!document.can_undo());
    }
}
