//! Same-source folds resume SCX/grapheme context only after real paragraph
//! terminators. Hidden regions never receive fabricated script runs.

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
    /// Installs the same layout plan before initial source consumption, sharing
    /// bounded proofs without another source or script policy.
    pub(crate) fn install_fold_plan(&mut self, plan: FoldPlan) -> Result<()> {
        self.guard(&self.authority.clone())?;
        if plan.authority() != &self.authority || self.machine.byte != 0 || self.machine.ready {
            // A later or foreign plan cannot reinterpret text already consumed under
            // this job's original visible-source and script-context authority.
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
        } // A whole-root aggregate cannot lend hidden-region context to visible runs.
        Ok(())
    }
}

/// Caps ordinary scanning at this fold's actual header line ending, preventing
/// hidden-body reads before the explicit structural jump.
pub(super) fn read_end(job: &MarkdownScriptJob, requested: usize) -> usize {
    job.folds
        .as_ref()
        .and_then(|state| state.plan.spans().get(state.next))
        .map_or(requested, |span| requested.min(span.header_end.byte))
}

/// True means this quantum serviced real paragraph settlement or tail sampling.
/// Source, cancellation, and actual metadata failures still propagate unchanged.
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
        // Complete earlier cluster/bracket actions through the same machine before
        // transferring any structural coverage beyond this real header terminator.
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
    // Do not reread a terminator shared with the header. Otherwise sample only
    // the actual tail CR/LF, never more than two bytes.
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
        // The same fold retains its partial tail sample; a short quantum cannot
        // authorize a jump or manufacture the missing terminal context.
        return Ok(true);
    }
    // Recheck cancellation immediately before changing storage coverage. The
    // original proof, sampled controls, and machine must agree on this one jump.
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

/// Ordinary queries cover the whole source, but explicit folds grant no visible
/// SCX result inside hidden text. A later run cannot stand in for that position.
pub(super) fn hidden(job: &MarkdownScriptJob, unit: MarkdownScriptUnit, offset: usize) -> bool {
    job.folds
        .as_ref()
        .is_some_and(|state| state.plan.hides(offset, unit == MarkdownScriptUnit::Utf16))
}
