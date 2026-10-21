//! Persist one original worker trial. Input ingestion and glyph inspection can advance in quanta;
//! shaping itself remains the original complete synchronous pass.
use omni_markdown_core::{
    MARKDOWN_LAYOUT_SOURCE_BYTES, MarkdownShapingBudget, MarkdownShapingJob,
    MarkdownShapingProfile, MarkdownShapingResourceObserver,
};

use super::*;

/// Inspect at most 256 actual output records per step without changing the complete grapheme or missing-glyph conclusion.
pub const NATIVE_MARKDOWN_FONT_TRIAL_GLYPHS: usize = 256;

/// Account for three work classes independently; zero budget pauses only that phase while still checking original source/cancellation.
#[derive(Clone, Copy, Debug)]
pub struct NativeMarkdownFontTrialBudget {
    pub input: MarkdownShapingBudget,
    pub glyphs: usize,
    /// Permit at most one original synchronous shape call per step, without claiming that a vendor pass fits a time quantum.
    pub shape_calls: usize,
}
impl Default for NativeMarkdownFontTrialBudget {
    /// Reuse Core's finite input budget and this worker's fixed glyph/shape ceilings.
    fn default() -> Self {
        Self {
            input: MarkdownShapingBudget::default(),
            glyphs: NATIVE_MARKDOWN_FONT_TRIAL_GLYPHS,
            shape_calls: 1,
        }
    }
}

/// Only Finished authorizes consuming a complete result; input completion or shape return does not prove every glyph was checked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownFontTrialStatus {
    Ingesting,
    AwaitingShape,
    CheckingGlyphs,
    Finished,
}

/// Core errors return no partial input counters; preserve the admitted finite upper bound instead of reporting unknown work as zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownFontTrialInputProgress {
    NotAttempted,
    Counted {
        bytes: usize,
        scalars: usize,
    },
    Failed {
        byte_limit: usize,
        scalar_limit: usize,
    },
}

/// Advance one phase per step; glyphs counts successfully read original records and shape_calls counts actual original API calls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeMarkdownFontTrialStep {
    pub status: NativeMarkdownFontTrialStatus,
    pub input: NativeMarkdownFontTrialInputProgress,
    pub glyphs: usize,
    pub shape_calls: usize,
}

/// Construct and retain the non-Send Job inside its original worker; only the trial packet and final result cross threads.
pub struct NativeMarkdownFontTrialWorker {
    #[cfg(feature = "native-paged-production")]
    probe: Option<omni_markdown_core::MarkdownLineFontProbe>,
    identity: Arc<()>,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    cancellation: MarkdownShapingCancellation,
    backing: Option<MarkdownOriginalBackingObserver>,
    job: Option<MarkdownShapingJob>,
    resources: Option<MarkdownShapingResourceObserver>,
    status: NativeMarkdownFontTrialStatus,
    checked: usize,
    facts: NativeMarkdownFontGlyphFacts,
    result: Option<NativeMarkdownFontTrialResult>,
}

