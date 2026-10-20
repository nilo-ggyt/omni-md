//! 表格硬边界只结清此前真实 LB token，再以原末控制恢复同一 DFA；表格内部没有断行权限。

use super::*;
use crate::markdown_layout_source::table_rows::{TableCursor, TablePlan};

pub(super) struct State {
    pub(super) cursor: TableCursor,
    jumped: bool,
}

impl MarkdownLineBreakJob {
    /// 同一个空动态目录在首次正文消费前共享，后续表格行只追加原结构事实。
    pub(crate) fn install_table_plan(&mut self, plan: TablePlan) -> Result<()> {
        plan.require(&self.source)?;
        self.tables = Some(State {
            cursor: TableCursor::new(plan),
            jumped: false,
        });
        Ok(())
    }
}

/// 每步仍支付原 records/bytes/scalars；行体长度没有进入任何文本借用或常驻数组。
pub(super) fn advance(
    job: &mut MarkdownLineBreakJob,
    budget: MarkdownLineBreakBudget,
    step: &mut MarkdownLineBreakStep,
) -> Result<bool> {
    let Some(state) = &mut job.tables else {
        return Ok(false);
    };
    state.cursor.plan.require(&job.source)?;
    let Some(span) = state.cursor.peek(&mut step.records, budget.records)? else {
        return Ok(false);
    };
    if !state.jumped && job.machine.byte_offset() >= span.end.byte as u64 {
        state.cursor.finish();
        return Ok(true);
    }
    if job.machine.byte_offset() < span.start.byte as u64 {
        return Ok(false);
    }
    if !state.jumped {
        while job.machine.has_before_table(span.start.byte) {
            if step.records == budget.records {
                return Ok(true);
            }
            if let Some(event) = job.machine.drain_before_table(span.start.byte) {
                job.index.push(event, job.machine.needs_dictionary())?;
            }
            step.records += 1; // 真实待决规则结算即使没有断点也消费一个有限状态操作。
        }
        job.machine.begin_table_tail(span);
        state.jumped = true;
    }
    let start = job.machine.byte_offset() as usize;
    let end = span
        .end
        .byte
        .min(start.saturating_add((budget.bytes - step.bytes).min(budget.scalars - step.scalars)));
    let mut failure = None;
    job.source
        .visit_byte_range(start..end, |bytes| {
            job.source_borrow_max = job.source_borrow_max.max(bytes.len());
            for byte in bytes {
                job.source_bytes_read += 1;
                step.bytes += 1;
                let before = job.machine.scalar_count();
                if let Err(error) = job
                    .machine
                    .push(job.machine.byte_offset(), &[*byte], |_| {})
                {
                    failure = Some(error);
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
                step.scalars += (job.machine.scalar_count() - before) as usize;
            }
            Ok(())
        })
        .or_else(|error| {
            if error == MarkdownDocumentBufferError::RangeOutsideDocument && failure.is_some() {
                Ok(())
            } else {
                Err(error)
            }
        })?;
    if let Some(error) = failure {
        return Err(error.into());
    }
    if job.machine.byte_offset() == span.end.byte as u64 {
        state.cursor.finish();
        state.jumped = false;
    }
    Ok(true)
}

/// 已收费的原行只裁界读取，不为解析预算取一个尚未发布的后继行。
pub(super) fn read_end(job: &MarkdownLineBreakJob, end: usize) -> usize {
    job.tables
        .as_ref()
        .and_then(|state| state.cursor.current())
        .map_or(end, |span| end.min(span.start.byte))
}
