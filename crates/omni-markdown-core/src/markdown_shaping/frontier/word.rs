//! Confined prefix facts for the original single-font ASCII word. Its unread tail is neither a new
//! paragraph nor glyph-Ready output.
use super::*;
use crate::{MarkdownLayoutBreakAnswer, MarkdownLayoutBreakQuery, MarkdownLayoutBreakUnit};

/// Attach only to an actual frontier child; ordinary public job construction and arbitrary ranges
/// do not acquire this proof.
pub(in crate::markdown_shaping) struct WordPrefixProof {
    owner: Weak<()>,
    job: Weak<()>,
    plan: Weak<harfrust::ShapePlan>,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    original: Range<usize>,
    prefix: Range<usize>,
    options: ValidatedOptions,
    cancellation: MarkdownShapingCancellation,
}

pub(super) fn same_options(left: &ValidatedOptions, right: &ValidatedOptions) -> bool {
    left.direction == right.direction
        && left.script == right.script
        && left.language == right.language
        && left.points.to_bits() == right.points.to_bits()
        && left.scale == right.scale
        && left.output_divisor == right.output_divisor
        && left.beginning_of_text == right.beginning_of_text
        && left.end_of_text == right.end_of_text
        && left.features == right.features
}

impl WordPrefixProof {
    pub(super) fn new(
        owner: &Arc<()>,
        job: &MarkdownShapingJob,
        original: Range<usize>,
        cancellation: &MarkdownShapingCancellation,
    ) -> Result<Self> {
        let payload = job.payload.as_ref().ok_or(MarkdownShapingError::Poisoned)?;
        let plan = payload
            .frontier_plan
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        Ok(Self {
            owner: Arc::downgrade(owner),
            job: Arc::downgrade(&job.identity),
            plan: Arc::downgrade(plan),
            source: job.source_authority.clone(),
            font: job.font_authority.clone(),
            original,
            prefix: job.source_range(),
            options: job.options.clone(),
            cancellation: cancellation.clone(),
        })
    }

    fn require(&self, job: &MarkdownShapingJob) -> Result<usize> {
        let payload = job.payload.as_ref().ok_or(MarkdownShapingError::Poisoned)?;
        payload.source.health()?; // Original Source failure precedes cancellation or another same-named font.
        if !payload.source.permits(&self.source)
            || !payload.font.permits(&self.font)
            || !Weak::ptr_eq(&self.job, &Arc::downgrade(&job.identity))
            || job.source_range() != self.prefix
            || !same_options(&job.options, &self.options)
            || payload
                .frontier_plan
                .as_ref()
                .is_none_or(|plan| !Weak::ptr_eq(&self.plan, &Arc::downgrade(plan)))
            || self.owner.strong_count() == 0
            || self.prefix.end > self.original.end
        {
            return Err(MarkdownShapingError::InvalidRange);
        }
        if self.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled);
        }
        Ok(self.original.end) // Report the actual word end without enlarging this child's readable glyph range.
    }
}

impl MarkdownShapingJob {
    /// The sole Store producer consumes facts only from this actual Ready job; a public Range
    /// cannot sign a certified field independently.
    pub(crate) fn frontier_word_end(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Option<usize>> {
        self.guard(source, font)?;
        if self.status() != MarkdownShapingStatus::Ready {
            return Err(MarkdownShapingError::Pending);
        }
        let count = self.profile()?.glyph_count;
        self.glyph(source, font, count)?; // Actual EOF is a separate authority gate, not merely a profile length.
        let result = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?
            .frontier_word
            .as_ref()
            .map(|proof| proof.require(self))
            .transpose();
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.poison(error),
        }
    }
}

