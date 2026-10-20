//! 原表格 raw 格的只读布局捕获；同一结构票与冻结 piece 切片不授予 inline 或输入权限。

use super::*;
use crate::markdown_document_buffer::{MarkdownTableRequestError, VerifiedMarkdownTableCellSource};

#[cfg(feature = "paged-line-compose")]
mod display;
#[cfg(feature = "paged-line-compose")]
pub use display::{
    MarkdownTableCellDisplayPreparation, MarkdownTableCellDisplayPreparationStep,
    MarkdownTableCellDisplayResourceObserver, MarkdownTableCellDisplayResources,
    MarkdownTableCellDisplaySource, MarkdownTableCellVisibleSourceRanges,
};

impl MarkdownLayoutTextSource {
    /// 只能消费原表格生产者的封闭端点；格内原始内容按 4 KiB 借用，不构造整格字符串或草稿。
    pub fn capture_table_cell(
        document: &MarkdownDocumentBuffer,
        cell: &VerifiedMarkdownTableCellSource,
    ) -> Result<Self, MarkdownTableRequestError> {
        let snapshot = document.table_cell_layout_snapshot(cell)?;
        let bytes = snapshot.byte_len();
        let utf16 = snapshot.utf16_len();
        let generation = document.generation();
        // 端点 lease 已真正归还；同一原票最后核验后才发布局部域 Source。
        cell.canonical_range_for(document)?;
        Ok(Self(Arc::new(Payload {
            snapshot,
            generation,
            bytes,
            utf16,
            domain: SourceDomain::ReadOnlyCell(Box::new(cell.clone())),
            dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
        })))
    }

    /// 只借最初的 raw 格身份，Native 消费者仍须用原 Doc 重核；该票不是可见语义或编辑许可。
    #[must_use]
    pub fn read_only_cell_source(&self) -> Option<&VerifiedMarkdownTableCellSource> {
        match &self.0.domain {
            SourceDomain::ReadOnlyCell(cell) => Some(cell),
            SourceDomain::Canonical | SourceDomain::ActiveCellDraft(_) => None,
        }
    }
}

#[cfg(test)]
mod tests;
