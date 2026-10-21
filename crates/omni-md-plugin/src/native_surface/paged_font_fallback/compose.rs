//! Bind the original NeedStyle's sealed grapheme ticket to the same font cascade,
//! without constructing or accepting another grapheme/script index.

use objc2_core_foundation::{CFRange, CFString};
use omni_markdown_core::{
    MarkdownLineComposeBudget, MarkdownLineComposeError, MarkdownLineComposeStyle,
    MarkdownLineComposer, MarkdownLineFontProbe, MarkdownLineFontProbeSegmentAnswer,
    MarkdownLineFontProbeSegmentStep,
};

use super::*;

/// Native supplies original style and font budgets only; Core's original ticket provides direction, script, source range and BOT/EOT.
pub struct NativeMarkdownProbedFontFallbackRequest {
    pub style: MarkdownLineComposeStyle,
    pub languages: &'static [&'static str],
    pub cascade_budget: NativeMarkdownFontCascadeBudget,
    pub maximum_font_bytes: u64,
}

/// Preserve original Source/Shaping categories while adapting only actual Composer-specific failures.
pub(super) fn error(cause: MarkdownLineComposeError) -> NativeMarkdownFontFallbackError {
    match cause {
        MarkdownLineComposeError::Source(cause) => NativeMarkdownFontFallbackError::Source(cause),
        // The original probe delegates to the same Shaping Job. Preserve cancellation
        // and storage types instead of disguising resource-retirement failure as ordinary Compose failure.
        MarkdownLineComposeError::Shaping(cause) => NativeMarkdownFontFallbackError::Shaping(cause),
        other => NativeMarkdownFontFallbackError::Compose(other),
    }
}

impl NativeMarkdownFontFallback {
    /// Consume one actual grapheme issued by the current original request, without allocating
    /// another text dependency graph or reading a complete paragraph during construction.
    ///
    /// Coverage still authorizes this grapheme only; the first one's success proves no following text or complete run.
    ///
    /// # Errors
    /// Reject invalid original scope, Source or style; every native font still uses original capture/round-trip verification.
    pub fn new_with_probe(
        probe: MarkdownLineFontProbe,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        request: NativeMarkdownProbedFontFallbackRequest,
        mtm: MainThreadMarker,
    ) -> Result<Self> {
        let options = probe.shaping_options(&request.style).map_err(error)?;
        let source = probe.source().map_err(error)?.clone();
        if !base.core_font().permits(authority) {
            return Err(NativeMarkdownFontFallbackError::FontMismatch);
        }
        if !base.accepts_shaping_size(options.points) {
            return Err(NativeMarkdownFontFallbackError::InvalidCluster);
        }
        // The original probe already certified this complete grapheme's UTF-16 and byte
        // endpoints. Do not repeat remote coordinate conversion through ordinary new;
        // shaping_options owns style validation and the same owner/guard owns backing/cancellation.
        let owner = Self {
            authority: source.authority(),
            backing: source.original_backing_observer(),
            source: Some(source),
            base: authority.clone(),
            request: NativeMarkdownFontFallbackRequest {
                source_utf16: probe.source_range(),
                options,
                languages: request.languages,
                cascade_budget: request.cascade_budget,
                maximum_font_bytes: request.maximum_font_bytes,
            },
            cascade: None,
            active: None,
            tried: 0,
            probe_source_bytes: 0,
            stopped: false,
            mtm,
            probe: Some(probe),
        };
        owner
            .probe
            .as_ref()
            .expect("原票已移交此 owner")
            .validate()
            .map_err(error)?;
        Ok(owner)
    }

