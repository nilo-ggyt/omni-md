//! Shape font fallback for one actual grapheme, separating native candidate ownership from pure Core worker trials.
//!
//! This module starts no threads. The caller moves a trial packet into a real worker,
//! joins it, then consumes its result on the main thread. Packets capture the same
//! Source/CoreFont; non-Send ShapeJobs are constructed and destroyed inside that worker.
use std::{ops::Range, rc::Rc, sync::Arc};

use objc2_foundation::MainThreadMarker;
use omni_markdown_core::{
    MARKDOWN_SHAPING_MAX_FEATURES, MarkdownDocumentBufferError, MarkdownGraphemeAnswer,
    MarkdownGraphemeError, MarkdownGraphemeJob, MarkdownGraphemeQuery, MarkdownGraphemeUnit,
    MarkdownLayoutFont, MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownOriginalBackingObserver, MarkdownScriptAnswer,
    MarkdownScriptError, MarkdownScriptJob, MarkdownScriptUnit, MarkdownShapingCancellation,
    MarkdownShapingError, MarkdownShapingIoError, MarkdownShapingOptions,
    MarkdownShapingResourceSnapshot,
};
use thiserror::Error;

use super::{
    paged_font_cascade::{
        NativeMarkdownFontCascade, NativeMarkdownFontCascadeBudget, NativeMarkdownFontCascadeError,
    },
    paged_fonts::{
        NativeMarkdownFontCapture, NativeMarkdownPagedFont, NativeMarkdownPagedFontError,
    },
    paged_line_fonts::NativeMarkdownLineFont,
};

// Bound language-tag copying to Core's existing 64-byte budget. Core alone validates
// admitted syntax; oversized tags are neither truncated nor replaced with a default language.
const MAX_TRIAL_LANGUAGE_BYTES: usize = 64;