impl NativeMarkdownFontTrial {
    /// Consume the original packet before constructing a Job on this thread. Main-thread use
    /// or construction failure becomes that same nonce's terminal error, without a fabricated
    /// zero-resource receipt; into_result still returns the original failure.
    #[must_use]
    pub fn into_worker(self) -> NativeMarkdownFontTrialWorker {
        let Self {
            #[cfg(feature = "native-paged-production")]
            probe,
            source,
            source_authority,
            font,
            font_authority,
            range,
            options,
            cancellation,
            identity,
        } = self;
        let mut worker = NativeMarkdownFontTrialWorker {
            #[cfg(feature = "native-paged-production")]
            probe,
            identity,
            source: source_authority,
            font: font_authority,
            cancellation,
            backing: source.original_backing_observer(),
            job: None,
            resources: None,
            status: NativeMarkdownFontTrialStatus::Ingesting,
            checked: 0,
            facts: NativeMarkdownFontGlyphFacts {
                glyph_count: 0,
                missing_glyphs: 0,
                flags_union: 0,
                input_scalars: 0,
                source_bytes_read: 0,
                source_borrow_max_bytes: 0,
            },
            result: None,
        };
        let created = (|| {
            #[cfg(feature = "native-paged-production")]
            if let Some(probe) = &worker.probe {
                probe.validate().map_err(compose::error)?;
                if MainThreadMarker::new().is_some() {
                    return Err(NativeMarkdownFontFallbackError::MainThreadShape);
                }
                // Outer grapheme and current scalar shaping segment share one frozen source
                // ticket. Consume this real segment only; the original Composer advances
                // after this Job/glyph storage actually returns, without rescanning a remote prefix.
                return probe
                    .shaping_job(font, &worker.font, options, worker.cancellation.clone())
                    .map_err(compose::error);
            }
            // The public complete-trial path lacking a Composer ticket still validates both
            // endpoints through its original constructor, without renewing authority from local bounds.
            source.byte_offset_for_utf16(range.start)?;
            if MainThreadMarker::new().is_some() {
                return Err(NativeMarkdownFontFallbackError::MainThreadShape);
            }
            // Actual source failure takes precedence over thread scheduling; long graphemes
            // cannot enter the main thread's synchronous shaping algorithm.
            Ok(MarkdownShapingJob::new_with_cancellation(
                source,
                &worker.source,
                font,
                &worker.font,
                range,
                options,
                worker.cancellation.clone(),
            )?)
        })();
        match created {
            Ok(job) => {
                worker.resources = Some(job.resource_observer());
                worker.job = Some(job);
            }
            Err(error) => worker.finish(Err(error)),
        }
        // Publish an observer only after actual construction. None on failure means no receipt was acquired.
        worker
    }

    /// Drive this same worker to completion synchronously; the caller still executes and joins
    /// a real worker. This convenience entry adds no second missing-glyph/candidate algorithm
    /// and does not promise interruptible shaping of a long grapheme.
    pub fn run(self) -> NativeMarkdownFontTrialResult {
        let mut worker = self.into_worker();
        while worker.status() != NativeMarkdownFontTrialStatus::Finished {
            worker.step(NativeMarkdownFontTrialBudget::default());
        }
        // Default budgets make real progress in every phase; this loop neither polls a background task nor starts a thread.
        worker.into_result()
    }
}

impl NativeMarkdownFontTrialWorker {
    /// Observe the phase without upgrading authority; Finished still needs result consumption and original fallback identity checks.
    #[must_use]
    pub fn status(&self) -> NativeMarkdownFontTrialStatus {
        self.status
    }

    /// The original cancellation flag holds no body, font or Job claim; requesting stop does not prove resource release.
    #[must_use]
    pub fn cancellation(&self) -> MarkdownShapingCancellation {
        self.cancellation.clone()
    }

    /// Lend the original Job's atomic observer; None after construction failure is not successful zero-resource evidence.
    #[must_use]
    pub fn resource_observer(&self) -> Option<MarkdownShapingResourceObserver> {
        self.resources.clone()
    }

    /// Advance the current phase only, clamping glyph/shape work to fixed ceilings without widening source coverage.
    pub fn step(&mut self, budget: NativeMarkdownFontTrialBudget) -> NativeMarkdownFontTrialStep {
        let mut step = NativeMarkdownFontTrialStep {
            status: self.status,
            input: NativeMarkdownFontTrialInputProgress::NotAttempted,
            glyphs: 0,
            shape_calls: 0,
        };
        if self.status == NativeMarkdownFontTrialStatus::Finished {
            self.refresh_terminal_error();
            return step;
        }
        // Terminal steps replay no ingestion or shaping; later shared-source failure still revokes old successful facts.
        match self.advance(budget, &mut step) {
            Ok(true) => self.finish(Ok(self.facts)),
            // Zero budget or a phase transition preserves this Job without certifying incomplete glyph facts.
            Ok(false) => {}
            Err(error) => self.finish(Err(error)),
        }
        step.status = self.status;
        step
    }

