//! 原 Style 请求在真实结构行首交给同一 Grid；最终几何归还后才向原行目录提交块元素。
use super::*;
use crate::VerifiedMarkdownLayoutTableRow;
use crate::markdown_layout_source::table_rows::TablePlanWriter;

/// 请求只持真实结构出版物与弱 Source；复制不会创建正文、格编辑会话或第二 worker。
#[derive(Clone)]
pub struct MarkdownLineTableRowRequest {
    nonce: std::sync::Arc<()>,
    row: VerifiedMarkdownLayoutTableRow,
    source: MarkdownLayoutTextAuthority,
}
/// 只有已完成所有真实格的 Grid 才归还此几何；列宽按完整 cell frame 的 26.6 数值封存。
pub struct MarkdownLineTableRowResponse {
    request: MarkdownLineTableRowRequest,
    registered: MarkdownLineRegisteredFont,
    geometry: TableGeometry,
}
pub(in crate::markdown_line_composer) struct TableGeometry {
    pub(in crate::markdown_line_composer) width: i64,
    pub(in crate::markdown_line_composer) height: i64,
    pub(in crate::markdown_line_composer) columns: Box<[i64]>,
}
pub(super) struct PendingRow {
    request: MarkdownLineTableRowRequest,
    response: Option<MarkdownLineTableRowResponse>,
}
impl MarkdownLineTableRowRequest {
    /// 原 sealed 行票负责 cell factory 和 canonical 来源，调用方不能改写坐标制造相邻行。
    pub fn row(&self) -> &VerifiedMarkdownLayoutTableRow {
        &self.row
    }
    /// 字体仅保同一 palette 的真实活性，块不使用文字 points；宽高来自实际 Grid 完成结果。
    pub fn respond_registered(
        self,
        registered: &MarkdownLineRegisteredFont,
        height_26_6: i64,
        width_26_6: i64,
        column_widths_26_6: &[i64],
    ) -> Result<MarkdownLineTableRowResponse> {
        if height_26_6 <= 0
            || width_26_6 <= 0
            || column_widths_26_6.len() != self.row.columns()
            || column_widths_26_6.len() > 256
            || column_widths_26_6.iter().any(|width| *width <= 0)
            || column_widths_26_6
                .iter()
                .try_fold(0i64, |total, width| total.checked_add(*width))
                != Some(width_26_6)
        {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        registered.registration().lease()?;
        Ok(MarkdownLineTableRowResponse {
            request: self,
            registered: registered.clone(),
            geometry: TableGeometry {
                width: width_26_6,
                height: height_26_6,
                columns: column_widths_26_6.into(),
            },
        })
    }
}
impl MarkdownLineComposer {
    /// 正文保原 frontier，另分享空结构目录；Source/活动格的原构造不取得表格流权限。
    pub fn new_with_table_flow(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        fonts: MarkdownLineFontCatalog,
        width_26_6: i64,
        cancellation: MarkdownLineComposeCancellation,
    ) -> Result<Self> {
        let mut result =
            Self::new_with_frontier(source, authority, fonts, width_26_6, cancellation)?;
        let payload = result
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        let writer = TablePlanWriter::new(&payload.source);
        payload.store.set_table_plan(Some(writer.plan()));
        payload.coordinator.install_table_flow(writer)?;
        Ok(result)
    }

    /// 原文字行必须已完成；只在当前 Style nonce 将实际物理行交给 Grid，不丢在途 Measure。
    pub fn begin_table_row(
        &mut self,
        request: &MarkdownLineStyleRequest,
        row: VerifiedMarkdownLayoutTableRow,
    ) -> Result<MarkdownLineTableRowRequest> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !matches!(payload.stage, Stage::Style)
                || payload.start != row.source_range().start
                || payload.measure.is_some()
                || payload.current.is_some()
                || payload.next.is_some()
                || payload.item.is_some()
                || payload.resolver.is_some()
                || payload.graphemes.is_some()
                || payload.table.is_some()
            {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            payload.coordinator.begin_table_row(request, &row)?;
            let request = MarkdownLineTableRowRequest {
                nonce: std::sync::Arc::new(()),
                row,
                source: self.authority.clone(),
            };
            payload.request = None;
            payload.next_start = None; // 原 text LineEnd 的一次性起点不能跨越真实独立块。
            payload.table = Some(PendingRow {
                request: request.clone(),
                response: None,
            });
            payload.stage = Stage::TableRow;
            Ok(request)
        })();
        self.finish(result)
    }

    /// 同坐标的旧 Row 响应也不能回到新请求；实际注册及 Source 由原 Store 再核。
    pub fn provide_table_row(&mut self, response: MarkdownLineTableRowResponse) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let pending = payload
                .table
                .as_mut()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?;
            if !matches!(payload.stage, Stage::TableRow)
                || pending.response.is_some()
                || !std::sync::Arc::ptr_eq(&pending.request.nonce, &response.request.nonce)
                || response.request.source != self.authority
            {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            response.registered.registration().lease()?;
            pending.response = Some(response);
            Ok(())
        })();
        self.finish(result)
    }
}