    /// Read the first scalar only after actual Base coverage failure. It selects a candidate;
    /// the same worker still shapes the original complete grapheme to prove coverage.
    pub(super) fn for_string_font(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        palette: &[Rc<NativeMarkdownLineFont>],
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontCapture> {
        self.guard(base, authority, current)?;
        let (read, scalar) = self
            .probe
            .as_ref()
            .expect("首 scalar 候选只来自原协调器真实 probe")
            .first_scalar();
        self.probe_source_bytes = self
            .probe_source_bytes
            .checked_add(read)
            .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
        // Producer still consumes actual read accounting if the original ticket was revoked after the read.
        let scalar = scalar.map_err(error)?;
        self.guard(base, authority, current)?;
        let mut bytes = [0_u8; 4];
        let string = CFString::from_str(scalar.encode_utf8(&mut bytes));
        self.guard(base, authority, current)?;
        // SAFETY: string contains one complete original scalar, at most two UTF-16
        // units. The original base and ticket stay valid across this synchronous call.
        let native = unsafe {
            base.native_font()
                .for_string(&string, CFRange::new(0, scalar.len_utf16() as _))
        };
        self.guard(base, authority, current)?;
        drop(string);
        self.guard(base, authority, current)?;
        let valid = || self.guard(base, authority, current).is_ok();
        let captured = NativeMarkdownPagedFont::capture_reusing(
            &native,
            palette,
            self.request.maximum_font_bytes,
            self.mtm,
            &valid,
        );
        // Actual original Source errno takes precedence over capture failure or callback revocation.
        self.guard(base, authority, current)?;
        drop(native);
        self.guard(base, authority, current)?;
        captured.map_err(NativeMarkdownFontFallbackError::ForStringCapture)
    }

    /// Take only this original owner's actual bounded reads; callers account for them before consuming success, unavailable capture or failure.
    pub(in crate::native_surface) fn take_probe_source_bytes(&mut self) -> usize {
        std::mem::take(&mut self.probe_source_bytes)
    }

    /// Advance one native candidate under the same original ticket. Palette reuse requires exact candidate identity;
    /// the original worker still performs the pure Core trial.
    pub fn next_trial_probed(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        palette: &[Rc<NativeMarkdownLineFont>],
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackStep> {
        let result = self.issue(base, authority, None, palette, current);
        self.finish_probed(result)
    }

    /// Consume only an actually released segment result; None means this candidate still has another segment, never Selected.
    pub fn complete_trial_probed(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        result: NativeMarkdownFontTrialResult,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Option<NativeMarkdownFontFallbackCompletion>> {
        let result = self.complete(base, authority, None, result, current);
        self.finish_probed(result)
    }

    /// Restore the original Composer to the same outer-grapheme start only after the
    /// missing-glyph candidate's Job/capture have returned. The next font retries earlier
    /// successful segments too, rather than resuming at the failure and skipping full coverage.
    pub(in crate::native_surface) fn restart_probe(
        &mut self,
        composer: &mut MarkdownLineComposer,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<()> {
        let result = (|| {
            self.guard(base, authority, current)?;
            if self.active.is_some() {
                return Err(NativeMarkdownFontFallbackError::InvalidState);
            }
            composer
                .restart_font_probe(self.probe.as_mut().expect("原 probed owner 持同一 probe"))
                .map_err(error)?;
            self.guard(base, authority, current)
        })();
        self.finish_probed(result)
    }

    /// Advance one original Composer metadata quantum and return its actual Step even
    /// when later authority fails. Intermediate Ready authorizes the same candidate's next
    /// Job; only Complete permits consuming all segments' actual coverage facts.
    pub(in crate::native_surface) fn step_probe_segment(
        &mut self,
        composer: &mut MarkdownLineComposer,
        style: &MarkdownLineComposeStyle,
        budget: MarkdownLineComposeBudget,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> (
        Option<MarkdownLineFontProbeSegmentStep>,
        Result<Option<NativeMarkdownFontFallbackCompletion>>,
    ) {
        let mut step = None;
        let bytes = self.active.as_ref().and_then(|active| {
            active
                .font
                .as_ref()
                .and_then(NativeMarkdownFontCapture::fresh_bytes_observer)
        });
        let result = (|| {
            self.guard(base, authority, current)?;
            if self
                .active
                .as_ref()
                .is_some_and(|active| active.phase == ProbeTrialPhase::Worker)
            {
                return Err(NativeMarkdownFontFallbackError::InvalidState);
            }
            let probe = self.probe.as_mut().expect("原 probed owner 持同一 probe");
            let advance = self
                .active
                .as_ref()
                .is_some_and(|active| active.phase == ProbeTrialPhase::NextSegment)
                || !probe.shaping_segment_ready().map_err(error)?;
            if !advance {
                // The initial ordinary or already-delivered next segment keeps its original
                // fast path, without rebuilding options or probing another segment.
                return Ok(None);
            }
            let actual = composer
                .step_font_probe_segment(probe, budget)
                .map_err(error)?;
            let answer = actual.answer;
            // Later Native/current validation may fail; actual work already performed still belongs in caller accounting.
            step = Some(actual);
            self.guard(base, authority, current)?;
            match answer {
                MarkdownLineFontProbeSegmentAnswer::Pending => Ok(None),
                MarkdownLineFontProbeSegmentAnswer::Ready => {
                    self.request.options = self
                        .probe
                        .as_ref()
                        .expect("同一 probe")
                        .shaping_options(style)
                        .map_err(error)?;
                    if let Some(active) = &mut self.active {
                        active.phase = ProbeTrialPhase::SegmentReady;
                    }
                    Ok(None)
                }
                MarkdownLineFontProbeSegmentAnswer::Complete => {
                    let active = self
                        .active
                        .take()
                        .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
                    if active.phase != ProbeTrialPhase::NextSegment {
                        return Err(NativeMarkdownFontFallbackError::InvalidState);
                    }
                    let facts = active
                        .facts
                        .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
                    self.select_choice(base, active, facts, current).map(Some)
                }
            }
        })();
        // Final Choice transfers capture ownership. Check actual capture release only
        // on failure, without misclassifying intermediate success as retirement.
        let result = self.finish_probed(result);
        let result = match (result, bytes) {
            (Err(cause), Some(bytes))
                if !bytes.mapping_drop_completed()
                    || !bytes.unmap_succeeded()
                    || bytes.mapped_bytes() != 0
                    || !bytes.source_descriptor_closed()
                    || !bytes.backing_descriptor_closed()
                    || bytes.failed() =>
            {
                Err(NativeMarkdownFontFallbackError::TrialCleanup {
                    cause: Box::new(cause),
                    cleanup: Box::new(NativeMarkdownFontFallbackError::FontBytesNotReleased),
                })
            }
            (result, _) => result,
        };
        (step, result)
    }

    /// Read original Composer facts only; Pending constructs no packet and switches no candidate.
    pub(in crate::native_surface) fn probe_segment_ready(&self) -> Result<bool> {
        self.probe
            .as_ref()
            .expect("原 probed owner 持同一 probe")
            .shaping_segment_ready()
            .map_err(error)
    }

    /// The same candidate's previous segment actually returned. Assign the next segment a
    /// unique packet identity without recapturing its font or increasing candidate count.
    pub(super) fn continue_trial(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackStep> {
        self.guard(base, authority, current)?;
        if !self.probe_segment_ready()? {
            return Err(NativeMarkdownFontFallbackError::InvalidState);
        }
        let active = self.active.as_mut().expect("续段持唯一原候选");
        if active.phase != ProbeTrialPhase::SegmentReady {
            return Err(NativeMarkdownFontFallbackError::InvalidState);
        }
        let selected = active
            .font
            .as_ref()
            .map(NativeMarkdownFontCapture::font)
            .unwrap_or(base);
        let identity = Arc::new(());
        let trial = NativeMarkdownFontTrial {
            probe: self.probe.clone(),
            source: self.source.as_ref().expect("原候选仍持 Source").clone(),
            source_authority: self.authority.clone(),
            font: selected.core_font().clone(),
            font_authority: active.authority.clone(),
            range: self.request.source_utf16.clone(),
            options: self.request.options.clone(),
            cancellation: active.cancellation.clone(),
            identity: identity.clone(),
        };
        active.identity = identity;
        active.phase = ProbeTrialPhase::Worker;
        self.guard(base, authority, current)?;
        Ok(NativeMarkdownFontFallbackStep::Trial(Box::new(trial)))
    }

    /// Retire this original candidate owner on failure while preserving its complete typed cause chain.
    fn finish_probed<T>(&mut self, result: Result<T>) -> Result<T> {
        // Ending trial/cascade/Source ownership cannot flatten an existing combined Source/shaper error.
        result.map_err(|cause| self.stop_preserving_error(cause))
    }
}
