//! Reshape the original measure's selected item using the same frozen Source
//! and its original paragraph context across soft-line boundaries.
//!
//! Cluster-proven safe candidates can reuse exact advances; unsafe or interior
//! candidates require actual shaping. Final prefix/remainder retain real jobs
//! advanced by their existing worker owner; this module starts no execution.
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use thiserror::Error;

#[cfg(feature = "paged-safe-glyph-slices")]
mod emergency;
mod resolved_shape;
#[cfg(feature = "paged-line-compose")]
pub(crate) mod stored;
#[cfg(feature = "paged-safe-glyph-slices")]
pub use emergency::{
    MarkdownEmergencyGlyphSliceAnswer, MarkdownEmergencyGlyphSliceBoundary,
    MarkdownEmergencyGlyphSliceJob, MarkdownEmergencyGlyphSliceLine,
    MarkdownEmergencyGlyphSliceStep, MarkdownEmergencyGlyphSliceWork,
    MarkdownEmergencyShapedPrefixBoundary,
};
#[cfg(feature = "paged-safe-glyph-slices")]
pub(crate) use resolved_shape::MarkdownEmergencyResolvedChoice;
pub(crate) use resolved_shape::MarkdownResolvedLineStart;
pub use resolved_shape::{MarkdownSoftLineResolvedParts, MarkdownSoftLineResolvedShape};

use crate::{
    MarkdownDocumentBufferError, MarkdownGlyphClusterAnswer, MarkdownGlyphClusterError,
    MarkdownGlyphClusterIndex, MarkdownGraphemeAnswer, MarkdownGraphemeQuery,
    MarkdownLayoutBreakError, MarkdownLayoutBreakJob, MarkdownLayoutTextSourceObserver,
    MarkdownOriginalBackingObserver, MarkdownShapedGlyph, MarkdownShapingBudget,
    MarkdownShapingCancellation, MarkdownShapingError, MarkdownShapingJob,
    MarkdownShapingResourceObserver, MarkdownShapingStatus, MarkdownSoftLineMeasureCancellation,
    MarkdownSoftLineMeasureChoice, MarkdownSoftLineMeasureError, MarkdownSoftLineMeasureJob,
    markdown_soft_line_measure::{
        MarkdownSoftLineResolveSeed,
        provider::{JobGlyphProvider, ResolutionProvider},
    },
};

pub const MARKDOWN_SOFT_LINE_RESOLVE_GLYPHS: usize = 256;
pub const MARKDOWN_SOFT_LINE_RESOLVE_CANDIDATES: usize = 64;
const UNSAFE: u32 = harfrust::GlyphFlags::UNSAFE_TO_BREAK.to_bits();

