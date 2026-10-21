//! Measure finite break candidates from original same-Source glyph items; a width choice is not a Native soft line that is ready to draw.
//!
//! Consume actual clusters and GCB/UAX14 intersections in order, choosing the last fitting candidate before the first overflowing opportunity.
//! Signed advances need not be monotonic, so widths are not binary-searched and intervening clusters are not skipped.
//! Unsafe cuts, missing glyphs, dictionary work, and item continuation retain their explicit next stages; this module creates no shaper or window BOT/EOT.
use std::{
    ops::Range,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use thiserror::Error;
pub(crate) mod provider;
use provider::{GlyphProvider, GlyphProviderOrigin, JobGlyphProvider};
#[cfg(feature = "paged-shaping-frontier")]
mod frontier;

use crate::{
    MarkdownDocumentBufferError, MarkdownGlyphClusterAnswer, MarkdownGlyphClusterError,
    MarkdownGlyphClusterIndex, MarkdownLayoutBreakAnswer, MarkdownLayoutBreakCursor,
    MarkdownLayoutBreakError, MarkdownLayoutBreakJob, MarkdownLayoutBreakQuery,
    MarkdownLayoutBreakUnit, MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownLayoutTextSourceObserver, MarkdownLineBreakBoundary,
    MarkdownLineBreakKind, MarkdownOriginalBackingObserver, MarkdownShapingDirection,
    MarkdownShapingError, MarkdownShapingJob,
};

pub const MARKDOWN_SOFT_LINE_BREAK_QUERIES: usize = 64;
pub const MARKDOWN_SOFT_LINE_CLUSTERS: usize = 256;
const UNSAFE_TO_BREAK: u32 = harfrust::GlyphFlags::UNSAFE_TO_BREAK.to_bits();

#[derive(Clone, Default)]
pub struct MarkdownSoftLineMeasureCancellation(Arc<AtomicBool>);
impl MarkdownSoftLineMeasureCancellation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        // Share one revocation request across the original line and its derived permits; this does not close separately owned producers.
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    /// Store accepts only the same cancellation request; two currently false values do not establish shared future revocation.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn same_request(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A line start comes from a real selected opportunity; this request does not create a paragraph or a new shaping context.
#[derive(Clone)]
pub struct MarkdownSoftLineMeasureRequest {
    pub start_utf16: usize,
    pub width_26_6: i64,
    pub cancellation: MarkdownSoftLineMeasureCancellation,
}

#[derive(Debug, Error)]
pub enum MarkdownSoftLineMeasureError {
    #[error("软行测量源权限不匹配")]
    SourceMismatch,
    #[error("软行测量不属于原字形任务")]
    JobMismatch,
    #[error("软行测量范围、方向或配额无效")]
    InvalidRequest,
    #[error("软行测量坐标或advance溢出")]
    Overflow,
    #[error("软行机会与原字形覆盖不一致")]
    InconsistentCoverage,
    #[error("软行测量已取消")]
    Cancelled,
    #[error("软行测量已永久撤权")]
    Poisoned,
    #[error("软行原源已失败")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("软行原字形任务失败")]
    Shaping(#[from] MarkdownShapingError),
    #[error("软行cluster索引失败")]
    Cluster(#[from] MarkdownGlyphClusterError),
    #[error("软行断点索引失败")]
    Break(#[from] MarkdownLayoutBreakError),
    #[cfg(feature = "paged-line-compose")]
    #[error("已认证的存储字形来源失败")]
    Stored(#[source] Box<crate::MarkdownLineComposeError>),
}
type Result<T> = std::result::Result<T, MarkdownSoftLineMeasureError>;

/// Bound the two kinds of index access independently; one break query lets the original intersection inspect one actual candidate.
/// These are logical work limits, not elapsed-time, allocation, or kernel RSS measurements.
#[derive(Clone, Copy, Debug)]
pub struct MarkdownSoftLineMeasureBudget {
    pub break_queries: usize,
    pub clusters: usize,
}
impl Default for MarkdownSoftLineMeasureBudget {
    fn default() -> Self {
        Self {
            break_queries: MARKDOWN_SOFT_LINE_BREAK_QUERIES,
            clusters: MARKDOWN_SOFT_LINE_CLUSTERS,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSoftLineMeasuredBreak {
    pub source_utf16: Range<usize>,
    pub x_advance_26_6: i64,
    pub boundary: MarkdownLineBreakBoundary,
    /// Original flags at the following cluster's start; flags from other glyphs in the selected prefix must not be mixed into this value.
    pub boundary_flags: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineReshapeReason {
    LineStartInsideCluster,
    BoundaryInsideCluster,
    UnsafeToBreak,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineMeasureChoice {
    MeasuredBreak(MarkdownSoftLineMeasuredBreak),
    NeedsReshape {
        source_utf16: Range<usize>,
        boundary_utf16: usize,
        boundary_cluster_utf16: Range<usize>,
        boundary_flags: u32,
        measured_prefix_26_6: i64,
        full_width_26_6: Option<i64>,
        reason: MarkdownSoftLineReshapeReason,
    },
    NeedsFontFallback {
        cluster_utf16: Range<usize>,
        missing_glyphs: usize,
    },
    NeedsDictionary(MarkdownLineBreakBoundary),
    NeedsEmergencyBreak(MarkdownSoftLineMeasuredBreak),
    /// The actual certified prefix already overflows; word_end describes the original closed word, not current ready glyph coverage or a UAX opportunity.
    #[cfg(feature = "paged-shaping-frontier")]
    NeedsFrontierEmergencyPrefix {
        source_utf16: Range<usize>,
        word_end_utf16: usize,
        measured_prefix_26_6: i64,
    },
    /// An emergency slice verified by original per-GCB work and actual following-cluster flags, without a natural UAX opportunity.
    #[cfg(feature = "paged-shaping-frontier")]
    MeasuredEmergencyBreak {
        source_utf16: Range<usize>,
        x_advance_26_6: i64,
        boundary_flags: u32,
        overflowing_grapheme: bool,
    },
    /// The current item is consumed, but following-item glyph evidence is still needed; this is not document EOF or line readiness.
    NeedNextItem {
        source_utf16: Range<usize>,
        x_advance_26_6: i64,
        last_fitting: Option<MarkdownSoftLineMeasuredBreak>,
    },
    EmptySource,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineMeasureAnswer {
    PendingDependencies,
    Yielded,
    Selected(MarkdownSoftLineMeasureChoice),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSoftLineMeasureStep {
    pub break_queries: usize,
    pub clusters: usize,
    pub answer: MarkdownSoftLineMeasureAnswer,
}

struct Fit {
    measured: MarkdownSoftLineMeasuredBreak,
    boundary_cluster: Range<usize>,
}
struct Payload {
    source: MarkdownLayoutTextSource,
    start_cursor: Option<MarkdownLayoutBreakCursor>,
    cursor: Option<MarkdownLayoutBreakCursor>,
    candidate: Option<MarkdownLineBreakBoundary>,
    next_query: usize,
    scan: usize,
    advance: i64,
    fitting: Option<Fit>,
    selected: Option<MarkdownSoftLineMeasureChoice>,
    #[cfg(feature = "paged-line-compose")]
    folded_boundary: Option<usize>,
    #[cfg(feature = "paged-shaping-frontier")]
    word_probe: Option<frontier::WordClusterProbe>,
    #[cfg(feature = "paged-shaping-frontier")]
    word_safe: Option<frontier::WordSafeState>,
}

/// Derive only from the original Measure's internal selection; assembling public Choice fields cannot create reshaping authority.
#[cfg(feature = "paged-soft-line-resolve")]
pub(crate) struct MarkdownSoftLineResolveSeed {
    pub source: MarkdownLayoutTextSource,
    pub authority: MarkdownLayoutTextAuthority,
    pub font: MarkdownLayoutFontAuthority,
    pub owner: Weak<()>,
    pub origin: GlyphProviderOrigin,
    #[cfg(feature = "paged-safe-glyph-slices")]
    pub revoked: Arc<AtomicBool>,
    pub item: Range<usize>,
    pub start: usize,
    pub width: i64,
    pub choice: MarkdownSoftLineMeasureChoice,
    pub cancellation: MarkdownSoftLineMeasureCancellation,
}

/// Retain no strong ShapeJob, cluster/break index, or Font edge; each operation reborrows the actual original owner for verification.
/// The payload owns one immutable Source claim; failure or Drop releases that claim, while a selected result alone does not.
pub struct MarkdownSoftLineMeasureJob {
    #[cfg(feature = "paged-soft-line-resolve")]
    identity: Arc<()>,
    #[cfg(feature = "paged-safe-glyph-slices")]
    proof_revoked: Arc<AtomicBool>,
    #[cfg(feature = "paged-safe-glyph-slices")]
    line_alive: Arc<AtomicBool>,
    payload: Option<Payload>,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    origin: GlyphProviderOrigin,
    generation: u64,
    direction: MarkdownShapingDirection,
    backing: Option<MarkdownOriginalBackingObserver>,
    glyph_count: usize,
    item: Range<usize>,
    start: usize,
    width: i64,
    cancellation: MarkdownSoftLineMeasureCancellation,
    cancelled: bool,
    resources: MarkdownLayoutTextSourceObserver,
    #[cfg(feature = "paged-line-compose")]
    start_issued: std::cell::Cell<bool>,
}
impl MarkdownSoftLineMeasureJob {
    /// Consume only a single-use start ticket derived after the preceding actual LineEnd commit; an arbitrary GCB/range is not a UAX opportunity.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn after_selected_start(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        provider: &mut dyn GlyphProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        request: MarkdownSoftLineMeasureRequest,
        proof: provider::SelectedLineStart,
    ) -> Result<Self> {
        proof.require(
            &source,
            authority,
            font,
            provider,
            request.start_utf16,
            &request.cancellation,
        )?;
        let mut job = Self::new_with_provider(source, authority, font, provider, breaks, request)?;
        let payload = job
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
        // Keep all constructor checks for actual Source/Font/completion, scalar endpoints, generation, and cancellation.
        // Only the GCB actually selected and committed as this Store's latest LineEnd bypasses the natural-opportunity check; its ticket is consumed by value.
        payload.source.health()?;
        payload.start_cursor = None;
        Ok(job)
    }

    /// Accept only an actual GCB start privately certified by resolution; an emergency cut does not become an original UAX14 allowed opportunity.
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn after_resolved_start(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        shape: &mut MarkdownShapingJob,
        breaks: &mut MarkdownLayoutBreakJob,
        request: MarkdownSoftLineMeasureRequest,
        proof: &crate::markdown_soft_line_resolve::MarkdownResolvedLineStart,
    ) -> Result<Self> {
        proof.require(&source, authority, font, shape, breaks, request.start_utf16)?;
        let mut job = Self::new(source, authority, font, shape, breaks, request)?;
        job.payload
            .as_mut()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
            .start_cursor = None;
        // Preserve constructor endpoint/direction/actual Ready checks. Only the original Unicode-opportunity check is bypassed:
        // a sealed proof must certify completed resolution, a complete GCB, and this actual tail Job, rather than external coordinates or a bool.
        Ok(job)
    }

    /// Bind an actually Ready horizontal item without creating a window-specific run or inferring a paragraph boundary.
    ///
    /// # Errors
    /// Reject foreign Source/Font authority, non-scalar endpoints, a false empty-item tail, nonhorizontal direction, or invalid width.
    pub fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        shape: &mut MarkdownShapingJob,
        breaks: &mut MarkdownLayoutBreakJob,
        request: MarkdownSoftLineMeasureRequest,
    ) -> Result<Self> {
        Self::new_with_provider(
            source,
            authority,
            font,
            &mut JobGlyphProvider::new(shape, None),
            breaks,
            request,
        )
    }

    /// The sealed Store adapter borrows only actually admitted data; external crates cannot implement a provider from a public range.
    pub(crate) fn new_with_provider(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        provider: &mut dyn GlyphProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        request: MarkdownSoftLineMeasureRequest,
    ) -> Result<Self> {
        let MarkdownSoftLineMeasureRequest {
            start_utf16,
            width_26_6,
            cancellation,
        } = request;
        // Check the captured Source before caller-supplied authority or producer metadata, retaining its real failure as the leading result.
        source.health()?;
        if !source.permits(authority) {
            return Err(MarkdownSoftLineMeasureError::SourceMismatch);
        }
        if cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        let profile = provider.require(authority, font)?; // Verify actual Source/Font/completion; a stored producer does not impersonate a Job.
        breaks.require_source(authority)?;
        if profile.generation != source.generation() {
            return Err(MarkdownSoftLineMeasureError::SourceMismatch);
        }
        let item = profile.source_utf16.clone();
        // An item endpoint is not a new line start in a nonempty Source; continuation must instead consume the original handoff protocol.
        if width_26_6 <= 0
            || !matches!(
                profile.direction,
                MarkdownShapingDirection::LeftToRight | MarkdownShapingDirection::RightToLeft
            )
            || start_utf16 < item.start
            || start_utf16 > item.end
            || (start_utf16 == item.end && source.utf16_len() != 0)
        {
            return Err(MarkdownSoftLineMeasureError::InvalidRequest);
        }
        source.byte_offset_for_utf16(start_utf16)?; // A low-surrogate position is not a scalar boundary; length metadata cannot authorize that start.
        let start_cursor = if start_utf16 == 0 {
            None // The actual document origin is a line start; an arbitrary piece or shaping window origin is not.
        } else {
            Some(breaks.cursor(
                MarkdownLayoutBreakUnit::Utf16,
                start_utf16,
                MarkdownLayoutBreakQuery::At,
            )?)
        };
        let resources = source.resource_observer();
        let backing = source.original_backing_observer();
        // Coordinate lookup may have touched real source storage; recheck health and cancellation before publishing the owning payload.
        source.health()?;
        if cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        Ok(Self {
            #[cfg(feature = "paged-soft-line-resolve")]
            identity: Arc::new(()),
            #[cfg(feature = "paged-safe-glyph-slices")]
            proof_revoked: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "paged-safe-glyph-slices")]
            line_alive: Arc::new(AtomicBool::new(true)),
            payload: Some(Payload {
                source,
                start_cursor,
                cursor: None,
                candidate: None,
                next_query: start_utf16,
                scan: start_utf16,
                advance: 0,
                fitting: None,
                selected: None,
                #[cfg(feature = "paged-line-compose")]
                folded_boundary: None,
                #[cfg(feature = "paged-shaping-frontier")]
                word_probe: None,
                #[cfg(feature = "paged-shaping-frontier")]
                word_safe: None,
            }),
            source: authority.clone(),
            font: font.clone(),
            origin: profile.origin,
            generation: profile.generation,
            direction: profile.direction,
            backing,
            glyph_count: profile.glyphs,
            item,
            start: start_utf16,
            width: width_26_6,
            cancellation,
            cancelled: false,
            resources,
            #[cfg(feature = "paged-line-compose")]
            start_issued: std::cell::Cell::new(false),
        })
    }

    fn guard_provider(&self, provider: &mut dyn GlyphProvider) -> Result<()> {
        // Keep the original backing's first errno observable even after failure has released this Measure's Source claim.
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        if let Some(payload) = &self.payload {
            payload.source.health()?;
        }
        if self.origin != provider.origin() {
            return Err(MarkdownSoftLineMeasureError::JobMismatch);
        } // Check original opaque origin before a foreign Job can replace identity failure with its Font error; the actual Source errno was checked first.
        let facts = provider.require(&self.source, &self.font)?;
        if self.origin != facts.origin
            || self.item != facts.source_utf16
            || self.glyph_count != facts.glyphs
            || self.generation != facts.generation
            || self.direction != facts.direction
        {
            return Err(MarkdownSoftLineMeasureError::JobMismatch);
        }
        // Original coverage, generation, direction, and glyph count must all remain attached to this provider before accepting its result.
        if self.cancelled || self.cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        if self.payload.is_none() {
            return Err(MarkdownSoftLineMeasureError::Poisoned);
        }
        Ok(()) // Identity, all facts, and real health must agree; old directory lengths or an equal Font name cannot reissue authority.
    }
    fn fail(&mut self, error: &MarkdownSoftLineMeasureError) {
        #[cfg(feature = "paged-safe-glyph-slices")]
        {
            self.proof_revoked.store(true, Ordering::Release);
            self.line_alive.store(false, Ordering::Release);
        } // Failure revokes both current-item and whole-line historical receipts; a successful continuation revokes only the current-item ticket.
        self.cancelled |= matches!(error, MarkdownSoftLineMeasureError::Cancelled);
        drop(self.payload.take()); // Release this layer's single strong Source claim on failure; no result or cursor secretly retains it.
    }

    /// Derive the next stage from that same actual selection; creation requires a selected result, an uncancelled request, and original Job authority.
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn resolve_seed(
        &self,
        shape: &mut MarkdownShapingJob,
    ) -> Result<MarkdownSoftLineResolveSeed> {
        self.resolve_seed_provider(&mut JobGlyphProvider::new(shape, None))
    }

    /// Share internal selection copying while requiring the original provider; equal fields in a new directory cannot replace the original request.
    #[cfg(feature = "paged-soft-line-resolve")]
    fn resolve_seed_provider(
        &self,
        provider: &mut dyn GlyphProvider,
    ) -> Result<MarkdownSoftLineResolveSeed> {
        self.guard_provider(provider)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
        let choice = payload
            .selected
            .clone()
            .ok_or(MarkdownSoftLineMeasureError::InvalidRequest)?;
        Ok(MarkdownSoftLineResolveSeed {
            source: payload.source.clone(),
            authority: self.source.clone(),
            font: self.font.clone(),
            owner: Arc::downgrade(&self.identity),
            origin: self.origin.clone(),
            #[cfg(feature = "paged-safe-glyph-slices")]
            revoked: Arc::clone(&self.proof_revoked),
            item: self.item.clone(),
            start: self.start,
            width: self.width,
            choice,
            cancellation: self.cancellation.clone(),
        })
    }

    /// Each resolution operation reborrows the original Measure and Shape; equal bytes/width in another measurement do not replace its nonce.
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn require_resolve(
        &self,
        owner: &Weak<()>,
        shape: &mut MarkdownShapingJob,
    ) -> Result<()> {
        self.require_resolve_provider(owner, &mut JobGlyphProvider::new(shape, None))
    }

    /// Job and Store share the Source-first, nonce, and actual-provider checks without reissuing the original output identity.
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn require_resolve_provider(
        &self,
        owner: &Weak<()>,
        provider: &mut dyn GlyphProvider,
    ) -> Result<()> {
        if let Some(payload) = &self.payload {
            payload.source.health()?; // Original I/O failure precedes a foreign nonce or a superficial cancellation result.
        }
        if !Weak::ptr_eq(owner, &Arc::downgrade(&self.identity)) {
            return Err(MarkdownSoftLineMeasureError::JobMismatch);
        }
        self.guard_provider(provider)
    }

    /// Only the original unsafe/emergency selection enters controlled reshaping; an already safe line uses the existing selected permit.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn resolution_permit<'a>(
        &'a self,
        provider: &mut dyn GlyphProvider,
    ) -> Result<provider::ResolutionMeasurePermit<'a>> {
        self.guard_provider(provider)?;
        self.require_resolution()?;
        self.require_live_line()?;
        Ok(provider::ResolutionMeasurePermit::new(self))
    }

    #[cfg(feature = "paged-line-compose")]
    fn require_resolution(&self) -> Result<()> {
        match self
            .payload
            .as_ref()
            .and_then(|payload| payload.selected.as_ref())
        {
            Some(
                MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(_)
                | MarkdownSoftLineMeasureChoice::NeedsReshape { .. },
            ) => Ok(()),
            #[cfg(feature = "paged-shaping-frontier")]
            Some(MarkdownSoftLineMeasureChoice::NeedsFrontierEmergencyPrefix { .. }) => Ok(()),
            _ => Err(MarkdownSoftLineMeasureError::InvalidRequest),
        } // Font fallback, dictionary work, incomplete dependencies, and NeedNextItem cannot become resolved soft lines through this entry.
    }

    /// When the whole selected line lies in an earlier item of this Store, restore its actual Font/origin as the reshaping anchor.
    /// First obtain resolution_permit using the current provider; the handoff borrows one Store at a time and does not synthesize cross-item text.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn restore_selected_item(
        &mut self,
        previous: &mut dyn GlyphProvider,
        previous_font: &MarkdownLayoutFontAuthority,
    ) -> Result<()> {
        let result = (|| {
            self.require_live_line()?;
            self.require_resolution()?;
            let profile = previous.require(&self.source, previous_font)?;
            if !matches!(self.origin, provider::GlyphProviderOrigin::Stored { .. })
                || !self.origin.same_provider_family(&profile.origin)
            {
                return Err(MarkdownSoftLineMeasureError::JobMismatch);
            } // Only the original Store's actual earlier directory can anchor this handoff; another same-Source Store or a Job cannot.
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            let selected = match payload.selected.as_ref() {
                Some(MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(value)) => {
                    &value.source_utf16
                }
                Some(MarkdownSoftLineMeasureChoice::NeedsReshape { source_utf16, .. }) => {
                    source_utf16
                }
                #[cfg(feature = "paged-shaping-frontier")]
                Some(MarkdownSoftLineMeasureChoice::NeedsFrontierEmergencyPrefix {
                    source_utf16,
                    ..
                }) => source_utf16,
                _ => return Err(MarkdownSoftLineMeasureError::InvalidRequest),
            };
            if selected.start != self.start
                || selected.is_empty()
                || selected.start < profile.source_utf16.start
                || selected.end > profile.source_utf16.end
                || profile.source_utf16.end > self.item.start
                || profile.generation != self.generation
            {
                return Err(MarkdownSoftLineMeasureError::InvalidRequest);
            } // Selection must lie completely in this one actual earlier item; a range spanning Fonts/scripts still needs its own composition capability.
            #[cfg(feature = "paged-safe-glyph-slices")]
            {
                self.proof_revoked.store(true, Ordering::Release);
                self.proof_revoked = Arc::new(AtomicBool::new(false));
            } // Revoke old current-item tickets while retaining the line nonce, choice, width, and alive bit; no independent Measure is reissued.
            self.font = previous_font.clone();
            self.origin = profile.origin;
            self.glyph_count = profile.glyphs;
            self.direction = profile.direction;
            self.item = profile.source_utf16;
            self.guard_provider(previous)
        })();
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    /// Continue the original line only after it explicitly requests another item; direction, Font, and style boundaries do not reset accumulated width.
    ///
    /// Both actually Ready Jobs must be exactly adjacent on the same Source. The previous Job is borrowed only for this verification,
    /// not retained afterward; the caller still owns cross-item glyphs/spools, and continuation does not imply drawing or whole-line readiness.
    ///
    /// # Errors
    /// A foreign previous item, gap, overlap, wrong Source/Font, nonhorizontal item, or invalid stage permanently revokes this measurement.
    pub fn continue_item(
        &mut self,
        previous: &mut MarkdownShapingJob,
        next: &mut MarkdownShapingJob,
        next_font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
    ) -> Result<()> {
        self.prepare_continue(&mut JobGlyphProvider::new(previous, None))?
            .continue_with(&mut JobGlyphProvider::new(next, None), next_font, breaks)
    }

    /// Verify the actual previous item before exclusively borrowing this Measure. The ticket does not borrow that provider,
    /// so its Store borrow can end before borrowing the next item, without overlapping mutable Store references or a shared lock.
    pub(crate) fn prepare_continue(
        &mut self,
        previous: &mut dyn GlyphProvider,
    ) -> Result<provider::PreparedMeasureContinuation<'_>> {
        let result = self
            .guard_provider(previous)
            .and_then(|()| self.require_next_item());
        if let Err(error) = result {
            self.fail(&error);
            return Err(error);
        }
        Ok(provider::PreparedMeasureContinuation::new(self))
    }

    /// Temporary permits and handoffs share this stage check; public Choice fields do not prove actual consumption of the previous item.
    fn require_next_item(&self) -> Result<()> {
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
        match &payload.selected {
            Some(MarkdownSoftLineMeasureChoice::NeedNextItem {
                source_utf16,
                x_advance_26_6,
                ..
            }) if source_utf16 == &(self.start..self.item.end)
                && *x_advance_26_6 == payload.advance
                && payload.scan == self.item.end
                && !self.item.is_empty() =>
            {
                Ok(())
            }
            _ => Err(MarkdownSoftLineMeasureError::InvalidRequest),
        } // An unfinished item, final opportunity, reshaping stage, or missing Font cannot be overwritten by the next item.
    }

    fn continue_prepared(
        &mut self,
        next: &mut dyn GlyphProvider,
        next_font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
    ) -> Result<()> {
        let result = (|| {
            // Preparation leaves only the ticket's exclusive borrow of this state; still recheck actual Source health and cancellation.
            // Valid adjacent coordinates cannot hide an I/O failure that arose while waiting to reborrow the Store.
            self.require_live_line()?;
            self.require_next_item()?;
            breaks.require_source(&self.source)?;
            let profile = next.require(&self.source, next_font)?;
            #[cfg(feature = "paged-line-compose")]
            let folded = next.folded_section();
            if !self.origin.same_provider_family(&profile.origin) {
                return Err(MarkdownSoftLineMeasureError::JobMismatch);
            } // Stored directories hand off only within the original Store; a second same-Source file does not continue its certified history.
            let range = profile.source_utf16.clone();
            let source_len = self
                .payload
                .as_ref()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
                .source
                .utf16_len();
            if range.start != self.item.end
                || range.is_empty()
                || range.end > source_len
                || profile.generation != self.generation
                || !matches!(
                    profile.direction,
                    MarkdownShapingDirection::LeftToRight | MarkdownShapingDirection::RightToLeft
                )
            {
                return Err(MarkdownSoftLineMeasureError::InvalidRequest);
            } // Actual global ranges must be adjacent without gaps/overlaps; an item end still does not establish paragraph EOF.
            #[cfg(feature = "paged-line-compose")]
            if folded
                .as_ref()
                .is_some_and(|hidden| hidden != &range || profile.glyphs != 0)
            {
                return Err(MarkdownSoftLineMeasureError::InvalidRequest);
            } // Only this Store's actual zero-glyph control admits a hidden range; an ordinary shaping item cannot authorize skipping text.
            self.require_live_line()?;
            #[cfg(feature = "paged-safe-glyph-slices")]
            {
                self.proof_revoked.store(true, Ordering::Release);
                self.proof_revoked = Arc::new(AtomicBool::new(false));
            } // Revoke only the current item's tickets and keep the actual line scope; failure or Drop revokes the whole line.
            self.font = next_font.clone();
            self.origin = profile.origin;
            self.glyph_count = profile.glyphs;
            self.direction = profile.direction;
            self.item = range;
            #[cfg(feature = "paged-shaping-frontier")]
            {
                self.payload
                    .as_mut()
                    .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
                    .word_probe = None;
            }
            self.payload
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
                .selected = None;
            #[cfg(feature = "paged-line-compose")]
            if let Some(hidden) = folded {
                // Retain the measured header's accumulated width and skip only the hidden interior's opportunities.
                // Query the actual terminal Mandatory; following-cluster flags still use the original NeedNextItem/SelectedPermit chain.
                let payload = self
                    .payload
                    .as_mut()
                    .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
                payload.cursor = Some(breaks.cursor(
                    MarkdownLayoutBreakUnit::Utf16,
                    hidden.end,
                    MarkdownLayoutBreakQuery::At,
                )?);
                payload.candidate = None;
                payload.next_query = hidden.start;
                payload.scan = hidden.end;
                payload.folded_boundary = Some(hidden.end);
                #[cfg(feature = "paged-shaping-frontier")]
                {
                    payload.word_safe = None;
                }
            }
            // Outside the explicit fold branch, keep the cursor, candidate, accumulated advance, and last fitting unchanged.
            // The next step still reads actual following-cluster flags at the seam; item segmentation is not a safe break.
            self.guard_provider(next)
        })();
        let result = match self.require_live_line() {
            // Preserve a real Source failure found after handoff work instead of returning a weaker metadata/cancellation outcome.
            Err(MarkdownSoftLineMeasureError::Source(error)) => Err(error.into()),
            _ => result,
        };
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    /// Check only this line's existing Source and cancellation scalars; this cannot replace provider verification of actual Font/completion authority.
    fn require_live_line(&self) -> Result<()> {
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        self.payload
            .as_ref()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
            .source
            .health()?;
        if self.cancelled || self.cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        Ok(())
    }

    /// Issue this temporary permit only at actual NeedNextItem, borrowing the original Measure until the producer finishes using it.
    #[cfg(feature = "paged-safe-glyph-slices")]
    pub(crate) fn measured_item_permit<'a>(
        &'a self,
        provider: &mut dyn GlyphProvider,
    ) -> Result<provider::MeasuredItemPermit<'a>> {
        self.guard_provider(provider)?;
        self.require_next_item()?;
        Ok(provider::MeasuredItemPermit::new(self))
    }

    /// Only a safe natural/emergency cut actually selected by the original state machine can issue this temporary permit; a public Choice copy cannot.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn selected_permit<'a>(
        &'a self,
        provider: &mut dyn GlyphProvider,
    ) -> Result<provider::SelectedMeasurePermit<'a>> {
        self.guard_provider(provider)?;
        self.require_selected()?;
        Ok(provider::SelectedMeasurePermit::new(self))
    }

    #[cfg(feature = "paged-line-compose")]
    fn require_selected(&self) -> Result<provider::SelectedMeasureSelection> {
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
        match payload.selected.as_ref() {
            Some(MarkdownSoftLineMeasureChoice::MeasuredBreak(selected))
                if selected.source_utf16.start == self.start
                    && selected.source_utf16.end > self.start
                    && selected.source_utf16.end <= self.item.end
                    && selected.boundary.utf16 == selected.source_utf16.end
                    && selected.boundary_flags & UNSAFE_TO_BREAK == 0 =>
            {
                Ok(provider::SelectedMeasureSelection {
                    source_utf16: selected.source_utf16.clone(),
                    x_advance_26_6: selected.x_advance_26_6,
                    boundary_flags: selected.boundary_flags,
                    kind: provider::SelectedMeasureKind::Natural(selected.boundary),
                })
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Some(MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak {
                source_utf16,
                x_advance_26_6,
                boundary_flags,
                overflowing_grapheme,
            }) if source_utf16.start == self.start
                && source_utf16.end > self.start
                && source_utf16.end <= self.item.end
                && boundary_flags & UNSAFE_TO_BREAK == 0 =>
            {
                Ok(provider::SelectedMeasureSelection {
                    source_utf16: source_utf16.clone(),
                    x_advance_26_6: *x_advance_26_6,
                    boundary_flags: *boundary_flags,
                    kind: provider::SelectedMeasureKind::Emergency {
                        overflowing_grapheme: *overflowing_grapheme,
                    },
                })
            }
            _ => Err(MarkdownSoftLineMeasureError::InvalidRequest),
        } // A measured width does not make missing glyphs, dictionary work, pending item joins, unsafe cuts, or cluster interiors safe line ends.
    }

    /// Read original indexes in finite quanta; Pending requires actual dependency progress, while Yielded retains this task's scan position.
    pub fn step(
        &mut self,
        shape: &mut MarkdownShapingJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineMeasureBudget,
    ) -> Result<MarkdownSoftLineMeasureStep> {
        self.step_with_provider(
            &mut JobGlyphProvider::new(shape, Some(clusters)),
            breaks,
            budget,
        )
    }
    pub(crate) fn step_with_provider(
        &mut self,
        provider: &mut dyn GlyphProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineMeasureBudget,
    ) -> Result<MarkdownSoftLineMeasureStep> {
        // Even a cached choice must pass the actual source/provider checks again before it can leave this step.
        let result = self.advance(provider, breaks, budget);
        let result = match self.payload.as_ref().map(|payload| payload.source.health()) {
            Some(Err(error)) => Err(error.into()), // An actual shared Source failure precedes this step's metadata/cancellation result.
            _ => result.and_then(|value| {
                self.guard_provider(provider)?;
                Ok(value)
            }),
        };
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    fn advance(
        &mut self,
        provider: &mut dyn GlyphProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineMeasureBudget,
    ) -> Result<MarkdownSoftLineMeasureStep> {
        self.guard_provider(provider)?;
        breaks.require_source(&self.source)?;
        // Both index dimensions must be usable; a zero quota is an invalid request rather than an invented Pending dependency.
        if budget.break_queries == 0 || budget.clusters == 0 {
            return Err(MarkdownSoftLineMeasureError::InvalidRequest);
        }
        let mut step = MarkdownSoftLineMeasureStep {
            break_queries: 0,
            clusters: 0,
            answer: MarkdownSoftLineMeasureAnswer::Yielded,
        };
        loop {
            self.guard_provider(provider)?; // Check revocation before each fixed-record operation, rather than after an entire opportunity or long cluster.
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            if let Some(choice) = &payload.selected {
                step.answer = MarkdownSoftLineMeasureAnswer::Selected(choice.clone());
                return Ok(step);
            }
            if payload.source.utf16_len() == 0 {
                payload.selected = Some(MarkdownSoftLineMeasureChoice::EmptySource);
                continue; // Only the actual empty Source qualifies; an empty item or unexplored prefix is not an empty document.
            }
            if let Some(cursor) = &mut payload.start_cursor {
                // Resume the same At cursor under the break-query quota; only a Known opportunity discharges this start obligation.
                if step.break_queries == budget.break_queries.min(MARKDOWN_SOFT_LINE_BREAK_QUERIES)
                {
                    return Ok(step);
                }
                step.break_queries += 1;
                match breaks.query(cursor, 1)? {
                    MarkdownLayoutBreakAnswer::PendingSource => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(step);
                    }
                    MarkdownLayoutBreakAnswer::Yielded => continue,
                    MarkdownLayoutBreakAnswer::Known(Some(_)) => payload.start_cursor = None,
                    MarkdownLayoutBreakAnswer::Known(None) => {
                        return Err(MarkdownSoftLineMeasureError::InvalidRequest);
                    }
                } // A nonzero start needs a confirmed opportunity; a script, Font, or window boundary is not a selected line start.
                continue;
            }
            #[cfg(feature = "paged-shaping-frontier")]
            if self.advance_word_frontier(provider, breaks, budget, &mut step)? {
                return Ok(step);
            }
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            if payload.candidate.is_none() {
                // One saved cursor locates the next actual intersection; an unresolved query does not manufacture a candidate or skip ahead.
                if step.break_queries == budget.break_queries.min(MARKDOWN_SOFT_LINE_BREAK_QUERIES)
                {
                    return Ok(step);
                }
                if payload.cursor.is_none() {
                    payload.cursor = Some(breaks.cursor(
                        MarkdownLayoutBreakUnit::Utf16,
                        payload.next_query,
                        MarkdownLayoutBreakQuery::Next,
                    )?);
                }
                step.break_queries += 1;
                match breaks.query(
                    payload
                        .cursor
                        .as_mut()
                        .ok_or(MarkdownSoftLineMeasureError::InconsistentCoverage)?,
                    1,
                )? {
                    MarkdownLayoutBreakAnswer::PendingSource => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(step);
                    }
                    MarkdownLayoutBreakAnswer::Yielded => continue,
                    MarkdownLayoutBreakAnswer::Known(Some(candidate)) => {
                        if candidate.utf16 <= payload.next_query
                            || candidate.utf16 > payload.source.utf16_len()
                        {
                            return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                        }
                        #[cfg(feature = "paged-line-compose")]
                        if let Some(end) = payload.folded_boundary.take()
                            && (candidate.utf16 != end
                                || candidate.kind != MarkdownLineBreakKind::Mandatory)
                        {
                            return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                        } // Even explicit fold intent cannot relabel a non-paragraph coordinate as Mandatory or document EOF.
                        payload.cursor = None;
                        payload.candidate = Some(candidate);
                    }
                    MarkdownLayoutBreakAnswer::Known(None) => {
                        return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                    }
                }
                continue;
            }
            let candidate = payload
                .candidate
                .ok_or(MarkdownSoftLineMeasureError::InconsistentCoverage)?;
            if candidate.needs_dictionary {
                payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedsDictionary(candidate));
                continue; // A default SA opportunity does not prove completed dictionary work or permit issuing a complete soft line.
            }
            let target = candidate.utf16.min(self.item.end);
            // Prefix consumption and the following-cluster lookup share the same cluster quota, independently of break queries.
            if step.clusters == budget.clusters.min(MARKDOWN_SOFT_LINE_CLUSTERS) {
                return Ok(step);
            }
            if payload.scan < target {
                step.clusters += 1;
                let value = match provider.cluster(&self.source, &self.font, payload.scan)? {
                    MarkdownGlyphClusterAnswer::Pending => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(step);
                    }
                    MarkdownGlyphClusterAnswer::Known(Some(value)) => value,
                    MarkdownGlyphClusterAnswer::Known(None) => {
                        return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                    }
                };
                if value.source_utf16.start > payload.scan
                    || value.source_utf16.end <= payload.scan
                    || value.source_utf16.end > self.item.end
                {
                    return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                } // A hole in the actual index is a data error, not an ordinary boundary that merely needs reshaping.
                if value.source_utf16.start != payload.scan || value.source_utf16.end > target {
                    let at_start = value.source_utf16.start < payload.scan;
                    payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedsReshape {
                        source_utf16: self.start..target,
                        boundary_utf16: if at_start { self.start } else { target },
                        boundary_cluster_utf16: value.source_utf16,
                        boundary_flags: value.flags,
                        measured_prefix_26_6: payload.advance,
                        full_width_26_6: None,
                        reason: if at_start {
                            MarkdownSoftLineReshapeReason::LineStartInsideCluster
                        } else {
                            MarkdownSoftLineReshapeReason::BoundaryInsideCluster
                        },
                    });
                    continue; // The actual opportunity lies inside a shaping cluster; accumulated width is not the complete candidate width.
                }
                if value.missing_glyphs > 0 {
                    payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedsFontFallback {
                        cluster_utf16: value.source_utf16,
                        missing_glyphs: value.missing_glyphs,
                    });
                    continue; // Glyph zero's advance cannot stand in for the width produced by an actual fallback Font.
                }
                // Preserve the signed original advance and detect overflow; clamping or taking an absolute value would change the measured prefix.
                payload.advance = payload
                    .advance
                    .checked_add(value.x_advance_26_6)
                    .ok_or(MarkdownSoftLineMeasureError::Overflow)?;
                payload.scan = value.source_utf16.end;
                continue;
            }
            #[cfg(feature = "paged-line-compose")]
            let paragraph_control = candidate.kind == MarkdownLineBreakKind::Mandatory
                && provider.ends_paragraph_control();
            #[cfg(not(feature = "paged-line-compose"))]
            let paragraph_control = false;
            if candidate.utf16 > self.item.end
                || (candidate.utf16 == self.item.end
                    && self.item.end != payload.source.utf16_len()
                    && !paragraph_control)
            {
                // A fitting earlier opportunity can finish an overflowing prefix; otherwise retain it while requesting actual next-item evidence.
                if payload.advance > self.width
                    && let Some(fit) = payload.fitting.take()
                {
                    payload.selected = Some(selected(fit));
                } else {
                    payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedNextItem {
                        source_utf16: self.start..self.item.end,
                        x_advance_26_6: payload.advance,
                        last_fitting: payload.fitting.as_ref().map(|fit| fit.measured.clone()),
                    });
                }
                continue; // An item end is not paragraph EOF; do not issue a new break without following flags and complete candidate width.
            }
            // Break safety belongs to the cluster beginning after the candidate, not to flags accumulated from the preceding glyphs.
            let (flags, boundary_cluster) = if target < self.item.end {
                step.clusters += 1;
                match provider.cluster(&self.source, &self.font, target)? {
                    MarkdownGlyphClusterAnswer::Pending => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(step);
                    }
                    MarkdownGlyphClusterAnswer::Known(Some(value))
                        if value.source_utf16.start == target =>
                    {
                        (value.flags, value.source_utf16)
                    }
                    _ => return Err(MarkdownSoftLineMeasureError::InconsistentCoverage),
                }
            } else {
                (0, target..target) // Actual EOF or an original zero-glyph paragraph control needs no successor; an ordinary item still needs real following flags.
            };
            let fit = Fit {
                measured: MarkdownSoftLineMeasuredBreak {
                    source_utf16: self.start..candidate.utf16,
                    x_advance_26_6: payload.advance,
                    boundary: candidate,
                    boundary_flags: flags,
                },
                boundary_cluster,
            };
            if payload.advance > self.width {
                // Stop at the first overflowing opportunity. Without an earlier fit, return an explicit emergency stage rather than a fabricated fitting line.
                payload.selected = Some(match payload.fitting.take() {
                    Some(previous) => selected(previous),
                    None => MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(fit.measured),
                }); // Do not cut a long word by arbitrary character count or claim its overflowing width fits the viewport.
                continue;
            }
            if candidate.kind == MarkdownLineBreakKind::Mandatory {
                // A fitting mandatory opportunity ends selection now; selected() still preserves any unsafe-boundary reshaping requirement.
                payload.selected = Some(selected(fit));
                continue;
            }
            payload.fitting = Some(fit);
            payload.next_query = candidate.utf16;
            payload.candidate = None; // Continue the original logical prefix at the next opportunity without rereading earlier clusters/text.
        }
    }

    /// Observe actual destruction of the same Source payload without keeping this Job, external indexes, or Font alive.
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownLayoutTextSourceObserver {
        self.resources.clone()
    }
    /// End this owner's lifetime and return its Source claim; the caller's independently owned original shaper and indexes remain its responsibility.
    pub fn close(self) {
        drop(self);
    }
}
#[cfg(feature = "paged-safe-glyph-slices")]
impl Drop for MarkdownSoftLineMeasureJob {
    fn drop(&mut self) {
        // Revoke derived permits without clearing sealed spools or immutable glyphs, and without claiming their resources have already been destroyed.
        self.proof_revoked.store(true, Ordering::Release);
        self.line_alive.store(false, Ordering::Release);
    }
}
fn selected(fit: Fit) -> MarkdownSoftLineMeasureChoice {
    // An unsafe following cluster requires original-context reshaping even when width fits; retain the complete measured width and exact boundary evidence.
    if fit.measured.boundary_flags & UNSAFE_TO_BREAK != 0 {
        MarkdownSoftLineMeasureChoice::NeedsReshape {
            boundary_utf16: fit.measured.source_utf16.end,
            source_utf16: fit.measured.source_utf16,
            boundary_cluster_utf16: fit.boundary_cluster,
            boundary_flags: fit.measured.boundary_flags,
            measured_prefix_26_6: fit.measured.x_advance_26_6,
            full_width_26_6: Some(fit.measured.x_advance_26_6),
            reason: MarkdownSoftLineReshapeReason::UnsafeToBreak,
        }
    } else {
        MarkdownSoftLineMeasureChoice::MeasuredBreak(fit.measured)
    }
}

#[cfg(test)]
mod tests;
