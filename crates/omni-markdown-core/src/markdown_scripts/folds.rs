//! 同源折叠只在真实 B 终止符之后恢复 SCX/GCB 上下文；隐区没有伪脚本 run。

use super::*;
use crate::markdown_layout_source::folds::FoldPlan;

pub(super) struct State {
    plan: FoldPlan,
    next: usize,
    header_settled: bool,
    tail: [u8; 2],
    tail_read: usize,
}

impl MarkdownScriptJob {
    /// 同一布局配置在首次源消费前交给原 job；只共享有限 proof，不另建 Source 或脚本规则。
    pub(crate) fn install_fold_plan(&mut self, plan: FoldPlan) -> Result<()> {
        self.guard(&self.authority.clone())?;
        if plan.authority() != &self.authority || self.machine.byte != 0 || self.machine.ready {
            return Err(MarkdownScriptError::SourceMismatch);
        }
        self.folds = Some(State {
            plan,
            next: 0,
            header_settled: false,
            tail: [0; 2],
            tail_read: 0,
        });
        #[cfg(feature = "paged-shaping-frontier")]
        {
            self.printable = None;
            self.completed_ascii_suffix = None;
        } // 原完整 root 摘要不能越过显式隐区，把其上下文借给可见 run。
        Ok(())
    }
}

/// 常规扫描只读到本 fold 的真实 header 换行后，不能在到达跳转阶段之前读入隐藏 body。
pub(super) fn read_end(job: &MarkdownScriptJob, requested: usize) -> usize {
    job.folds
        .as_ref()
        .and_then(|state| state.plan.spans().get(state.next))
        .map_or(requested, |span| requested.min(span.header_end.byte))
}

/// 返回 true 表示本量子专门完成真实段落结算/末端取样；来源错误、取消和实际 metadata 失败仍上交。
pub(super) fn advance_boundary(
    job: &mut MarkdownScriptJob,
    budget: MarkdownScriptBudget,
    step: &mut MarkdownScriptStep,
) -> Result<bool> {
    let Some(state) = &mut job.folds else {
        return Ok(false);
    };
    let Some(span) = state.plan.spans().get(state.next).copied() else {
        return Ok(false);
    };
    if job.machine.byte < span.header_end.byte {
        return Ok(false);
    }
    if !state.header_settled {
        if !job.machine.settle_hard_boundary(
            span.header_end.byte,
            span.header_end.utf16,
            &mut job.store,
            step,
            budget,
        )? {
            return Ok(true);
        }
        state.header_settled = true;
    }
    // 与 header 为同一终止符时不倒退重读；否则只读取真实尾 CR/LF 的最多两个字节。
    let tail_start = span.tail_start.byte.max(span.header_end.byte);
    let tail_len = span.end.byte - tail_start;
    if tail_len > state.tail.len() {
        return Err(MarkdownScriptError::CorruptMetadata);
    }
    let available = budget
        .bytes
        .saturating_sub(step.bytes)
        .min(budget.scalars.saturating_sub(step.scalars));
    let take = available.min(tail_len - state.tail_read);
    if take != 0 {
        let start = tail_start + state.tail_read;
        let end = start + take;
        job.source.visit_byte_range(start..end, |bytes| {
            job.source_borrow_max = job.source_borrow_max.max(bytes.len());
            for byte in bytes {
                state.tail[state.tail_read] = *byte;
                state.tail_read += 1;
                step.bytes += 1;
                step.scalars += 1;
            }
            Ok(())
        })?;
    }
    if state.tail_read != tail_len {
        return Ok(true);
    }
    job.cancellation.check()?;
    job.store.skip_fold(&span)?;
    job.machine
        .resume_after_fold(&span, &state.tail[..state.tail_read])?;
    state.next += 1;
    state.header_settled = false;
    state.tail = [0; 2];
    state.tail_read = 0;
    Ok(true)
}

/// 原查询默认完整 Source；显式 fold 内部则没有可见 SCX 权限，不能把后继 run 当当前结果。
pub(super) fn hidden(job: &MarkdownScriptJob, unit: MarkdownScriptUnit, offset: usize) -> bool {
    job.folds
        .as_ref()
        .is_some_and(|state| state.plan.hides(offset, unit == MarkdownScriptUnit::Utf16))
}
