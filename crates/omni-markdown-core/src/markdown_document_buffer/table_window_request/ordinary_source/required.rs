//! 普通来源 owner 内的真实单行续权；沿原 batch 回放，不产生第二份 Global 或 ordinary Ready。

use super::super::semantic::{RequiredLocation, RequiredPublication};
use super::*;
use crate::markdown_document_buffer::MarkdownTableViewportRequiredRowQuery;
use crate::markdown_viewport_presentation::table_window::navigation::MarkdownTableWindowNavigation;

/// 两层 publication 都固定在签发时；同一表坐标不能借另一份 source publication 续权。
pub struct MarkdownViewportSourceRequiredRowQuery {
    publication: Weak<SourcePublication>,
    table_query: MarkdownTableViewportRequiredRowQuery,
}

/// Row 与真正 End 都交原来源行票；没有 window index、裸 End 或可以自行签发来源的整数。
pub enum MarkdownViewportSourceRequiredRowResult {
    Row {
        row: VerifiedMarkdownTableRowSource,
    },
    TableEnd {
        terminal: VerifiedMarkdownTableRowSource,
    },
}

impl VerifiedMarkdownViewportSources {
    /// origin 必须是本 publication 实际结构行，方向只产生待解析目标，不能以整数获得行权限。
    /// None 仅表示表头向前的真实 TableStart；Next/NextRow 都查询后一物理表行，列策略由原格负责。
    pub fn required_row_query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        origin: &VerifiedMarkdownTableRowSource,
        navigation: MarkdownTableWindowNavigation,
    ) -> Result<Option<MarkdownViewportSourceRequiredRowQuery>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        origin.require_for(document, authority)?;
        if !Rc::ptr_eq(&origin.publication, &self.publication) {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let requested_row = match navigation {
            MarkdownTableWindowNavigation::Previous => match origin.ordinal.checked_sub(1) {
                Some(row) => row,
                None => return Ok(None),
            },
            MarkdownTableWindowNavigation::Next | MarkdownTableWindowNavigation::NextRow => origin
                .ordinal
                .checked_add(1)
                .ok_or(MarkdownTableRequestError::InvalidRequest)?,
        };
        let table_query = self.table.required_structure_query_for(
            document,
            &origin.ticket,
            origin.ordinal,
            requested_row,
        )?;
        Ok(Some(MarkdownViewportSourceRequiredRowQuery {
            publication: Rc::downgrade(&self.publication),
            table_query,
        }))
    }

    /// Some 只表示原 Replay 完成后的实际 Row/terminal；目标离开视觉范围也交同一 publication 的封闭票。
    pub fn required_row_result_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<MarkdownViewportSourceRequiredRowResult>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        let Some(result) = self.table.required_publication_for(document)? else {
            return Ok(None);
        };
        let (index, ordinal, ended) = match result {
            RequiredPublication::Row {
                location: RequiredLocation::Structure(index),
                row_ordinal,
            } => (*index, *row_ordinal, false),
            RequiredPublication::End {
                terminal: RequiredLocation::Structure(index),
                end,
            } => (*index, end.last_body_row_ordinal().unwrap_or(0), true),
            _ => return Err(MarkdownTableRequestError::InvalidRequest),
        };
        let ticket = self
            .table
            .structural_tables()
            .get(index)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let row = VerifiedMarkdownTableRowSource {
            ticket: Rc::clone(ticket),
            publication: Rc::clone(&self.publication),
            ordinal,
            terminal: match result {
                RequiredPublication::End { end, .. } => Some(end.clone()),
                RequiredPublication::Row { .. } => ticket
                    .table_end
                    .as_ref()
                    .filter(|end| end.last_body_row_ordinal().unwrap_or(0) == ordinal)
                    .cloned(),
            },
        };
        row.require_for(document, authority)?;
        Ok(Some(if ended {
            MarkdownViewportSourceRequiredRowResult::TableEnd { terminal: row }
        } else {
            MarkdownViewportSourceRequiredRowResult::Row { row }
        }))
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