impl MarkdownShapingFrontierJob {
    pub(super) fn prepare_word(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
        metadata_budget: usize,
        step: &mut MarkdownShapingFrontierStep,
    ) -> FrontierResult<bool> {
        self.guard(source, font)?;
        if !self.word_source_checked {
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            if !payload
                .source
                .is_printable_ascii_range(self.original.clone())
                .map_err(MarkdownShapingError::from)?
            {
                self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                    MarkdownShapingFrontierReason::UncertifiedWord,
                );
                return Ok(false); // The current 257 bytes cannot prove the unread tail's character domain.
            }
            self.guard(source, font)?;
            // This font's nonnegative bound cannot cover a later font in the same word. Require an
            // actual UAX opportunity at item end: verified source length proves EOF; other endpoints
            // need the original BreakJob's actual answer.
            if self.original.end != payload.source.utf16_len() {
                if self.word_end_cursor.is_none() {
                    self.word_end_cursor = Some(breaks.cursor(
                        MarkdownLayoutBreakUnit::Utf16,
                        self.original.end,
                        MarkdownLayoutBreakQuery::At,
                    )?);
                }
                match breaks.query(
                    self.word_end_cursor
                        .as_mut()
                        .ok_or(MarkdownShapingFrontierError::Poisoned)?,
                    1,
                )? {
                    MarkdownLayoutBreakAnswer::PendingSource
                    | MarkdownLayoutBreakAnswer::Yielded => return Ok(false),
                    MarkdownLayoutBreakAnswer::Known(Some(boundary))
                        if !boundary.needs_dictionary => {}
                    MarkdownLayoutBreakAnswer::Known(_) => {
                        self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                            MarkdownShapingFrontierReason::UncertifiedWord,
                        );
                        return Ok(false);
                    }
                }
                self.word_end_cursor = None;
            }
            self.word_source_checked = true;
        }
        if self.local_plan() {
            return self.prepare_positioned(source, font, metadata_budget, step);
        } // Default advances certify only an independent plan; local plans need actual GPOS/variation/trak bounds.
        if !self.word_closure_checked {
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let plan = payload
                .plan
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let actual_plan = plan.plan();
            if self.word_closure.is_none() {
                self.word_closure = Some(
                    actual_plan
                        .ascii_nonmissing_closure_job(payload.font.font())
                        .map_err(|_| MarkdownShapingFrontierError::Poisoned)?,
                );
            }
            let closure = self
                .word_closure
                .as_mut()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let before = closure.checked_records();
            let result = closure.step(&actual_plan, payload.font.font(), metadata_budget);
            step.plan_records = closure.checked_records() - before;
            self.guard(source, font)?; // Source failure during closure work still precedes a font rejection.
            match result {
                Ok(Some(_)) => {}
                Ok(None) => return Ok(false),
                Err(_) => {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::UncertifiedWord,
                    );
                    return Ok(false); // Nonzero nominal cmap alone cannot rule out GSUB deletion or missing output.
                }
            }
            self.guard(source, font)?;
            self.word_closure_checked = true;
            self.word_closure = None; // Return the fixed cursor; its text-free bitset is no mapping-release receipt.
        }
        if self.nonnegative.is_none() {
            let remaining = metadata_budget - step.plan_records;
            if remaining == 0 {
                return Ok(false);
            }
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            let plan = payload
                .plan
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            if self.advance_scan.is_none() {
                self.advance_scan = Some(
                    advance::DefaultAdvanceScan::new(
                        advance::AdvanceScanOrigin {
                            source: &payload.source,
                            source_authority: source,
                            font: &payload.font,
                            font_authority: font,
                        },
                        &payload.options,
                        plan,
                        self.cancellation.clone(),
                    )
                    .map_err(advance_error)?,
                );
            }
            let progress = self
                .advance_scan
                .as_mut()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?
                .step(&payload.source, source, remaining)
                .map_err(advance_error)?;
            step.metadata_glyphs = progress.queried;
            match progress.answer {
                advance::AdvanceScanAnswer::Pending => return Ok(false),
                advance::AdvanceScanAnswer::Negative { .. } => {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::NegativeAdvance,
                    );
                    drop(self.advance_scan.take());
                    return Ok(false);
                }
                advance::AdvanceScanAnswer::Complete => {
                    self.nonnegative = Some(
                        self.advance_scan
                            .take()
                            .ok_or(MarkdownShapingFrontierError::Poisoned)?
                            .finish(&payload.source, source)
                            .map_err(advance_error)?,
                    );
                }
            }
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        let proof = self
            .nonnegative
            .as_ref()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        if !proof
            .matches(
                advance::AdvanceScanOrigin {
                    source: &payload.source,
                    source_authority: source,
                    font: &payload.font,
                    font_authority: font,
                },
                &payload.options,
                payload
                    .plan
                    .as_ref()
                    .ok_or(MarkdownShapingFrontierError::Poisoned)?,
            )
            .map_err(advance_error)?
            || proof.minimum_26_6() < 0
        {
            return Err(MarkdownShapingFrontierError::Poisoned);
        }
        self.guard(source, font)?;
        Ok(true)
    }
}

fn advance_error(error: advance::AdvanceScanError) -> MarkdownShapingFrontierError {
    match error {
        advance::AdvanceScanError::Shaping(error) => error.into(), // Preserve the original Source errno/error layer.
        other => other.into(), // Metadata failures retain typed causes rather than resumable unknown/success.
    }
}
