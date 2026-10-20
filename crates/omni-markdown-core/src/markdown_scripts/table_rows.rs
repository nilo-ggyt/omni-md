//! 原 SCX 续体先结清行前义务，表格内部不制造脚本 run；末真实硬控制恢复下一正文上下文。

use super::MarkdownScriptError as Error;
use super::*;
use crate::markdown_layout_source::table_rows::{TableCursor, TablePlan};

pub(super) struct State {
    pub(super) cursor: TableCursor,
    jumped: bool,
    tail: [u8; 2],
    read: usize,
}

impl MarkdownScriptJob {
    /// 只分享同 Source 动态结构目录；普通正文仍保原 Printable/frontier 优化。
    pub(crate) fn install_table_plan(&mut self, plan: TablePlan) -> Result<()> {
        plan.require(&self.source)?;
        self.tables = Some(State {
            cursor: TableCursor::new(plan),
            jumped: false,
            tail: [0; 2],
            read: 0,
        });
        Ok(())
    }
}

/// 同一量子只处理当前结构行，正文读取、记录和配对栈仍受原三个独立额度控制。
pub(super) fn advance(
    job: &mut MarkdownScriptJob,
    budget: MarkdownScriptBudget,
    step: &mut MarkdownScriptStep,
) -> Result<bool> {
    let Some(state) = &mut job.tables else {
        return Ok(false);
    };
    state.cursor.plan.require(&job.source)?;
    let Some(span) = state.cursor.peek(&mut step.stack_ops, budget.stack_ops)? else {
        return Ok(false);
    };
    if !state.jumped && job.machine.byte >= span.end.byte {
        state.cursor.finish();
        return Ok(true);
    }
    if job.machine.byte < span.start.byte {
        return Ok(false);
    }
    if !state.jumped {
        if !job
            .machine
            .settle_before_table(span, &mut job.store, step, budget)?
        {
            return Ok(true);
        }
        job.store.skip_table(span)?;
        job.machine.begin_table_tail(span);
        #[cfg(feature = "paged-shaping-frontier")]
        {
            job.printable = None;
            job.completed_ascii_suffix = None;
        }
        state.jumped = true;
    }
    let size = span.end.byte - span.tail.byte;
    let take =
        (size - state.read).min((budget.bytes - step.bytes).min(budget.scalars - step.scalars));
    let start = span.tail.byte + state.read;
    job.source.visit_byte_range(start..start + take, |bytes| {
        job.source_borrow_max = job.source_borrow_max.max(bytes.len());
        for byte in bytes {
            state.tail[state.read] = *byte;
            state.read += 1;
            step.bytes += 1;
            step.scalars += 1;
        }
        Ok(())
    })?;
    if state.read == size {
        job.machine.finish_table(span, &state.tail[..size])?;
        state.cursor.finish();
        state.jumped = false;
        state.read = 0;
        state.tail = [0; 2];
    }
    Ok(true)
}

/// 普通扫描只到已授权结构行首；尚未取得原行票的地方继续原 Source 解析规则。
pub(super) fn read_end(job: &MarkdownScriptJob, end: usize) -> usize {
    job.tables
        .as_ref()
        .and_then(|state| state.cursor.current())
        .map_or(end, |span| end.min(span.start.byte))
}

/// Script 的同步查询不把目录空洞解释成 Common、Latin 或不存在的真实 raw run。
pub(super) fn hidden(
    job: &MarkdownScriptJob,
    unit: MarkdownScriptUnit,
    offset: usize,
) -> Result<bool> {
    job.tables.as_ref().map_or(Ok(false), |state| {
        state
            .cursor
            .plan
            .contains_character(offset, unit == MarkdownScriptUnit::Utf16)
            .map_err(Error::from)
    })
}
