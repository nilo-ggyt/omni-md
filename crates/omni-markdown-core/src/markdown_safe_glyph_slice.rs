//! Glyph slices at original measured safe breaks borrow real subranges of the
//! original Ready output, preserving the remaining shaped text.
//!
//! Factories consume the same measure's selected opportunity and recheck its
//! grapheme/cluster endpoints and UNSAFE_TO_BREAK flags. Complex boundaries stay
//! with the actual resolver; slice metadata alone grants no Native readiness.
use crate::{
    MarkdownDocumentBufferError, MarkdownGlyphCluster, MarkdownGlyphClusterAnswer,
    MarkdownGlyphClusterError, MarkdownGlyphClusterIndex, MarkdownGlyphClusterResourceObserver,
    MarkdownGlyphClusterStatus, MarkdownGraphemeAnswer, MarkdownGraphemeQuery,
    MarkdownLayoutBreakError, MarkdownLayoutBreakJob, MarkdownLayoutFontAuthority,
    MarkdownLayoutTextAuthority, MarkdownOriginalBackingObserver, MarkdownShapingError,
    MarkdownShapingJob, MarkdownShapingStatus, MarkdownSoftLineMeasureCancellation,
    MarkdownSoftLineMeasureChoice, MarkdownSoftLineMeasureError, MarkdownSoftLineMeasureJob,
    markdown_soft_line_measure::MarkdownSoftLineResolveSeed,
};
use std::{
    ops::Range,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use thiserror::Error;
mod spool;
pub use spool::{
    MARKDOWN_SAFE_SPOOL_SLICE_GLYPHS, MarkdownMeasuredItemSpoolProof, MarkdownSafeSpoolSlice,
    MarkdownSafeSpoolSliceError, MarkdownSafeSpoolSliceJob, MarkdownSafeSpoolSliceStep,
    MarkdownSafeSpoolSliceView,
};

const UNSAFE: u32 = harfrust::GlyphFlags::UNSAFE_TO_BREAK.to_bits();

#[derive(Debug, Error)]
pub enum MarkdownSafeGlyphSliceError {
    #[error("安全切片必须来自原测量的实际安全断点")]
    NeedsResolution,
    #[error("安全切片依赖尚未完成")]
    PendingDependencies,
    #[error("安全切片不属于原任务或原测量")]
    OwnerMismatch,
    #[error("安全切片原测量已取消")]
    Cancelled,
    #[error("安全切片已经永久撤权")]
    Poisoned,
    #[error("安全切片坐标、字形或advance不一致")]
    InconsistentCoverage,
    #[error("安全切片坐标或advance溢出")]
    Overflow,
    #[error("安全切片原源失败")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("安全切片原字形失败")]
    Shaping(#[from] MarkdownShapingError),
    #[error("安全切片原测量失败")]
    Measure(#[from] MarkdownSoftLineMeasureError),
    #[error("安全切片原cluster索引失败")]
    Cluster(#[from] MarkdownGlyphClusterError),
    #[error("安全切片原断点索引失败")]
    Break(#[from] MarkdownLayoutBreakError),
}
type Result<T> = std::result::Result<T, MarkdownSafeGlyphSliceError>;

/// Facts describe original shaping output. Native geometry, fallback selection,
/// whole-line bidi ordering, and permission to draw require their own owners.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSafeGlyphSliceFacts {
    pub source_utf16: Range<usize>,
    pub visual_glyphs: Range<usize>,
    pub x_advance_26_6: i64,
    pub y_advance_26_6: i64,
}

/// Private, non-Clone ownership carries weak authorities and fixed metadata;
/// it keeps no document, Source, font, or cluster-index descriptor alive.
pub struct MarkdownSafeGlyphSlice {
    facts: MarkdownSafeGlyphSliceFacts,
    shape: Weak<()>,
    measure: Weak<()>,
    revoked: Arc<AtomicBool>,
    cancellation: MarkdownSoftLineMeasureCancellation,
    index: MarkdownGlyphClusterResourceObserver,
    backing: Option<MarkdownOriginalBackingObserver>,
    pub(crate) source: MarkdownLayoutTextAuthority,
    pub(crate) font: MarkdownLayoutFontAuthority,
    count: usize,
    generation: u64,
    poisoned: bool,
}
impl MarkdownSafeGlyphSlice {
    #[must_use]
    /// Copy finite output coordinates without extending any shaping authority.
    pub fn facts(&self) -> MarkdownSafeGlyphSliceFacts {
        self.facts.clone()
    }

    /// Reborrow the original job for every batch and final publication. Source
    /// and job I/O failures take precedence over cancellation or expired tickets.
    pub(crate) fn require(&mut self, job: &mut MarkdownShapingJob) -> Result<()> {
        let result = (|| {
            // An already observed backing failure remains the actual error even
            // if a later task cancellation would also reject this slice.
            if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
                return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
            }
            let profile = job.profile()?;
            job.glyph(&self.source, &self.font, self.count)?;
            // Equal array sizes cannot substitute for the original shaping
            // allocation and generation from which these coordinates were made.
            if !Weak::ptr_eq(&self.shape, &job.index_identity())
                || profile.glyph_count != self.count
                || profile.source_generation != self.generation
            {
                return Err(MarkdownSafeGlyphSliceError::OwnerMismatch);
            }
            if self.cancellation.is_cancelled() {
                // Cancellation belongs to the original measure and ends every
                // slice derived from that operation.
                return Err(MarkdownSafeGlyphSliceError::Cancelled);
            }
            if self.poisoned
                || self.revoked.load(Ordering::Acquire)
                || self.measure.upgrade().is_none()
            {
                // A failed check is permanent; a surviving shaping job cannot
                // revive a retired measure or its revoked publication ticket.
                return Err(MarkdownSafeGlyphSliceError::Poisoned);
            }
            let index = self.index.snapshot();
            if index.owner_closed || index.cleanup_failed {
                return Err(MarkdownSafeGlyphSliceError::Poisoned);
            } // Index retirement closes its real owner. Single-page indexes may
            // have no FD; this observer cannot retain or reopen their buffer.
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        } // Later coincidental external recovery never restores this slice.
        result
    }

    /// Consume only the original emergency selector's private proof. That
    /// selector advances continuation start while the measure retains its start.
    pub(crate) fn from_emergency_selection(
        choice: &crate::markdown_soft_line_resolve::MarkdownEmergencyResolvedChoice<'_>,
        measure: &MarkdownSoftLineMeasureJob,
        job: &mut MarkdownShapingJob,
        breaks: &mut MarkdownLayoutBreakJob,
        index: &mut MarkdownGlyphClusterIndex,
    ) -> Result<Self> {
        let seed = choice.seed();
        seed.source.health()?;
        measure.require_resolve(&seed.owner, job)?;
        if index.status() != MarkdownGlyphClusterStatus::Ready {
            // Incomplete cluster coverage cannot certify either slice endpoint.
            return Err(MarkdownSafeGlyphSliceError::PendingDependencies);
        }
        let facts = facts(seed, job, breaks, index, choice.range())?;
        if facts.x_advance_26_6 != choice.width() {
            // The selector and actual cluster prefix difference must describe
            // the same width before any candidate can be published.
            return Err(MarkdownSafeGlyphSliceError::InconsistentCoverage);
        }
        let value = Self::make(seed, job, index, facts)?;
        // Metadata reads can fail or retire authority; check the original
        // Source and measure again before returning the constructed candidate.
        seed.source.health()?;
        measure.require_resolve(&seed.owner, job)?;
        Ok(value)
    }

    /// Share the same operation's cancellation signal without owning its job.
    pub(crate) fn cancellation(&self) -> MarkdownSoftLineMeasureCancellation {
        self.cancellation.clone()
    }

    /// Bind validated coordinates to original weak owners, then perform the
    /// same live-job check required by later consumers.
    fn make(
        seed: &MarkdownSoftLineResolveSeed,
        job: &mut MarkdownShapingJob,
        index: &MarkdownGlyphClusterIndex,
        facts: MarkdownSafeGlyphSliceFacts,
    ) -> Result<Self> {
        let profile = job.profile()?;
        let source = job.source_range();
        if facts.visual_glyphs.start > facts.visual_glyphs.end
            || facts.visual_glyphs.end > profile.glyph_count
            || facts.source_utf16.is_empty()
            || facts.source_utf16.start < source.start
            || facts.source_utf16.end > source.end
        {
            return Err(MarkdownSafeGlyphSliceError::InconsistentCoverage);
        } // A corrupt page record must fail before its range can authorize
        // out-of-bounds array access or overflowing index arithmetic.
        let mut value = Self {
            facts,
            shape: job.index_identity(),
            measure: seed.owner.clone(),
            revoked: Arc::clone(&seed.revoked),
            cancellation: seed.cancellation.clone(),
            index: index.resource_observer(),
            backing: seed.source.original_backing_observer(),
            source: seed.authority.clone(),
            font: seed.font.clone(),
            count: profile.glyph_count,
            generation: profile.source_generation,
            poisoned: false,
        };
        value.require(job)?;
        Ok(value)
    }
}

/// The selected prefix and remainder refer to the same original shaping output;
/// neither creates a new shaper or copies complete glyph/text arrays.
pub struct MarkdownSafeGlyphSlices {
    line: MarkdownSafeGlyphSlice,
    remainder: Option<MarkdownSafeGlyphSlice>,
}
impl MarkdownSafeGlyphSlices {
    /// Derive both portions from the measure's private selected-break seed,
    /// preserving original Source/font authority throughout metadata acquisition.
    pub fn from_measure(
        measure: &MarkdownSoftLineMeasureJob,
        job: &mut MarkdownShapingJob,
        breaks: &mut MarkdownLayoutBreakJob,
        index: &mut MarkdownGlyphClusterIndex,
    ) -> Result<Self> {
        let seed = measure.resolve_seed(job)?;
        let MarkdownSoftLineMeasureChoice::MeasuredBreak(selected) = &seed.choice else {
            return Err(MarkdownSafeGlyphSliceError::NeedsResolution);
        }; // Public Choice fields cannot turn emergency, missing-glyph, or
        // dictionary-pending outcomes into a measured safe opportunity.
        if job.status() != MarkdownShapingStatus::Ready
            || index.status() != MarkdownGlyphClusterStatus::Ready
        {
            // Both the glyph array and cluster index must be complete before
            // selected coordinates may reference either output.
            return Err(MarkdownSafeGlyphSliceError::PendingDependencies);
        }
        let range = selected.source_utf16.clone();
        if range.start != seed.start
            || range.start < seed.item.start
            || range.end > seed.item.end
            || range.is_empty()
            || selected.boundary_flags & UNSAFE != 0
        {
            // Only this item's original nonempty prefix, with no unsafe break
            // flag, is eligible for a slice without additional resolution.
            return Err(MarkdownSafeGlyphSliceError::NeedsResolution);
        }
        let line_facts = facts(&seed, job, breaks, index, range.clone())?;
        if line_facts.x_advance_26_6 != selected.x_advance_26_6 {
            return Err(MarkdownSafeGlyphSliceError::InconsistentCoverage);
        } // Actual cluster prefix differences must equal measured width;
        // accumulated widths across items cannot certify a single-item slice.
        let remainder_facts = if range.end < seed.item.end {
            // A remainder uses the same item output and independently verified
            // endpoints, keeping continuation text out of a second shaper.
            Some(facts(&seed, job, breaks, index, range.end..seed.item.end)?)
        } else {
            // A fully consumed item owns no empty remainder ticket.
            None
        };
        measure.require_resolve(&seed.owner, job)?;
        let line = MarkdownSafeGlyphSlice::make(&seed, job, index, line_facts)?;
        let remainder = remainder_facts
            .map(|facts| MarkdownSafeGlyphSlice::make(&seed, job, index, facts))
            .transpose()?;
        seed.source.health()?; // Final source health preserves any actual mapped
        // backing failure after the metadata I/O above.
        Ok(Self { line, remainder })
    }
    #[must_use]
    /// Move each single-use slice to its eventual consumer without cloning it.
    pub fn into_parts(self) -> (MarkdownSafeGlyphSlice, Option<MarkdownSafeGlyphSlice>) {
        (self.line, self.remainder)
    }
}

/// Resolve one source position through the original cluster index, retaining
/// Pending and absent coverage as distinct failures.
fn cluster(
    index: &mut MarkdownGlyphClusterIndex,
    job: &mut MarkdownShapingJob,
    at: usize,
) -> Result<MarkdownGlyphCluster> {
    match index.containing(job, at)? {
        MarkdownGlyphClusterAnswer::Known(Some(value)) => Ok(value),
        MarkdownGlyphClusterAnswer::Pending => {
            Err(MarkdownSafeGlyphSliceError::PendingDependencies)
        }
        MarkdownGlyphClusterAnswer::Known(None) => {
            Err(MarkdownSafeGlyphSliceError::InconsistentCoverage)
        }
    }
}
/// Certify original grapheme/cluster endpoints and exact prefix advances before
/// exposing a contiguous visual interval of the already shaped array.
fn facts(
    seed: &MarkdownSoftLineResolveSeed,
    job: &mut MarkdownShapingJob,
    breaks: &mut MarkdownLayoutBreakJob,
    index: &mut MarkdownGlyphClusterIndex,
    range: Range<usize>,
) -> Result<MarkdownSafeGlyphSliceFacts> {
    for at in [range.start, range.end] {
        match breaks.grapheme_query(&seed.authority, at, MarkdownGraphemeQuery::IsBoundary)? {
            MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == at => {}
            MarkdownGraphemeAnswer::Pending => {
                return Err(MarkdownSafeGlyphSliceError::PendingDependencies);
            }
            _ => return Err(MarkdownSafeGlyphSliceError::NeedsResolution),
        } // Original GCB proof covers both endpoints. Range length cannot
        // legitimize surrogate interiors, combining-cluster interiors, or an
        // unknown suffix.
    }
    let first = cluster(index, job, range.start)?;
    let last = cluster(index, job, range.end - 1)?;
    if first.source_utf16.start != range.start
        || last.source_utf16.end != range.end
        || (range.start != seed.item.start && first.flags & UNSAFE != 0)
    {
        // Both source endpoints must coincide with real cluster endpoints; an
        // interior start also requires permission to break before that cluster.
        return Err(MarkdownSafeGlyphSliceError::NeedsResolution);
    }
    if range.end < seed.item.end {
        let next = cluster(index, job, range.end)?;
        if next.source_utf16.start != range.end || next.flags & UNSAFE != 0 {
            // The following cluster owns UNSAFE_TO_BREAK at this boundary.
            return Err(MarkdownSafeGlyphSliceError::NeedsResolution);
        }
    } // Prefix-only flag aggregation cannot establish a safe following edge.
    let x = last
        .prefix_x_26_6
        .checked_add(last.x_advance_26_6)
        .and_then(|end| end.checked_sub(first.prefix_x_26_6))
        .ok_or(MarkdownSafeGlyphSliceError::Overflow)?;
    let y = last
        .prefix_y_26_6
        .checked_add(last.y_advance_26_6)
        .and_then(|end| end.checked_sub(first.prefix_y_26_6))
        .ok_or(MarkdownSafeGlyphSliceError::Overflow)?;
    // RTL logical endpoints appear in reverse visual order. Take the outer
    // endpoints of the original continuous clusters without reversing glyphs.
    let visual = first.glyphs.start.min(last.glyphs.start)..first.glyphs.end.max(last.glyphs.end);
    Ok(MarkdownSafeGlyphSliceFacts {
        source_utf16: range,
        visual_glyphs: visual,
        x_advance_26_6: x,
        y_advance_26_6: y,
    })
}

#[cfg(test)]
mod tests;