/// 每次只推进原依赖、GCB、item 或四条块记录中的一种，未完成 Grid 不会前移正文游标。
pub(super) fn advance(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownLineComposeBudget,
    step: &mut MarkdownLineComposeStep,
) -> Result<()> {
    let pending = payload
        .table
        .as_mut()
        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
    match payload.stage {
        Stage::TableRow => {
            if pending.response.is_none() {
                step.answer = MarkdownLineComposeAnswer::NeedsTableRow(pending.request.clone());
                return Ok(());
            }
            let (progress, ready) = payload
                .coordinator
                .advance_table_row(&pending.request.row, budget)?;
            step.bytes = progress.bytes;
            step.scalars = progress.scalars;
            step.records = progress.records;
            step.metadata_units = progress.metadata_units;
            if ready {
                payload.store.set_table_skip(pending.request.row.span());
                payload.stage = Stage::TableGraphemes;
            }
        }
        Stage::TableGraphemes => {
            let progress = payload.store.capture_graphemes_through(
                payload.coordinator.word_breaks()?,
                pending.request.row.source_range().end,
                budget.records.min(256),
                budget.metadata_units.min(256),
            )?;
            step.records = progress.records;
            step.metadata_units = progress.metadata_units;
            if progress.ready {
                let response = pending
                    .response
                    .take()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let range = pending.request.row.source_range();
                let value = super::super::controls::ValidatedControl {
                    request: MarkdownLineInlineControl::new(
                        authority.clone(),
                        range.clone(),
                        MarkdownLineInlineControlKind::TableRow,
                    ),
                    registered: response.registered,
                    // 独立块的范围就是它的真实布局域，不把多物理行结构伪装成 UBA 段落。
                    facts: MarkdownLineItemFacts {
                        source_utf16: range.clone(),
                        paragraph_utf16: range,
                        script: *b"Zyyy",
                        level: 0,
                        direction: crate::MarkdownShapingDirection::LeftToRight,
                        points: 0.0,
                        beginning_of_text: true,
                        end_of_text: true,
                    },
                    paragraph: MarkdownLineParagraphStyle {
                        tab_stops: Vec::new(),
                        default_tab_interval_26_6: None,
                    },
                    presentation: crate::MarkdownPresentationStyles::default(),
                    paragraph_inset_26_6: 0,
                    table: Some(response.geometry),
                };
                payload.store.begin_control(value)?;
                payload.stage = Stage::TableCapture;
            }
        }
        Stage::TableCapture => {
            let before = payload.store.record_count()?;
            if let Some(proof) = payload.store.step_control(budget.records.min(256))? {
                payload.store.begin_table_line(&proof)?;
                payload.stage = Stage::TablePublish;
            }
            step.records = payload
                .store
                .record_count()?
                .checked_sub(before)
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        }
        Stage::TablePublish => {
            let before = payload.store.record_count()?;
            if let Some(line) = payload.store.step_table_line(budget.records.min(256))? {
                payload.coordinator.finish_table_row(&pending.request.row)?;
                payload.start = line.source_utf16.end;
                payload.table = None;
                payload.stage = Stage::Start;
                step.answer = MarkdownLineComposeAnswer::LineReady(line);
            }
            step.records = payload
                .store
                .record_count()?
                .checked_sub(before)
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        }
        _ => return Err(MarkdownLineComposeError::InvalidRequest),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
