//! Transfer actual jobs at resolved boundaries. Read-only spooling and subsequent measurement
//! consume these same children; callers never receive a replaceable mutable Job borrow.
use std::{ops::Range, sync::Weak};

use super::*;
use crate::{
    MarkdownGlyphClusterStep, MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownShapedSpool, MarkdownShapedSpoolBuilder,
    MarkdownShapedSpoolStep, MarkdownSoftLineMeasureBudget, MarkdownSoftLineMeasureRequest,
    MarkdownSoftLineMeasureStep,
};

/// Only an actually resolved remainder creates this line-start proof. Equal coordinates, font
/// names, or a public boundary copy cannot reconstruct its original Source/Font/shape authority.
#[derive(Clone)]
pub(crate) struct MarkdownResolvedLineStart {
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    shape: Weak<()>,
    at: usize,
}
impl MarkdownResolvedLineStart {
    pub(crate) fn require(
        &self,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        shape: &mut MarkdownShapingJob,
        breaks: &mut MarkdownLayoutBreakJob,
        at: usize,
    ) -> std::result::Result<(), MarkdownSoftLineMeasureError> {
        // Source health outranks identity rejection. Original source authority, actual shape
        // allocation, and both supplied/original font tickets must still belong to this same job.
        source.health()?;
        if self.source != *authority || !source.permits(authority) {
            return Err(MarkdownSoftLineMeasureError::SourceMismatch);
        }
        if !Weak::ptr_eq(&self.shape, &shape.index_identity()) {
            return Err(MarkdownSoftLineMeasureError::JobMismatch);
        }
        let profile = shape.profile()?;
        shape.glyph(authority, font, profile.glyph_count)?;
        shape.glyph(&self.source, &self.font, profile.glyph_count)?;
        // Font authority is not compared by name or copied fields. The actual job's original
        // font owner must accept both tickets through its real EOF glyph primitive.
        if at != self.at || shape.source_range().start != at {
            return Err(MarkdownSoftLineMeasureError::InvalidRequest);
        }
        match breaks.grapheme_query(authority, at, MarkdownGraphemeQuery::IsBoundary)? {
            MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == at => {}
            _ => return Err(MarkdownSoftLineMeasureError::InvalidRequest),
        } // Original GCB must still be known; this private proof never bypasses Unicode boundary validation.
        source.health()?;
        Ok(())
    }
}

/// Actual resolved prefix and optional tail, without Clone, public fields, or a constructor that
/// reconstructs owners from copied coordinates.
pub struct MarkdownSoftLineResolvedParts {
    boundary: MarkdownSoftLineResolvedBoundary,
    line: MarkdownSoftLineResolvedShape,
    remainder: Option<MarkdownSoftLineResolvedShape>,
}
impl MarkdownSoftLineResolvedParts {
    pub(super) fn new(
        boundary: MarkdownSoftLineResolvedBoundary,
        line: MarkdownSoftLineResolvedShape,
        remainder: Option<MarkdownSoftLineResolvedShape>,
    ) -> Self {
        // Accept already-owned concrete children at the sole internal handoff boundary.
        Self {
            boundary,
            line,
            remainder,
        }
    }

    /// Consume this unique handoff. Coordinate copies are facts only; actual next-line authority
    /// remains encapsulated by the concrete remainder owner.
    pub fn into_parts(
        self,
    ) -> (
        MarkdownSoftLineResolvedBoundary,
        MarkdownSoftLineResolvedShape,
        Option<MarkdownSoftLineResolvedShape>,
    ) {
        (self.boundary, self.line, self.remainder)
    }
}

/// Worker-local actual child. Its underlying Job is !Send; do not force this value onto another
/// worker or the Native main thread.
pub struct MarkdownSoftLineResolvedShape {
    owned: Option<OwnedShape>,
    source: Option<MarkdownLayoutTextSource>,
    authority: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    shape: Weak<()>,
    start: Option<MarkdownResolvedLineStart>,
    backing: Option<MarkdownOriginalBackingObserver>,
}
impl MarkdownSoftLineResolvedShape {
    pub(super) fn new(
        mut owned: OwnedShape,
        source: MarkdownLayoutTextSource,
        authority: MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFontAuthority,
        resolved_start: bool,
    ) -> Result<Self> {
        // Retain the actual immutable Source and concrete child; weak shape identity prevents
        // substitution without extending another original job's lifetime.
        source.health()?;
        let job = owned.job()?;
        let profile = job.profile()?;
        job.glyph(&authority, &font, profile.glyph_count)?;
        let shape = job.index_identity();
        let start = resolved_start.then(|| MarkdownResolvedLineStart {
            source: authority.clone(),
            font: font.clone(),
            shape: shape.clone(),
            at: job.source_range().start,
        });
        let backing = source.original_backing_observer();
        Ok(Self {
            owned: Some(owned),
            source: Some(source),
            authority,
            font,
            shape,
            start,
            backing,
        })
    }

