//! Intersect grapheme and default line boundaries on the same frozen source, yielding at quota rather than searching without a bound.
//!
//! This stage certifies boundaries only. Width selection, mixed fonts, unsafe-break reshaping, dictionary support, and Native geometry
//! retain their own owners; a default Unicode opportunity is not an already laid-out visible soft line.

use std::rc::{Rc, Weak};

use thiserror::Error;

use crate::{
    MarkdownGraphemeAnswer as GraphemeAnswer, MarkdownGraphemeBudget, MarkdownGraphemeError,
    MarkdownGraphemeJob, MarkdownGraphemeQuery, MarkdownGraphemeResourceObserver,
    MarkdownGraphemeResourceSnapshot, MarkdownGraphemeUnit, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownLineBreakAnswer as LineAnswer, MarkdownLineBreakBoundary,
    MarkdownLineBreakBudget, MarkdownLineBreakError, MarkdownLineBreakJob, MarkdownLineBreakQuery,
    MarkdownLineBreakResourceObserver, MarkdownLineBreakResourceSnapshot, MarkdownLineBreakUnit,
};

#[derive(Debug, Error)]
pub enum MarkdownLayoutBreakError {
    #[error("断点源权限不匹配")]
    SourceMismatch,
    #[error("断点查询不属于原任务")]
    CursorMismatch,
    #[error("断点配额或范围无效")]
    InvalidRequest,
    #[error("断点任务已撤权或取消")]
    Poisoned,
    #[error("字素与断行的同一源坐标不一致")]
    InconsistentCoordinates,
    #[error("断点字素阶段失败")]
    Grapheme(#[from] MarkdownGraphemeError),
    #[error("断点默认断行阶段失败")]
    Line(#[from] MarkdownLineBreakError),
    #[error("断点原冻结源已经不可用")]
    Source(#[from] crate::MarkdownDocumentBufferError),
}
type Result<T> = std::result::Result<T, MarkdownLayoutBreakError>;

/// Check at most 64 candidates per query; a larger caller quota cannot turn this API into an unbounded document scan.
pub const MARKDOWN_LAYOUT_BREAK_CANDIDATES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLayoutBreakUnit {
    Byte,
    Utf16,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLayoutBreakQuery {
    At,
    Previous,
    Next,
}

/// Clone finite query state with weak owner identity and coordinates only, without retaining Source, indexes, fonts, or Native controls.
#[derive(Clone)]
pub struct MarkdownLayoutBreakCursor {
    owner: Weak<()>,
    unit: MarkdownLayoutBreakUnit,
    query: MarkdownLayoutBreakQuery,
    at: usize,
    candidate: Option<MarkdownLineBreakBoundary>,
    completed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLayoutBreakAnswer {
    /// The original algorithm prefix needs actual progress; unknown coverage does not mean an empty result.
    PendingSource,
    /// Candidate quota is exhausted; the same cursor retains the exact continuation without recapturing source or query intent.
    Yielded,
    Known(Option<MarkdownLineBreakBoundary>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownLayoutBreakStep {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub ready: bool,
    pub needs_dictionary: bool,
}

#[derive(Clone)]
pub struct MarkdownLayoutBreakResourceObserver {
    grapheme: MarkdownGraphemeResourceObserver,
    line: MarkdownLineBreakResourceObserver,
}
impl MarkdownLayoutBreakResourceObserver {
    #[must_use]
    pub fn snapshots(
        &self,
    ) -> (
        MarkdownGraphemeResourceSnapshot,
        MarkdownLineBreakResourceSnapshot,
    ) {
        (self.grapheme.snapshot(), self.line.snapshot())
    }
}

struct Payload {
    source: MarkdownLayoutTextSource,
    grapheme: MarkdownGraphemeJob,
    line: MarkdownLineBreakJob,
    // Endpoint tickets retain weak scope only; retiring this payload on failure, cancellation, or Drop revokes issued coordinates.
    #[cfg(feature = "paged-shaping-frontier")]
    endpoint_scope: Rc<()>,
}

/// Own two independent paged anonymous indexes; only the same frozen source capsule can authorize both algorithms.
pub struct MarkdownLayoutBreakJob {
    owner: Rc<()>,
    payload: Option<Payload>,
    authority: MarkdownLayoutTextAuthority,
    maximum: [usize; 2],
    grapheme_first: bool,
    resources: MarkdownLayoutBreakResourceObserver,
    // Only an actual raw-LB pending query requests line-source progress; GCB waits and candidate yields cannot masquerade as that demand.
    #[cfg(feature = "paged-shaping-frontier")]
    line_source_demand: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    grapheme_source_demand: bool,
}

impl MarkdownLayoutBreakJob {
    /// Share this Source's actual paragraph proof between original algorithms, whose cursors skip only explicitly hidden bodies.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn install_fold_plan(
        &mut self,
        plan: crate::markdown_layout_source::folds::FoldPlan,
    ) -> Result<()> {
        let authority = self.authority.clone();
        self.require_source(&authority)?;
        let result = (|| {
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLayoutBreakError::Poisoned)?;
            payload.grapheme.install_fold_plan(plan.clone())?;
            payload.line.install_fold_plan(plan)?;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => self.poison(error),
        }
    }

    /// Share only the original structural-row directory for table flow; both Unicode cursors retain their actual state and original quotas.
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(crate) fn install_table_plan(
        &mut self,
        plan: crate::markdown_layout_source::table_rows::TablePlan,
    ) -> Result<()> {
        let authority = self.authority.clone();
        self.require_source(&authority)?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?;
        payload.grapheme.install_table_plan(plan.clone())?;
        payload.line.install_table_plan(plan)?;
        Ok(())
    }

    /// Observe both original machines' actual canonical progress; a requested position cannot manufacture consumed coverage.
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(crate) fn table_passed(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        end: usize,
    ) -> Result<bool> {
        self.require_source(authority)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?;
        Ok(payload.grapheme.table_passed(end) && payload.line.table_passed(end))
    }

    /// Consumers distinguish original whole-source ordinals from this explicit visible sequence; neither domain authorizes the other.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn grapheme_ordinal_domain(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<crate::MarkdownGraphemeOrdinalDomain> {
        self.require_source(authority)?;
        Ok(self
            .payload
            .as_ref()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .ordinal_domain())
    }

    pub fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<Self> {
        // One captured Source identity creates both real algorithms; partial construction failure drops its already created child.
        if !source.permits(authority) {
            return Err(MarkdownLayoutBreakError::SourceMismatch);
        }
        source.health()?;
        let maximum = [source.byte_len(), source.utf16_len()];
        let grapheme = MarkdownGraphemeJob::new(source.clone())?;
        let line = MarkdownLineBreakJob::new(source.clone())?;
        let resources = MarkdownLayoutBreakResourceObserver {
            grapheme: grapheme.resource_observer(),
            line: line.resource_observer(),
        };
        Ok(Self {
            owner: Rc::new(()),
            payload: Some(Payload {
                source,
                grapheme,
                line,
                #[cfg(feature = "paged-shaping-frontier")]
                endpoint_scope: Rc::new(()),
            }),
            authority: authority.clone(),
            maximum,
            grapheme_first: true,
            resources,
            #[cfg(feature = "paged-shaping-frontier")]
            line_source_demand: false,
            #[cfg(feature = "paged-shaping-frontier")]
            grapheme_source_demand: false,
        })
    }

    /// Check actual health before consuming original Source authority; another capture of equal text/generation cannot impersonate this job.
    ///
    /// # Errors
    /// Actual source failure precedes identity mismatch; failure through this boundary drops both indexes and the Source strong edge.
    /// Do not recapture, restore old prefixes, or grant Native layout/input authority.
    pub fn require_source(&mut self, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
        let result = (|| {
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownLayoutBreakError::Poisoned)?;
            payload.source.health()?;
            if authority != &self.authority || !payload.source.permits(authority) {
                return Err(MarkdownLayoutBreakError::SourceMismatch);
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => self.poison(error), // Share the same actual failure-close boundary as step/query.
        }
    }

    /// Emergency soft-line candidates reuse this job's original GCB index, without another grapheme job or treating an unknown successor as EOF.
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn grapheme_query(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        offset: usize,
        query: MarkdownGraphemeQuery,
    ) -> Result<GraphemeAnswer> {
        self.require_source(authority)?;
        let result = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .query(authority, MarkdownGraphemeUnit::Utf16, offset, query);
        self.require_source(authority)?; // Recheck same-source health after child-index I/O to preserve actual source-error precedence.
        match result {
            Ok(answer) => {
                #[cfg(feature = "paged-shaping-frontier")]
                if answer == GraphemeAnswer::Pending {
                    self.grapheme_source_demand = true;
                }
                Ok(answer)
            }
            Err(error) => self.poison(error.into()),
        }
    }

    /// Read the same Ready GCB table's record count, without handing line output an incomplete prefix or another Source's table.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn ready_grapheme_count(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<Option<usize>> {
        self.require_source(authority)?;
        let result = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .ready_boundary_count(authority);
        self.require_source(authority)?;
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.poison(error.into()),
        }
    }

    /// Transfer at most 256 actual byte/UTF-16 records; false remains unknown, while error invalidates the batch and retires its original owner.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn read_ready_graphemes(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        range: std::ops::Range<usize>,
        output: &mut [crate::MarkdownGraphemeBoundary],
    ) -> Result<bool> {
        self.require_source(authority)?;
        let result = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .read_ready_boundaries(authority, range, output);
        self.require_source(authority)?;
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.poison(error.into()),
        }
    }

    fn poison<T>(&mut self, error: MarkdownLayoutBreakError) -> Result<T> {
        if matches!(
            &error,
            MarkdownLayoutBreakError::Grapheme(crate::MarkdownGraphemeError::FoldedRange)
                | MarkdownLayoutBreakError::Line(crate::MarkdownLineBreakError::FoldedRange)
        ) {
            return Err(error);
        } // Explicitly folded coverage is a domain mismatch, not index corruption; retain the healthy visible worker.
        drop(self.payload.take()); // Drop actual indexes/Source ownership; external cursors and observers keep no resource claims alive.
        Err(error)
    }

    /// Both algorithms share this call's total quota and alternate first turn, rather than each receiving a full copy of the budget.
    pub fn step(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineBreakBudget,
    ) -> Result<MarkdownLayoutBreakStep> {
        let result = self.advance(authority, budget);
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.poison(error),
        }
    }

    fn advance(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineBreakBudget,
    ) -> Result<MarkdownLayoutBreakStep> {
        // Children each check their actual original Source identity; never recapture the document to replace their authority.
        if budget.bytes == 0 || budget.scalars == 0 || budget.records == 0 {
            return Err(MarkdownLayoutBreakError::InvalidRequest);
        }
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?;
        payload.source.health()?; // Even zero-work/cache-hit stages cannot bypass permanent revocation of the shared Source.
        let mut total = MarkdownLayoutBreakStep {
            bytes: 0,
            scalars: 0,
            records: 0,
            ready: false,
            needs_dictionary: false,
        };
        for grapheme in [self.grapheme_first, !self.grapheme_first] {
            if total.bytes == budget.bytes
                || total.scalars == budget.scalars
                || total.records == budget.records
            {
                break; // With no remaining quota, the second algorithm waits for the next step instead of flushing EOF for free.
            }
            let bytes = budget.bytes - total.bytes;
            let scalars = budget.scalars - total.scalars;
            let records = budget.records - total.records;
            if grapheme {
                let step = payload.grapheme.step(
                    authority,
                    MarkdownGraphemeBudget {
                        bytes,
                        scalars,
                        records,
                    },
                )?;
                total.bytes += step.bytes;
                total.scalars += step.scalars;
                total.records += step.records;
            } else {
                let step = payload.line.step(
                    authority,
                    MarkdownLineBreakBudget {
                        bytes,
                        scalars,
                        records,
                    },
                )?;
                total.bytes += step.bytes;
                total.scalars += step.scalars;
                total.records += step.records;
            }
        }
        self.grapheme_first = !self.grapheme_first;
        total.ready = payload.grapheme.is_ready() && payload.line.is_ready();
        total.needs_dictionary = payload.line.storage_profile().needs_dictionary;
        Ok(total)
    }

    /// Create a read-only intent for this original job; out-of-range positions are rejected rather than clamped into valid EOF queries.
    pub fn cursor(
        &self,
        unit: MarkdownLayoutBreakUnit,
        at: usize,
        query: MarkdownLayoutBreakQuery,
    ) -> Result<MarkdownLayoutBreakCursor> {
        if self.payload.is_none() {
            return Err(MarkdownLayoutBreakError::Poisoned);
        }
        self.payload
            .as_ref()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .source
            .health()?;
        let maximum = match unit {
            MarkdownLayoutBreakUnit::Byte => self.maximum[0],
            MarkdownLayoutBreakUnit::Utf16 => self.maximum[1],
        };
        if at > maximum {
            return Err(MarkdownLayoutBreakError::InvalidRequest);
        }
        Ok(MarkdownLayoutBreakCursor {
            owner: Rc::downgrade(&self.owner),
            unit,
            query,
            at,
            candidate: None,
            completed: false,
        })
    }

    /// Check a bounded number of actual opportunities without allocating an intersection Vec or copying original indexes.
    pub fn query(
        &mut self,
        cursor: &mut MarkdownLayoutBreakCursor,
        candidate_budget: usize,
    ) -> Result<MarkdownLayoutBreakAnswer> {
        if !Weak::ptr_eq(&cursor.owner, &Rc::downgrade(&self.owner)) || cursor.completed {
            return Err(MarkdownLayoutBreakError::CursorMismatch); // Completed/foreign intent cannot consume this job.
        }
        if candidate_budget == 0 {
            return Err(MarkdownLayoutBreakError::InvalidRequest);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        {
            // Each original query reports its own blocking reason; a previous line wait cannot contaminate a later GCB or Known result.
            self.line_source_demand = false;
        }
        let result = self.query_inner(
            cursor,
            candidate_budget.min(MARKDOWN_LAYOUT_BREAK_CANDIDATES),
        );
        match result {
            Ok(answer) => Ok(answer),
            Err(error) => self.poison(error),
        }
    }

    fn query_inner(
        &mut self,
        cursor: &mut MarkdownLayoutBreakCursor,
        candidate_budget: usize,
    ) -> Result<MarkdownLayoutBreakAnswer> {
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?;
        payload.source.health()?; // Raw Line Known(None) must check health too; a later GCB branch cannot supply the missing check.
        let (line_unit, grapheme_unit) = match cursor.unit {
            MarkdownLayoutBreakUnit::Byte => {
                (MarkdownLineBreakUnit::Byte, MarkdownGraphemeUnit::Byte)
            }
            MarkdownLayoutBreakUnit::Utf16 => {
                (MarkdownLineBreakUnit::Utf16, MarkdownGraphemeUnit::Utf16)
            }
        };
        for _ in 0..candidate_budget {
            let candidate = if let Some(candidate) = cursor.candidate {
                candidate // Retry the same actual opportunity after a pending grapheme prefix, without skipping to its successor.
            } else {
                let query = match cursor.query {
                    MarkdownLayoutBreakQuery::At => MarkdownLineBreakQuery::IsBoundary,
                    MarkdownLayoutBreakQuery::Previous => MarkdownLineBreakQuery::Previous,
                    MarkdownLayoutBreakQuery::Next => MarkdownLineBreakQuery::Next,
                };
                match payload
                    .line
                    .query(&self.authority, line_unit, cursor.at, query)?
                {
                    LineAnswer::Pending => {
                        #[cfg(feature = "paged-shaping-frontier")]
                        {
                            self.line_source_demand = true;
                        }
                        return Ok(MarkdownLayoutBreakAnswer::PendingSource);
                    }
                    LineAnswer::Known(None) => {
                        cursor.completed = true;
                        return Ok(MarkdownLayoutBreakAnswer::Known(None));
                    }
                    LineAnswer::Known(Some(candidate)) => candidate,
                }
            };
            let position = match cursor.unit {
                MarkdownLayoutBreakUnit::Byte => candidate.byte,
                MarkdownLayoutBreakUnit::Utf16 => candidate.utf16,
            };
            match payload.grapheme.query(
                &self.authority,
                grapheme_unit,
                position,
                MarkdownGraphemeQuery::IsBoundary,
            )? {
                GraphemeAnswer::Pending => {
                    cursor.candidate = Some(candidate);
                    #[cfg(feature = "paged-shaping-frontier")]
                    {
                        self.grapheme_source_demand = true;
                    }
                    return Ok(MarkdownLayoutBreakAnswer::PendingSource);
                }
                GraphemeAnswer::Known(Some(boundary)) => {
                    if boundary.byte != candidate.byte || boundary.utf16 != candidate.utf16 {
                        return Err(MarkdownLayoutBreakError::InconsistentCoordinates);
                    }
                    cursor.completed = true;
                    return Ok(MarkdownLayoutBreakAnswer::Known(Some(candidate)));
                }
                GraphemeAnswer::Known(None) => {
                    if cursor.query == MarkdownLayoutBreakQuery::At {
                        cursor.completed = true;
                        return Ok(MarkdownLayoutBreakAnswer::Known(None));
                    }
                    // This original opportunity lies inside a grapheme; continue strict Previous/Next while preserving the same position at yield.
                    cursor.candidate = None;
                    cursor.at = position;
                }
            }
        }
        Ok(MarkdownLayoutBreakAnswer::Yielded)
    }

    #[must_use]
    pub fn resource_observer(&self) -> MarkdownLayoutBreakResourceObserver {
        self.resources.clone()
    }
    pub fn cancel(&mut self) {
        drop(self.payload.take()); // Return this job's two index owners without mutating the externally shared immutable Source.
    }
}

#[cfg(test)]
mod tests;

#[cfg(feature = "paged-shaping-frontier")]
mod prefix;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use prefix::MarkdownCertifiedGraphemeEndpoint;
