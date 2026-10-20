//! 表格独立 flow 行的真实权限；只从原 publication 取行，物理行尾仍从同一冻结 Source 确认。

use std::rc::Rc;

use super::*;
use crate::MarkdownGraphemeBoundary;
use crate::markdown_document_buffer::{
    MarkdownTableRequestError, MarkdownViewportSourceAuthority, VerifiedMarkdownTableRowSource,
    VerifiedMarkdownViewportSources,
};

/// 一个原物理行对应一个独立表格 flow 元素；表头同时消费同源 delimiter，不能把它另画成空行。
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
    /// 原 Global 在任何整行复制之前交付的 opaque 行；结构权限只暂停 flow，单格仍须各自完成原 inline 请求。
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
        // 原物理票已含完整 EOL；这里只取实际至多两个单元，给同一 Unicode DFA 保留真实尾上下文。
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
    /// 原窗口的已发布 row 才有 flow 身份；裸范围、相同正文的另一 Source 和 header context 均不能造行。
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
        // 行内容票不含 CR/LF；只借至多两个真实单元，CRLF 不能在两个 flow 元素间拆开。
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
    /// 同一个封闭端点供原 Unicode job 跳过结构内容；不借此授予内部 scalar 的布局权限。
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

    /// 只借最初 canonical Source 的弱身份，不能保活正文或活动 Doc。
    pub(crate) fn source_authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.0.source
    }
    /// 当前 Doc/选择与同一个完整 canonical Source 缺一不可；冻结结构不会重签后继输入权限。
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

    /// 完整 physical 范围包含真实行尾；表头元素另包含其同源 delimiter。
    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        self.0.start.utf16..self.0.end.utf16
    }
    /// 表身份沿原物理 header byte，绝对 row 不由窗口下标推导。
    #[must_use]
    pub fn coordinate(&self) -> (usize, usize) {
        (self.0.header, self.0.ordinal)
    }
    /// 列数来自原 header，缺格也保留同一个真实列空间。
    #[must_use]
    pub fn columns(&self) -> usize {
        self.0.columns
    }

    /// 内层消费沿原同一次语义 publication；请求只构造有限作业，不签未完成的显示源。
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
