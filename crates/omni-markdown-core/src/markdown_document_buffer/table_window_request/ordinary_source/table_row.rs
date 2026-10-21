//! Real structural rows bound to one SourcePublication; raw rows and header/delimiter context do not grant cell inline readiness.

use super::*;

#[derive(Clone)]
/// Share only the original structural ticket and publication metadata, without a document, mapping, body String, or second Global.
pub struct VerifiedMarkdownTableRowSource {
    pub(super) ticket: Rc<MarkdownTableStructuralWindow>,
    pub(super) publication: Rc<SourcePublication>,
    pub(super) ordinal: usize,
    /// An End from the original structural publication or RequiredRow replay; attaching it preserves the original multirow ticket.
    pub(super) terminal: Option<VerifiedTableEnd>,
}

impl VerifiedMarkdownTableRowSource {
    /// Require the original Source/root, complete selection, and context; frozen source data alone grants no new edit authority.
    pub fn require_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        document
            .original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        self.publication.check(document, authority)
    }

    pub fn coordinate_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(usize, usize, usize), MarkdownTableRequestError> {
        self.require_for(document, authority)?;
        Ok((
            self.ticket.header.header_byte_start,
            self.ordinal,
            self.ticket.header.columns,
        ))
    }

    /// Lend a real terminal End to the document's commit boundary; ordinary row or EOF coordinates cannot authorize appending.
    pub(in crate::markdown_document_buffer) fn terminal_end_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<&VerifiedTableEnd>, MarkdownTableRequestError> {
        self.require_for(document, authority)?;
        let Some(end) = self.terminal.as_ref() else {
            return Ok(None);
        };
        if end.header_byte_start() != self.ticket.header.header_byte_start
            || end.last_body_row_ordinal().unwrap_or(0) != self.ordinal
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok(Some(end))
    }

    /// Header and delimiter form one flow element; body rows retain their complete physical EOL or actual EOF, independent of frame ends.
    pub fn physical_utf16_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Range<usize>, MarkdownTableRequestError> {
        self.require_for(document, authority)?;
        Ok(self.utf16_range())
    }

    pub fn physical_byte_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Range<usize>, MarkdownTableRequestError> {
        self.require_for(document, authority)?;
        Ok(if self.ordinal == 0 {
            self.ticket.header.header_byte_start..self.ticket.header.separator_byte_end
        } else {
            let row = self.row();
            row.byte_start..row.byte_end
        })
    }

    fn utf16_range(&self) -> Range<usize> {
        if self.ordinal == 0 {
            self.ticket.header_utf16.start..self.ticket.delimiter_utf16.end
        } else {
            let row = self.row();
            row.utf16_start..row.utf16_end
        }
    }

    fn row(&self) -> &TableRowFact {
        self.ticket
            .body
            .iter()
            .find(|row| row.row_ordinal == self.ordinal)
            .expect("结构行只能从本原票实际存在的成员创建")
    }
}

impl VerifiedMarkdownViewportSources {
    /// Search only this finite source page. TABLE styles cannot manufacture membership, and positions outside coverage remain unknown.
    pub fn table_row_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        position_utf16: usize,
    ) -> Result<Option<VerifiedMarkdownTableRowSource>, MarkdownTableRequestError> {
        document
            .original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        self.check(document, authority)?;
        if !self.requested.contains(&position_utf16) {
            return Ok(None);
        }
        for ticket in self.table.structural_tables() {
            let ordinal = if ticket.include_header
                && (ticket.header_utf16.start..ticket.delimiter_utf16.end).contains(&position_utf16)
            {
                Some(0)
            } else {
                ticket
                    .body
                    .iter()
                    .find(|row| (row.utf16_start..row.utf16_end).contains(&position_utf16))
                    .map(|row| row.row_ordinal)
            };
            if let Some(ordinal) = ordinal {
                return Ok(Some(VerifiedMarkdownTableRowSource {
                    ticket: Rc::clone(ticket),
                    publication: Rc::clone(&self.publication),
                    ordinal,
                    terminal: ticket
                        .table_end
                        .as_ref()
                        .filter(|end| end.last_body_row_ordinal().unwrap_or(0) == ordinal)
                        .cloned(),
                }));
            }
        }
        Ok(None)
    }
}
