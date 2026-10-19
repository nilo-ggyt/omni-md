//! 原单字体ASCII词的封闭前缀事实；未读尾部既不是新段落，也没有glyphReady权限。
use super::*;
use crate::{MarkdownLayoutBreakAnswer, MarkdownLayoutBreakQuery, MarkdownLayoutBreakUnit};

/// 只能装入实际frontier child；公开原Job构造和任意Range都不会取得此字段。
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
        payload.source.health()?; // 旧Source真实错误不能被原取消或另一个同名字体覆盖。
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
        Ok(self.original.end) // 只报告真实词项末端；不把它扩为本child的可读glyph范围。
    }
}

impl MarkdownShapingJob {
    /// 唯一Store producer仅能从这个实际Ready任务消费事实，不能以公开Range自行签认证字段。
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
        self.glyph(source, font, count)?; // 完整实际EOF是独立门，不用profile长度替代。
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
                return Ok(false); // 当前257B不是未读尾部的字符域证明；全项不满足便保持未知。
            }
            self.guard(source, font)?;
            // 一个Font的非负事实不覆盖同词后继别的Font。原item必须闭在真实UAX机会，
            // 真SourceEOF由已验证长度提供；其他端点只消费原BreakJob的实际查询结果。
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
        } // 原默认advance证明只适用独立计划；Body须另计真实GPOS/variation/trak下界。
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
            self.guard(source, font)?; // 即使字体闭包被拒，期间原Source错误仍优先报告。
            match result {
                Ok(Some(_)) => {}
                Ok(None) => return Ok(false),
                Err(_) => {
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::UncertifiedWord,
                    );
                    return Ok(false); // 不能从nominal cmap非0猜后续GSUB不会删字或产0。
                }
            }
            self.guard(source, font)?;
            self.word_closure_checked = true;
            self.word_closure = None; // 完成后清固定游标；bitset不含正文，也不是映射释放收据。
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
        advance::AdvanceScanError::Shaping(error) => error.into(), // 原Source errno保持原Error层级。
        other => other.into(), // 内部元数据失败保typed cause，不改成可继续的未知或成功。
    }
}
