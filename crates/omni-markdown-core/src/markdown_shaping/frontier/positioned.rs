//! 完整局部计划的真实定位下界；原Source/Font/选项与取消票保持同一个owner。
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
            self.positioned_scan = Some(
                certificate
                    .positioned_advance_job(payload.options.scale, Some(payload.options.points)),
            );
        }
        let scan = self
            .positioned_scan
            .as_mut()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        let records = scan.checked_records();
        let glyphs = scan.queried_glyphs();
        let result = scan.step(&plan.plan(), payload.font.font(), remaining);
        let queried = scan.queried_glyphs() - glyphs;
        step.metadata_glyphs += queried;
        step.plan_records += scan.checked_records() - records - queried;
        self.guard(source, font)?; // 同共享Source真正首错优先于不适用、取消及元数据完成。
        match result {
            Ok(None) => Ok(false),
            Ok(Some(facts)) => {
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