#[cfg(feature = "native-paged-production")]
mod compose;
mod trial;
#[cfg(feature = "native-paged-production")]
pub use compose::NativeMarkdownProbedFontFallbackRequest;
pub use trial::{
    NATIVE_MARKDOWN_FONT_TRIAL_GLYPHS, NativeMarkdownFontTrialBudget,
    NativeMarkdownFontTrialInputProgress, NativeMarkdownFontTrialStatus,
    NativeMarkdownFontTrialStep, NativeMarkdownFontTrialWorker,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Identify where the actual ordered font trial obtained its candidate, without substituting a font name for identity.
pub enum NativeMarkdownFontOrigin {
    Base,
    /// After a complete Base trial proves missing glyphs, use the system's actual preferred font for this grapheme's first scalar.
    #[cfg(feature = "native-paged-production")]
    ForString,
    Cascade(usize),
}
#[derive(Clone)]
/// Bind one complete grapheme's original shaping context to finite native candidate/capture budgets.
pub struct NativeMarkdownFontFallbackRequest {
    pub source_utf16: Range<usize>,
    /// Preserve real itemization's script/direction/style/BOT/EOT instead of guessing again from this grapheme or a 4-KiB window.
    pub options: MarkdownShapingOptions,
    pub languages: &'static [&'static str],
    pub cascade_budget: NativeMarkdownFontCascadeBudget,
    pub maximum_font_bytes: u64,
}
#[derive(Debug, Error)]
/// Preserve typed source, shaping and resource failures across candidate acquisition, worker completion and retirement.
pub enum NativeMarkdownFontFallbackError {
    #[cfg(feature = "native-paged-production")]
    #[error("原组合器字体试次权限失效：{0}")]
    Compose(#[source] omni_markdown_core::MarkdownLineComposeError),
    #[error("原字体回退权限已撤销")]
    Revoked,
    #[error("字体回退已停止或仍有未完成试次")]
    InvalidState,
    #[error("字体回退源权限不匹配")]
    SourceMismatch,
    #[error("字体回退原字体权限不匹配")]
    FontMismatch,
    #[error("请求不是同脚本样式的一个完整字素")]
    InvalidCluster,
    #[error("字体试次与原签发身份不匹配")]
    TrialMismatch,
    #[error("字体试次被错误地放在主线程执行")]
    MainThreadShape,
    #[error("字体试次字形资源没有实际完整释放")]
    GlyphStoresNotReleased,
    #[error("字体试次字形分页首次I/O失败：{0:?}")]
    GlyphStoreIo(MarkdownShapingIoError),
    #[error("字体试次操作与资源归还同时失败：{cause}；另一失败：{cleanup}")]
    TrialCleanup {
        #[source]
        cause: Box<Self>,
        cleanup: Box<Self>,
    },
    #[error("拒绝字体的字节映射没有实际完整释放")]
    FontBytesNotReleased,
    #[error("字体回退计数溢出")]
    Overflow,
    #[error("冻结字体回退源已经失败")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("真实字素索引失败")]
    Grapheme(#[from] MarkdownGraphemeError),
    #[error("真实脚本索引失败")]
    Script(#[from] MarkdownScriptError),
    #[error("系统字体候选失败")]
    Cascade(#[from] NativeMarkdownFontCascadeError),
    #[error("系统字体候选{ordinal}无法捕获同源字体: {error}")]
    CandidateCapture {
        ordinal: usize,
        #[source]
        error: NativeMarkdownFontCascadeError,
    },
    #[cfg(feature = "native-paged-production")]
    #[error("系统首字符优先候选无法捕获同源字体：{0}")]
    ForStringCapture(#[source] NativeMarkdownPagedFontError),
    #[error("实际字体整形失败")]
    Shaping(#[from] MarkdownShapingError),
}
type Result<T> = std::result::Result<T, NativeMarkdownFontFallbackError>;

impl NativeMarkdownFontFallbackError {
    /// Only this actual trial's requested cancellation permits retiring its exact stale/cancelled
    /// outcomes normally; arbitrary Source failures remain failures.
    fn is_cancelled_retirement(&self, cancelled: bool) -> bool {
        if !cancelled {
            // Without this trial's actual cancellation, failure cannot be guessed to mean the user left.
            return false;
        }
        match self {
            Self::Revoked
            | Self::Shaping(MarkdownShapingError::Cancelled)
            | Self::Source(MarkdownDocumentBufferError::PreparedTextInputStale)
            | Self::Shaping(MarkdownShapingError::Source(
                MarkdownDocumentBufferError::PreparedTextInputStale,
            )) => true,
            #[cfg(feature = "native-paged-production")]
            Self::Compose(
                omni_markdown_core::MarkdownLineComposeError::Cancelled
                | omni_markdown_core::MarkdownLineComposeError::Source(
                    MarkdownDocumentBufferError::PreparedTextInputStale,
                ),
            ) => true,
            // Poisoned state, storage errno and other source errors remain failures without a broad exemption.
            _ => false,
        }
    }

    /// Identify only the leading original-backing errno; the caller must separately prove both observations belong to one Result/Source.
    fn original_backing_errno(&self) -> Option<i32> {
        match self {
            Self::Source(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno))
            | Self::Shaping(MarkdownShapingError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            )) => Some(*errno),
            #[cfg(feature = "native-paged-production")]
            Self::Compose(omni_markdown_core::MarkdownLineComposeError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            )) => Some(*errno),
            Self::TrialCleanup { cause, .. } => cause.original_backing_errno(),
            // Edit storage or another cause cannot masquerade as this original-file observer's repeated first failure.
            _ => None,
        }
    }

    /// Prioritize actual source-storage errno during retirement; expired cell authority is not a disk failure.
    pub(in crate::native_surface) fn is_source_io(&self) -> bool {
        match self {
            Self::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
                | MarkdownDocumentBufferError::EditStorageUnavailable(_),
            )
            | Self::Shaping(MarkdownShapingError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
                | MarkdownDocumentBufferError::EditStorageUnavailable(_),
            )) => true,
            #[cfg(feature = "native-paged-production")]
            Self::Compose(omni_markdown_core::MarkdownLineComposeError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
                | MarkdownDocumentBufferError::EditStorageUnavailable(_),
            )) => true,
            Self::TrialCleanup { cause, cleanup } => cause.is_source_io() || cleanup.is_source_io(),
            // Match typed causes without string inspection or treating every Source variant as ordinary revocation.
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Finite counters from actual shaping/output traversal, without retaining glyph records or source text.
pub struct NativeMarkdownFontGlyphFacts {
    pub glyph_count: usize,
    pub missing_glyphs: usize,
    pub flags_union: u32,
    pub input_scalars: usize,
    pub source_bytes_read: usize,
    pub source_borrow_max_bytes: usize,
}

/// Identify the same immutable Native/Core font without retaining glyph arrays or a strong Source claim.
/// A successor composer still merges/shapes adjacent items; this choice proves neither a whole run nor a soft line.
pub struct NativeMarkdownFontChoice {
    origin: NativeMarkdownFontOrigin,
    source_utf16: Range<usize>,
    facts: NativeMarkdownFontGlyphFacts,
    font: Option<NativeMarkdownPagedFont>,
    base: MarkdownLayoutFontAuthority,
    selected: MarkdownLayoutFontAuthority,
    source: MarkdownLayoutTextAuthority,
}
impl NativeMarkdownFontChoice {
    /// Read the original candidate origin without changing the result's authenticated identity.
    #[must_use]
    pub fn origin(&self) -> NativeMarkdownFontOrigin {
        self.origin
    }
    /// Copy the original global UTF-16 range; changing the copy cannot transfer font evidence to another source.
    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        self.source_utf16.clone()
    }
    /// Return finite actual shaping facts, without a glyph array or cmap-based coverage guess.
    #[must_use]
    pub fn facts(&self) -> NativeMarkdownFontGlyphFacts {
        self.facts
    }
    /// Lend the exact font that passed its trial; a Base choice still requires the caller's original base owner.
    pub fn font<'a>(
        &'a self,
        base: &'a NativeMarkdownPagedFont,
        source: &MarkdownLayoutTextSource,
    ) -> Result<&'a NativeMarkdownPagedFont> {
        // Only an original complete worker result creates Choice, with both endpoints already
        // validated in the same immutable Source. Recheck health and authority without
        // decoding the whole prefix up to a remote UTF-16 start merely to borrow the font.
        source.byte_offset_for_utf16(0)?;
        if source.authority() != self.source {
            return Err(NativeMarkdownFontFallbackError::SourceMismatch);
        }
        if !base.core_font().permits(&self.base) {
            return Err(NativeMarkdownFontFallbackError::FontMismatch);
        }
        let font = self.font.as_ref().unwrap_or(base);
        if !font.core_font().permits(&self.selected) {
            return Err(NativeMarkdownFontFallbackError::FontMismatch);
        }
        Ok(font)
    }

