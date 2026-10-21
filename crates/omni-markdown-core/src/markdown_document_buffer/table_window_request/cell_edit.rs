//! Prepare one cell edit from its original structural receipt. Classification may
//! traverse preceding source and the complete target row, but actual reads remain
//! bounded to 4 KiB frames; no whole-row projection or inline Ready is constructed.

use super::super::table_cell_edit::{
    MarkdownTableCellEditPlan, TableCellEditContext, missing_cell_separators,
};
use super::*;
use pulldown_cmark::block_frontier::{TableCellSemanticScanner, TableDelimiterScanner};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Ready verifies original structural endpoints only. One consumer takes either
/// an edit plan or read Source; this grants no preview styles or native mounting.
pub enum MarkdownTableCellEditStep {
    Progress { work: SemanticWork },
    Ready { work: SemanticWork },
    NotTable { work: SemanticWork },
}

/// Own only the original structural continuation, bounded coordinates and one
/// lexical cursor, without retaining the document, mappings or text.
pub struct MarkdownTableCellEditRequest {
    source: MarkdownTableSourceIdentity,
    structure: Option<MarkdownTableWindowStructureRequest>,
    ticket: Option<MarkdownTableStructuralWindow>,
    context: Option<TableCellEditContext>,
    scanner: Option<TableCellSemanticScanner>,
    // Stream the same actual delimiter for this column only. Alignment needs no
    // complete-line String or Vec of all column alignments.
    delimiter: Option<TableDelimiterScanner>,
    delimiter_next: usize,
    delimiter_end: usize,
    alignment: Option<crate::MarkdownTableAlignment>,
    next: usize,
    content_start: usize,
    utf16: usize,
    trimmed_utf16: Range<usize>,
    selected: Option<Range<usize>>,
    row: usize,
    column: usize,
    ready: Option<VerifiedMarkdownTableCellSource>,
    finished: bool,
    failed: Option<MarkdownTableRequestError>,
}

impl MarkdownDocumentBuffer {
    /// Prepare the original cell from actual header identity and absolute row/column.
    /// Request coordinates cannot bypass the structural producer's membership/End proof.
    pub fn table_cell_edit_request(
        &self,
        header_byte_start: usize,
        row_ordinal: usize,
        column: usize,
    ) -> Result<MarkdownTableCellEditRequest, MarkdownTableRequestError> {
        self.original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        let structure = self.table_window_structure_request(
            MarkdownTableWindowTarget::RequiredRow {
                header_byte_start,
                row_ordinal,
            },
            0..0,
        )?;
        Ok(MarkdownTableCellEditRequest {
            source: structure.source.clone(),
            structure: Some(structure),
            ticket: None,
            context: None,
            scanner: None,
            delimiter: None,
            delimiter_next: 0,
            delimiter_end: 0,
            alignment: None,
            next: 0,
            content_start: 0,
            utf16: 0,
            trimmed_utf16: 0..0,
            selected: None,
            row: row_ordinal,
            column,
            ready: None,
            finished: false,
            failed: None,
        })
    }
}

