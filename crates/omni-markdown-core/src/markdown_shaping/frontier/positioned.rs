//! Actual positioning lower bounds for the complete local plan, retaining the original Source,
//! font, options, and cancellation authority.
use super::*;

pub(super) struct PositionedNonnegativeProof {
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    plan: Weak<harfrust::ShapePlan>,
    options: ValidatedOptions,
    cancellation: MarkdownShapingCancellation,
    minimum: i32,
}
impl PositionedNonnegativeProof {
    fn require(
        &self,
        payload: &FrontierPayload,
        authority: &MarkdownLayoutTextAuthority,
    ) -> FrontierResult<()> {
        // A cached bound remains subordinate to its actual Source, font payload, original plan,
        // complete options, and cancellation; equal font names or scale alone are insufficient.
        payload
            .source
            .health()
            .map_err(MarkdownShapingError::from)?;
        if &self.source != authority
            || !payload.source.permits(&self.source)
            || !payload.font.permits(&self.font)
            || !word::same_options(&payload.options, &self.options)
            || payload
                .plan
                .as_ref()
                .is_none_or(|p| !Weak::ptr_eq(&self.plan, &Arc::downgrade(&p.plan())))
            || self.minimum < 0
        {
            return Err(MarkdownShapingError::InvalidRange.into());
        }
        if self.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled.into());
        }
        Ok(())
    }
}
impl MarkdownShapingFrontierJob {
    pub(super) fn prepare_positioned(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        metadata_budget: usize,
        step: &mut MarkdownShapingFrontierStep,
    ) -> FrontierResult<bool> {
        self.guard(source, font)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        if let Some(proof) = &self.positioned_proof {
            proof.require(payload, source)?;
            return Ok(true);
        }
        let remaining = metadata_budget
            .checked_sub(step.plan_records + step.metadata_glyphs)
            .ok_or(MarkdownShapingError::InvalidOptions)?;
        if remaining == 0 {
            return Ok(false);
        }
        let plan = payload
            .plan
            .as_ref()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        if self.positioned_scan.is_none() {
            let certificate = self
                .local_certificate
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            self.positioned_scan =
                Some(certificate.positioned_advance_job(Some(payload.options.points)));
        }
        let scan = self
            .positioned_scan
            .as_mut()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        let records = scan.checked_records();
        let glyphs = scan.queried_glyphs();
        // Certify using execution's same font, scale, and immutable horizontal-advance table;
        // the vendor cannot substitute a reconstructed default hmtx for actual nominal metrics.
        let shaper = payload
            .font
            .shaper_at_scale(payload.options.scale)
            .map_err(|_| MarkdownShapingError::InvalidOptions)?;
        let result = scan.step(&plan.plan(), &shaper, remaining);
        let queried = scan.queried_glyphs() - glyphs;
        step.metadata_glyphs += queried;
        step.plan_records += scan.checked_records() - records - queried;
        self.guard(source, font)?; // Actual shared Source failure precedes unsupported/cancelled/completed metadata results.
        match result {
            Ok(None) => Ok(false),
            Ok(Some(facts)) => {
                // Quantize the actual positioned bound at the original size. A negative result
                // remains unsupported; do not clamp it or certify only a favorable prefix.
                let payload = self
                    .payload
                    .as_ref()
                    .ok_or(MarkdownShapingFrontierError::Poisoned)?;
                let minimum = payload.options.quantize(facts.minimum_raw);
                if minimum < 0 {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::NegativeAdvance,
                    );
                    self.positioned_scan = None;
                    return Ok(false);
                }
                self.positioned_proof = Some(PositionedNonnegativeProof {
                    source: source.clone(),
                    font: font.clone(),
                    plan: Arc::downgrade(
                        &payload
                            .plan
                            .as_ref()
                            .ok_or(MarkdownShapingFrontierError::Poisoned)?
                            .plan(),
                    ),
                    options: payload.options.clone(),
                    cancellation: self.cancellation.clone(),
                    minimum,
                });
                self.positioned_scan = None;
                self.positioned_proof
                    .as_ref()
                    .ok_or(MarkdownShapingFrontierError::Poisoned)?
                    .require(payload, source)?;
                Ok(true)
            }
            Err(reason) => {
                self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                    MarkdownShapingFrontierReason::Plan(reason),
                );
                self.positioned_scan = None;
                Ok(false)
            }
        }
    }
}
