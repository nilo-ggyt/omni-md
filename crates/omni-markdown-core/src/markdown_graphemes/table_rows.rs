//! 原 GCB 机器只沿真实 table 物理硬边界跨过正文；末控制仍进入同一 Unicode DFA。

use super::*;
use crate::markdown_layout_source::table_rows::{TableCursor, TablePlan};

pub(super) struct State {
    pub(super) cursor: TableCursor,
    jumped: bool,
}

impl MarkdownGraphemeJob {
    /// 只在原 job 首次读取前装入共享动态目录，后来行追加无需换 Source 或重建索引。
    pub(crate) fn install_table_plan(&mut self, plan: TablePlan) -> Result<()> {
        plan.require(&self.source)?;
        self.tables = Some(State {
            cursor: TableCursor::new(plan),
            jumped: false,
        });
        Ok(())
    }
}

/// 一个结构 record 或两个末控制 byte 都支付原配额；跳过长度不会成为读取或 scalar 计数。
pub(super) fn advance(
    job: &mut MarkdownGraphemeJob,
    budget: MarkdownGraphemeBudget,
    step: &mut MarkdownGraphemeStep,
) -> Result<bool> {
    let Some(state) = &mut job.tables else {
        return Ok(false);
    };
    state.cursor.plan.require(&job.source)?;
    let Some(span) = state.cursor.peek(&mut step.records, budget.records)? else {
        return Ok(false);
    };
    if !state.jumped && job.machine.byte >= span.end.byte {
        state.cursor.finish();
        return Ok(true); // 已有原真实前缀越过短行，不重读，也不删除已经成立的 Unicode 事实。
    }
    if job.machine.byte < span.start.byte {
        return Ok(false);
    }
    if !state.jumped {
        if !job
            .machine
            .begin_table(span, &mut job.index, step, budget.records)?
        {
            return Ok(true);
        }
        state.jumped = true;
    }
    let start = job.machine.byte;
    let end = span.end.byte.min(
        start
            .saturating_add(budget.bytes - step.bytes)
            .min(start.saturating_add(budget.scalars - step.scalars)),
    );
    let mut failure = None;
    job.source
        .visit_byte_range(start..end, |bytes| {
            job.source_borrow_max = job.source_borrow_max.max(bytes.len());
            for byte in bytes {
                job.source_bytes_read += 1;
                if let Err(error) = job.machine.push_fold_tail(*byte, &mut job.index, step) {
                    failure = Some(error);
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
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
        return Err(error);
    }
    if job.machine.byte == span.end.byte && step.records < budget.records {
        job.machine.finish_table(span, &mut job.index, step)?;
        state.cursor.finish();
        state.jumped = false;
    }
    Ok(true)
}

/// 普通扫描只能到当前已封闭 row 的首硬边界，绝不预读它的正文来猜结束位置。
pub(super) fn read_end(job: &MarkdownGraphemeJob, end: usize) -> usize {
    job.tables
        .as_ref()
        .and_then(|state| state.cursor.current())
        .map_or(end, |span| end.min(span.start.byte))
}