    /// Consume the choice and transfer its actual fallback capture into the session palette;
    /// Base returns None to reuse the original object. This transfers font ownership only,
    /// proving the original single grapheme rather than whole-item or WholeWord coverage.
    ///
    /// # Errors
    /// Reject changed Source, base/selected font identity or caller authority without reconstructing a substitute by name.
    pub fn into_captured_font(
        mut self,
        base: &NativeMarkdownPagedFont,
        source: &MarkdownLayoutTextSource,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Option<NativeMarkdownPagedFont>> {
        // Actual source failure takes precedence; equal-length foreign content cannot acquire this capture.
        self.font(base, source)?;
        let permitted = current();
        // current may enter Native and reenter; validate original source/font identity after it returns.
        self.font(base, source)?;
        if !permitted {
            return Err(NativeMarkdownFontFallbackError::Revoked);
        }
        // Move only the actual original object, without a new native getter, Source or glyph cache.
        Ok(self.font.take())
    }
}

/// A Send packet carries original pure Core values and fixed trial identity, without CTFont, a native closure or ShapeJob.
pub struct NativeMarkdownFontTrial {
    #[cfg(feature = "native-paged-production")]
    probe: Option<omni_markdown_core::MarkdownLineFontProbe>,
    source: MarkdownLayoutTextSource,
    source_authority: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFont,
    font_authority: MarkdownLayoutFontAuthority,
    range: Range<usize>,
    options: MarkdownShapingOptions,
    cancellation: MarkdownShapingCancellation,
    identity: Arc<()>,
}
/// Own one trial's actual outcome and terminal observations, identified by the same original packet nonce.
pub struct NativeMarkdownFontTrialResult {
    identity: Arc<()>,
    outcome: Result<NativeMarkdownFontGlyphFacts>,
    resources: Option<MarkdownShapingResourceSnapshot>,
    // The worker's last probe/Source can drop after result formation. Retain only atomic
    // observation and this trial's cancellation flag, never body text or its owner.
    backing: Option<MarkdownOriginalBackingObserver>,
    cancellation: MarkdownShapingCancellation,
}
impl NativeMarkdownFontTrialResult {
    /// None means construction never acquired a resource receipt, not that zero-resource validation passed.
    #[must_use]
    pub fn resources(&self) -> Option<MarkdownShapingResourceSnapshot> {
        self.resources
    }
    /// Every exit consumes the same real snapshot. Only owner-requested retirement may
    /// ignore its already-requested ordinary cancellation; admission failure preserves
    /// the original typed error, including cancellation, instead of reporting successful retirement.
    pub(in crate::native_surface) fn retire(self, preserve_cancelled: bool) -> Result<()> {
        let cancelled = self.cancellation.requested();
        let mut cause = self
            .outcome
            .err()
            .filter(|error| preserve_cancelled || !error.is_cancelled_retirement(cancelled));
        // Read this actual Job's complete resource facts before returning even a Source
        // failure; this trial's resources are not accounted for by the Composer alone.
        let cleanup = self.resources.and_then(|resources| {
            let unreleased = resources.files_open != 0
                || resources.mappings_open != 0
                || resources.mapped_bytes != 0
                || resources.work_bytes != 0
                || resources.cleanup_failed
                || !resources.shaper_access_certified;
            match resources.first_error {
                Some(error) => Some(NativeMarkdownFontFallbackError::GlyphStoreIo(error)),
                None if unreleased => Some(NativeMarkdownFontFallbackError::GlyphStoresNotReleased),
                // The actual snapshot proves release; an outer None never fabricates an all-zero receipt.
                None => None,
            }
        });
        if let Some(cleanup) = cleanup {
            cause = Some(match cause {
                None => cleanup,
                Some(original)
                    if matches!((&original, &cleanup),
                        (NativeMarkdownFontFallbackError::GlyphStoreIo(first), NativeMarkdownFontFallbackError::GlyphStoreIo(second)) if first == second
                    ) || matches!(
                        (&original, &cleanup),
                        (
                            NativeMarkdownFontFallbackError::GlyphStoresNotReleased,
                            NativeMarkdownFontFallbackError::GlyphStoresNotReleased
                        )
                    ) =>
                {
                    // This same Result's outcome already reflects the identical snapshot failure; report it once.
                    original
                }
                Some(original) => {
                    let (cause, cleanup) = if original.is_source_io() {
                        // Keep actual Source failure first and retain the independent glyph-resource failure.
                        (original, cleanup)
                    } else {
                        // Resource failure precedes other operation failures here; retirement preserves both.
                        (cleanup, original)
                    };
                    NativeMarkdownFontFallbackError::TrialCleanup {
                        cause: Box::new(cause),
                        cleanup: Box::new(cleanup),
                    }
                }
            });
        }
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|backing| backing.first_errno())
        {
            let already_recorded = cause
                .as_ref()
                .is_some_and(|cause| cause.original_backing_errno() == Some(errno));
            if !already_recorded {
                // This observer belongs to the same actual Result. Final Source Drop errno
                // leads the chain while preserving earlier shaper paging/release failures.
                // Deduplicate this atomic first cause only, never foreign results by text or equal integers.
                let source = NativeMarkdownFontFallbackError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
                );
                cause = Some(match cause {
                    Some(cleanup) => NativeMarkdownFontFallbackError::TrialCleanup {
                        cause: Box::new(source),
                        cleanup: Box::new(cleanup),
                    },
                    None => source,
                });
            }
        }
        cause.map_or(Ok(()), Err)
    }

    /// Retire even a result rejected by admission; ordinary revocation or the primary error cannot hide actual resource failure.
    fn retire_after(
        self,
        cause: NativeMarkdownFontFallbackError,
        same_trial: bool,
    ) -> NativeMarkdownFontFallbackError {
        let cancelled = self.cancellation.requested();
        match self.retire(false) {
            Ok(()) => cause,
            Err(cleanup) => {
                if cause.is_cancelled_retirement(cancelled) {
                    // Ordinary revocation is no second failure; report the actual resource failure itself.
                    return cleanup;
                }
                if same_trial {
                    let repeated_source = cause
                        .original_backing_errno()
                        .is_some_and(|errno| cleanup.original_backing_errno() == Some(errno));
                    if repeated_source {
                        // Keep the full retirement chain, including its real shaper failure, when eliminating repeated Source reporting.
                        return cleanup;
                    }
                    #[cfg(feature = "native-paged-production")]
                    if matches!(
                        (&cause, &cleanup),
                        (
                            NativeMarkdownFontFallbackError::Compose(
                                omni_markdown_core::MarkdownLineComposeError::Poisoned
                            ),
                            NativeMarkdownFontFallbackError::Compose(
                                omni_markdown_core::MarkdownLineComposeError::Poisoned
                            )
                        )
                    ) {
                        // Packet and admission for the same nonce observed the same consumed
                        // probe's Poisoned outcome, without another cleanup cause. Remove this
                        // duplicate only; foreign trials and compound failures never use this branch.
                        return cleanup;
                    }
                }
                let (cause, cleanup) = if cleanup.is_source_io() && !cause.is_source_io() {
                    // Final Source Drop errno leads, retaining the original operation in the same error chain.
                    (cleanup, cause)
                } else {
                    // Preserve an existing Source cause or the original ordering of two real operation failures.
                    (cause, cleanup)
                };
                NativeMarkdownFontFallbackError::TrialCleanup {
                    cause: Box::new(cause),
                    cleanup: Box::new(cleanup),
                }
            }
        }
    }

    /// Lend the actual worker outcome for reporting/control; private fields prevent manufacturing a successful coverage receipt.
    pub fn outcome(
        &self,
    ) -> std::result::Result<&NativeMarkdownFontGlyphFacts, &NativeMarkdownFontFallbackError> {
        self.outcome.as_ref()
    }
}
/// Describe the original candidate cursor's actual progress; pending/unavailable states certify no glyph coverage.
pub enum NativeMarkdownFontFallbackStep {
    PendingDependencies,
    /// The system candidate has no file capturable by this Core shaper. It was never
    /// shaped and has no coverage conclusion. The caller explicitly chooses whether to
    /// continue its cursor; success means the first capturable font with no missing glyphs.
    UnavailableCandidate {
        ordinal: usize,
        cause: NativeMarkdownFontCascadeError,
    },
    /// The preferred candidate lacks a local file and proves no grapheme coverage; enter the original global cascade next quantum.
    #[cfg(feature = "native-paged-production")]
    UnavailableForString {
        cause: NativeMarkdownPagedFontError,
    },
    /// Allocate the exclusive packet only for an actual trial; pending/exhausted states do not occupy a full shaping input on the stack.
    Trial(Box<NativeMarkdownFontTrial>),
    Exhausted {
        tried: usize,
    },
}
/// Reject a candidate with an actually observed missing glyph, or transfer the fully checked original font choice.
pub enum NativeMarkdownFontFallbackCompletion {
    Rejected {
        origin: NativeMarkdownFontOrigin,
        facts: NativeMarkdownFontGlyphFacts,
    },
    Selected(NativeMarkdownFontChoice),
}
/// Own exactly one candidate capture and cancellation authority while its real trial is in flight.
struct Active {
    identity: Arc<()>,
    cancellation: MarkdownShapingCancellation,
    font: Option<NativeMarkdownFontCapture>,
    authority: MarkdownLayoutFontAuthority,
    origin: NativeMarkdownFontOrigin,
    #[cfg(feature = "native-paged-production")]
    phase: ProbeTrialPhase,
    #[cfg(feature = "native-paged-production")]
    facts: Option<NativeMarkdownFontGlyphFacts>,
}

