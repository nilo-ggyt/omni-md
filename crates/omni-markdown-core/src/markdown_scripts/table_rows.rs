//! Settles original SCX obligations before a table row without inventing runs
//! inside it. Its actual trailing hard control restores subsequent body context.

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
    /// Shares a dynamic structural directory from this same source only. Ordinary
    /// text retains its original printable/frontier optimization.
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

/// Services only the current structural row in this quantum. Source bytes,
/// records, and bracket-stack operations retain their independent budgets.
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
        // Ordinary processing already passed this row; retire its cursor entry
        // without replaying a structural jump or duplicating script coverage.
        state.cursor.finish();
        return Ok(true);
    }
    if job.machine.byte < span.start.byte {
        // Earlier visible body text still belongs to ordinary SCX processing.
        return Ok(false);
    }
    if !state.jumped {
        // Settle actual earlier obligations before recording the one structural
        // hole. Only then may the machine move to the row's real tail controls.
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
        // Only the complete real CR/LF tail restores grapheme context and consumes
        // this row. Partial samples remain owned by this same pending state.
        job.machine.finish_table(span, &state.tail[..size])?;
        state.cursor.finish();
        state.jumped = false;
        state.read = 0;
        state.tail = [0; 2];
    }
    Ok(true)
}

/// Caps ordinary reads at an authorized row start. Positions lacking an original
/// row receipt continue under the original source-parsing rules.
pub(super) fn read_end(job: &MarkdownScriptJob, end: usize) -> usize {
    job.tables
        .as_ref()
        .and_then(|state| state.cursor.current())
        .map_or(end, |span| end.min(span.start.byte))
}

/// Synchronous script queries never reinterpret a structural-directory hole as
/// Common, Latin, or an actual raw run that was never produced.
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