    /// Debit one real phase after original Source/Font/probe/cancellation validation, including zero-budget calls.
    fn advance(
        &mut self,
        budget: NativeMarkdownFontTrialBudget,
        step: &mut NativeMarkdownFontTrialStep,
    ) -> Result<bool> {
        #[cfg(feature = "native-paged-production")]
        if let Some(probe) = &self.probe {
            probe.validate().map_err(compose::error)?;
        }
        // Even zero budget validates original source/style scope; a consumed response cannot continue an old font trial.
        let job = self
            .job
            .as_mut()
            .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
        // No cached phase bypasses the actual Source/Font/paging/cancellation health boundary.
        job.profile()?;
        match self.status {
            NativeMarkdownFontTrialStatus::Ingesting => {
                if budget.input.bytes == 0 || budget.input.scalars == 0 {
                    return Ok(false);
                }
                // Core supplies no partial counters on failure. Record the actual admitted
                // upper bounds first; replace them with exact counters only after success.
                step.input = NativeMarkdownFontTrialInputProgress::Failed {
                    byte_limit: budget
                        .input
                        .bytes
                        .min(budget.input.scalars)
                        .min(MARKDOWN_LAYOUT_SOURCE_BYTES),
                    scalar_limit: budget
                        .input
                        .scalars
                        .min(budget.input.bytes)
                        .min(MARKDOWN_LAYOUT_SOURCE_BYTES),
                };
                let actual = job.step(&self.source, &self.font, budget.input)?;
                step.input = NativeMarkdownFontTrialInputProgress::Counted {
                    bytes: actual.bytes,
                    scalars: actual.scalars,
                };
                if actual.status == omni_markdown_core::MarkdownShapingStatus::AwaitingShape {
                    self.status = NativeMarkdownFontTrialStatus::AwaitingShape;
                }
                Ok(false)
            }
            NativeMarkdownFontTrialStatus::AwaitingShape => {
                if budget.shape_calls == 0 {
                    return Ok(false);
                }
                step.shape_calls = 1;
                // This is the original complete synchronous vendor pass, without a bounded-time claim.
                job.shape(&self.source, &self.font)?;
                let profile = job.profile()?;
                self.facts = facts(profile);
                self.status = NativeMarkdownFontTrialStatus::CheckingGlyphs;
                Ok(false)
            }
            NativeMarkdownFontTrialStatus::CheckingGlyphs => {
                let remaining = self.facts.glyph_count - self.checked;
                let count = budget
                    .glyphs
                    .min(NATIVE_MARKDOWN_FONT_TRIAL_GLYPHS)
                    .min(remaining);
                for _ in 0..count {
                    let glyph = job
                        .glyph(&self.source, &self.font, self.checked)?
                        .ok_or(NativeMarkdownFontFallbackError::InvalidCluster)?;
                    #[cfg(all(
                        feature = "native-paged-production",
                        feature = "native-markdown-runtime-probe"
                    ))]
                    if self.facts.glyph_count <= 8
                        && let Some(probe) = &self.probe
                        && probe.source_range().len() > 1
                        && probe.source_range().end <= 8
                        && std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some()
                    {
                        // Diagnose the actual glyph just read by this loop, without rereading
                        // the spool, reshaping or treating a trial candidate as final selection.
                        // The main-thread Choice event separately identifies the registered final font.
                        eprintln!(
                            "native_font_trial_glyph whole_gcb={:?} record_index={} actual_glyph={glyph:?}",
                            probe.source_range(),
                            self.checked,
                        );
                    }
                    // Count only a successfully read original record. Later failure revokes
                    // the entire trial, without publishing its already-inspected prefix.
                    step.glyphs += 1;
                    self.checked += 1;
                    self.facts.missing_glyphs = self
                        .facts
                        .missing_glyphs
                        .checked_add(usize::from(glyph.glyph_id == 0))
                        .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
                    self.facts.flags_union |= glyph.flags;
                }
                if self.checked == self.facts.glyph_count {
                    if job.glyph(&self.source, &self.font, self.checked)?.is_some() {
                        return Err(NativeMarkdownFontFallbackError::InvalidCluster);
                    }
                    // The real EOF getter certifies actual record count, including an empty output.
                    return Ok(true);
                }
                Ok(false)
            }
            NativeMarkdownFontTrialStatus::Finished => {
                Err(NativeMarkdownFontFallbackError::InvalidState)
            }
        }
    }

    /// Return the actual Job before observing terminal resource facts; retain no glyph arrays in the result.
    fn finish(&mut self, outcome: Result<NativeMarkdownFontGlyphFacts>) {
        // Return paged arrays and this Job's Source/Font claims before observing its terminal receipt.
        drop(self.job.take());
        let resources = self
            .resources
            .as_ref()
            .map(MarkdownShapingResourceObserver::snapshot);
        let outcome = if matches!(
            &outcome,
            Err(NativeMarkdownFontFallbackError::Source(_))
                | Err(NativeMarkdownFontFallbackError::Shaping(
                    MarkdownShapingError::Source(_)
                ))
        ) {
            // Original source failure precedes paging/cancellation; the final backing Drop is checked again below.
            outcome
        } else if let Some(error) = resources.and_then(|resources| resources.first_error) {
            Err(NativeMarkdownFontFallbackError::GlyphStoreIo(error))
        } else if resources.is_some_and(|resources| {
            resources.files_open != 0
                || resources.mappings_open != 0
                || resources.mapped_bytes != 0
                || resources.work_bytes != 0
                || resources.cleanup_failed
                || !resources.shaper_access_certified
        }) {
            Err(NativeMarkdownFontFallbackError::GlyphStoresNotReleased)
        } else {
            outcome
        };
        self.result = Some(NativeMarkdownFontTrialResult {
            identity: self.identity.clone(),
            outcome,
            resources,
            backing: self.backing.clone(),
            cancellation: self.cancellation.clone(),
        });
        self.status = NativeMarkdownFontTrialStatus::Finished;
        self.refresh_terminal_error();
    }

    /// Recheck shared original-source/probe health before allowing a terminal success to escape.
    fn refresh_terminal_error(&mut self) {
        #[cfg(feature = "native-paged-production")]
        let probe_error = self.probe.as_ref().and_then(|probe| probe.validate().err());
        let Some(result) = self.result.as_mut() else {
            return;
        };
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|backing| backing.first_errno())
        {
            result.outcome =
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        #[cfg(feature = "native-paged-production")]
        if let Some(cause) = probe_error
            && (matches!(
                &cause,
                omni_markdown_core::MarkdownLineComposeError::Source(_)
            ) || result.outcome.is_ok())
        {
            result.outcome = Err(compose::error(cause));
        }
        // Draft revocation is an original-source failure before later cancellation; an
        // expired weak scope does not replace an existing paging failure.
        if result.outcome.is_ok() && self.cancellation.requested() {
            result.outcome = Err(MarkdownShapingError::Cancelled.into());
        }
        // Check source/original ticket before cancellation; old success cannot bypass actual source health.
    }

    /// Consume a complete terminal result; early consumption requests original cancellation
    /// and actually returns the Job instead of treating its prefix as success.
    pub fn into_result(mut self) -> NativeMarkdownFontTrialResult {
        if self.status != NativeMarkdownFontTrialStatus::Finished {
            self.cancellation.request();
            let outcome = match self.job.as_mut() {
                Some(job) => match job.profile() {
                    Err(error) => Err(error.into()),
                    Ok(_) => Err(MarkdownShapingError::Cancelled.into()),
                },
                None => Err(NativeMarkdownFontFallbackError::InvalidState),
            };
            self.finish(outcome);
        }
        self.refresh_terminal_error();
        self.result.take().expect("终态试次必须保有唯一结果")
    }
}

impl Drop for NativeMarkdownFontTrialWorker {
    /// Early return still requests stop and destroys the actual Job; independent observers retain no business resources.
    fn drop(&mut self) {
        if self.status != NativeMarkdownFontTrialStatus::Finished {
            self.cancellation.request();
        }
        // Dropping an unconsumed result cannot defer returning this worker's actual Job.
        drop(self.job.take());
    }
}

/// Copy only finite profile counters; missing-glyph facts are established by the later actual output traversal.
fn facts(profile: MarkdownShapingProfile) -> NativeMarkdownFontGlyphFacts {
    NativeMarkdownFontGlyphFacts {
        glyph_count: profile.glyph_count,
        missing_glyphs: 0,
        flags_union: 0,
        input_scalars: profile.input_scalars,
        source_bytes_read: profile.source_bytes_read,
        source_borrow_max_bytes: profile.source_borrow_max_bytes,
    }
}
