//! Table hard boundaries settle earlier actual line-break tokens, then restore
//! the same DFA from its trailing control. Table interiors grant no break authority.

use super::*;
use crate::markdown_layout_source::table_rows::{TableCursor, TablePlan};

pub(super) struct State {
    pub(super) cursor: TableCursor,
    jumped: bool,
}

impl MarkdownLineBreakJob {
    /// Shares the same initially empty dynamic directory before consuming body text.
    /// Later table rows append only their original structural facts.
    pub(crate) fn install_table_plan(&mut self, plan: TablePlan) -> Result<()> {
        plan.require(&self.source)?;
        self.tables = Some(State {
            cursor: TableCursor::new(plan),
            jumped: false,
        });
        Ok(())
    }
}

/// Every step still charges records, bytes, and scalars. Row-body length never
/// becomes a whole-row text lease or a retained array.
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
        // The original decoder already passed this structural row. Consume its
        // cursor once without replaying a jump or generating duplicate boundaries.
        state.cursor.finish();
        return Ok(true);
    }
    if job.machine.byte_offset() < span.start.byte as u64 {
        // Earlier ordinary body text still needs the original line-breaking rules.
        return Ok(false);
    }
    if !state.jumped {
        // Settle every earlier actual pending token before abandoning table-body
        // context. Each rule transition remains charged even when it emits no break.
        while job.machine.has_before_table(span.start.byte) {
            if step.records == budget.records {
                return Ok(true);
            }
            if let Some(event) = job.machine.drain_before_table(span.start.byte) {
                job.index.push(event, job.machine.needs_dictionary())?;
            }
            step.records += 1; // Settling a real pending rule costs one operation even when no break is emitted.
        }
        job.machine.begin_table_tail(span);
        state.jumped = true;
    }
    let start = job.machine.byte_offset() as usize;
    let end = span
        .end
        .byte
        .min(start.saturating_add((budget.bytes - step.bytes).min(budget.scalars - step.scalars)));
    // The table proof grants only this real trailing-control extent. Feed it
    // through the same DFA byte by byte, never parse or copy the row interior.
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
            // This local sentinel only transports the actual machine failure out
            // of the bounded visitor. Real source/lease errors retain precedence.
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
        // Only complete tail consumption retires this row; a short quota retains
        // the same decoder/cursor state for the next admitted quantum.
        state.cursor.finish();
        state.jumped = false;
    }
    Ok(true)
}

/// Uses already charged original rows to bound reads; parser budget cannot
/// authorize obtaining an unpublished successor row.
pub(super) fn read_end(job: &MarkdownLineBreakJob, end: usize) -> usize {
    job.tables
        .as_ref()
        .and_then(|state| state.cursor.current())
        .map_or(end, |span| end.min(span.start.byte))
}
