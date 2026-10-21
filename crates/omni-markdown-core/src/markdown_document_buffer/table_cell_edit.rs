//! A bounded cell's single source-replacement plan. Filling missing cells keeps
//! actual pipes, containers and EOLs, using the original canonical Undo entry.

use super::{table_source::MarkdownTableSourceIdentity, *};
use crate::markdown_viewport_presentation::{
    MarkdownPresentationStyles,
    table_window::{
        MarkdownProjectedTableWindow, MarkdownTableWindowCellSource, MarkdownTableWindowError,
        TABLE_WINDOW_MAX_BYTES, TABLE_WINDOW_MAX_COLUMNS,
    },
};

#[derive(Debug)]
/// Retain one bounded replacement and weak root identity. The native caller must
/// still admit its own current editing-intent token before committing this plan.
pub struct MarkdownTableCellEditPlan {
    pub(super) cell: VerifiedMarkdownTableCellSource,
    replacement: String,
    /// Measure actual missing-cell separators separately from explicit payload.
    /// Replacement text cannot impersonate the prefix used to activate a draft.
    pub(super) separator_prefix_bytes: usize,
}

#[derive(Clone, Debug)]
/// Editing context for the original member row and Header/End. This contains
/// neither inline-style Ready authority nor a copy of the row text.
pub(super) struct TableCellEditContext {
    pub header: Range<usize>,
    pub delimiter: Range<usize>,
    pub row_bytes: Range<usize>,
    pub row_utf16: Range<usize>,
    pub row_ordinal: usize,
    pub column: usize,
    pub columns: usize,
    pub end: Option<pulldown_cmark::block_frontier::VerifiedTableEnd>,
}

/// Only consecutive missing cells follow the last actual cell. A structural
/// trailing pipe already supplies the first separator and must not be added twice.
pub(super) fn missing_cell_separators(
    column: usize,
    first_missing: usize,
    trailing_pipe: bool,
) -> Result<usize, MarkdownTableWindowError> {
    let before = column
        .checked_sub(first_missing)
        .ok_or(MarkdownTableWindowError::ParserMismatch)?;
    Ok(before + usize::from(!trailing_pipe))
}

impl MarkdownTableCellEditPlan {
    /// Capture original column alignment before consuming the draft plan, without
    /// looking up another table from raw text or the current cursor.
    ///
    /// # Errors
    /// Reject Source failure or revoked root identity without consuming the plan
    /// or creating a draft.
    pub fn alignment_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<crate::MarkdownTableAlignment, MarkdownTableRequestError> {
        self.cell.alignment_for(document)
    }

