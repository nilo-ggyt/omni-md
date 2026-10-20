//! 同一RootLeaf出版物的真实相邻样式；它只允许试右字素，不把样式末端直接当作可整形闭项。

use std::rc::Rc;

use super::{
    MarkdownLayoutTextAuthority, MarkdownLayoutTextSource, MarkdownLayoutTextSourceDomain,
};
use crate::markdown_document_buffer::{
    MarkdownOrdinarySemanticState, MarkdownTableRequestError, MarkdownViewportSourceAuthority,
    VerifiedMarkdownViewportSources,
};
use crate::{MarkdownDocumentBuffer, MarkdownLineStyleRequest, MarkdownPresentationQuery};

/// 私有构造的相邻语义事实；没有正文、活Doc或Font，只保同一次出版物的有限metadata。
/// 原request保留真实nonce供响应安装时核对；本票不能进入需要Send的Request或FontProbe。
pub struct VerifiedMarkdownRootLeafStyleBoundary {
    source: MarkdownLayoutTextAuthority,
    request: MarkdownLineStyleRequest,
    publication: Rc<VerifiedMarkdownViewportSources>,
    authority: MarkdownViewportSourceAuthority,
    left: MarkdownPresentationQuery,
    right: MarkdownPresentationQuery,
}

impl MarkdownLayoutTextSource {
    /// 从同一真实RootLeaf取得左右查询，窗口末端、空洞或相同样式均不签发边界。
    /// 本入口不查询GCB、不读正文、不创建Parser；后继仍须付费取得完整右字素并实际选择另一Font。
    ///
    /// # Errors
    /// 原Source I/O优先返回；不同Doc根、局部源、异源request或失效出版物不能借数值位置续权。
    pub fn verified_root_leaf_style_boundary(
        &self,
        document: &MarkdownDocumentBuffer,
        publication: Rc<VerifiedMarkdownViewportSources>,
        authority: MarkdownViewportSourceAuthority,
        request: &MarkdownLineStyleRequest,
    ) -> Result<Option<VerifiedMarkdownRootLeafStyleBoundary>, MarkdownTableRequestError> {
        self.health().map_err(MarkdownTableRequestError::Source)?;
        if self.domain() != MarkdownLayoutTextSourceDomain::Canonical
            || !self.0.snapshot.shares_current_table_root(document)
            || !self.permits(request.source())
        {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        let range = request.source_range();
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) =
            publication.ordinary_semantics_for(document, authority)?
        else {
            return Ok(None); // SourcesOnly与其它Ready不能冒充当前RootLeaf语义。
        };
        let Some(left) = leaf.query_for(document, authority, range.start)? else {
            return Ok(None);
        };
        let end = left.utf16_range.end;
        if end <= range.start || end >= range.end {
            return Ok(None); // 只认证原请求内部的真实交界，不把请求或开放Script末端升级为自然边界。
        }
        let Some(right) = leaf.query_for(document, authority, end)? else {
            return Ok(None); // query会裁到真实coverage，右侧未知时左query的截尾没有封口权。
        };
        if right.utf16_range.start != end || right.styles == left.styles {
            return Ok(None); // 相同样式的相邻coverage也不能靠页/块边缘伪造语义变化。
        }
        Ok(Some(VerifiedMarkdownRootLeafStyleBoundary {
            source: self.authority(),
            request: request.clone(),
            publication,
            authority,
            left,
            right,
        }))
    }
}

impl VerifiedMarkdownRootLeafStyleBoundary {
    /// 同一冻结Source与强持的原出版物共同界定票的生命周期；不持第二份正文或活Doc。
    pub(crate) fn require_source(
        &self,
        source: &MarkdownLayoutTextSource,
    ) -> Result<(), MarkdownTableRequestError> {
        source.health().map_err(MarkdownTableRequestError::Source)?;
        if !source.permits(&self.source) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        self.publication.require_live_publication()
    }

    pub(crate) fn source(&self) -> &MarkdownLayoutTextAuthority {
        &self.source
    }

    pub(crate) fn request(&self) -> &MarkdownLineStyleRequest {
        &self.request
    }

    pub(crate) fn left(&self) -> &MarkdownPresentationQuery {
        &self.left
    }

    pub(crate) fn right(&self) -> &MarkdownPresentationQuery {
        &self.right
    }

    /// Native只借原票签出的右侧语义及同一ScopeBody所需出版物，不从当前新窗口替换来源。
    /// 原Composer仍负责核右NeedStyle的nonce、完整GCB和预算；此查询本身不消耗右字素。
    ///
    /// # Errors
    /// Source首错、Doc/选择换代、异源或越出原右语义/Script作用域均拒绝，不返回默认样式。
    pub fn right_query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
    ) -> Result<
        (
            MarkdownPresentationQuery,
            Rc<VerifiedMarkdownViewportSources>,
            MarkdownViewportSourceAuthority,
        ),
        MarkdownTableRequestError,
    > {
        self.require_source(source)?;
        if !source.0.snapshot.shares_current_table_root(document)
            || !source.permits(request.source())
        {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        // 持有同原Rc使Leaf内部的publication弱票真实存活；当前Doc/完整方向选择仍由原check核验。
        self.publication
            .ordinary_semantics_for(document, self.authority)?;
        let range = request.source_range();
        if range.start != self.left.utf16_range.end
            || range.start >= range.end
            || range.end > self.right.utf16_range.end
            || range.end > self.request.source_range().end
            || request.paragraph_range() != self.request.paragraph_range()
            || request.script() != self.request.script()
            || request.level() != self.request.level()
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok((self.right.clone(), self.publication.clone(), self.authority))
    }
}

#[cfg(all(test, feature = "paged-shaping-frontier"))]
mod tests;