/// Keep only one actual segment in flight; the original Composer advances after its successful predecessor returns, without parallel Jobs.
#[cfg(feature = "native-paged-production")]
#[derive(Clone, Copy, Eq, PartialEq)]
enum ProbeTrialPhase {
    Worker,
    NextSegment,
    SegmentReady,
}

/// Consume candidates for one explicit active phase, with one actual trial in flight and no cache of every font.
pub struct NativeMarkdownFontFallback {
    #[cfg(feature = "native-paged-production")]
    probe: Option<omni_markdown_core::MarkdownLineFontProbe>,
    source: Option<MarkdownLayoutTextSource>,
    authority: MarkdownLayoutTextAuthority,
    base: MarkdownLayoutFontAuthority,
    backing: Option<MarkdownOriginalBackingObserver>,
    request: NativeMarkdownFontFallbackRequest,
    cascade: Option<NativeMarkdownFontCascade>,
    active: Option<Active>,
    tried: usize,
    // Work actually spent reading this owner's first scalar, not yet returned to Producer.
    // Failure/unavailable exits retain the same accounting obligation.
    #[cfg(feature = "native-paged-production")]
    probe_source_bytes: usize,
    stopped: bool,
    mtm: MainThreadMarker,
}
impl NativeMarkdownFontFallback {
    /// Capture original Source/Font identity without constructing cascade candidates while grapheme/script dependencies remain pending.
    pub fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        base: &NativeMarkdownPagedFont,
        base_authority: &MarkdownLayoutFontAuthority,
        request: NativeMarkdownFontFallbackRequest,
        mtm: MainThreadMarker,
    ) -> Result<Self> {
        source.byte_offset_for_utf16(request.source_utf16.start)?;
        if source.authority() != *authority {
            return Err(NativeMarkdownFontFallbackError::SourceMismatch);
        }
        if !base.core_font().permits(base_authority) {
            return Err(NativeMarkdownFontFallbackError::FontMismatch);
        }
        if request
            .options
            .language
            .as_ref()
            .is_some_and(|language| language.len() > MAX_TRIAL_LANGUAGE_BYTES)
        {
            // Reject oversized tags before issue clones options or creates a trial, without duplicating Core's grammar.
            return Err(MarkdownShapingError::InvalidOptions.into());
        }
        if request.source_utf16.start >= request.source_utf16.end
            || request.source_utf16.end > source.utf16_len()
            || !base.accepts_shaping_size(request.options.points)
            || request.options.features.len() > MARKDOWN_SHAPING_MAX_FEATURES
        {
            // Validate point size against the same Font payload's double binding and unique
            // f32 representation; a lossy round-trip must not reject valid sizes such as 14.3.
            return Err(NativeMarkdownFontFallbackError::InvalidCluster);
        }
        // Require an actual scalar boundary, excluding a low-surrogate interior endpoint.
        source.byte_offset_for_utf16(request.source_utf16.end)?;
        let backing = source.original_backing_observer();
        Ok(Self {
            #[cfg(feature = "native-paged-production")]
            probe: None,
            source: Some(source),
            authority: authority.clone(),
            base: base_authority.clone(),
            backing,
            request,
            cascade: None,
            active: None,
            tried: 0,
            #[cfg(feature = "native-paged-production")]
            probe_source_bytes: 0,
            stopped: false,
            mtm,
        })
    }
    /// Recheck original source health, font identities and style scope before and after potentially reentrant caller authority.
    fn guard(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<()> {
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let source = self
            .source
            .as_ref()
            .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
        // Construction proved both endpoints in this immutable Source. Check health only,
        // without reborrowing remote leaves to decode the same position again.
        source.byte_offset_for_utf16(0)?;
        if source.authority() != self.authority {
            return Err(NativeMarkdownFontFallbackError::SourceMismatch);
        }
        #[cfg(feature = "native-paged-production")]
        if let Some(probe) = &self.probe {
            probe.validate().map_err(compose::error)?;
        }
        // Original source health comes first; a consumed style scope cannot continue a
        // font trial merely because the underlying source remains alive.
        if !base.core_font().permits(&self.base) || !base.core_font().permits(authority) {
            return Err(NativeMarkdownFontFallbackError::FontMismatch);
        }
        let valid = !self.stopped && current();
        // current may reenter and consume the style ticket. Recheck Source/scope afterward,
        // rather than continuing solely on the previously returned Boolean.
        source.byte_offset_for_utf16(0)?;
        #[cfg(feature = "native-paged-production")]
        if let Some(probe) = &self.probe {
            probe.validate().map_err(compose::error)?;
        }
        if !valid {
            return Err(NativeMarkdownFontFallbackError::Revoked);
        }
        Ok(())
    }
    /// Require one complete grapheme in the original script run, or consume the Composer's already-certified same-source ticket.
    fn cluster(
        &self,
        dependencies: Option<(&mut MarkdownGraphemeJob, &mut MarkdownScriptJob)>,
    ) -> Result<bool> {
        #[cfg(feature = "native-paged-production")]
        if let Some(probe) = &self.probe {
            probe.validate().map_err(compose::error)?;
            // Private construction bound the Composer's real single-grapheme ticket;
            // do not rebuild or rescan grapheme/script dependencies.
            return Ok(true);
        }
        let (graphemes, scripts) =
            dependencies.ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
        let range = &self.request.source_utf16;
        match graphemes.query(
            &self.authority,
            MarkdownGraphemeUnit::Utf16,
            range.start,
            MarkdownGraphemeQuery::IsBoundary,
        )? {
            MarkdownGraphemeAnswer::Pending => return Ok(false),
            MarkdownGraphemeAnswer::Known(None) => {
                return Err(NativeMarkdownFontFallbackError::InvalidCluster);
            }
            MarkdownGraphemeAnswer::Known(Some(_)) => {}
        }
        match graphemes.query(
            &self.authority,
            MarkdownGraphemeUnit::Utf16,
            range.start,
            MarkdownGraphemeQuery::Next,
        )? {
            MarkdownGraphemeAnswer::Pending => return Ok(false),
            MarkdownGraphemeAnswer::Known(Some(end)) if end.utf16 == range.end => {}
            _ => return Err(NativeMarkdownFontFallbackError::InvalidCluster),
        }
        // Two valid boundaries are insufficient: the end must be the strictly next
        // boundary, preventing several graphemes from becoming one coverage trial.
        match scripts.run_at(&self.authority, MarkdownScriptUnit::Utf16, range.start)? {
            MarkdownScriptAnswer::Pending => Ok(false),
            MarkdownScriptAnswer::Known(Some(run))
                if run.start_utf16 <= range.start
                    && run.end_utf16 >= range.end
                    && run.script.iso15924_tag() == self.request.options.script =>
            {
                Ok(true)
            }
            _ => Err(NativeMarkdownFontFallbackError::InvalidCluster),
        }
        // The complete grapheme must belong to its original script run; the first
        // scalar's cmap or script alone cannot establish that fact.
    }
    /// Revoke this candidate owner and return its native/source claims; the caller separately owns worker join.
    fn stop(&mut self) {
        self.stopped = true;
        if let Some(active) = self.active.take() {
            active.cancellation.request();
            drop(active);
        }
        drop(self.cascade.take());
        #[cfg(feature = "native-paged-production")]
        drop(self.probe.take());
        // A worker can still hold pure Core clones. Returning this claim proves no
        // worker completion; the caller must join and inspect its actual receipt.
        drop(self.source.take());
    }

    /// Prepare one candidate on the main thread; actual shaping consumes its returned
    /// worker packet instead of hiding a long synchronous algorithm in this entry.
    pub fn next_trial(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        graphemes: &mut MarkdownGraphemeJob,
        scripts: &mut MarkdownScriptJob,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackStep> {
        let result = self.issue(base, authority, Some((graphemes, scripts)), &[], current);
        if result.is_err() {
            self.stop();
            if let Some(errno) = self
                .backing
                .as_ref()
                .and_then(|observer| observer.first_errno())
            {
                return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
            }
            // A real failure observed during final Source release precedes superficial
            // revocation; this error exit must not hide the cleanup's source failure.
        }
        result
    }
    /// Advance only this original ordered candidate cursor, preserving one in-flight trial and its actual capture.
    fn issue(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        dependencies: Option<(&mut MarkdownGraphemeJob, &mut MarkdownScriptJob)>,
        palette: &[Rc<NativeMarkdownLineFont>],
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackStep> {
        self.guard(base, authority, current)?;
        if self.active.is_some() {
            #[cfg(feature = "native-paged-production")]
            if self.probe.is_some() {
                return self.continue_trial(base, authority, current);
            }
            return Err(NativeMarkdownFontFallbackError::InvalidState);
        }
        if !self.cluster(dependencies)? {
            return Ok(NativeMarkdownFontFallbackStep::PendingDependencies);
        }
        self.guard(base, authority, current)?;
        #[cfg(feature = "native-paged-production")]
        if self.tried == 1 && self.probe.is_some() {
            // tried advances only after a real trial or explicit unavailable capture.
            // The first-scalar system preference follows an actual Base coverage failure;
            // it proves no whole grapheme, which start_trial still sends to the same worker.
            let font = match self.for_string_font(base, authority, palette, current) {
                Ok(font) => font,
                Err(NativeMarkdownFontFallbackError::ForStringCapture(
                    cause @ NativeMarkdownPagedFontError::FileSourceUnavailable,
                )) => {
                    self.tried = self
                        .tried
                        .checked_add(1)
                        .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
                    self.guard(base, authority, current)?;
                    return Ok(NativeMarkdownFontFallbackStep::UnavailableForString { cause });
                }
                Err(cause) => return Err(cause),
            };
            return self.start_trial(
                base,
                authority,
                NativeMarkdownFontOrigin::ForString,
                Some(font),
                current,
            );
        }
        let (origin, font) = if self.tried == 0 {
            (NativeMarkdownFontOrigin::Base, None)
        } else {
            if self.cascade.is_none() {
                let valid = || self.guard(base, authority, current).is_ok();
                let result = NativeMarkdownFontCascade::capture(
                    base,
                    authority,
                    self.request.languages,
                    self.request.cascade_budget,
                    self.mtm,
                    &valid,
                );
                self.guard(base, authority, current)?;
                self.cascade = Some(result?);
            }
            // Move the cascade out before Native calls, ending self.cascade's mutable
            // borrow. Each primitive's callback retains full original Source authority,
            // rather than substituting an observer-only health approximation.
            let mut cascade = self
                .cascade
                .take()
                .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
            let valid = || self.guard(base, authority, current).is_ok();
            let candidate = cascade.next(base, authority, &valid);
            self.guard(base, authority, current)?;
            let Some(candidate) = candidate? else {
                let tried = self.tried;
                self.stop();
                return Ok(NativeMarkdownFontFallbackStep::Exhausted { tried });
            };
            let font = candidate.capture_reusing(
                base,
                authority,
                palette,
                self.request.maximum_font_bytes,
                &valid,
            );
            self.guard(base, authority, current)?;
            let font = match font {
                Ok(font) => font,
                Err(
                    cause @ NativeMarkdownFontCascadeError::Font(
                        NativeMarkdownPagedFontError::FileSourceUnavailable,
                    ),
                ) => {
                    let ordinal = candidate.ordinal();
                    self.cascade = Some(cascade);
                    self.tried = self
                        .tried
                        .checked_add(1)
                        .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
                    self.guard(base, authority, current)?;
                    // No Job or coverage receipt was created. Do not replay, rebuild
                    // the cascade or assign this candidate's identity to a different font.
                    return Ok(NativeMarkdownFontFallbackStep::UnavailableCandidate {
                        ordinal,
                        cause,
                    });
                }
                Err(error) => {
                    // Other source, parsing and I/O failures revoke immediately;
                    // they cannot be reclassified as an unavailable candidate.
                    return Err(NativeMarkdownFontFallbackError::CandidateCapture {
                        ordinal: candidate.ordinal(),
                        error,
                    });
                }
            };
            // Keep the original issuer and ordered system list instead of acquiring a replacement cursor.
            self.cascade = Some(cascade);
            (
                NativeMarkdownFontOrigin::Cascade(candidate.ordinal()),
                Some(font),
            )
        };
        self.start_trial(base, authority, origin, font, current)
    }

    /// Place one actual candidate in the sole in-flight slot. Origin changes ordering only,
    /// bypassing neither complete-grapheme shaping nor actual resource receipts.
    fn start_trial(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        origin: NativeMarkdownFontOrigin,
        font: Option<NativeMarkdownFontCapture>,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackStep> {
        let selected = font
            .as_ref()
            .map(NativeMarkdownFontCapture::font)
            .unwrap_or(base);
        let font_authority = selected.core_font().authority();
        let identity = Arc::new(());
        let cancellation = MarkdownShapingCancellation::default();
        let trial = NativeMarkdownFontTrial {
            #[cfg(feature = "native-paged-production")]
            probe: self.probe.clone(),
            source: self
                .source
                .as_ref()
                .ok_or(NativeMarkdownFontFallbackError::InvalidState)?
                .clone(),
            source_authority: self.authority.clone(),
            font: selected.core_font().clone(),
            font_authority: font_authority.clone(),
            range: self.request.source_utf16.clone(),
            options: self.request.options.clone(),
            cancellation: cancellation.clone(),
            identity: identity.clone(),
        };
        self.active = Some(Active {
            identity,
            cancellation,
            font,
            authority: font_authority,
            origin,
            #[cfg(feature = "native-paged-production")]
            phase: ProbeTrialPhase::Worker,
            #[cfg(feature = "native-paged-production")]
            facts: None,
        });
        self.tried = self
            .tried
            .checked_add(1)
            .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
        self.guard(base, authority, current)?;
        Ok(NativeMarkdownFontFallbackStep::Trial(Box::new(trial)))
    }

    /// Expose only the active Cascade capture's existing fixed resource observer for native probes.
    /// Base/no capture returns None; the observer retains no Font, Source, descriptor, or mapping.
    #[cfg(feature = "native-paged-font-fallback-probe")]
    #[must_use]
    pub fn active_font_bytes_observer(
        &self,
    ) -> Option<omni_markdown_core::MarkdownLayoutFontBytesResourceObserver> {
        self.active
            .as_ref()
            .and_then(|active| active.font.as_ref())
            .map(|capture| capture.font().bytes_observer())
    }

    /// Call only after the actual worker completes and joins; its result authorizes this original grapheme's font choice only.
    pub fn complete_trial(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        graphemes: &mut MarkdownGraphemeJob,
        scripts: &mut MarkdownScriptJob,
        result: NativeMarkdownFontTrialResult,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackCompletion> {
        let completion = self
            .complete(base, authority, Some((graphemes, scripts)), result, current)
            .and_then(|completion| completion.ok_or(NativeMarkdownFontFallbackError::InvalidState));
        completion.map_err(|cause| self.stop_preserving_error(cause))
    }
    /// End original native fields after failure while preserving an existing complete real Source error chain.
    fn stop_preserving_error(
        &mut self,
        cause: NativeMarkdownFontFallbackError,
    ) -> NativeMarkdownFontFallbackError {
        self.stop();
        if cause.is_source_io() {
            // Do not replace a TrialCleanup chain's real shaper cause with a single outer Source error.
            return cause;
        }
        match self
            .backing
            .as_ref()
            .and_then(|backing| backing.first_errno())
        {
            // Prioritize only a newly observed Source cause after the original stop,
            // retaining the complete earlier operation failure as the second cause.
            Some(errno) => NativeMarkdownFontFallbackError::TrialCleanup {
                cause: Box::new(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
                ),
                cleanup: Box::new(cause),
            },
            None => cause,
        }
    }

    /// Consume the original result and independently verify any fresh capture's release on failure.
    fn complete(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        dependencies: Option<(&mut MarkdownGraphemeJob, &mut MarkdownScriptJob)>,
        result: NativeMarkdownFontTrialResult,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Option<NativeMarkdownFontFallbackCompletion>> {
        // Capture trial identity before the first Native/current call; only the same
        // nonce proves that later Source errors derive from this one observation.
        let same_trial = self
            .active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.identity, &result.identity));
        // A fresh capture's unique new mapping must end with its failed trial. A reused
        // font sheds this claim only; its actual final release remains the original Body's obligation.
        let bytes = if same_trial {
            self.active
                .as_ref()
                .and_then(|active| active.font.as_ref())
                .and_then(NativeMarkdownFontCapture::fresh_bytes_observer)
        } else {
            None // A foreign result cannot prove that this active packet's legitimate font owners have ended.
        };
        let completion =
            self.complete_trial_inner(base, authority, dependencies, result, current, same_trial);
        let Err(cause) = completion else {
            return completion; // Selected transfers this capture to Choice; its legitimate owner must remain live.
        };
        let already_font_failure = matches!(
            &cause,
            NativeMarkdownFontFallbackError::FontBytesNotReleased
        );
        let cause = self.stop_preserving_error(cause); // Drop the original active/cascade/probe before reading its observer.
        if let Some(bytes) = bytes
            && (!bytes.mapping_drop_completed()
                || !bytes.unmap_succeeded()
                || bytes.mapped_bytes() != 0
                || !bytes.source_descriptor_closed()
                || !bytes.backing_descriptor_closed()
                || bytes.failed())
            && !already_font_failure
        {
            // Keep the original Source/operation chain first and attach the actual font cleanup failure.
            // Rejected may already report this capture's FontBytesNotReleased; do not duplicate that failure.
            return Err(NativeMarkdownFontFallbackError::TrialCleanup {
                cause: Box::new(cause),
                cleanup: Box::new(NativeMarkdownFontFallbackError::FontBytesNotReleased),
            });
        }
        Err(cause)
    }

    /// Admit the original nonce, consume glyph facts, and transfer ownership; errors return to the real stop boundary.
    fn complete_trial_inner(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        dependencies: Option<(&mut MarkdownGraphemeJob, &mut MarkdownScriptJob)>,
        result: NativeMarkdownFontTrialResult,
        current: &(impl Fn() -> bool + ?Sized),
        same_trial: bool,
    ) -> Result<Option<NativeMarkdownFontFallbackCompletion>> {
        let admission = (|| {
            self.guard(base, authority, current)?;
            if !self.cluster(dependencies)? {
                return Err(NativeMarkdownFontFallbackError::InvalidCluster);
            }
            let active = self
                .active
                .as_ref()
                .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
            if !Arc::ptr_eq(&active.identity, &result.identity) {
                return Err(NativeMarkdownFontFallbackError::TrialMismatch);
            }
            Ok(())
        })();
        if let Err(cause) = admission {
            // Set this trial's actual cancellation before retirement can classify its
            // exact old-cell/style revocation as an ordinary cancelled retirement.
            self.stop();
            return Err(result.retire_after(cause, same_trial));
        }
        if result.outcome.is_err() {
            // Consume the same result's complete snapshot, preserving Source and actual
            // glyph-release failures through original retirement. preserve_cancelled=true
            // prevents an already-observed failure from becoming successful voluntary cancellation.
            return Err(result
                .retire(true)
                .expect_err("保留原错误的结果退休不能成功"));
        }
        let facts = result.outcome?;
        #[cfg(feature = "native-paged-production")]
        let mut facts = facts;
        let resources = result
            .resources
            .ok_or(NativeMarkdownFontFallbackError::GlyphStoresNotReleased)?;
        if resources.files_open != 0
            || resources.mappings_open != 0
            || resources.mapped_bytes != 0
            || resources.work_bytes != 0
            || resources.cleanup_failed
            || resources.first_error.is_some()
            || !resources.shaper_access_certified
        {
            return Err(NativeMarkdownFontFallbackError::GlyphStoresNotReleased);
        }
        // Worker return, successful shape and guessed zeros cannot substitute for actual array/descriptor release.
        self.guard(base, authority, current)?;
        let active = self
            .active
            .take()
            .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
        #[cfg(feature = "native-paged-production")]
        let mut active = active;
        #[cfg(feature = "native-paged-production")]
        if self.probe.is_some() {
            if active.phase != ProbeTrialPhase::Worker {
                return Err(NativeMarkdownFontFallbackError::InvalidState);
            }
            if let Some(previous) = active.facts {
                // Accumulate only segments already actually inspected and released;
                // the first segment's facts alone cannot certify the entire outer grapheme.
                for (value, addition) in [
                    (&mut facts.glyph_count, previous.glyph_count),
                    (&mut facts.missing_glyphs, previous.missing_glyphs),
                    (&mut facts.input_scalars, previous.input_scalars),
                    (&mut facts.source_bytes_read, previous.source_bytes_read),
                ] {
                    *value = value
                        .checked_add(addition)
                        .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
                }
                facts.flags_union |= previous.flags_union;
                facts.source_borrow_max_bytes = facts
                    .source_borrow_max_bytes
                    .max(previous.source_borrow_max_bytes);
            }
        }
        if facts.missing_glyphs > 0 {
            // Actual missing glyphs advance the original ordered cascade. Even a reused
            // font needs a new shape; another grapheme's successful coverage cannot transfer here.
            let bytes = active
                .font
                .as_ref()
                .and_then(NativeMarkdownFontCapture::fresh_bytes_observer);
            let origin = active.origin;
            drop(active);
            if let Some(bytes) = bytes
                && (!bytes.unmap_succeeded()
                    || !bytes.mapping_drop_completed()
                    || bytes.mapped_bytes() != 0
                    || !bytes.source_descriptor_closed()
                    || !bytes.backing_descriptor_closed()
                    || bytes.failed())
            {
                return Err(NativeMarkdownFontFallbackError::FontBytesNotReleased);
            }
            self.guard(base, authority, current)?;
            return Ok(Some(NativeMarkdownFontFallbackCompletion::Rejected {
                origin,
                facts,
            }));
        }
        #[cfg(feature = "native-paged-production")]
        if let Some(probe) = &self.probe
            && !probe.complete_single_segment().map_err(compose::error)?
        {
            active.facts = Some(facts);
            active.phase = ProbeTrialPhase::NextSegment;
            self.active = Some(active);
            // The same candidate still has real segments in this outer grapheme;
            // authorize neither Choice nor a different font yet.
            return Ok(None);
        }
        self.select_choice(base, active, facts, current).map(Some)
    }

    /// Transfer the original Font capture only after every actual segment has no missing glyph and its Job has returned.
    fn select_choice(
        &mut self,
        base: &NativeMarkdownPagedFont,
        active: Active,
        facts: NativeMarkdownFontGlyphFacts,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackCompletion> {
        let choice = NativeMarkdownFontChoice {
            origin: active.origin,
            source_utf16: self.request.source_utf16.clone(),
            facts,
            font: active.font.map(NativeMarkdownFontCapture::into_font),
            base: self.base.clone(),
            selected: active.authority,
            source: self.authority.clone(),
        };
        let source = self
            .source
            .take()
            .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
        #[cfg(feature = "native-paged-production")]
        let probe = self.probe.take();
        // Keep the original probe/source through final Native/current revalidation,
        // while returning the cascade first. Choice receives the selected font;
        // no business cache remains hidden in this retired candidate owner.
        self.stop();
        source.byte_offset_for_utf16(0)?;
        let valid = current() && base.core_font().permits(&self.base);
        // Both original endpoints were certified; check health without decoding the remote grapheme prefix again.
        source.byte_offset_for_utf16(0)?;
        #[cfg(feature = "native-paged-production")]
        if let Some(probe) = &probe {
            probe.validate().map_err(compose::error)?;
        }
        if !valid {
            return Err(NativeMarkdownFontFallbackError::Revoked);
        }
        drop(source);
        #[cfg(feature = "native-paged-production")]
        drop(probe);
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        // A successful coverage result cannot hide actual I/O failure during final Source Drop.
        Ok(NativeMarkdownFontFallbackCompletion::Selected(choice))
    }
    /// Request original-trial cancellation and return native fields; the caller still joins and checks real worker resource receipts.
    pub fn close(mut self) {
        self.stop();
    }
}
impl Drop for NativeMarkdownFontFallback {
    /// Early return retires this same owner rather than leaving a candidate, cascade or strong Source claim resident.
    fn drop(&mut self) {
        self.stop();
    }
}
