//! 同一 SourcePublication 的真实结构行；不把 raw 行或 H/D 上下文升级成单格 inline Ready。

use super::*;

#[derive(Clone)]
/// 原结构票与原来源发布各只保共享 metadata；没有文档、映射、正文 String 或另一个 Global。
pub struct VerifiedMarkdownTableRowSource {
    pub(super) ticket: Rc<MarkdownTableStructuralWindow>,
    pub(super) publication: Rc<SourcePublication>,
    pub(super) ordinal: usize,
    /// 同一次真实结构发布或 RequiredRow 的 End；不为附终点复制、替换原多行结构票。
    pub(super) terminal: Option<VerifiedTableEnd>,
}

impl VerifiedMarkdownTableRowSource {
    /// 当前 Source/root、完整选择与 context 都必须仍属同一发布；冻结源不授新编辑权限。
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

    /// 只让原 Doc 的提交边界借真正 terminal End；普通 Row/EOF 坐标不提供追加权限。
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

    /// H 与 D 是同一个 flow 元素，body 保原物理行的完整 EOL 或真实 EOF；不按当前 frame 尾推结束。
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
    /// 查询只遍历当前有限来源页，不能扫整表或从 TABLE 样式位补造成员；范围外仍然未知。
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
