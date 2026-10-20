//! 原表格生产者签发的单格原始来源；结构票经原文档准入才可变成草稿计划，不授予 inline Ready。

use super::table_cell_edit::TableCellEditContext;
use super::table_source::MarkdownTableSourceIdentity;
use super::*;
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowCellSource;

#[derive(Clone, Debug)]
/// 源根弱身份、真实行列与原始端点共用一张票；没有正文、活动文档或 Native 强引用。
pub struct VerifiedMarkdownTableCellSource {
    pub(super) source: MarkdownTableSourceIdentity,
    pub(super) context: TableCellEditContext,
    pub(super) range: Range<usize>,
    pub(super) implicit_empty: bool,
    /// 原 Scanner 或有限窗口已经证实的补列数量；零长范围本身无法区分显式空格与缺格。
    pub(super) separators: usize,
    /// 同一真实 separator 扫描或原有限 Parser 投影封存的列方向；空格也保此事实。
    pub(super) alignment: crate::MarkdownTableAlignment,
    pub(super) inline_seed:
        Option<std::sync::Arc<pulldown_cmark::global_frontier::VerifiedInlineSemanticSeed>>,
}

impl VerifiedMarkdownTableCellSource {
    /// 空格也先保真实源错误；相同长度、另一个文档或 Undo 回来的旧根均不能续签这张票。
    pub(super) fn require(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(), MarkdownTableRequestError> {
        document
            .original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(())
    }

    /// 只返回原真实格的 canonical 范围；缺格的零长插入位置不因此获得源字符或补管道权限。
    pub fn canonical_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.require(document)?;
        Ok(&self.range)
    }

    /// 显式空格与隐式缺格保持原 Parser 区别；不能从显示均为空推断相同写入行为。
    pub fn source_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<MarkdownTableWindowCellSource, MarkdownTableRequestError> {
        self.require(document)?;
        Ok(if self.implicit_empty {
            MarkdownTableWindowCellSource::ImplicitEmpty {
                insertion_anchor_utf16: self.range.start,
            }
        } else {
            MarkdownTableWindowCellSource::Present(self.range.clone())
        })
    }

    /// 头部位置和绝对行列来自同一结构生产者，离屏窗口的 Vec 位置不参与身份。
    pub fn coordinate_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(usize, usize, usize), MarkdownTableRequestError> {
        self.require(document)?;
        Ok((
            self.context.header.start,
            self.context.row_ordinal,
            self.context.column,
        ))
    }

    /// 列方向与原 root/格票同寿；另一个同字节文档、Undo 旧根或失败来源都不能续用。
    ///
    /// # Errors
    /// 返回原 Source 首错或已撤销的 root 身份；不从调用者列号补造方向。
    pub fn alignment_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<crate::MarkdownTableAlignment, MarkdownTableRequestError> {
        self.require(document)?;
        Ok(self.alignment)
    }

    /// raw 格的同源语义起点仍不是样式 Ready；普通已发布窗口继续沿其原完整 runs 查询。
    pub fn inline_seed_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<
        Option<&pulldown_cmark::global_frontier::VerifiedInlineSemanticSeed>,
        MarkdownTableRequestError,
    > {
        self.require(document)?;
        Ok(self.inline_seed.as_deref())
    }
}

impl MarkdownDocumentBuffer {
    /// 消费原生产者的完整格票开始草稿计划；不接受裸范围，也不重新读取原格或另建结构请求。
    ///
    /// 显式空格直接替换自己的零长原范围，隐式缺格只补原 Scanner 已证实的分隔符。
    /// 计划仍仅弱认原 root，真正的 mmap/piece 共享和唯一草稿槽由 begin_prepared_cell_draft 负责。
    ///
    /// # Errors
    /// 原存储失败、另一个文档或旧源票均拒绝；补列预算错误保留原 typed 原因，不创建草稿。
    pub fn plan_verified_table_cell_draft(
        &self,
        cell: VerifiedMarkdownTableCellSource,
    ) -> Result<MarkdownTableCellEditPlan, MarkdownTableRequestError> {
        cell.require(self)?; // 源错误先于旧票；相同长度、另一个 Doc 或 Undo 回来的 root 均不能续权。
        let plan = self
            .plan_table_cell_source_replacement(cell, "")
            .map_err(MarkdownTableRequestError::Window)?;
        plan.cell.require(self)?;
        Ok(plan)
    }

    /// 只有现有 verified 窗口可提供普通格来源；不接受裸范围，也不创建唯一活动草稿。
    pub fn verified_table_cell_source(
        &self,
        window: &VerifiedMarkdownTableWindow,
        row_ordinal: usize,
        column: usize,
    ) -> Result<VerifiedMarkdownTableCellSource, MarkdownTableRequestError> {
        self.original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        let presentation = window.presentation_for(self)?;
        let prepared = self.projected_table_cell_source(presentation, row_ordinal, column);
        self.original_backing_health()
            .map_err(MarkdownTableRequestError::Source)?;
        let mut cell = prepared.map_err(MarkdownTableRequestError::Window)?;
        cell.context.end = window.table_end_for(self)?.cloned();
        cell.require(self)?;
        Ok(cell)
    }

    /// 原 piece 路径只切出这一个 raw 格；实际字节继续共享原映射与编辑片，读取仍受布局量子约束。
    #[cfg(any(
        feature = "paged-graphemes",
        feature = "paged-line-breaks",
        feature = "paged-shaping"
    ))]
    pub(crate) fn table_cell_layout_snapshot(
        &self,
        cell: &VerifiedMarkdownTableCellSource,
    ) -> Result<MarkdownTextSnapshot, MarkdownTableRequestError> {
        cell.require(self)?;
        let byte_start = self
            .byte_offset_for_utf16(cell.range.start)
            .map_err(MarkdownTableRequestError::Source)?;
        let byte_end = self
            .byte_offset_for_utf16(cell.range.end)
            .map_err(MarkdownTableRequestError::Source)?;
        let (_, tail) = split_piece_tree(self, self.root.clone(), byte_start)
            .map_err(MarkdownTableRequestError::Source)?;
        let (root, _) = split_piece_tree(self, tail, byte_end - byte_start)
            .map_err(MarkdownTableRequestError::Source)?;
        let snapshot = MarkdownTextSnapshot::capture_table_cell_root(self, &root);
        cell.require(self)?; // 真实端点 lease 归还失败不能被冻结成功掩盖。
        Ok(snapshot)
    }
}