#[derive(Debug, Error)]
pub enum MarkdownSoftLineResolveError {
    #[error("软行解析阶段或配额无效")]
    InvalidRequest,
    #[error("软行候选、原glyph或实际重排结果不一致")]
    InconsistentCoverage,
    #[error("软行解析坐标或advance溢出")]
    Overflow,
    #[error("软行解析已永久撤权")]
    Poisoned,
    #[error("软行字形资源未实际完整归还")]
    ResourceCleanup,
    #[error("软行原冻结源失败")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("软行原测量已失权")]
    Measure(#[from] MarkdownSoftLineMeasureError),
    #[error("软行实际整形失败")]
    Shaping(#[from] MarkdownShapingError),
    #[error("软行原cluster索引失败")]
    Cluster(#[from] MarkdownGlyphClusterError),
    #[error("软行原断点索引失败")]
    Break(#[from] MarkdownLayoutBreakError),
    #[error("软行只读字形交付失败")]
    Spool(#[from] crate::MarkdownShapedSpoolError),
    #[cfg(feature = "paged-safe-glyph-slices")]
    #[error("原紧急安全字形切片失败")]
    SafeSlice(#[from] crate::MarkdownSafeGlyphSliceError),
}
type Result<T> = std::result::Result<T, MarkdownSoftLineResolveError>;

/// One resolution stage shares the original measure and shaper cancellation
/// signals across every trial, without resetting permission per candidate.
#[derive(Clone)]
pub struct MarkdownSoftLineResolveCancellation {
    measure: MarkdownSoftLineMeasureCancellation,
    shape: MarkdownShapingCancellation,
}
impl MarkdownSoftLineResolveCancellation {
    pub fn cancel(&self) {
        self.measure.cancel();
        self.shape.request(); // Deep shaping observes this original signal;
        // measure-only cancellation is also checked at turn boundaries.
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MarkdownSoftLineResolveBudget {
    pub input_bytes: usize,
    pub input_scalars: usize,
    pub glyphs: usize,
    pub candidates: usize,
}
impl Default for MarkdownSoftLineResolveBudget {
    fn default() -> Self {
        Self {
            input_bytes: 4096,
            input_scalars: 256,
            glyphs: MARKDOWN_SOFT_LINE_RESOLVE_GLYPHS,
            candidates: MARKDOWN_SOFT_LINE_RESOLVE_CANDIDATES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineResolvePending {
    FontFallback {
        source_utf16: Range<usize>,
        missing_glyphs: usize,
    },
    Dictionary,
    NextItem,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineResolvedKind {
    Natural,
    Emergency,
    OverflowingGrapheme,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineResolvedSide {
    Line,
    Remainder,
}

/// Actual shaped source/glyph facts leave Native geometry, multi-item line
/// composition, and line-level bidi ordering to their later owning stages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSoftLineResolvedBoundary {
    pub source_utf16: Range<usize>,
    pub remainder_utf16: Range<usize>,
    pub kind: MarkdownSoftLineResolvedKind,
    pub x_advance_26_6: i64,
    pub line_glyphs: usize,
    pub remainder_glyphs: usize,
    pub line_flags_union: u32,
    pub remainder_flags_union: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineResolveAnswer {
    EmptySource,
    PendingDependencies,
    Yielded,
    Pending(MarkdownSoftLineResolvePending),
    Resolved(MarkdownSoftLineResolvedBoundary),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSoftLineResolveStep {
    pub input_bytes: usize,
    /// Actual scalars consumed this turn, accounted independently from UTF-8 bytes.
    pub input_scalars: usize,
    /// Adjacent context read by newly constructed jobs, separate from later input.
    pub context_bytes: usize,
    pub glyphs: usize,
    pub candidates: usize,
    pub shapes: usize,
    pub answer: MarkdownSoftLineResolveAnswer,
}

#[derive(Default)]
struct ResourceFacts {
    active: AtomicUsize,
    peak: AtomicUsize,
    created: AtomicUsize,
    released: AtomicUsize,
    failed: AtomicBool,
}
#[derive(Clone)]
pub struct MarkdownSoftLineResolveResourceObserver {
    source: MarkdownLayoutTextSourceObserver,
    facts: Arc<ResourceFacts>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownSoftLineResolveResources {
    pub source_payload_released: bool,
    pub active_jobs: usize,
    pub peak_active_jobs: usize,
    pub jobs_created: usize,
    pub jobs_released: usize,
    pub cleanup_failed: bool,
}
impl MarkdownSoftLineResolveResourceObserver {
    #[must_use]
    pub fn snapshot(&self) -> MarkdownSoftLineResolveResources {
        MarkdownSoftLineResolveResources {
            source_payload_released: self.source.payload_released(),
            active_jobs: self.facts.active.load(Ordering::Acquire),
            peak_active_jobs: self.facts.peak.load(Ordering::Acquire),
            jobs_created: self.facts.created.load(Ordering::Acquire),
            jobs_released: self.facts.released.load(Ordering::Acquire),
            cleanup_failed: self.facts.failed.load(Ordering::Acquire),
        }
    }
}

/// This local owner settles each child job by actual Drop followed by its
/// original resource observer, with no manual successful-release operation.
struct OwnedShape {
    job: Option<MarkdownShapingJob>,
    observer: MarkdownShapingResourceObserver,
    resources: Arc<ResourceFacts>,
}
impl OwnedShape {
    fn new(job: MarkdownShapingJob, resources: &Arc<ResourceFacts>) -> Result<Self> {
        // Account only real child jobs. Counter exhaustion leaves a negative
        // receipt and returns the supplied job through ordinary Rust ownership.
        if resources
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .is_err()
            || resources
                .created
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
                .is_err()
        {
            resources.failed.store(true, Ordering::Release);
            return Err(MarkdownSoftLineResolveError::Overflow);
        }
        resources
            .peak
            .fetch_max(resources.active.load(Ordering::Acquire), Ordering::AcqRel);
        let observer = job.resource_observer();
        Ok(Self {
            job: Some(job),
            observer,
            resources: resources.clone(),
        })
    }
    fn job(&mut self) -> Result<&mut MarkdownShapingJob> {
        self.job
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)
    }
}
impl Drop for OwnedShape {
    fn drop(&mut self) {
        drop(self.job.take());
        let observed = self.observer.snapshot();
        if observed.files_open != 0
            || observed.mappings_open != 0
            || observed.mapped_bytes != 0
            || observed.cleanup_failed
            || observed.first_error.is_some()
        {
            self.resources.failed.store(true, Ordering::Release);
        } // Font/Source may still have legitimate independent owners. These
        // observations settle this job's actual glyph mappings and files only.
        if self
            .resources
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .is_err()
            || self
                .resources
                .released
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
                .is_err()
        {
            self.resources.failed.store(true, Ordering::Release); // Counter
            // inconsistency remains negative; Drop does not certify success.
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Exact,
    Emergency,
}
#[derive(Clone, Copy)]
enum Stage {
    Choose,
    Input,
    Shape,
    Scan,
    TailInput,
    TailShape,
    TailScan,
    Finished,
    #[cfg(feature = "paged-safe-glyph-slices")]
    Reusable,
    #[cfg(feature = "paged-safe-glyph-slices")]
    ShapedPrefix,
}
#[derive(Clone, Copy, Default)]
struct GlyphFacts {
    count: usize,
    width: i64,
    flags: u32,
    missing: usize,
}
struct Fit {
    end: usize,
    width: i64,
    shape: Option<OwnedShape>,
    facts: Option<GlyphFacts>,
}
struct Payload {
    seed: MarkdownSoftLineResolveSeed,
    mode: Mode,
    limit: usize,
    next: usize,
    candidate: usize,
    stage: Stage,
    current: Option<OwnedShape>,
    best: Option<Fit>,
    line: Option<OwnedShape>,
    tail: Option<OwnedShape>,
    scan: GlyphFacts,
    line_facts: GlyphFacts,
    selected_end: usize,
    expected_width: Option<i64>,
    kind: MarkdownSoftLineResolvedKind,
    answer: Option<MarkdownSoftLineResolveAnswer>,
    final_prefix: bool,
    #[cfg(feature = "paged-safe-glyph-slices")]
    reuse_safe: bool,
    #[cfg(feature = "paged-safe-glyph-slices")]
    defer_tail: bool,
}

/// Original measure/shape authority is weak and reborrowed for each operation;
/// resolution retains at most two real paged child jobs.
pub struct MarkdownSoftLineResolveJob {
    payload: Option<Payload>,
    cancellation: MarkdownSoftLineResolveCancellation,
    resources: MarkdownSoftLineResolveResourceObserver,
    backing: Option<MarkdownOriginalBackingObserver>,
}
impl MarkdownSoftLineResolveJob {
    /// Capture the original selected seed now; child shapers are constructed
    /// only when the owning worker actually advances the resolver.
    pub fn new(
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<Self> {
        let seed = measure.resolve_seed(original)?;
        Self::from_seed(
            seed,
            JobGlyphProvider::new(original, None).shaping_cancellation(),
        ) // Every producer supplies its actual original cancellation signal at
        // this same selector boundary.
    }

    /// Assemble from an already selected original seed and its actual producer
    /// signal. Stored output supplies its real provider rather than a fake job.
    fn from_seed(
        seed: MarkdownSoftLineResolveSeed,
        shape: MarkdownShapingCancellation,
    ) -> Result<Self> {
        let cancellation = MarkdownSoftLineResolveCancellation {
            measure: seed.cancellation.clone(),
            shape,
        };
        let resources = MarkdownSoftLineResolveResourceObserver {
            source: seed.source.resource_observer(),
            facts: Arc::new(ResourceFacts::default()),
        };
        let backing = seed.source.original_backing_observer();
        let (mode, limit, pending) = match &seed.choice {
            // Measured/reshape choices retain the selected actual endpoint.
            MarkdownSoftLineMeasureChoice::MeasuredBreak(value) => {
                (Mode::Exact, value.source_utf16.end, None)
            }
            MarkdownSoftLineMeasureChoice::NeedsReshape { source_utf16, .. } => {
                (Mode::Exact, source_utf16.end, None)
            }
            MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(value) => {
                (Mode::Emergency, value.source_utf16.end, None)
            }
            #[cfg(feature = "paged-shaping-frontier")]
            MarkdownSoftLineMeasureChoice::NeedsFrontierEmergencyPrefix {
                source_utf16, ..
            } => (Mode::Emergency, source_utf16.end, None), // Only this actual
            // provider prefix is eligible; word_end grants no shaping/UAX edge.
            #[cfg(feature = "paged-shaping-frontier")]
            MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak { source_utf16, .. } => {
                (Mode::Emergency, source_utf16.end, None)
            } // The explicit resolver can recheck this selection; production
            // Store consumes its private safe ticket directly.
            MarkdownSoftLineMeasureChoice::NeedsFontFallback {
                cluster_utf16,
                missing_glyphs,
            } => (
                Mode::Exact,
                seed.start,
                Some(MarkdownSoftLineResolvePending::FontFallback {
                    source_utf16: cluster_utf16.clone(),
                    missing_glyphs: *missing_glyphs,
                }),
            ),
            MarkdownSoftLineMeasureChoice::NeedsDictionary(_) => (
                Mode::Exact,
                seed.start,
                Some(MarkdownSoftLineResolvePending::Dictionary),
            ),
            MarkdownSoftLineMeasureChoice::NeedNextItem { .. } => (
                Mode::Exact,
                seed.start,
                Some(MarkdownSoftLineResolvePending::NextItem),
            ),
            MarkdownSoftLineMeasureChoice::EmptySource => (Mode::Exact, seed.start, None),
        };
        let pending = pending.or_else(|| {
            // A selected interval spanning beyond this actual item still needs
            // the next item, rather than treating an item edge as paragraph EOF.
            (seed.start < seed.item.start || limit > seed.item.end)
                .then_some(MarkdownSoftLineResolvePending::NextItem)
        });
        let answer = if matches!(seed.choice, MarkdownSoftLineMeasureChoice::EmptySource) {
            // Only the original explicit empty-source choice establishes this
            // terminal outcome without glyph work.
            Some(MarkdownSoftLineResolveAnswer::EmptySource)
        } else {
            pending.map(MarkdownSoftLineResolveAnswer::Pending)
        };
        let next = seed.start;
        Ok(Self {
            payload: Some(Payload {
                seed,
                mode,
                limit,
                next,
                candidate: next,
                stage: if answer.is_some() {
                    Stage::Finished
                } else {
                    Stage::Choose
                },
                current: None,
                best: None,
                line: None,
                tail: None,
                scan: GlyphFacts::default(),
                line_facts: GlyphFacts::default(),
                selected_end: next,
                expected_width: None,
                kind: MarkdownSoftLineResolvedKind::Natural,
                answer,
                final_prefix: false,
                #[cfg(feature = "paged-safe-glyph-slices")]
                reuse_safe: false,
                #[cfg(feature = "paged-safe-glyph-slices")]
                defer_tail: false,
            }),
            cancellation,
            resources,
            backing,
        })
    }

    #[must_use]
    pub fn cancellation(&self) -> MarkdownSoftLineResolveCancellation {
        self.cancellation.clone()
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownSoftLineResolveResourceObserver {
        self.resources.clone()
    }

    /// Consume the resolver into its actual jobs; public Copy coordinates
    /// cannot manufacture authority over the following line start.
    ///
    /// After transfer the original measure may retire; each actual shape still
    /// checks its own Source/font/nonce and original cancellation signal.
    pub fn take_parts(
        mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<MarkdownSoftLineResolvedParts> {
        self.guard(measure, original)?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        let boundary = match &payload.answer {
            Some(MarkdownSoftLineResolveAnswer::Resolved(boundary)) => boundary.clone(),
            _ => return Err(MarkdownSoftLineResolveError::InvalidRequest),
        };
        let line = payload
            .line
            .take()
            .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
        let line = MarkdownSoftLineResolvedShape::new(
            line,
            payload.seed.source.clone(),
            payload.seed.authority.clone(),
            payload.seed.font.clone(),
            false,
        )?;
        let remainder = match payload.tail.take() {
            Some(tail) => Some(MarkdownSoftLineResolvedShape::new(
                tail,
                payload.seed.source.clone(),
                payload.seed.authority.clone(),
                payload.seed.font.clone(),
                true,
            )?),
            None => None, // An empty remainder creates neither another document
                          // nor a placeholder shaping job.
        };
        let parts = MarkdownSoftLineResolvedParts::new(boundary, line, remainder);
        self.guard(measure, original)?; // Recheck after every transfer. Failure
        // drops the actual candidate parts without publishing a partial prefix.
        drop(self.payload.take());
        Ok(parts)
    }

    fn guard(
        &self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<()> {
        self.guard_provider(measure, &mut JobGlyphProvider::new(original, None))
    }

    /// Preserve actual backing/Source failure before matching the original
    /// provider and measure; later state cannot refresh an obsolete selector.
    fn guard_provider(
        &self,
        measure: &MarkdownSoftLineMeasureJob,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<()> {
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        payload.seed.source.health()?;
        if payload.seed.origin != provider.origin() {
            return Err(MarkdownSoftLineMeasureError::JobMismatch.into());
        } // Valid measure continuation keeps its nonce; an old selector still
        // cannot adopt a later item's directory to replace its font/range.
        measure.require_resolve_provider(&payload.seed.owner, provider)?;
        if self.resources.facts.failed.load(Ordering::Acquire) {
            return Err(MarkdownSoftLineResolveError::ResourceCleanup);
        }
        Ok(())
    }

    /// Advance at most one actual shape per turn on the existing worker. The
    /// synchronous shaping algorithm does not prove a main-thread latency bound.
    pub fn step(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownSoftLineResolveStep> {
        self.step_provider(
            measure,
            &mut JobGlyphProvider::new(original, Some(clusters)),
            breaks,
            budget,
        )
    }

    /// Recheck Source after the bounded operation and after failure disposal;
    /// candidate/cancellation errors cannot hide the original backing errno.
    fn step_provider(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        provider: &mut dyn ResolutionProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownSoftLineResolveStep> {
        let result = self.advance(measure, provider, breaks, budget);
        let result = match self.payload.as_ref().map(|p| p.seed.source.health()) {
            Some(Err(error)) => Err(error.into()),
            _ => result.and_then(|answer| {
                self.guard_provider(measure, provider)?;
                Ok(answer)
            }),
        };
        if result.is_err() {
            drop(self.payload.take());
        }
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        } // Final Source return can expose the actual error too; retain it
        // instead of a candidate, cancellation, or control-state error.
        result
    }

    /// Spend separate input/scalar/glyph/candidate allowances while preserving
    /// one original candidate and real child jobs across turns.
    fn advance(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        provider: &mut dyn ResolutionProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownSoftLineResolveStep> {
        self.guard_provider(measure, provider)?;
        if budget.input_bytes == 0
            || budget.input_scalars == 0
            || budget.glyphs == 0
            || budget.candidates == 0
        {
            // Zero allowances cannot advance any admitted stage meaningfully.
            return Err(MarkdownSoftLineResolveError::InvalidRequest);
        }
        let mut step = MarkdownSoftLineResolveStep {
            input_bytes: 0,
            input_scalars: 0,
            context_bytes: 0,
            glyphs: 0,
            candidates: 0,
            shapes: 0,
            answer: MarkdownSoftLineResolveAnswer::Yielded,
        };
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        breaks.require_source(&payload.seed.authority)?;
        if let Some(answer) = &payload.answer {
            // Return the same established outcome after authority validation;
            // this observation does not rerun a completed external operation.
            step.answer = answer.clone();
            return Ok(step);
        }
        match payload.stage {
            Stage::Choose => {
                // Candidate enumeration follows actual grapheme boundaries and
                // yields on incomplete GCB data instead of inventing an edge.
                for _ in 0..budget.candidates.min(MARKDOWN_SOFT_LINE_RESOLVE_CANDIDATES) {
                    let end = match payload.mode {
                        Mode::Exact => payload.limit,
                        Mode::Emergency => match breaks.grapheme_query(
                            &payload.seed.authority,
                            payload.next,
                            MarkdownGraphemeQuery::Next,
                        )? {
                            MarkdownGraphemeAnswer::Pending => {
                                step.answer = MarkdownSoftLineResolveAnswer::PendingDependencies;
                                return Ok(step);
                            }
                            MarkdownGraphemeAnswer::Known(Some(value))
                                if value.utf16 > payload.next && value.utf16 <= payload.limit =>
                            {
                                value.utf16
                            }
                            _ => return Err(MarkdownSoftLineResolveError::InconsistentCoverage),
                        },
                    };
                    match breaks.grapheme_query(
                        &payload.seed.authority,
                        end,
                        MarkdownGraphemeQuery::IsBoundary,
                    )? {
                        MarkdownGraphemeAnswer::Pending => {
                            step.answer = MarkdownSoftLineResolveAnswer::PendingDependencies;
                            return Ok(step);
                        }
                        MarkdownGraphemeAnswer::Known(Some(_)) => {}
                        _ => return Err(MarkdownSoftLineResolveError::InconsistentCoverage),
                    } // The selected endpoint must remain an original GCB
                    // boundary, excluding surrogate or grapheme interiors.
                    if end == payload.seed.item.end && end != payload.seed.source.utf16_len() {
                        let following = provider
                            .following_item_flags(&payload.seed.authority, &payload.seed.font)?;
                        if following.is_none_or(|flags| flags & UNSAFE != 0) {
                            pending(payload, MarkdownSoftLineResolvePending::NextItem);
                            break; // An unknown/unsafe successor cannot turn an
                            // item/Store edge into paragraph EOF.
                        } // The same Store's actual following cluster supplies
                        // right-side proof under original Source/font/GCB/context.
                    }
                    step.candidates += 1;
                    payload.candidate = end;
                    payload.next = end;
                    if matches!(payload.mode, Mode::Emergency)
                        && let Some(width) = safe_width(payload, provider, end)?
                    {
                        if width <= payload.seed.width {
                            // Safe complete original clusters supply exact
                            // signed width without creating another trial job.
                            payload.best = Some(Fit {
                                end,
                                width,
                                shape: None,
                                facts: None,
                            });
                            if end == payload.limit {
                                step.context_bytes =
                                    choose_best(payload, provider, &self.resources.facts)?;
                                break;
                            }
                            continue; // Continue the same original array's
                            // proven safe endpoints and signed advances.
                        }
                        #[cfg(feature = "paged-safe-glyph-slices")]
                        if payload.reuse_safe {
                            if payload.best.is_none() {
                                payload.kind = MarkdownSoftLineResolvedKind::OverflowingGrapheme;
                                payload.best = Some(Fit {
                                    end,
                                    width,
                                    shape: None,
                                    facts: None,
                                });
                            } // Preserve even the first overwide whole grapheme
                            // and explicitly report overflow instead of splitting it.
                            step.context_bytes =
                                choose_best(payload, provider, &self.resources.facts)?;
                            break; // A proven safe overwide candidate has the
                            // same reshaped width; do not duplicate that trial.
                        }
                    }
                    step.context_bytes =
                        start_current(payload, provider, &self.resources.facts, false)?;
                    break; // Unsafe/interior or other overwide candidates need
                    // actual shaping before terminating the width search.
                }
            }
            Stage::Input | Stage::TailInput => {
                // Prefix and tail each consume their real source through the
                // same maximum 4 KiB lease and caller's scalar allowance.
                let tail = matches!(payload.stage, Stage::TailInput);
                let owned = if tail {
                    payload.tail.as_mut()
                } else {
                    payload.current.as_mut()
                }
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
                let input = owned.job()?.step(
                    &payload.seed.authority,
                    &payload.seed.font,
                    MarkdownShapingBudget {
                        bytes: budget.input_bytes.min(4096),
                        scalars: budget.input_scalars,
                    },
                )?;
                step.input_bytes = input.bytes;
                step.input_scalars = input.scalars;
                if input.status == MarkdownShapingStatus::AwaitingShape {
                    payload.stage = if tail { Stage::TailShape } else { Stage::Shape };
                }
            }
            Stage::Shape | Stage::TailShape => {
                // Run one actual worker-owned synchronous shape. Finite input
                // leases alone are not evidence of bounded shaping CPU time.
                let tail = matches!(payload.stage, Stage::TailShape);
                let owned = if tail {
                    payload.tail.as_mut()
                } else {
                    payload.current.as_mut()
                }
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
                owned
                    .job()?
                    .shape(&payload.seed.authority, &payload.seed.font)?;
                payload.scan = GlyphFacts::default();
                payload.stage = if tail { Stage::TailScan } else { Stage::Scan };
                step.shapes = 1;
            }
            Stage::Scan | Stage::TailScan => {
                // Accumulate only actual output records in bounded batches;
                // source character counts cannot predict shaped glyph facts.
                let tail = matches!(payload.stage, Stage::TailScan);
                let owned = if tail {
                    payload.tail.as_mut()
                } else {
                    payload.current.as_mut()
                }
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
                let job = owned.job()?;
                let profile = job.profile()?;
                for _ in 0..budget.glyphs.min(MARKDOWN_SOFT_LINE_RESOLVE_GLYPHS) {
                    let Some(glyph) = job.glyph(
                        &payload.seed.authority,
                        &payload.seed.font,
                        payload.scan.count,
                    )?
                    else {
                        break;
                    };
                    payload.scan.count += 1;
                    payload.scan.width = payload
                        .scan
                        .width
                        .checked_add(i64::from(glyph.x_advance))
                        .ok_or(MarkdownSoftLineResolveError::Overflow)?;
                    payload.scan.flags |= glyph.flags;
                    payload.scan.missing += usize::from(glyph.glyph_id == 0);
                    step.glyphs += 1;
                } // Actual glyph fields retain signed advance and original
                // flags, without cmap or input-length approximations.
                if payload.scan.count == profile.glyph_count {
                    // Confirm actual output EOF and original shaper access
                    // before deciding fallback, completed tail, or accepted fit.
                    if job
                        .glyph(
                            &payload.seed.authority,
                            &payload.seed.font,
                            profile.glyph_count,
                        )?
                        .is_some()
                        || !profile.shaper_access_certified
                        || profile.source_borrow_max_bytes > 4096
                    {
                        return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
                    }
                    if payload.scan.missing > 0 {
                        let range = job.source_range();
                        pending(
                            payload,
                            MarkdownSoftLineResolvePending::FontFallback {
                                source_utf16: range,
                                missing_glyphs: payload.scan.missing,
                            },
                        );
                    } else if tail {
                        finish(payload)?;
                    } else {
                        step.context_bytes =
                            accept_current(payload, provider, &self.resources.facts)?;
                    }
                }
            }
            #[cfg(feature = "paged-safe-glyph-slices")]
            Stage::Reusable | Stage::ShapedPrefix => {} // Explicit wrappers
            // consume these states; unresolved remaining text is never Ready.
            Stage::Finished => return Err(MarkdownSoftLineResolveError::InconsistentCoverage),
        }
        if let Some(answer) = &payload.answer {
            step.answer = answer.clone();
        }
        Ok(step)
    }

    /// Read one real new glyph while borrowing the original measure/shape;
    /// a new same-text job cannot replace this output's original authority.
    pub fn glyph(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
        side: MarkdownSoftLineResolvedSide,
        ordinal: usize,
    ) -> Result<Option<MarkdownShapedGlyph>> {
        let result = (|| {
            self.guard(measure, original)?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
            if !matches!(
                payload.answer,
                Some(MarkdownSoftLineResolveAnswer::Resolved(_))
            ) {
                return Err(MarkdownSoftLineResolveError::InvalidRequest);
            }
            let job = match side {
                MarkdownSoftLineResolvedSide::Line => payload.line.as_mut(),
                MarkdownSoftLineResolvedSide::Remainder => payload.tail.as_mut(),
            };
            match job {
                Some(job) => {
                    Ok(job
                        .job()?
                        .glyph(&payload.seed.authority, &payload.seed.font, ordinal)?)
                }
                None if side == MarkdownSoftLineResolvedSide::Remainder && ordinal == 0 => Ok(None),
                None => Err(MarkdownSoftLineResolveError::InvalidRequest),
            }
        })();
        let result = match self
            .payload
            .as_ref()
            .map(|payload| payload.seed.source.health())
        {
            Some(Err(error)) => Err(error.into()),
            _ => result.and_then(|glyph| {
                self.guard(measure, original)?;
                Ok(glyph)
            }),
        };
        if result.is_err() {
            drop(self.payload.take());
        }
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        result
    }

    /// Dispose every real child job before checking receipts. The caller's
    /// independent original shape/index may still legitimately retain Source.
    pub fn close(mut self) -> Result<MarkdownSoftLineResolveResources> {
        drop(self.payload.take());
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let facts = self.resources.snapshot();
        if facts.active_jobs != 0
            || facts.jobs_created != facts.jobs_released
            || facts.cleanup_failed
        {
            return Err(MarkdownSoftLineResolveError::ResourceCleanup);
        }
        Ok(facts)
    }
}

/// Retire all candidate jobs when real font/dictionary/item work is still
/// required; none of their glyphs can bypass the unresolved dependency.
fn pending(payload: &mut Payload, reason: MarkdownSoftLineResolvePending) {
    drop(payload.current.take());
    drop(payload.best.take());
    drop(payload.line.take());
    drop(payload.tail.take());
    payload.answer = Some(MarkdownSoftLineResolveAnswer::Pending(reason));
    payload.stage = Stage::Finished;
}
/// Recover exact width only across complete safe clusters of the same original
/// provider, preserving signed prefix differences and explicit unknown results.
fn safe_width(
    payload: &Payload,
    provider: &mut dyn ResolutionProvider,
    end: usize,
) -> Result<Option<i64>> {
    let start = match provider
        .cluster(
            &payload.seed.authority,
            &payload.seed.font,
            payload.seed.start,
        )
        .map_err(provider_error)?
    {
        MarkdownGlyphClusterAnswer::Known(Some(cluster))
            if cluster.source_utf16.start == payload.seed.start
                && (payload.seed.start == payload.seed.item.start
                    || cluster.flags & UNSAFE == 0) =>
        {
            cluster
        }
        _ => return Ok(None),
    };
    let edge = if end == payload.seed.item.end {
        match provider
            .cluster(&payload.seed.authority, &payload.seed.font, end - 1)
            .map_err(provider_error)?
        {
            MarkdownGlyphClusterAnswer::Known(Some(cluster)) if cluster.source_utf16.end == end => {
                cluster
                    .prefix_x_26_6
                    .checked_add(cluster.x_advance_26_6)
                    .ok_or(MarkdownSoftLineResolveError::Overflow)?
            }
            _ => return Ok(None),
        }
    } else {
        match provider
            .cluster(&payload.seed.authority, &payload.seed.font, end)
            .map_err(provider_error)?
        {
            MarkdownGlyphClusterAnswer::Known(Some(cluster))
                if cluster.source_utf16.start == end && cluster.flags & UNSAFE == 0 =>
            {
                cluster.prefix_x_26_6
            }
            _ => return Ok(None),
        }
    };
    Ok(Some(
        edge.checked_sub(start.prefix_x_26_6)
            .ok_or(MarkdownSoftLineResolveError::Overflow)?,
    ))
}
/// Construct an actual prefix trial from this provider, recording only context
/// bytes the successful constructor really read before input ingestion starts.
fn start_current(
    payload: &mut Payload,
    provider: &mut dyn ResolutionProvider,
    resources: &Arc<ResourceFacts>,
    final_prefix: bool,
) -> Result<usize> {
    let mut job = provider.reshape_subrange(
        &payload.seed.authority,
        &payload.seed.font,
        payload.seed.start..payload.candidate,
    )?;
    // Before ingest, actual job reads contain adjacent context only. Propagate
    // constructor/profile failure rather than fabricating work from the request
    // extent or a configured halo size.
    let context_bytes = job.profile()?.source_bytes_read;
    let status = job.status();
    payload.current = Some(OwnedShape::new(job, resources)?);
    payload.final_prefix = final_prefix;
    payload.stage = if status == MarkdownShapingStatus::Ingesting {
        Stage::Input
    } else {
        Stage::Shape
    };
    Ok(context_bytes)
}
/// Accept the completed actual trial or return to GCB search. Width failures
/// retire only the relevant real child, retaining the best proven complete fit.
fn accept_current(
    payload: &mut Payload,
    provider: &mut dyn ResolutionProvider,
    resources: &Arc<ResourceFacts>,
) -> Result<usize> {
    if payload.final_prefix {
        // The final real shape must equal the width previously certified by
        // original safe clusters before it becomes the selected line.
        if payload.expected_width != Some(payload.scan.width) {
            return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
        }
        payload.line = payload.current.take();
        payload.line_facts = payload.scan;
        return start_tail(payload, provider, resources);
    }
    let fit = payload.scan.width <= payload.seed.width;
    if fit {
        // Move the actual successful trial into best; replacing an older fit
        // returns its job through its existing resource owner.
        payload.best = Some(Fit {
            end: payload.candidate,
            width: payload.scan.width,
            shape: payload.current.take(),
            facts: Some(payload.scan),
        });
        if matches!(payload.mode, Mode::Exact) || payload.candidate == payload.limit {
            return choose_best(payload, provider, resources);
        }
        payload.stage = Stage::Choose;
    } else if matches!(payload.mode, Mode::Exact) {
        drop(payload.current.take());
        payload.mode = Mode::Emergency;
        payload.next = payload.seed.start;
        payload.stage = Stage::Choose; // An overwide actual exact trial restarts
    // search at real GCBs rather than certifying the original estimate.
    } else if payload.best.is_some() {
        drop(payload.current.take());
        return choose_best(payload, provider, resources);
    } else {
        payload.kind = MarkdownSoftLineResolvedKind::OverflowingGrapheme;
        payload.best = Some(Fit {
            end: payload.candidate,
            width: payload.scan.width,
            shape: payload.current.take(),
            facts: Some(payload.scan),
        });
        // Preserve the complete first grapheme when even it overflows. Any new
        // selected job reports its actual context-read cost through choose_best.
        return choose_best(payload, provider, resources);
    }
    Ok(0) // Existing candidate transfer or phase changes read no new context.
}
/// Move the selected real fit or its private safe proof into the appropriate
/// next stage without deriving readiness from public coordinates alone.
fn choose_best(
    payload: &mut Payload,
    provider: &mut dyn ResolutionProvider,
    resources: &Arc<ResourceFacts>,
) -> Result<usize> {
    let mut best = payload
        .best
        .take()
        .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
    payload.selected_end = best.end;
    if payload.kind != MarkdownSoftLineResolvedKind::OverflowingGrapheme {
        payload.kind = if best.end == payload.limit {
            MarkdownSoftLineResolvedKind::Natural
        } else {
            MarkdownSoftLineResolvedKind::Emergency
        };
    }
    #[cfg(feature = "paged-safe-glyph-slices")]
    if payload.reuse_safe && best.shape.is_none() {
        payload.expected_width = Some(best.width);
        payload.stage = Stage::Reusable;
        return Ok(0); // The wrapper still checks actual GCB/index authority;
        // transferring this candidate reads no new context.
    }
    match best.shape.take() {
        Some(shape) => {
            // Reuse the actual already-shaped fit and its exact glyph facts.
            payload.line = Some(shape);
            payload.line_facts = best
                .facts
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
            start_tail(payload, provider, resources)
        }
        None => {
            // Materialize the selected prefix as a real job for subsequent
            // per-glyph consumption, preserving the certified expected width.
            payload.candidate = best.end;
            payload.expected_width = Some(best.width);
            start_current(payload, provider, resources, true)
        }
    }
}
/// Retain no placeholder tail. Either defer an explicitly owned continuation,
/// finish at the real item edge, or construct the actual remainder job.
fn start_tail(
    payload: &mut Payload,
    provider: &mut dyn ResolutionProvider,
    resources: &Arc<ResourceFacts>,
) -> Result<usize> {
    #[cfg(feature = "paged-safe-glyph-slices")]
    if payload.defer_tail {
        if payload.line.is_none() || payload.tail.is_some() {
            return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
        }
        payload.stage = Stage::ShapedPrefix;
        return Ok(0); // Only the actual shaped prefix is transferred; remaining
        // source stays a private cursor with no new context read.
    }
    if payload.selected_end == payload.seed.item.end {
        payload.scan = GlyphFacts::default();
        return finish(payload).map(|()| 0); // A truly empty remainder seals the
        // existing prefix without a new job/context.
    }
    let mut job = provider.reshape_subrange(
        &payload.seed.authority,
        &payload.seed.font,
        payload.selected_end..payload.seed.item.end,
    )?;
    // This turn owns actual tail-constructor context reads. Later ingestion
    // returns its own consumed bytes/scalars rather than counting context twice.
    let context_bytes = job.profile()?.source_bytes_read;
    let status = job.status();
    payload.tail = Some(OwnedShape::new(job, resources)?);
    payload.stage = if status == MarkdownShapingStatus::Ingesting {
        Stage::TailInput
    } else {
        Stage::TailShape
    };
    Ok(context_bytes)
}
/// Publish exact selected/tail facts only after their real output stages have
/// completed, retaining the original item boundaries and overflow classification.
fn finish(payload: &mut Payload) -> Result<()> {
    if payload.selected_end <= payload.seed.start || payload.selected_end > payload.seed.item.end {
        return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
    }
    payload.answer = Some(MarkdownSoftLineResolveAnswer::Resolved(
        MarkdownSoftLineResolvedBoundary {
            source_utf16: payload.seed.start..payload.selected_end,
            remainder_utf16: payload.selected_end..payload.seed.item.end,
            kind: payload.kind,
            x_advance_26_6: payload.line_facts.width,
            line_glyphs: payload.line_facts.count,
            remainder_glyphs: payload.scan.count,
            line_flags_union: payload.line_facts.flags,
            remainder_flags_union: payload.scan.flags,
        },
    ));
    payload.stage = Stage::Finished;
    Ok(())
}

#[cfg(test)]
mod tests;

/// Preserve the existing job error hierarchy; Store-specific first causes
/// remain represented by the original Measure::Stored error.
fn provider_error(error: MarkdownSoftLineMeasureError) -> MarkdownSoftLineResolveError {
    match error {
        MarkdownSoftLineMeasureError::Source(error) => error.into(),
        MarkdownSoftLineMeasureError::Shaping(error) => error.into(),
        MarkdownSoftLineMeasureError::Cluster(error) => error.into(),
        other => other.into(),
    }
}
