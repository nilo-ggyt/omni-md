//! Capture historical items while the original measure/job is still live, then
//! borrow the original sealed output file in bounded batches.
use super::*;
use crate::{
    MarkdownShapedGlyph, MarkdownShapedSpool, MarkdownShapedSpoolError,
    MarkdownShapingCancellation, MarkdownShapingDirection,
    markdown_shaping::MarkdownSpoolSliceOrigin,
};

/// Maximum real glyph records read by one step or delivered in one batch.
pub const MARKDOWN_SAFE_SPOOL_SLICE_GLYPHS: usize = 256;
#[derive(Debug, Error)]
pub enum MarkdownSafeSpoolSliceError {
    #[error("历史item尚未被原测量完整消费，或最终机会仍需重整形")]
    NeedsResolution,
    #[error("历史item不在原已选行内")]
    OutsideSelectedLine,
    #[error("历史item字形范围、配额或累计值不一致")]
    InconsistentCoverage,
    #[error("历史item字形坐标或advance溢出")]
    Overflow,
    #[error("历史item收据已经撤权")]
    Poisoned,
    #[error("历史item原测量已取消")]
    Cancelled,
    #[error("历史item原源失败")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("历史item原测量失败")]
    Measure(#[from] MarkdownSoftLineMeasureError),
    #[error("历史item原字形任务失败")]
    Shaping(#[from] MarkdownShapingError),
    #[error("历史item原断点索引失败")]
    Break(#[from] MarkdownLayoutBreakError),
    #[error("历史item原只读输出失败")]
    Spool(#[from] MarkdownShapedSpoolError),
}
type Result<T> = std::result::Result<T, MarkdownSafeSpoolSliceError>;

/// Weak line authority and shared cancellation/revocation facts survive valid
/// continuation, while failed or dropped original owners revoke that authority.
struct LineProof {
    measure: Weak<()>,
    alive: Arc<AtomicBool>,
    cancellation: MarkdownSoftLineMeasureCancellation,
}
impl LineProof {
    /// Require the original line's live operation without retaining its measure.
    fn require(&self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            // Cancellation ends all historical item permissions for this line.
            return Err(MarkdownSafeSpoolSliceError::Cancelled);
        }
        if !self.alive.load(Ordering::Acquire) || self.measure.upgrade().is_none() {
            // A retained receipt cannot revive a retired or failed line owner.
            return Err(MarkdownSafeSpoolSliceError::Poisoned);
        }
        Ok(())
    }
}

/// A non-Clone historical receipt is captured at NeedNextItem while its original
/// job is live. A later same-named font cannot issue that original permission.
pub struct MarkdownMeasuredItemSpoolProof {
    origin: MarkdownSpoolSliceOrigin,
    line: LineProof,
    measured: Range<usize>,
}
impl MarkdownMeasuredItemSpoolProof {
    /// Authenticate one measured item's real job and exact sealed spool before
    /// continuation retires that job and its font payload.
    pub fn capture(
        measure: &MarkdownSoftLineMeasureJob,
        job: &mut MarkdownShapingJob,
        spool: &mut MarkdownShapedSpool,
    ) -> Result<Self> {
        use crate::markdown_soft_line_measure::provider::JobGlyphProvider;
        let permit = measure
            .measured_item_permit(&mut JobGlyphProvider::new(job, None))
            .map_err(|error| match error {
                MarkdownSoftLineMeasureError::InvalidRequest => {
                    // Preserve the public phase error for a measure that has
                    // not reached its real NeedNextItem boundary.
                    MarkdownSafeSpoolSliceError::NeedsResolution
                }
                other => MarkdownSafeSpoolSliceError::Measure(other),
            })?;
        let facts = permit.require(&mut JobGlyphProvider::new(job, None))?;
        let origin = spool.safe_slice_origin(&facts.source)?;
        spool.require_safe_slice_job(&origin, job)?;
        if origin.range != facts.source_utf16
            || origin.count != facts.glyphs
            || origin.generation != facts.generation
            || origin.direction != facts.direction
            || facts.origin
                != crate::markdown_soft_line_measure::provider::GlyphProviderOrigin::Job(
                    job.index_identity(),
                )
        {
            // Source extent, glyph count, generation, direction, and original
            // job identity must all describe this same captured output.
            return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
        }
        job.glyph(&facts.source, &facts.font, facts.glyphs)?;
        // Recheck the same permit after actual output/source access; no later
        // job can supply a fresh authority to bless this historical item.
        permit.require(&mut JobGlyphProvider::new(job, None))?;
        Ok(Self {
            origin,
            measured: facts.measured_utf16,
            line: LineProof {
                measure: facts.line.owner,
                alive: facts.line.alive,
                cancellation: facts.line.cancellation,
            },
        }) // Share Store's real NeedNextItem boundary while retaining only the
        // finite proof, rather than cloning a complete Source for capture.
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownSafeSpoolSliceStep {
    pub glyphs: usize,
    pub ready: bool,
    pub pending_dependencies: bool,
}
#[derive(Clone, Copy)]
enum Phase {
    StartGcb,
    EndGcb,
    SeekStart { low: usize, high: usize },
    SeekEnd { low: usize, high: usize },
    CheckStart,
    CheckEnd,
    Accumulate { rank: usize },
    CheckAfter { rank: usize },
    Ready,
}
enum Progress {
    Metadata,
    Read,
    Pending,
}

/// Each turn reads at most 256 real records. Binary search, flag checks, and
/// advance accumulation operate incrementally on the original sealed file.
pub struct MarkdownSafeSpoolSliceJob {
    proof: Option<MarkdownMeasuredItemSpoolProof>,
    phase: Phase,
    range: Range<usize>,
    first: usize,
    last: usize,
    x: i64,
    y: i64,
    poisoned: bool,
    shape_cancellation: MarkdownShapingCancellation,
}
impl MarkdownSafeSpoolSliceJob {
    /// Intersect the selected line with an already captured historical item,
    /// keeping the original line/Source and spool instance identities.
    pub fn new(
        measure: &MarkdownSoftLineMeasureJob,
        current: &mut MarkdownShapingJob,
        spool: &mut MarkdownShapedSpool,
        proof: MarkdownMeasuredItemSpoolProof,
    ) -> Result<Self> {
        let seed = measure.resolve_seed(current)?;
        spool.require_safe_slice_origin(&proof.origin)?;
        proof.line.require()?;
        if !Weak::ptr_eq(&seed.owner, &proof.line.measure) || seed.authority != proof.origin.source
        {
            // Equal text or a different live measure cannot replace the
            // original line owner that issued this historical receipt.
            return Err(MarkdownSafeSpoolSliceError::Poisoned);
        }
        let MarkdownSoftLineMeasureChoice::MeasuredBreak(selected) = &seed.choice else {
            // Emergency and unresolved outcomes still require the resolver.
            return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
        };
        let range = selected.source_utf16.start.max(proof.origin.range.start)
            ..selected.source_utf16.end.min(proof.origin.range.end);
        if range.is_empty() {
            // This item contributes no glyphs to the selected source line.
            return Err(MarkdownSafeSpoolSliceError::OutsideSelectedLine);
        }
        if range.start < proof.measured.start || range.end > proof.measured.end {
            // An item's unconsumed suffix has no measured historical proof.
            return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
        }
        seed.source.health()?;
        Ok(Self {
            proof: Some(proof),
            phase: Phase::StartGcb,
            range,
            first: 0,
            last: 0,
            x: 0,
            y: 0,
            poisoned: false,
            shape_cancellation: current.cancellation(),
        })
    }
    /// Borrow the single proof until seal consumes it; absence is irreversible.
    fn proof(&self) -> Result<&MarkdownMeasuredItemSpoolProof> {
        self.proof
            .as_ref()
            .ok_or(MarkdownSafeSpoolSliceError::Poisoned)
    }
    /// Check original Source/I/O before cancellation and metadata revocation at
    /// both ends of a bounded step.
    fn guard(
        &self,
        measure: &MarkdownSoftLineMeasureJob,
        current: &mut MarkdownShapingJob,
        spool: &mut MarkdownShapedSpool,
    ) -> Result<()> {
        let proof = self.proof()?;
        spool.require_safe_slice_origin(&proof.origin)?;
        measure.require_resolve(&proof.line.measure, current)?;
        proof.line.require()?;
        if self.poisoned {
            // A previous failed step cannot resume after external recovery.
            return Err(MarkdownSafeSpoolSliceError::Poisoned);
        }
        Ok(())
    }
    /// Read one real record by logical rank using its original file authority.
    fn glyph(&self, spool: &mut MarkdownShapedSpool, rank: usize) -> Result<MarkdownShapedGlyph> {
        let origin = &self.proof()?.origin;
        let index = visual_index(origin, rank)?;
        spool
            .glyph(&origin.authority, &origin.source, &origin.font, index)?
            .ok_or(MarkdownSafeSpoolSliceError::InconsistentCoverage)
    }
    /// Advance logical rank in source order. RTL selects reverse visual indices
    /// without reversing glyph records or changing their source clusters.
    fn advance_one(
        &mut self,
        spool: &mut MarkdownShapedSpool,
        breaks: &mut MarkdownLayoutBreakJob,
    ) -> Result<Progress> {
        let origin = self.proof()?.origin.clone();
        match self.phase {
            Phase::StartGcb | Phase::EndGcb => {
                // Both source endpoints require completed GCB proof before any
                // glyph rank can authorize a slice boundary.
                let start = matches!(self.phase, Phase::StartGcb);
                let at = if start {
                    self.range.start
                } else {
                    self.range.end
                };
                match breaks.grapheme_query(
                    &origin.source,
                    at,
                    MarkdownGraphemeQuery::IsBoundary,
                )? {
                    MarkdownGraphemeAnswer::Pending => return Ok(Progress::Pending),
                    MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == at => {}
                    _ => return Err(MarkdownSafeSpoolSliceError::NeedsResolution),
                }
                self.phase = if start {
                    Phase::EndGcb
                } else {
                    Phase::SeekStart {
                        low: 0,
                        high: origin.count,
                    }
                };
                Ok(Progress::Metadata)
            }
            Phase::SeekStart { low, high } | Phase::SeekEnd { low, high } => {
                // Search lower bounds in original logical-cluster order; each
                // actual record read consumes one unit of this turn's budget.
                let start = matches!(self.phase, Phase::SeekStart { .. });
                let target = if start {
                    self.range.start
                } else {
                    self.range.end
                };
                if low == high {
                    // Finished searches publish only numeric ranks. Separate
                    // endpoint checks still establish exact cluster coverage.
                    if start {
                        self.first = low;
                        self.phase = Phase::SeekEnd {
                            low: 0,
                            high: origin.count,
                        };
                    } else {
                        self.last = low;
                        self.phase = Phase::CheckStart;
                    }
                    return Ok(Progress::Metadata);
                }
                let middle = low + (high - low) / 2;
                let glyph = self.glyph(spool, middle)?;
                let (low, high) = if glyph.source_utf16_cluster < target {
                    (middle + 1, high)
                } else {
                    (low, middle)
                };
                self.phase = if start {
                    Phase::SeekStart { low, high }
                } else {
                    Phase::SeekEnd { low, high }
                };
                Ok(Progress::Read)
            }
            Phase::CheckStart => {
                if self.first > self.last || self.last > origin.count {
                    // Reject inconsistent ranks before subtraction/indexing.
                    return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
                }
                self.phase = Phase::CheckEnd;
                if self.range.start != origin.range.start {
                    // An interior start must equal a real cluster start; the
                    // complete item start already has its original authority.
                    if self.first == origin.count
                        || self.glyph(spool, self.first)?.source_utf16_cluster != self.range.start
                    {
                        return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
                    }
                    Ok(Progress::Read)
                } else {
                    Ok(Progress::Metadata)
                }
            }
            Phase::CheckEnd => {
                self.phase = Phase::Accumulate { rank: self.first };
                if self.range.end != origin.range.end {
                    // An interior end must coincide with the following cluster
                    // start; an arbitrary cut inside a cluster needs resolution.
                    if self.last == origin.count
                        || self.glyph(spool, self.last)?.source_utf16_cluster != self.range.end
                    {
                        return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
                    }
                    Ok(Progress::Read)
                } else {
                    Ok(Progress::Metadata)
                }
            }
            Phase::Accumulate { rank } => {
                if rank == self.last {
                    // End-of-slice accumulation still leaves the following
                    // cluster's unsafe-break flags to check before readiness.
                    self.phase = Phase::CheckAfter { rank: self.last };
                    return Ok(Progress::Metadata);
                }
                let glyph = self.glyph(spool, rank)?;
                if !self.range.contains(&glyph.source_utf16_cluster) || glyph.glyph_id == 0 {
                    // Missing glyphs and records outside certified source
                    // coverage cannot become a drawable measured prefix.
                    return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
                }
                if self.range.start != origin.range.start
                    && glyph.source_utf16_cluster == self.range.start
                    && glyph.flags & UNSAFE != 0
                {
                    return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
                } // Check every glyph flag in the starting cluster; a clear
                // first-glyph flag cannot certify the remaining records.
                self.x = self
                    .x
                    .checked_add(i64::from(glyph.x_advance))
                    .ok_or(MarkdownSafeSpoolSliceError::Overflow)?;
                self.y = self
                    .y
                    .checked_add(i64::from(glyph.y_advance))
                    .ok_or(MarkdownSafeSpoolSliceError::Overflow)?;
                self.phase = Phase::Accumulate { rank: rank + 1 };
                Ok(Progress::Read)
            }
            Phase::CheckAfter { rank } => {
                // UNSAFE_TO_BREAK belongs to the following complete cluster.
                // Its records may span several bounded turns.
                if self.range.end == origin.range.end || rank == origin.count {
                    self.phase = Phase::Ready;
                    return Ok(Progress::Metadata);
                }
                let glyph = self.glyph(spool, rank)?;
                if glyph.source_utf16_cluster != self.range.end {
                    self.phase = Phase::Ready;
                } else {
                    if glyph.flags & UNSAFE != 0 {
                        return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
                    }
                    self.phase = Phase::CheckAfter { rank: rank + 1 };
                } // A long following cluster remains unfinished across turns;
                // inspecting only its first record cannot establish Ready.
                Ok(Progress::Read)
            }
            Phase::Ready => Ok(Progress::Metadata),
        }
    }
    /// Spend one finite record budget, yielding on incomplete grapheme data and
    /// checking original authority again before publishing progress.
    pub fn step(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        current: &mut MarkdownShapingJob,
        spool: &mut MarkdownShapedSpool,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: usize,
    ) -> Result<MarkdownSafeSpoolSliceStep> {
        let result = (|| {
            self.guard(measure, current, spool)?;
            if budget == 0 || budget > MARKDOWN_SAFE_SPOOL_SLICE_GLYPHS {
                // Callers cannot enlarge this job's independent read allowance.
                return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
            }
            let mut glyphs = 0;
            let mut pending_dependencies = false;
            while glyphs < budget && !matches!(self.phase, Phase::Ready) {
                match self.advance_one(spool, breaks)? {
                    Progress::Read => glyphs += 1,
                    Progress::Metadata => {}
                    Progress::Pending => {
                        // The dependency owner must advance; this job does not
                        // spin or fabricate a boundary from unknown coverage.
                        pending_dependencies = true;
                        break;
                    }
                } // Metadata-only transitions are finite; actual glyph records
                // consume this turn's read quota.
            }
            self.guard(measure, current, spool)?;
            Ok(MarkdownSafeSpoolSliceStep {
                glyphs,
                ready: matches!(self.phase, Phase::Ready),
                pending_dependencies,
            })
        })();
        if result.is_err() {
            self.poisoned = true;
            // A partially accumulated candidate remains unusable permanently.
        }
        result
    }
    /// Consume a fully verified candidate into a read ticket for its original
    /// spool, without opening another descriptor or copying the output file.
    pub fn seal(
        mut self,
        measure: &MarkdownSoftLineMeasureJob,
        current: &mut MarkdownShapingJob,
        spool: &mut MarkdownShapedSpool,
    ) -> Result<MarkdownSafeSpoolSlice> {
        self.guard(measure, current, spool)?;
        if !matches!(self.phase, Phase::Ready) {
            // Numeric ranks alone cannot bypass pending boundary/flag checks.
            return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
        }
        let proof = self
            .proof
            .take()
            .ok_or(MarkdownSafeSpoolSliceError::Poisoned)?;
        let backwards = backwards(&proof.origin);
        let visual = if backwards {
            // Original RTL logical ranks map to the reverse visual interval.
            proof.origin.count - self.last..proof.origin.count - self.first
        } else {
            // LTR keeps the same half-open ranks in the original output array.
            self.first..self.last
        };
        Ok(MarkdownSafeSpoolSlice {
            proof,
            cancellation: self.shape_cancellation,
            facts: MarkdownSafeGlyphSliceFacts {
                source_utf16: self.range,
                visual_glyphs: visual,
                x_advance_26_6: self.x,
                y_advance_26_6: self.y,
            },
            poisoned: false,
        })
    }
}
/// Direction belongs to the captured original spool, not a current font/job.
fn backwards(origin: &MarkdownSpoolSliceOrigin) -> bool {
    matches!(origin.direction, MarkdownShapingDirection::RightToLeft)
}
/// Convert one in-range logical rank without reversing or copying file data.
fn visual_index(origin: &MarkdownSpoolSliceOrigin, rank: usize) -> Result<usize> {
    if rank >= origin.count {
        // Reject before RTL subtraction can underflow at or beyond EOF.
        return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
    }
    Ok(if backwards(origin) {
        origin.count - rank - 1
    } else {
        rank
    })
}

/// Only the completed job issues this specific spool ticket. Public coordinate
/// facts do not construct or replace its original file/line authority.
pub struct MarkdownSafeSpoolSlice {
    proof: MarkdownMeasuredItemSpoolProof,
    cancellation: MarkdownShapingCancellation,
    facts: MarkdownSafeGlyphSliceFacts,
    poisoned: bool,
}
impl MarkdownSafeSpoolSlice {
    #[must_use]
    /// Copy fixed coordinates while keeping the single-use ticket private.
    pub fn facts(&self) -> MarkdownSafeGlyphSliceFacts {
        self.facts.clone()
    }
    /// Recheck original file health before cancellation and permanent rejection.
    fn require(&mut self, spool: &mut MarkdownShapedSpool) -> Result<()> {
        let result = (|| {
            spool.require_safe_slice_origin(&self.proof.origin)?;
            self.proof.line.require()?;
            if self.cancellation.requested() {
                // Original shaping cancellation ends delivery of this output.
                return Err(MarkdownSafeSpoolSliceError::Cancelled);
            }
            if self.poisoned {
                // A previously failed read cannot resume through another view.
                return Err(MarkdownSafeSpoolSliceError::Poisoned);
            }
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
            // Preserve rejection even if a later call has otherwise live facts.
        }
        result
    }
    /// Consume the ticket into an exclusive borrow of its actual sealed spool.
    pub fn view(
        mut self,
        spool: &mut MarkdownShapedSpool,
    ) -> Result<MarkdownSafeSpoolSliceView<'_>> {
        self.require(spool)?;
        Ok(MarkdownSafeSpoolSliceView { slice: self, spool })
    }
}
/// Rust's exclusive borrow prevents closing/replacing the original file during
/// reads, without creating another reader owner, mapping, or descriptor.
pub struct MarkdownSafeSpoolSliceView<'a> {
    slice: MarkdownSafeSpoolSlice,
    spool: &'a mut MarkdownShapedSpool,
}
impl MarkdownSafeSpoolSliceView<'_> {
    #[must_use]
    /// Observe the same finite facts while retaining the exclusive spool borrow.
    pub fn facts(&self) -> MarkdownSafeGlyphSliceFacts {
        self.slice.facts()
    }
    /// Read one complete bounded visual interval. A failure invalidates the
    /// entire caller buffer and permanently rejects the consumed slice ticket.
    pub fn read_batch(
        &mut self,
        range: Range<usize>,
        output: &mut [MarkdownShapedGlyph],
    ) -> Result<usize> {
        let result = (|| {
            self.slice.require(self.spool)?;
            let facts = &self.slice.facts;
            if range.start > range.end
                || range.end > facts.visual_glyphs.len()
                || range.len() != output.len()
                || range.len() > MARKDOWN_SAFE_SPOOL_SLICE_GLYPHS
            {
                // The caller supplies exactly one relative interval and output
                // span within both certified coverage and the fixed read quota.
                return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
            }
            let origin = &self.slice.proof.origin;
            let absolute =
                facts.visual_glyphs.start + range.start..facts.visual_glyphs.start + range.end;
            self.spool.read_batch(
                &origin.authority,
                &origin.source,
                &origin.font,
                absolute,
                output,
            )?;
            if output
                .iter()
                .any(|glyph| !facts.source_utf16.contains(&glyph.source_utf16_cluster))
            {
                // The sealed file must still associate every returned glyph
                // with this same certified source interval.
                return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
            }
            self.slice.require(self.spool)?;
            Ok(output.len())
        })();
        if result.is_err() {
            self.slice.poisoned = true;
        }
        result // A partially filled buffer grants no deliverable prefix on Err.
    }
}
