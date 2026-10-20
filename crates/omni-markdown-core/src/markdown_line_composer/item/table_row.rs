//! 表格作为原正文流的真实块元素交接；只停在原 Style nonce，不创建第二 Unicode worker。
use super::*;
use crate::VerifiedMarkdownLayoutTableRow;
use crate::markdown_layout_source::table_rows::{TablePlan, TablePlanWriter};

/// 同源样式重排遇到已封存行首，先请求当前语义行票，再进入原表格目录流程。
/// 这是原 Style 握手的结构作用域，不为 raw 表格补造 Script、UBA、字素或 Font 权限。
pub(super) fn known_row_request(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    step: &mut MarkdownLineItemStep,
) -> Result<bool> {
    let Some(tables) = payload.tables.as_ref() else {
        return Ok(false);
    };
    if payload.table_checked_at == Some(payload.cursor) {
        return Ok(false);
    }
    let (span, records) = tables.peek_published(&payload.source)?;
    payload.table_checked_at = Some(payload.cursor);
    step.records = records;
    step.metadata_units = 1; // 空计划也实际访问了原出版槽，但没有 pread 时不能记一条记录。
    let Some(span) = span else {
        return Ok(true);
    };
    if span.start.utf16 < payload.cursor {
        return Err(MarkdownLineComposeError::InconsistentCoverage);
    }
    if span.start.utf16 != payload.cursor {
        // 文字游标尚未到下一行；此量子的目录访问已经收费，下个量子继续原正文准备。
        return Ok(true);
    }
    let request = MarkdownLineStyleRequest {
        nonce: Arc::new(()),
        source: authority.clone(),
        shaping_seams: [None, None],
        range: span.start.utf16..span.end.utf16,
        scope: StyleScope::Table(span),
        continuation_font: None,
        continuation_bytes: None,
    };
    payload.request = Some(request.clone());
    payload.stage = Stage::Style;
    step.answer = MarkdownLineItemAnswer::NeedStyle(request);
    Ok(true)
}

impl MarkdownLineItemCoordinator {
    /// 构造时分享空目录；逐视口发现行时原四个 Unicode owner 读取同一结构记录。
    pub(in crate::markdown_line_composer) fn install_table_flow(
        &mut self,
        writer: TablePlanWriter,
    ) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if !matches!(payload.stage, Stage::Bidi) || payload.cursor != 0 {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        let plan = writer.plan();
        payload
            .resources
            .register_table_directory(plan.resource_observer());
        payload.scripts.install_table_plan(plan.clone())?;
        payload.breaks.install_table_plan(plan.clone())?;
        payload.tables = Some(writer);
        Ok(())
    }

    /// 样式重排保留原结构目录及语言 owner；新 writer 仍属于同一个 canonical Source。
    pub(in crate::markdown_line_composer) fn table_plan(&self) -> Option<TablePlan> {
        self.payload
            .as_ref()
            .and_then(|payload| payload.tables.as_ref().map(TablePlanWriter::plan))
    }

    /// 只接当前尚未响应的原请求；旧 nonce、其他源和非物理行首不能变成块控制。
    pub(in crate::markdown_line_composer) fn begin_table_row(
        &mut self,
        request: &MarkdownLineStyleRequest,
        row: &VerifiedMarkdownLayoutTableRow,
    ) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        let pending = payload
            .request
            .as_ref()
            .ok_or(MarkdownLineComposeError::InvalidRequest)?;
        if !matches!(payload.stage, Stage::Style)
            || !Arc::ptr_eq(&pending.nonce, &request.nonce)
            || pending.source != request.source
            || request.source != self.authority
            || !payload.source.permits(row.source_authority())
            || payload.cursor != row.source_range().start
            || request.range.start != payload.cursor
            || payload.item.is_some()
            || payload.closed_input.is_some()
            || payload.closed_fonts.is_some()
            || payload.frontier.is_some()
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if let StyleScope::Table(span) = &pending.scope
            && *span != row.span()
        {
            // 原目录只指示要重新索取哪一物理行；当前出版物必须交回完全相同的真实结构票。
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        // 这里只转交原 nonce；真实目录追加和后缀 flush 留给有预算的原 advance 量子。
        // 请求尚未产生实际 ShapeJob；撤销它的 FontProbe scope 和扫描壳，不能撤销既有 glyph。
        payload.font_probe_scope = None;
        payload.request = None;
        payload.scan = None;
        payload.open_probe = None;
        payload.pending_style = None;
        payload.stage = Stage::TableDirectory;
        Ok(())
    }

    /// 每次只推进一个原依赖，末 CR/LF 逐字节收费；表格内部不解析 SCX 或原正文 GCB。
    pub(in crate::markdown_line_composer) fn advance_table_row(
        &mut self,
        row: &VerifiedMarkdownLayoutTableRow,
        budget: MarkdownLineComposeBudget,
    ) -> Result<(MarkdownLineItemStep, bool)> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if !matches!(payload.stage, Stage::TableDirectory | Stage::TableRow)
            || !payload.source.permits(row.source_authority())
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        let end = row.source_range().end;
        let mut result = MarkdownLineItemStep {
            bytes: 0,
            context_bytes: 0,
            scalars: 0,
            records: 0,
            metadata_units: 0,
            shapes: 0,
            answer: MarkdownLineItemAnswer::Yielded,
        };
        if matches!(payload.stage, Stage::TableDirectory) {
            let progress = payload
                .tables
                .as_mut()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?
                .advance_row(&payload.source, row)?;
            result.records = progress.records;
            result.metadata_units = progress.metadata_units;
            if progress.ready {
                payload.scripts.table_announced();
                payload.stage = Stage::TableRow;
            }
            // 本量子的记录访问或真实出版已经收费；不同时再推进另一个 Unicode owner。
            return Ok((result, false));
        }
        if !payload.scripts.table_passed(end) {
            let step = payload.scripts.step(
                &self.authority,
                MarkdownScriptBudget {
                    bytes: budget.bytes,
                    scalars: budget.scalars,
                    records: budget.records,
                    stack_ops: budget.metadata_units,
                },
            )?;
            result.bytes = step.bytes;
            result.scalars = step.scalars;
            result.records = step.records;
            result.metadata_units = step.stack_ops;
            return Ok((result, false));
        }
        if !payload.breaks.table_passed(&self.authority, end)? {
            let step = payload.breaks.step(
                &self.authority,
                MarkdownLineBreakBudget {
                    bytes: budget.bytes,
                    scalars: budget.scalars,
                    records: budget.records,
                },
            )?;
            result.bytes = step.bytes;
            result.scalars = step.scalars;
            result.records = step.records;
            return Ok((result, false));
        }
        Ok((result, true))
    }

    /// 真实 row writer 已提交 LineEnd 后才越过 canonical 范围；下一正文沿原 ItemStart 恢复。
    pub(in crate::markdown_line_composer) fn finish_table_row(
        &mut self,
        row: &VerifiedMarkdownLayoutTableRow,
    ) -> Result<()> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        if !matches!(payload.stage, Stage::TableRow) || payload.cursor != row.source_range().start {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        payload.cursor = row.source_range().end;
        payload.stage = Stage::ItemStart;
        Ok(())
    }
}
