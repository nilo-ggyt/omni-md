//! Authority for independent table flow rows. Rows come from their original
//! publication, with physical line endings verified in the same frozen source.

use std::rc::Rc;

use super::*;
use crate::MarkdownGraphemeBoundary;
use crate::markdown_document_buffer::{
    MarkdownTableRequestError, MarkdownViewportSourceAuthority, VerifiedMarkdownTableRowSource,
    VerifiedMarkdownViewportSources,
};

/// One original physical row becomes one table-flow element. A header also
/// consumes its same-source delimiter, which must not render as an extra empty row.
#[derive(Clone)]
pub struct VerifiedMarkdownLayoutTableRow(Rc<Row>);

struct Row {
    source: MarkdownLayoutTextAuthority,
    publication: RowPublication,
    authority: MarkdownViewportSourceAuthority,
    header: usize,
    ordinal: usize,
    columns: usize,
    start: MarkdownGraphemeBoundary,
    end: MarkdownGraphemeBoundary,
    tail: MarkdownGraphemeBoundary,
    eof: bool,
}

enum RowPublication {
    Window {
        publication: Rc<VerifiedMarkdownViewportSources>,
        window: usize,
    },
    Structural(VerifiedMarkdownTableRowSource),
}

impl MarkdownLayoutTextSource {
    /// Consumes the opaque structural row produced before any whole-row copy. Its
    /// authority pauses flow only; each cell still completes its own inline request.
    pub fn verified_table_layout_row_from_source(
        &self,
        document: &MarkdownDocumentBuffer,
        row: VerifiedMarkdownTableRowSource,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<VerifiedMarkdownLayoutTableRow, MarkdownTableRequestError> {
        self.health().map_err(MarkdownTableRequestError::Source)?;
        if self.domain() != MarkdownLayoutTextSourceDomain::Canonical
            || !self.0.snapshot.shares_current_table_root(document)
        {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        row.require_for(document, authority)?;
        let utf16 = row.physical_utf16_range_for(document, authority)?;
        let bytes = row.physical_byte_range_for(document, authority)?;
        let (header, ordinal, columns) = row.coordinate_for(document, authority)?;
        // The physical receipt already includes complete EOL bytes. Read at most
        // two actual units to preserve real trailing context for the Unicode DFA.
        let mut tail = utf16.end;
        let snapshot = &self.0.snapshot;
        if tail > utf16.start {
            let last = snapshot
                .utf16_code_unit_at(tail - 1)
                .map_err(MarkdownTableRequestError::Source)?;
            if matches!(last, 10 | 13) {
                tail -= 1;
                if last == 10
                    && tail > utf16.start
                    && snapshot
                        .utf16_code_unit_at(tail - 1)
                        .map_err(MarkdownTableRequestError::Source)?
                        == 13
                {
                    tail -= 1;
                }
            }
        }
        let tail_byte = self
            .byte_offset_for_utf16(tail)
            .map_err(MarkdownTableRequestError::Source)?;
        row.require_for(document, authority)?;
        self.health().map_err(MarkdownTableRequestError::Source)?;
        Ok(VerifiedMarkdownLayoutTableRow(Rc::new(Row {
            source: self.authority(),
            publication: RowPublication::Structural(row),
            authority,
            header,
            ordinal,
            columns,
            start: MarkdownGraphemeBoundary {
                byte: bytes.start,
                utf16: utf16.start,
            },
            end: MarkdownGraphemeBoundary {
                byte: bytes.end,
                utf16: utf16.end,
            },
            tail: MarkdownGraphemeBoundary {
                byte: tail_byte,
                utf16: tail,
            },
            eof: utf16.end == self.utf16_len(),
        })))
    }
    /// Only a row published by the original window has flow identity. Raw ranges,
    /// another equal-text source, or header context cannot manufacture a row.
    pub fn verified_table_layout_row(
        &self,
        document: &MarkdownDocumentBuffer,
        publication: Rc<VerifiedMarkdownViewportSources>,
        authority: MarkdownViewportSourceAuthority,
        window: usize,
        ordinal: usize,
    ) -> Result<VerifiedMarkdownLayoutTableRow, MarkdownTableRequestError> {
        self.health().map_err(MarkdownTableRequestError::Source)?;
        if self.domain() != MarkdownLayoutTextSourceDomain::Canonical
            || !self.0.snapshot.shares_current_table_root(document)
        {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        let tables = publication.table_sources_for(document, authority)?;
        let verified = tables
            .window_at_for(document, window)?
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let table = verified.presentation_for(document)?;
        let row = table
            .row(ordinal)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let start = row.utf16_range.start;
        let content_end = if row.is_header {
            table.delimiter_utf16_range.end
        } else {
            row.utf16_range.end
        };
        let snapshot = &self.0.snapshot;
        if start != 0 {
            let previous = snapshot
                .utf16_code_unit_at(start - 1)
                .map_err(MarkdownTableRequestError::Source)?;
            if !matches!(previous, 10 | 13)
                || previous == 13
                    && snapshot
                        .utf16_code_unit_at(start)
                        .map_err(MarkdownTableRequestError::Source)?
                        == 10
            {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
        }
        // The content receipt excludes CR/LF. Read at most two actual units;
        // a CRLF must never split across two flow elements.
        let mut end = content_end;
        if end < self.utf16_len() {
            let first = snapshot
                .utf16_code_unit_at(end)
                .map_err(MarkdownTableRequestError::Source)?;
            if !matches!(first, 10 | 13) {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            end += 1;
            if first == 13
                && end < self.utf16_len()
                && snapshot
                    .utf16_code_unit_at(end)
                    .map_err(MarkdownTableRequestError::Source)?
                    == 10
            {
                end += 1;
            }
        }
        let position = |utf16| -> Result<MarkdownGraphemeBoundary, MarkdownTableRequestError> {
            Ok(MarkdownGraphemeBoundary {
                byte: self
                    .byte_offset_for_utf16(utf16)
                    .map_err(MarkdownTableRequestError::Source)?,
                utf16,
            })
        };
        let row = Row {
            source: self.authority(),
            publication: RowPublication::Window {
                publication: publication.clone(),
                window,
            },
            authority,
            header: table.header_byte_range.start,
            ordinal,
            columns: table.column_count,
            start: position(start)?,
            end: position(end)?,
            tail: position(content_end)?,
            eof: end == self.utf16_len(),
        };
        self.health().map_err(MarkdownTableRequestError::Source)?;
        publication.table_sources_for(document, authority)?;
        Ok(VerifiedMarkdownLayoutTableRow(Rc::new(row)))
    }
}

impl VerifiedMarkdownLayoutTableRow {
    /// Supplies the closed span used by the original Unicode job to skip structure.
    /// It grants no independent layout authority for interior scalars.
    pub(crate) fn span(&self) -> super::table_rows::TableSpan {
        super::table_rows::TableSpan {
            start: self.0.start,
            end: self.0.end,
            tail: self.0.tail,
            header: self.0.header,
            ordinal: self.0.ordinal,
            eof: self.0.eof,
        }
    }

    /// Borrows only the original canonical source's weak identity, retaining
    /// neither document text nor the active document.
    pub(crate) fn source_authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.0.source
    }
    /// Requires the current document/selection and the same complete canonical
    /// source. Frozen structure cannot reauthorize a later input generation.
    pub fn require_for(
        &self,
        document: &MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
    ) -> Result<(), MarkdownTableRequestError> {
        source.health().map_err(MarkdownTableRequestError::Source)?;
        if !source.permits(&self.0.source) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        match &self.0.publication {
            RowPublication::Window { publication, .. } => {
                publication.table_sources_for(document, self.0.authority)?;
            }
            RowPublication::Structural(row) => row.require_for(document, self.0.authority)?,
        }
        Ok(())
    }

    /// Complete physical range including actual EOL; header elements additionally
    /// include their same-source delimiter.
    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        self.0.start.utf16..self.0.end.utf16
    }
    /// Table identity follows the original physical header byte; absolute row
    /// identity never derives from a viewport index.
    #[must_use]
    pub fn coordinate(&self) -> (usize, usize) {
        (self.0.header, self.0.ordinal)
    }
    /// Column count comes from the original header. Missing cells retain that
    /// same actual column coordinate space.
    #[must_use]
    pub fn columns(&self) -> usize {
        self.0.columns
    }

    /// Cell consumption follows the same semantic publication. Preparation creates
    /// bounded work only and never authorizes an unfinished display source.
    pub fn prepare_cell(
        &self,
        document: &MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        column: usize,
    ) -> Result<MarkdownTableCellDisplayPreparation, MarkdownTableRequestError> {
        self.require_for(document, source)?;
        match &self.0.publication {
            RowPublication::Window {
                publication,
                window,
            } => MarkdownTableCellDisplayPreparation::new(
                document,
                publication.clone(),
                self.0.authority,
                *window,
                self.0.ordinal,
                column,
            ),
            RowPublication::Structural(row) => {
                MarkdownTableCellDisplayPreparation::from_row_source(
                    document,
                    row.clone(),
                    self.0.authority,
                    column,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests;