    fn guard(&mut self) -> Result<()> {
        // Preserve actual backing failure even after resources have been removed. Then require
        // the same live child allocation and its original Source/Font before exposing any fact.
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let source = self
            .source
            .as_ref()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        source.health()?;
        let owned = self
            .owned
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        if owned.resources.failed.load(Ordering::Acquire) {
            return Err(MarkdownSoftLineResolveError::ResourceCleanup);
        }
        let job = owned.job()?;
        if !Weak::ptr_eq(&self.shape, &job.index_identity()) {
            return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
        }
        let profile = job.profile()?;
        job.glyph(&self.authority, &self.font, profile.glyph_count)?;
        Ok(())
    }

    /// Only this module's fixed operations borrow the mutable job. Callers cannot mem::replace
    /// its registered resource identity while retaining an old observation or proof.
    fn with_job<T>(
        &mut self,
        visit: impl FnOnce(
            &mut MarkdownShapingJob,
            &MarkdownLayoutTextSource,
            &MarkdownLayoutTextAuthority,
            &MarkdownLayoutFontAuthority,
        ) -> Result<T>,
    ) -> Result<T> {
        let result = (|| {
            self.guard()?;
            let source = self
                .source
                .as_ref()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
            let job = self
                .owned
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?
                .job()?;
            visit(job, source, &self.authority, &self.font)
        })();
        let result = match self.source.as_ref().map(MarkdownLayoutTextSource::health) {
            Some(Err(error)) => Err(error.into()),
            _ => result.and_then(|answer| {
                self.guard()?;
                Ok(answer)
            }),
        };
        if result.is_err() {
            drop(self.owned.take());
            drop(self.source.take());
        } // Failure actually returns the job and Source, leaving no old immutable shell that can issue new authority.
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        result
    }

    /// Let the synthetic control account for actual input and adjacent context. These work facts
    /// do not replace Native acceptance or prove whole-product memory behavior.
    #[cfg(all(test, feature = "paged-safe-glyph-slices"))]
    pub(super) fn profile_for_test(&mut self) -> Result<crate::MarkdownShapingProfile> {
        self.with_job(|job, _, _, _| Ok(job.profile()?))
    }

