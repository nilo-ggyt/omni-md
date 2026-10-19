//! 普通来源 owner 内的真实单行续权；沿原 batch 回放，不产生第二份 Global 或 ordinary Ready。

use super::*;
use crate::markdown_document_buffer::{
    MarkdownTableViewportRequiredRowQuery, MarkdownTableViewportRequiredRowResult,
};

/// 两层 publication 都固定在签发时；同一表坐标不能借另一份 source publication 续权。
pub struct MarkdownViewportSourceRequiredRowQuery {
    publication: Weak<SourcePublication>,
    table_query: MarkdownTableViewportRequiredRowQuery,
}

impl VerifiedMarkdownViewportSources {
    /// origin 必须是本 publication 内实际已发布的表行；context-only H 不贡献编辑来源。
    pub fn required_row_query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        window_index: usize,
        origin_row: usize,
        requested_row: usize,
    ) -> Result<MarkdownViewportSourceRequiredRowQuery, MarkdownTableRequestError> {
        self.check(document, authority)?;
        let table_query =
            self.table
                .required_row_query_for(document, window_index, origin_row, requested_row)?;
        Ok(MarkdownViewportSourceRequiredRowQuery {
            publication: Rc::downgrade(&self.publication),
            table_query,
        })
    }

    /// Some 只表示真实 Row/End；None 尚无 RequiredRow 终点，SourcesAvailable 仍不授 ordinary 样式。
    pub fn required_row_result_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<MarkdownTableViewportRequiredRowResult<'_>>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        self.table.required_row_result_for(document)
    }
}

impl MarkdownViewportSourceRequest {
    /// 同一 source publication 的单行需求加入原 batch；视口逆向或缺原 checkpoint 明确拒绝。
    ///
    /// 成功仅启动 Pending，后续仍须逐次 advance 并取得 SourcesAvailable，再读取实际 Row/End。
    pub fn continue_forward_with_required_row(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
        authority: MarkdownViewportSourceAuthority,
        query: MarkdownViewportSourceRequiredRowQuery,
    ) -> Result<(), MarkdownTableRequestError> {
        self.check_forward_sources(document, previous, &viewport, authority)?;
        if !query
            .publication
            .ptr_eq(&Rc::downgrade(&previous.publication))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        // 内层继续核其原 TablePublication 和实际 origin 成员；不以 source 目录身份替代表格票。
        match self.batch.extend_from_ready_with_required_row(
            document,
            &previous.table,
            viewport.clone(),
            query.table_query,
        ) {
            Ok(()) => {} // 原 batch 已接住查询；后面仍须联合准入同活 ordinary，不能在这里提前返回成功。
            Err(
                error @ (MarkdownTableRequestError::PublicationSuperseded
                | MarkdownTableRequestError::CoverageRequiresCheckpoint
                | MarkdownTableRequestError::InvalidRequest),
            ) => {
                // 这些是内层状态迁移之前的 query/覆盖前置拒绝；原 owner 仍可接合法原 publication。
                return Err(error);
            }
            Err(error) => {
                self.fail(error); // quota 或真实 source 失败不能保留半份 required 回放；原外带结果仍只读。
                return Err(error);
            }
        }
        // 只有原 batch 接受后才准入额外 previous ordinary 目录；仍使用同一 quota 和累计 work。
        // retain 不重置 job、坐标转换和 origin_floor，未完 ordinary 始终先于其视口后继推进。
        let result = self.retain_forward_sources(document, previous, viewport);
        if let Err(error) = result {
            self.fail(error); // 联合库存超限即撤本次后继，不能退回另一 request 重发额度。
        }
        result
    }
}

#[cfg(test)]
mod tests;
