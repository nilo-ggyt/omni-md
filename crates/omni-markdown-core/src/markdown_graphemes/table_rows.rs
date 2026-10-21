//! The original GCB machine skips row bodies only across certified physical table edges.
//! Actual trailing controls still enter the same Unicode DFA.

use super::*;
use crate::markdown_layout_source::table_rows::{TableCursor, TablePlan};

pub(super) struct State {
    pub(super) cursor: TableCursor,
    jumped: bool,
}

impl MarkdownGraphemeJob {
    /// The coordinator installs this shared dynamic directory before reading the original job.
    /// Later row publication requires neither another Source nor a rebuilt index.
    pub(crate) fn install_table_plan(&mut self, plan: TablePlan) -> Result<()> {
        plan.require(&self.source)?;
        self.tables = Some(State {
            cursor: TableCursor::new(plan),
            jumped: false,
        });
        Ok(())
    }
}

/// Charge structural records and actual trailing control bytes to their original work dimensions;
/// skipped body length does not become byte reads or scalar decoding work.
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
        return Ok(true); // An existing real prefix already passed this short row; do not replay or erase its Unicode facts.
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
    // Read only remaining real CR/LF bytes, bounded independently by byte and scalar work.
    // A structural jump authorizes neither hidden text decoding nor fictional source reads.
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
            // Consume only this visitor's local machine-failure sentinel. A real Source
            // failure must propagate instead of being mistaken for a successful row tail.
            if error == MarkdownDocumentBufferError::RangeOutsideDocument && failure.is_some() {
                Ok(())
            } else {
                Err(error)
            }
        })?;
    if let Some(error) = failure {
        return Err(error);
    }
    // Commit the physical endpoint only after its complete real tail and with record
    // quota remaining; otherwise the same row stays pending for the next advance.
    if job.machine.byte == span.end.byte && step.records < budget.records {
        job.machine.finish_table(span, &mut job.index, step)?;
        state.cursor.finish();
        state.jumped = false;
    }
    Ok(true)
}

/// Ordinary scanning stops at the current certified row start without reading its body to guess an end.
pub(super) fn read_end(job: &MarkdownGraphemeJob, end: usize) -> usize {
    job.tables
        .as_ref()
        .and_then(|state| state.cursor.current())
        .map_or(end, |span| end.min(span.start.byte))
}