    pub fn source_range(&mut self) -> Result<Range<usize>> {
        // Return coordinates only after current original-owner checks, not as transferable authority.
        self.with_job(|job, _, _, _| Ok(job.source_range()))
    }
    pub fn glyph(&mut self, ordinal: usize) -> Result<Option<MarkdownShapedGlyph>> {
        // Read the actual child's glyph under its original Source/Font, including real EOF.
        self.with_job(|job, _, authority, font| Ok(job.glyph(authority, font, ordinal)?))
    }
    /// The closed Store producer verifies original Source/Font before reading actual Ready facts,
    /// without receiving a replaceable Job borrow.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn require_authorities(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<crate::MarkdownShapingProfile> {
        self.with_job(|job, _, _, _| {
            let facts = job.profile()?;
            job.glyph(source, font, facts.glyph_count)?;
            Ok(facts)
        }) // Actual EOF validates both tickets; equal font names or Source bytes cannot rebind this concrete child.
    }
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn instance_identity(&self) -> Weak<()> {
        // Identity alone retains neither the child nor any Source/font resources.
        self.shape.clone()
    }
    pub fn cluster_index(&mut self) -> Result<MarkdownGlyphClusterIndex> {
        // Construct the real index from this child, preserving its own job nonce and authorities.
        self.with_job(|job, _, authority, font| {
            Ok(MarkdownGlyphClusterIndex::new(job, authority, font)?)
        })
    }
    pub fn step_clusters(
        &mut self,
        index: &mut MarkdownGlyphClusterIndex,
        budget: usize,
    ) -> Result<MarkdownGlyphClusterStep> {
        // Advance the existing child-bound index; the index itself rejects another job's nonce.
        self.with_job(|job, _, _, _| Ok(index.step(job, budget)?))
    }
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn containing_cluster(
        &mut self,
        index: &mut MarkdownGlyphClusterIndex,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        self.with_job(|job, _, _, _| Ok(index.containing(job, offset)?))
    } // The index verifies the actual child nonce; the original item or equal-glyph replacement index cannot substitute.

    /// Only the actual tail's encapsulated resolved start can begin the next line; no external
    /// start coordinate may substitute for this proof.
    pub fn measure(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        width_26_6: i64,
        cancellation: MarkdownSoftLineMeasureCancellation,
    ) -> Result<MarkdownSoftLineMeasureJob> {
        let proof = self.start.clone();
        self.with_job(move |job, source, authority, font| {
            let proof = proof.ok_or(MarkdownSoftLineResolveError::InvalidRequest)?;
            Ok(MarkdownSoftLineMeasureJob::after_resolved_start(
                source.clone(),
                authority,
                font,
                job,
                breaks,
                MarkdownSoftLineMeasureRequest {
                    start_utf16: proof.at,
                    width_26_6,
                    cancellation,
                },
                &proof,
            )?)
        })
    }
    pub fn step_measure(
        &mut self,
        measure: &mut MarkdownSoftLineMeasureJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineMeasureBudget,
    ) -> Result<MarkdownSoftLineMeasureStep> {
        // Borrow the same real child for this measure's bounded turn; no fresh start ticket is captured.
        self.with_job(|job, _, _, _| Ok(measure.step(job, clusters, breaks, budget)?))
    }

    pub fn resolver(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
    ) -> Result<MarkdownSoftLineResolveJob> {
        // The original selected measure must accept this actual remainder job as its provider.
        self.with_job(|job, _, _, _| MarkdownSoftLineResolveJob::new(measure, job))
    }
    pub fn step_resolver(
        &mut self,
        resolver: &mut MarkdownSoftLineResolveJob,
        measure: &MarkdownSoftLineMeasureJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownSoftLineResolveStep> {
        // Advance through the concrete child's ownership boundary so failure retires that same child.
        self.with_job(|job, _, _, _| resolver.step(measure, job, clusters, breaks, budget))
    }

    pub fn take_resolved_parts(
        &mut self,
        resolver: MarkdownSoftLineResolveJob,
        measure: &MarkdownSoftLineMeasureJob,
    ) -> Result<MarkdownSoftLineResolvedParts> {
        // Consume the completed resolver's genuine children, not another boundary's coordinate copy.
        self.with_job(|job, _, _, _| resolver.take_parts(measure, job))
    }

    /// The existing read-only spool builder consumes this actual job, without another shaper or
    /// a document-text mirror.
    pub fn spool_builder(&mut self) -> Result<MarkdownShapedSpoolBuilder> {
        self.with_job(|job, _, authority, font| {
            Ok(MarkdownShapedSpoolBuilder::new(job, authority, font)?)
        })
    }
    pub fn step_spool(
        &mut self,
        builder: &mut MarkdownShapedSpoolBuilder,
        budget: usize,
    ) -> Result<MarkdownShapedSpoolStep> {
        // Preserve the builder's actual child binding while copying only its bounded glyph batch.
        self.with_job(|job, _, _, _| Ok(builder.step(job, budget)?))
    }
    pub fn seal_spool(
        &mut self,
        builder: MarkdownShapedSpoolBuilder,
    ) -> Result<MarkdownShapedSpool> {
        // Seal the same builder against this still-authorized child, transferring its actual output owner.
        self.with_job(|job, _, _, _| Ok(builder.seal(job)?))
    }

    pub fn close(self) -> Result<()> {
        // Keep only observers across actual owner Drop, then verify its original glyph mappings
        // and files. Independent Source/Font owners may legitimately remain elsewhere.
        let observer = self.owned.as_ref().map(|owned| owned.observer.clone());
        let backing = self.backing.clone();
        drop(self); // Return this actual job; ownership transfer alone is not resource destruction.
        if let Some(errno) = backing.as_ref().and_then(|observer| observer.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let facts = observer
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?
            .snapshot();
        if facts.files_open != 0
            || facts.mappings_open != 0
            || facts.mapped_bytes != 0
            || facts.cleanup_failed
            || facts.first_error.is_some()
        {
            return Err(MarkdownSoftLineResolveError::ResourceCleanup);
        }
        Ok(())
    }
}

/// Only the original selector's internal Reusable stage constructs this choice; a public boundary
/// copy or arbitrary Range cannot authorize safe emergency wrapping.
#[cfg(feature = "paged-safe-glyph-slices")]
pub(crate) struct MarkdownEmergencyResolvedChoice<'a> {
    seed: &'a MarkdownSoftLineResolveSeed,
    end: usize,
    width: i64,
}
#[cfg(feature = "paged-safe-glyph-slices")]
impl<'a> MarkdownEmergencyResolvedChoice<'a> {
    pub(super) fn new(payload: &'a Payload) -> Result<Self> {
        // Borrow the actual selected seed only for this reusable stage, requiring forward
        // progress and the already established exact width before safe-slice admission.
        if !payload.reuse_safe
            || !matches!(payload.stage, Stage::Reusable)
            || payload.selected_end <= payload.seed.start
            || payload.selected_end > payload.limit
        {
            return Err(MarkdownSoftLineResolveError::InvalidRequest);
        }
        Ok(Self {
            seed: &payload.seed,
            end: payload.selected_end,
            width: payload
                .expected_width
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?,
        })
    }
    pub(crate) fn seed(&self) -> &MarkdownSoftLineResolveSeed {
        self.seed
    }
    pub(crate) fn range(&self) -> Range<usize> {
        self.seed.start..self.end
    }
    pub(crate) fn width(&self) -> i64 {
        self.width
    }
}
