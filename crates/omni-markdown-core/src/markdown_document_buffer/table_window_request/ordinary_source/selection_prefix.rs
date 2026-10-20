//! 原请求已证明的 Root Plain 前缀只在同一次同步选择移交中借用，不保旧发布续权。

use super::*;

/// 原 Root Paragraph 在有限末端之前的样式不依赖选择；构造只来自同一请求的真实 Plain proof。
/// 借用同时约束请求、当前 Doc 和最后一次发布，不能跨修改/重投影留存为历史缓存。
/// 此能力不认证 GCB、软行或可编辑范围，也不能代替新选择的语义发布。
pub struct MarkdownPlainSelectionPrefix<'a> {
    request: &'a MarkdownViewportSourceRequest,
    document: &'a MarkdownDocumentBuffer,
    previous: &'a VerifiedMarkdownViewportSources,
    authority: MarkdownViewportSourceAuthority,
    utf16_end: usize,
}

impl MarkdownViewportSourceRequest {
    /// 借出当前已投影的无标记前缀，供同 Source Composer 在原栈内检查是否可以保留字形。
    /// None 完全不改变请求/发布；成功也不退休旧发布，调用方必须紧接原 reproject_selection。
    ///
    /// # Errors
    /// 真实 Source 首错、取消、异 Doc/代次、完整选择或非最新发布失权不降为能力缺失。
    pub fn plain_selection_prefix<'a>(
        &'a self,
        document: &'a MarkdownDocumentBuffer,
        previous: &'a VerifiedMarkdownViewportSources,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<MarkdownPlainSelectionPrefix<'a>>, MarkdownTableRequestError> {
        self.require_selection_prefix_publication(document, previous, authority)?;
        if !self.leaf_mode
            || self.job.is_some()
            || self.coordinate_active
            || self.cell_inline.is_some()
            || self.row_source_position.is_some()
        {
            return Ok(None); // 活跃作业仍有其原归还义务，不因新选择截断其输入或输出。
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = &previous.semantic else {
            return Ok(None);
        };
        let Some(job) = &self.semantic else {
            return Ok(None);
        };
        let Some(end) = job.selection_invariant_prefix(&self.source, leaf) else {
            return Ok(None);
        };
        if previous.table.windows_for(document)?.next().is_some()
            || !previous.table.structural_tables().is_empty()
        {
            return Ok(None); // table 的选择投影另有 owner，不能借普通样式填满其凸包。
        }
        let mut no_table_prefix = false;
        for gap in previous.table.gaps_for(document)? {
            let bytes = gap.byte_range_for(document)?;
            let utf16 = gap.utf16_range_for(document)?;
            no_table_prefix |= bytes.start == 0
                && end.byte <= bytes.end
                && utf16.start == 0
                && end.utf16 <= utf16.end;
        }
        if !no_table_prefix {
            return Ok(None); // 原 NoTableGap 必须真实覆盖同一前缀；空目录本身不是无表证明。
        }
        Ok(Some(MarkdownPlainSelectionPrefix {
            request: self,
            document,
            previous,
            authority,
            utf16_end: end.utf16,
        }))
    }

    /// 旧发布仍按旧完整选择绑定，但 Doc 已处于新选择；只在此显式移交入口分别核这两项事实。
    /// 所有检查均为原 owner 的只读观察，没有 Native 回调、源片借用或新的生命周期状态。
    fn require_selection_prefix_publication(
        &self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        document
            .byte_offset_for_utf16(0)
            .map_err(MarkdownTableRequestError::Source)?;
        check_authority(
            &self.source,
            self.context_revision,
            authority.selection,
            document,
            authority,
        )?;
        previous.publication.require_live()?;
        if !matches!(self.phase, SourcePhase::Consumed)
            || self.context_revision != previous.publication.context_revision
            || self.selection != previous.publication.selection
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        Ok(())
    }
}

impl MarkdownPlainSelectionPrefix<'_> {
    /// Composer 只消费同一 Canonical Source 的有限末端；不把裸 Range 交给 Native 当授权。
    /// Doc 的不可变借用挡住同栈换选择，Request 借用挡住退休/取消；仍在消费紧前复核原首错。
    pub(crate) fn end_for(
        &self,
        source: &crate::MarkdownLayoutTextSource,
    ) -> Result<usize, MarkdownTableRequestError> {
        source.health().map_err(MarkdownTableRequestError::Source)?;
        self.request.require_selection_prefix_publication(
            self.document,
            self.previous,
            self.authority,
        )?;
        if source.domain() != crate::MarkdownLayoutTextSourceDomain::Canonical
            || !source
                .snapshot_for_bidi()
                .shares_current_table_root(self.document)
        {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(self.utf16_end)
    }
}