    /// Recheck the actual root and generation before returning. After reentrant
    /// work such as NSString construction, the commit boundary must check again.
    pub fn replacement_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(&Range<usize>, &str), MarkdownTableWindowError> {
        if !self.cell.source.is_current(document) {
            // Editing, Undo and note changes revoke old integer coordinates even
            // when the successor happens to have exactly the same text length.
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        Ok((&self.cell.range, &self.replacement))
    }
}

impl MarkdownDocumentBuffer {
    /// Compose already-verified receipts inside Core. External callers cannot
    /// manufacture an edit plan from a fabricated projection with equal generation.
    /// escaped_cell_source has already escaped pipes and normalized user line
    /// breaks under the cell-input rule; this layer does not rewrite those bytes.
    pub(crate) fn plan_projected_table_cell_edit(
        &self,
        source: &MarkdownTableSourceIdentity,
        window: &MarkdownProjectedTableWindow,
        row_ordinal: usize,
        column_index: usize,
        escaped_cell_source: &str,
    ) -> Result<MarkdownTableCellEditPlan, MarkdownTableWindowError> {
        if !source.is_current(self) || window.generation != self.generation() {
            // Reject another root before reading its projected row end or deriving
            // any prefix for missing cells from that unrelated projection.
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let cell = self.projected_table_cell_source(window, row_ordinal, column_index)?;
        self.plan_table_cell_source_replacement(cell, escaped_cell_source)
    }

    /// Verified windows and edit plans share these endpoint rules. Return a
    /// structural Source only, without constructing replacement text or inline Ready.
    pub(super) fn projected_table_cell_source(
        &self,
        window: &MarkdownProjectedTableWindow,
        row_ordinal: usize,
        column_index: usize,
    ) -> Result<VerifiedMarkdownTableCellSource, MarkdownTableWindowError> {
        let row = window
            .row(row_ordinal)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let cell = window
            .cell(row_ordinal, column_index)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        if self.utf16_offset_for_byte(row.byte_range.start).ok() != Some(row.utf16_range.start)
            || self.utf16_offset_for_byte(row.byte_range.end).ok() != Some(row.utf16_range.end)
        {
            // Both byte and UTF-16 row coordinates must agree with the current
            // piece Source. Display coordinates cannot be used to replace raw text.
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let range = match &cell.source {
            MarkdownTableWindowCellSource::Present(range) => {
                if range.start < row.utf16_range.start
                    || range.start > range.end
                    || range.end > row.utf16_range.end
                    || self.byte_offset_for_utf16(range.start).is_err()
                    || self.byte_offset_for_utf16(range.end).is_err()
                {
                    // Trimming a present empty cell may leave an empty Present
                    // range. It remains an actual cell, not permission to add a column.
                    return Err(MarkdownTableWindowError::InvalidSource);
                }
                range.clone()
            }
            MarkdownTableWindowCellSource::ImplicitEmpty {
                insertion_anchor_utf16,
            } => {
                if *insertion_anchor_utf16 != row.utf16_range.end {
                    // A missing cell has only the original row-content-end anchor,
                    // no readable characters or permission to cross into the next EOL/row.
                    return Err(MarkdownTableWindowError::InvalidSource);
                }
                *insertion_anchor_utf16..*insertion_anchor_utf16
            }
        };
        let source = self
            .table_source_identity()
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let implicit_empty = matches!(
            cell.source,
            MarkdownTableWindowCellSource::ImplicitEmpty { .. }
        );
        // The bounded window and streaming Scanner each prove the same separator
        // fact when issuing a receipt. Consuming that plan need not reinterpret its
        // range. This bounded path reads only projected trailing padding; giant
        // cells already carry the Scanner's fact and are not reread in full.
        let separators = if implicit_empty {
            let insertion_anchor_utf16 = range.start;
            let cells = window
                .cells
                .get(row.cell_index_range.clone())
                .ok_or(MarkdownTableWindowError::InvalidSource)?;
            let first_missing = cells
                .iter()
                .position(|cell| {
                    matches!(
                        cell.source,
                        MarkdownTableWindowCellSource::ImplicitEmpty { .. }
                    )
                })
                .ok_or(MarkdownTableWindowError::ParserMismatch)?;
            if cells[first_missing..]
                .iter()
                .any(|cell| match &cell.source {
                    MarkdownTableWindowCellSource::ImplicitEmpty {
                        insertion_anchor_utf16: anchor,
                    } => *anchor != insertion_anchor_utf16,
                    MarkdownTableWindowCellSource::Present(_) => true,
                })
            {
                // The Parser appends only consecutive missing cells at one row
                // end. A later present cell cannot be treated as an empty insertion.
                return Err(MarkdownTableWindowError::ParserMismatch);
            }
            let tail_start = if let Some(index) = first_missing.checked_sub(1) {
                match &cells[index].source {
                    // Only after the last present cell may this tail contain the
                    // structural closing pipe or within-row padding.
                    MarkdownTableWindowCellSource::Present(range) => range.end,
                    MarkdownTableWindowCellSource::ImplicitEmpty { .. } => {
                        return Err(MarkdownTableWindowError::ParserMismatch);
                    }
                }
            } else {
                // A small row without a present predecessor still obeys this
                // bounded-window path's independent 64 KiB read limit.
                row.utf16_range.start
            };
            if tail_start < row.utf16_range.start || tail_start > insertion_anchor_utf16 {
                return Err(MarkdownTableWindowError::InvalidSource);
            }
            let tail_byte = self
                .byte_offset_for_utf16(tail_start)
                .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
            let tail = self
                .copy_table_source_range(tail_byte..row.byte_range.end, 64 * 1024)
                .map_err(|_| MarkdownTableWindowError::SourceLimit)?;
            let trimmed = tail.trim_end_matches([' ', '\t']);
            let trailing_pipe = trimmed.ends_with('|') && {
                // The character also needs the Parser's TABLE_BOUNDARY evidence.
                // An escaped literal pipe cannot close a structural column.
                let at = tail_start + trimmed.encode_utf16().count() - 1;
                let index = window.runs.partition_point(|run| run.utf16_range.end <= at);
                window.runs.get(index).is_some_and(|run| {
                    run.utf16_range.contains(&at)
                        && run
                            .styles
                            .contains(MarkdownPresentationStyles::TABLE_BOUNDARY)
                })
            };
            missing_cell_separators(column_index, first_missing, trailing_pipe)?
        } else {
            0 // A present empty range is already a cell; empty display alone never adds a separator.
        };
        Ok(VerifiedMarkdownTableCellSource {
            source,
            context: TableCellEditContext {
                header: window.header_byte_range.clone(),
                delimiter: window.delimiter_byte_range.clone(),
                row_bytes: row.byte_range.clone(),
                row_utf16: row.utf16_range.clone(),
                row_ordinal,
                column: column_index,
                columns: window.column_count,
                end: None, // The verified owner attaches End at its boundary; metadata alone cannot prove table end.
            },
            range,
            implicit_empty,
            separators,
            alignment: cell.alignment,
            inline_seed: None, // The verified window owns complete ordinary-cell semantics; this factory issues no new inline authority.
        })
    }

    /// Short windows and streamed original cells use one plan constructor. Each
    /// upstream producer supplies actual GFM endpoints, without a second filling protocol.
    pub(super) fn plan_table_cell_source_replacement(
        &self,
        cell: VerifiedMarkdownTableCellSource,
        escaped_cell_source: &str,
    ) -> Result<MarkdownTableCellEditPlan, MarkdownTableWindowError> {
        let separators = cell.separators;
        let range = &cell.range;
        let context = &cell.context;
        if !cell.source.is_current(self)
            || range.start < context.row_utf16.start
            || range.start > range.end
            || range.end > context.row_utf16.end
        {
            // Upstream already verified row/cell scalar boundaries. Check root
            // identity and row containment without rereading a whole cell for coordinates.
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        if context.columns == 0
            || context.columns > TABLE_WINDOW_MAX_COLUMNS
            || context.column >= context.columns
            || separators > context.columns
        {
            return Err(MarkdownTableWindowError::ColumnLimit);
        }
        if escaped_cell_source.len() > TABLE_WINDOW_MAX_BYTES
            || escaped_cell_source.contains(['\r', '\n'])
        {
            // This explicit-replacement entry remains bounded. Long drafts use
            // the same plan with streamed sanitization, not a larger String allowance.
            return Err(MarkdownTableWindowError::SourceLimit);
        }
        let total = escaped_cell_source
            .len()
            .saturating_add(separators.saturating_mul(3));
        if total > TABLE_WINDOW_MAX_BYTES {
            // Missing-cell separators count against the same source budget;
            // admitting payload cannot permit unbounded extra prefix growth.
            return Err(MarkdownTableWindowError::SourceLimit);
        }
        let mut replacement = String::with_capacity(total);
        for _ in 0..separators {
            // Fill the target's preceding empty columns in this one replacement,
            // using at most column_count separators and retaining a single Undo.
            replacement.push_str(" | ");
        }
        replacement.push_str(escaped_cell_source);
        Ok(MarkdownTableCellEditPlan {
            cell,
            replacement,
            separator_prefix_bytes: separators * 3,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_viewport_presentation::table_window::{
        MarkdownTableGlobalContext, MarkdownTableSourceLine, MarkdownTableWindowInput,
        project_table_window,
    };

    fn fixture(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        // Each case owns an isolated real file and its production document. The
        // source text below is finite fixture input, not a second storage adapter.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("S2-owned-cell-edit.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        (directory, document)
    }

    fn try_projected(
        source: &str,
        generation: u64,
        prefix: usize,
    ) -> Result<MarkdownProjectedTableWindow, MarkdownTableWindowError> {
        // This finite fixture can enumerate its known short lines. Production
        // global producers do not use this helper or materialize a whole line list.
        let mut byte_start = 0;
        let mut utf16_start = 0;
        let mut lines = Vec::new();
        for (source_line, text) in source.split_inclusive('\n').enumerate() {
            lines.push(MarkdownTableSourceLine {
                text,
                byte_start,
                utf16_start,
                source_line,
                row_ordinal: source_line.saturating_sub(1),
                container_prefix_bytes: prefix,
            });
            byte_start += text.len();
            utf16_start += text.encode_utf16().count();
        }
        project_table_window(&MarkdownTableWindowInput {
            generation,
            context_revision: generation,
            header: lines[0],
            delimiter: lines[1],
            body: &lines[2..],
            include_header: false,
            inherited_styles: MarkdownPresentationStyles::default(),
            container_prefix_runs: &[],
            global: MarkdownTableGlobalContext::Ready {
                reference_definitions: "",
                underline: &[],
            },
            active_selection: usize::MAX..usize::MAX,
        })
    }

    fn projected(source: &str, generation: u64, prefix: usize) -> MarkdownProjectedTableWindow {
        try_projected(source, generation, prefix)
            .unwrap_or_else(|error| panic!("source={source:?} error={error:?}"))
    }

    fn apply_and_check(row: &str, target: usize, expected_row: &str, prefix: &str, eol: &str) {
        let source = format!(
            "{prefix}| H1 | H2 | H3 | H4 |{eol}{prefix}| - | - | - | - |{eol}{prefix}{row}{eol}"
        );
        let expected = format!(
            "{prefix}| H1 | H2 | H3 | H4 |{eol}{prefix}| - | - | - | - |{eol}{prefix}{expected_row}{eol}"
        );
        let (_directory, mut document) = fixture(&source);
        let window = projected(&source, document.generation(), prefix.len());
        let identity = document.table_source_identity().unwrap();
        let plan = document
            .plan_projected_table_cell_edit(&identity, &window, 1, target, "新😀")
            .unwrap();
        let (range, replacement) = plan.replacement_for(&document).unwrap();
        let selection =
            MarkdownTextSelection::caret(range.start + replacement.encode_utf16().count());
        let range = range.clone();
        let replacement = replacement.to_owned();
        document
            .replace_utf16_range(range, &replacement, selection)
            .unwrap();
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 64 * 1024)
                .unwrap(),
            expected
        );
        let updated = projected(&expected, document.generation(), prefix.len());
        let MarkdownTableWindowCellSource::Present(range) =
            &updated.cell(1, target).unwrap().source
        else {
            panic!("已补目标仍不是真实Present cell");
        };
        let actual = document.materialize_viewport(range.clone(), 0).unwrap();
        assert_eq!(actual.text, "新😀");
        // One canonical replacement restores exact source/container/EOL bytes on
        // Undo. Returning to those bytes cannot revive the consumed generation's plan.
        assert!(document.undo().is_some());
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 64 * 1024)
                .unwrap(),
            source
        );
        assert!(plan.replacement_for(&document).is_err());
        assert!(document.redo().is_some());
        assert_eq!(
            document
                .copy_table_source_range(0..document.byte_len(), 64 * 1024)
                .unwrap(),
            expected
        );
    }

    #[test]
    fn implicit_cells_add_only_required_separators_and_preserve_atomic_history() {
        for (row, target, expected) in [
            ("a", 1, "a | 新😀"),
            ("a", 3, "a |  |  | 新😀"),
            ("|a|", 1, "|a|新😀"),
            ("|a|", 3, "|a| |  | 新😀"),
            ("|a||", 3, "|a|| | 新😀"),
            ("a\\|b", 1, "a\\|b | 新😀"),
            ("a\\\\|", 1, "a\\\\| | 新😀"),
            ("`a|b`", 3, "`a|b` |  | 新😀"),
        ] {
            // The actual pinned Parser proves missing cells and markers. Expected
            // edits do not guess membership from pipe parity or code-span heuristics.
            apply_and_check(row, target, expected, "", "\n");
        }
    }

    #[test]
    fn explicit_empty_and_crlf_quote_container_do_not_gain_a_spurious_column() {
        // A bare pipe is not a body row under the pinned Parser. A filling plan
        // cannot turn a nonmember line into another row of this table.
        let non_member = "| H1 | H2 | H3 | H4 |\n| - | - | - | - |\n|\n";
        assert_eq!(
            try_projected(non_member, 0, 0),
            Err(MarkdownTableWindowError::ParserMismatch)
        );
        apply_and_check("|a||", 1, "|a|新😀|", "", "\n");
        apply_and_check("|a|  ", 2, "|a|   | 新😀", "> ", "\r\n");
    }

    #[test]
    fn invalid_intent_source_or_non_normalized_input_preserves_bytes_and_history() {
        // Reject actual invalid input/coordinates before history mutation, then
        // invalidate a previously valid plan through an ordinary production edit.
        let source = "| H1 | H2 |\n| - | - |\n| a |\n";
        let (_directory, mut document) = fixture(source);
        let window = projected(source, document.generation(), 0);
        let identity = document.table_source_identity().unwrap();
        assert!(
            document
                .plan_projected_table_cell_edit(&identity, &window, 1, 1, "x\ny")
                .is_err()
        );
        assert!(
            document
                .plan_projected_table_cell_edit(&identity, &window, 999, 1, "x")
                .is_err()
        );
        assert!(!document.can_undo());
        let plan = document
            .plan_projected_table_cell_edit(&identity, &window, 1, 1, "x")
            .unwrap();
        document
            .insert_utf16(0, "changed", MarkdownTextSelection::caret(7))
            .unwrap();
        assert!(plan.replacement_for(&document).is_err());
        assert!(
            document
                .plan_projected_table_cell_edit(&identity, &window, 1, 1, "x")
                .is_err()
        );
    }
}
