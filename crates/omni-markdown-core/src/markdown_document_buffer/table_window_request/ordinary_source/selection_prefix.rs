//! 原请求证明的选择样式不变性只在同步移交中借用，不延长旧发布或字形句柄的权限。

use super::*;

/// 原 Plain 前缀或同一 Root Code 正文内的选择移动，不会改变已证明范围内的样式。
/// 借用同时约束请求、当前 Doc 和最后一次发布，不能跨修改/重投影留存为历史缓存。
/// 此能力不认证 GCB、软行或可编辑范围，也不能代替新选择的语义发布。
pub struct MarkdownSelectionStyleInvariant<'a> {
    request: &'a MarkdownViewportSourceRequest,
    document: &'a MarkdownDocumentBuffer,
    previous: &'a VerifiedMarkdownViewportSources,
    authority: MarkdownViewportSourceAuthority,
    kind: InvariantKind<'a>,
}

/// Root Code 的闭合 owner 来自原 producer，正文内没有可随选择变化的 inline marker；
/// Root scope 也排除了 quote/list 的逐行前缀。其它结构与 underline owner 对两个正文
/// 端点要么都包含、要么都不包含，所以此例可覆盖同 Source 已捕获的整个字形库存。
/// Plain 仍只授原 scanner 已真实读完的末端，绝不借 Code 例扩大未知段落的权限。
enum InvariantKind<'a> {
    PlainPrefix {
        utf16_end: usize,
    },
    RootCode {
        leaf: &'a VerifiedMarkdownOrdinaryLeaf,
    },
}

impl MarkdownViewportSourceRequest {
    /// 借出真实选择样式不变能力，供同 Source Composer 核全部已捕获字形和支持范围。
    /// None 完全不改变请求/发布；成功也不退休旧发布，调用方必须紧接原 reproject_selection。
    ///
    /// # Errors
    /// 真实 Source 首错、取消、异 Doc/代次、完整选择或非最新发布失权不降为能力缺失。
    pub fn selection_style_invariant<'a>(
        &'a self,
        document: &'a MarkdownDocumentBuffer,
        previous: &'a VerifiedMarkdownViewportSources,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<MarkdownSelectionStyleInvariant<'a>>, MarkdownTableRequestError> {
        self.require_selection_invariant_publication(document, previous, authority)?;
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
        if previous.table.windows_for(document)?.next().is_some()
            || !previous.table.structural_tables().is_empty()
        {
            return Ok(None); // 当前表格窗口另有投影 owner，不能拿普通 Leaf 填满其凸包。
        }
        let kind = if let Some(code) = leaf.root_code_leaf() {
            let body = code.root_code_body.as_ref().unwrap();
            if code.block.source_revision() != self.source.generation()
                || code.block.source_end() != self.source.byte_len()
                || !body.contains_selection(self.selection)
                || !body.contains_selection(authority.selection)
            {
                return Ok(None);
            }
            // 无表事实只认证这次有限窗口可以沿原 batch 重投影；全 Store 的样式
            // 不变性来自真实 Root Code owner，不能要求当前 Gap 伪装成全文无表证明。
            let mut covered = false;
            for gap in previous.table.gaps_for(document)? {
                let range = gap.utf16_range_for(document)?;
                covered |= range.start <= self.viewport.start && self.viewport.end <= range.end;
            }
            if !covered {
                return Ok(None);
            }
            InvariantKind::RootCode { leaf: code }
        } else {
            let Some(end) = self
                .semantic
                .as_ref()
                .and_then(|job| job.selection_invariant_prefix(&self.source, leaf))
            else {
                return Ok(None);
            };
            let mut covered = false;
            for gap in previous.table.gaps_for(document)? {
                let bytes = gap.byte_range_for(document)?;
                let utf16 = gap.utf16_range_for(document)?;
                covered |= bytes.start == 0
                    && end.byte <= bytes.end
                    && utf16.start == 0
                    && end.utf16 <= utf16.end;
            }
            if !covered {
                return Ok(None); // 空目录不是负证明；Plain 仍须原 Gap 覆盖整个已读前缀。
            }
            InvariantKind::PlainPrefix {
                utf16_end: end.utf16,
            }
        };
        Ok(Some(MarkdownSelectionStyleInvariant {
            request: self,
            document,
            previous,
            authority,
            kind,
        }))
    }

    /// 旧发布仍按旧完整选择绑定，但 Doc 已处于新选择；只在此显式移交入口分别核这两项事实。
    /// 所有检查均为原 owner 的只读观察，没有 Native 回调、源片借用或新的生命周期状态。
    fn require_selection_invariant_publication(
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

impl MarkdownSelectionStyleInvariant<'_> {
    /// Composer 只消费同一 Canonical Source 的已证末端，不把裸 Range 交给 Native 当授权。
    /// Doc 的不可变借用挡住同栈换选择，Request 借用挡住退休/取消；仍在消费紧前复核原首错。
    pub(crate) fn end_for(
        &self,
        source: &crate::MarkdownLayoutTextSource,
    ) -> Result<usize, MarkdownTableRequestError> {
        source.health().map_err(MarkdownTableRequestError::Source)?;
        self.request.require_selection_invariant_publication(
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
        match &self.kind {
            InvariantKind::PlainPrefix { utf16_end } => Ok(*utf16_end),
            InvariantKind::RootCode { leaf } => {
                // 全域能力仍由这一真实闭合 Leaf 绑定原 Doc 末端；不是 usize::MAX 或
                // 调用者提供的全局布尔。支持域检查、原 Font/Source 健康仍由 Composer 执行。
                if leaf.block.source_end() != self.document.byte_len() {
                    return Err(MarkdownTableRequestError::SourceSuperseded);
                }
                Ok(self.document.utf16_len())
            }
        }
    }
}