impl MarkdownTableCellEditRequest {
    /// Transfer the original RequiredRow replay receipt without creating another
    /// structural Global. All later scanning uses this same implementation.
    pub(super) fn from_ticket(
        ticket: MarkdownTableStructuralWindow,
        row: usize,
        column: usize,
    ) -> Self {
        Self {
            source: ticket.source.clone(),
            structure: None,
            ticket: Some(ticket),
            context: None,
            scanner: None,
            delimiter: None,
            delimiter_next: 0,
            delimiter_end: 0,
            alignment: None,
            next: 0,
            content_start: 0,
            utf16: 0,
            trimmed_utf16: 0..0,
            selected: None,
            row,
            column,
            ready: None,
            finished: false,
            failed: None,
        }
    }
    /// Each structural/cell step borrows at most 4096 logical bytes. Cancellation
    /// or revocation discards the candidate, without publishing partial edit authority.
    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
        budget: usize,
    ) -> Result<MarkdownTableCellEditStep, MarkdownTableRequestError> {
        let result = (|| {
            document
                .original_backing_health()
                .map_err(MarkdownTableRequestError::Source)?;
            if let Some(error) = self.failed {
                return Err(error);
            }
            if cancelled {
                return Err(MarkdownTableRequestError::Cancelled);
            }
            if !self.source.is_current(document) {
                return Err(MarkdownTableRequestError::SourceSuperseded);
            }
            if !(4096..=65536).contains(&budget) {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            self.advance_inner(document, budget.min(4096))
        })();
        match result {
            Ok(step) => Ok(step),
            Err(cause) => {
                // A child coordinate error cannot replace actual Source failure.
                // Drop all text-free continuation state and preserve the observed cause.
                let error = document
                    .original_backing_health()
                    .err()
                    .map(MarkdownTableRequestError::Source)
                    .unwrap_or(cause);
                self.failed = Some(error);
                self.structure = None;
                self.ticket = None;
                self.context = None;
                self.scanner = None;
                self.delimiter = None;
                self.selected = None;
                self.ready = None;
                Err(error)
            }
        }
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableCellEditStep, MarkdownTableRequestError> {
        if self.finished {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if self.ready.is_some() {
            return Ok(MarkdownTableCellEditStep::Ready {
                work: SemanticWork::default(),
            });
        }
        if let Some(structure) = self.structure.as_mut() {
            let step = structure.advance(document, false, budget)?;
            let measured = match step {
                MarkdownTableStructureStep::Progress { work }
                | MarkdownTableStructureStep::StructureReady { work }
                | MarkdownTableStructureStep::NotTable { work }
                | MarkdownTableStructureStep::TableEndReached { work } => work,
            };
            let work = SemanticWork {
                source_bytes: measured.byte_reads,
                source_tree_steps: measured.source_tree_steps,
                structural_transitions: measured.transitions,
                ..SemanticWork::default()
            };
            match step {
                MarkdownTableStructureStep::Progress { .. } => {}
                MarkdownTableStructureStep::StructureReady { .. } => {
                    self.ticket = structure.ready.take();
                    self.structure = None;
                }
                MarkdownTableStructureStep::NotTable { .. }
                | MarkdownTableStructureStep::TableEndReached { .. } => {
                    // End this request when the required cell does not exist.
                    // A real table End cannot turn missing-row coordinates into a target row.
                    self.structure = None;
                    self.finished = true;
                    return Ok(MarkdownTableCellEditStep::NotTable { work });
                }
            }
            return Ok(MarkdownTableCellEditStep::Progress { work });
        }
        if self.context.is_none() {
            let ticket = self
                .ticket
                .as_ref()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            let header = ticket.header;
            if self.column >= header.columns {
                return Err(MarkdownTableRequestError::Window(
                    MarkdownTableWindowError::ColumnLimit,
                ));
            }
            let (start, end, prefix, utf16) = if self.row == 0 {
                (
                    header.header_byte_start,
                    header.header_byte_end,
                    header.header_prefix_bytes,
                    ticket.header_utf16.clone(),
                )
            } else {
                let row = ticket
                    .body
                    .iter()
                    .find(|row| row.row_ordinal == self.row)
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                (
                    row.byte_start,
                    row.byte_end,
                    row.container_prefix_bytes,
                    row.utf16_start..row.utf16_end,
                )
            };
            let (header_end, header_reads) =
                line_content_end(document, header.header_byte_start..header.header_byte_end)?;
            let (delimiter_end, delimiter_reads) = line_content_end(
                document,
                header.separator_byte_start..header.separator_byte_end,
            )?;
            let (content_end, row_reads) = line_content_end(document, start..end)?;
            self.next = start;
            self.content_start = start + prefix;
            self.utf16 = utf16.start;
            let inline_row = if self.row == 0 {
                ticket.checkpoint.header_inline_row()
            } else {
                ticket
                    .inline_rows
                    .iter()
                    .find(|row| row.row_ordinal() == self.row)
                    .cloned()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?
            };
            // Math display is disabled. The vendor's original cell API receives
            // neutral context; actual pipe/escape grammar still owns cell boundaries.
            self.scanner = Some(
                inline_row
                    .cell_scanner(
                        self.column,
                        pulldown_cmark::math_context::MathContext::default(),
                    )
                    .map_err(|cause| MarkdownTableRequestError::Global(cause.into()))?,
            );
            self.context = Some(TableCellEditContext {
                header: header.header_byte_start..header_end,
                delimiter: header.separator_byte_start..delimiter_end,
                row_bytes: start..content_end,
                row_utf16: utf16.start..utf16.end - (end - content_end),
                row_ordinal: self.row,
                column: self.column,
                columns: header.columns,
                end: ticket.table_end.clone(),
            });
            // Original Global verified the delimiter's actual container prefix.
            // Skip it directly without having Native recognize quote/list syntax again.
            self.delimiter_next = header.separator_byte_start + header.separator_prefix_bytes;
            self.delimiter_end = delimiter_end;
            self.delimiter = Some(TableDelimiterScanner::new());
            self.ticket = None; // Identity and required End moved into this plan context; retain no complete window metadata.
            return Ok(MarkdownTableCellEditStep::Progress {
                work: SemanticWork {
                    source_bytes: header_reads + delimiter_reads + row_reads,
                    structural_transitions: 64,
                    ..SemanticWork::default()
                },
            });
        }
        if let Some(delimiter) = self.delimiter.as_mut() {
            // Use the same delimiter conversion as the original LineReader,
            // accounting for actual bytes, directory descent and state work.
            // Global/structural evidence already proved the full line and column
            // count, so stop at this column's close without rescanning later columns.
            let context = self.context.as_ref().unwrap();
            if self.delimiter_next < self.delimiter_end {
                let tree = tree_height(document);
                let count =
                    ((budget - tree - 64) / 4).min(self.delimiter_end - self.delimiter_next);
                let frame = document
                    .table_source_frame(self.delimiter_next, count)
                    .map_err(MarkdownTableRequestError::Source)?;
                let available = frame.bytes.len().min(count);
                if available == 0 {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                let mut used = 0;
                let mut completed = false;
                for byte in &frame.bytes[..available] {
                    used += 1;
                    if let Some((column, alignment)) = delimiter.push(*byte)
                        && column == context.column
                    {
                        self.alignment = Some(table_alignment(alignment));
                        completed = true;
                        break;
                    }
                }
                // Count bytes actually consumed through the target pipe, not an
                // unscanned frame suffix. Finish still returns typed Source failure;
                // finding alignment cannot conceal failed lease reclamation.
                self.delimiter_next += used;
                frame.finish().map_err(MarkdownTableRequestError::Source)?;
                if completed {
                    self.delimiter = None;
                }
                return Ok(MarkdownTableCellEditStep::Progress {
                    work: SemanticWork {
                        source_bytes: used,
                        source_tree_steps: tree,
                        structural_transitions: used * 3 + 64,
                        ..SemanticWork::default()
                    },
                });
            }
            if let Some((column, alignment)) = delimiter.final_column()
                && column == context.column
            {
                self.alignment = Some(table_alignment(alignment));
            }
            // Without a trailing pipe, the actual final column closes only at
            // content_end. Unknown/missing alignment cannot default to left.
            if delimiter.columns() != Some(context.columns) || self.alignment.is_none() {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            self.delimiter = None;
            return Ok(MarkdownTableCellEditStep::Progress {
                work: SemanticWork {
                    structural_transitions: 64,
                    ..SemanticWork::default()
                },
            });
        }
        let context = self.context.as_ref().unwrap();
        let scanner = self.scanner.as_mut().unwrap();
        let tree = tree_height(document);
        if self.next < context.row_bytes.end {
            // Charge per-byte conversion and original-tree location together.
            // A bounded borrow is not a claim that a whole cell was scanned for free.
            let count = ((budget - tree - 64) / 6).min(context.row_bytes.end - self.next);
            let frame = document
                .table_source_frame(self.next, count)
                .map_err(MarkdownTableRequestError::Source)?;
            let used = frame.bytes.len().min(count);
            if used == 0 {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            for (offset, byte) in frame.bytes[..used].iter().enumerate() {
                let at = self.next + offset;
                let before = self.utf16;
                // Immutable/canonical construction already guarantees UTF-8.
                // Count actual leading bytes; continuations crossing frame
                // boundaries cannot contribute the same UTF-16 scalar twice.
                self.utf16 += match byte {
                    0..=0x7f | 0xc2..=0xef => 1,
                    0xf0..=0xf4 => 2,
                    _ => 0,
                };
                if at < self.content_start {
                    continue;
                } // Container bytes affect original coordinates, but do not enter cell grammar.
                if at == self.content_start {
                    self.trimmed_utf16 = before..before;
                }
                let prior_trimmed = self.trimmed_utf16.clone();
                if scanner.push(*byte).is_some() && scanner.cell_count() == self.column + 1 {
                    self.selected = Some(prior_trimmed); // Keep actual trimmed endpoints before the closing pipe, not the next cell's leading pipe.
                }
                let trimmed = scanner.trimmed_range();
                if trimmed.start == at {
                    self.trimmed_utf16.start = before;
                } else if trimmed.start == at + 1 {
                    self.trimmed_utf16.start = self.utf16;
                }
                if trimmed.end == at {
                    self.trimmed_utf16.end = before;
                } else if trimmed.end == at + 1 {
                    self.trimmed_utf16.end = self.utf16;
                }
            }
            self.next += used;
            frame.finish().map_err(MarkdownTableRequestError::Source)?;
            return Ok(MarkdownTableCellEditStep::Progress {
                work: SemanticWork {
                    source_bytes: used,
                    source_tree_steps: tree,
                    structural_transitions: used * 5 + 64, // Vendor cell scanning still handles finite common state even with display extensions disabled.
                    ..SemanticWork::default()
                },
            });
        }
        if scanner.finish().is_some() && scanner.cell_count() == self.column + 1 {
            self.selected = Some(self.trimmed_utf16.clone());
        }
        let (range, separators, implicit_empty) = match self.selected.take() {
            Some(range) => (range, 0, false),
            None => {
                // Original grammar proved this target is among consecutive missing
                // cells. Add only separators required after its actual trailing pipe.
                let separators = missing_cell_separators(
                    self.column,
                    scanner.cell_count(),
                    scanner.trailing_pipe(),
                )
                .map_err(MarkdownTableRequestError::Window)?;
                (
                    context.row_utf16.end..context.row_utf16.end,
                    separators,
                    true,
                )
            }
        };
        // Editing and read-only layout consume these same structural endpoints.
        // This creates neither replacement text nor inline-style authority.
        self.ready = Some(VerifiedMarkdownTableCellSource {
            source: self.source.clone(),
            context: self.context.take().unwrap(),
            range,
            implicit_empty,
            separators,
            alignment: self.alignment.expect("原 separator 完整结束后才签发格票"),
            // Transfer Scanner-proven separator facts with their endpoints, not
            // as an unverified pair of offsets. The seed contains pure values and
            // original Arc/atomic scope pages, allowing actual layout workers to
            // release these shared pages across threads without retaining text bytes.
            inline_seed: scanner.take_semantic_seed().map(std::sync::Arc::new),
        });
        self.scanner = None;
        Ok(MarkdownTableCellEditStep::Ready {
            work: SemanticWork {
                structural_transitions: 64,
                ..SemanticWork::default()
            },
        })
    }

    /// Only one consumer receives these original endpoints. Editing uses the
    /// existing replacement rule; raw Source never implies preview Ready.
    pub fn take_plan_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<MarkdownTableCellEditPlan>, MarkdownTableRequestError> {
        self.take_raw_cell_for(document)?
            .map(|cell| document.plan_verified_table_cell_draft(cell))
            .transpose()
    }

    /// Giant cells transfer only Scanner-verified raw Source too, without occupying
    /// a draft slot, copying the source row, or granting inline authority.
    pub fn take_cell_source_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<VerifiedMarkdownTableCellSource>, MarkdownTableRequestError> {
        self.take_raw_cell_for(document)
    }

    /// Share the request's single consumption right. Actual health precedes stored
    /// failure/generation checks; old integer coordinates cannot issue another cell.
    fn take_raw_cell_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<VerifiedMarkdownTableCellSource>, MarkdownTableRequestError> {
        document
            .original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        if let Some(error) = self.failed {
            return Err(error);
        }
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        let cell = self.ready.take();
        if cell.is_some() {
            self.finished = true;
        }
        Ok(cell)
    }
}

/// The original Parser's default and explicit left alignment have the same layout
/// meaning. Map enum values only, without reinterpreting source bytes.
fn table_alignment(alignment: pulldown_cmark::Alignment) -> crate::MarkdownTableAlignment {
    match alignment {
        pulldown_cmark::Alignment::None | pulldown_cmark::Alignment::Left => {
            crate::MarkdownTableAlignment::Left
        }
        pulldown_cmark::Alignment::Center => crate::MarkdownTableAlignment::Center,
        pulldown_cmark::Alignment::Right => crate::MarkdownTableAlignment::Right,
    }
}

/// Confirm EOL from one or two actual trailing bytes. The suffix need not itself
/// be a UTF-8 string, and the complete row is never copied.
fn line_content_end(
    document: &MarkdownDocumentBuffer,
    range: Range<usize>,
) -> Result<(usize, usize), MarkdownTableRequestError> {
    let mut end = range.end;
    let mut reads = 0;
    if range.is_empty() {
        return Ok((end, reads));
    }
    let frame = document
        .table_source_frame(end - 1, 1)
        .map_err(MarkdownTableRequestError::Source)?;
    let last = frame.bytes[0];
    reads += 1;
    frame.finish().map_err(MarkdownTableRequestError::Source)?;
    if matches!(last, b'\r' | b'\n') {
        end -= 1;
    }
    if last == b'\n' && end > range.start {
        let frame = document
            .table_source_frame(end - 1, 1)
            .map_err(MarkdownTableRequestError::Source)?;
        if frame.bytes[0] == b'\r' {
            end -= 1;
        }
        reads += 1;
        frame.finish().map_err(MarkdownTableRequestError::Source)?;
    }
    Ok((end, reads))
}
