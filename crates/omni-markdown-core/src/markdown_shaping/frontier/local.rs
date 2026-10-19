//! 实际支持区和已验证中心视图。外部公开Range不能构造这个证明。
use super::*;

/// view只记录原Job/计划/协调器弱身份和固定范围，不复制glyph或持另一份Source。
pub(in crate::markdown_shaping) struct CertifiedCenterView {
    owner: Weak<()>,
    job: Weak<()>,
    plan: Weak<harfrust::ShapePlan>,
    pub(in crate::markdown_shaping) source: Range<usize>,
    pub(in crate::markdown_shaping) glyphs: Range<usize>,
    support: Range<usize>,
}
impl CertifiedCenterView {
    /// 仅封闭frontier在真实GCB、计划与逐glyph核验完成后调用；不存在公开Range签票入口。
    pub(super) fn new(
        owner: &Arc<()>,
        job: &MarkdownShapingJob,
        source: Range<usize>,
        glyphs: Range<usize>,
        support: Range<usize>,
    ) -> Result<Self> {
        let payload = job.payload.as_ref().ok_or(MarkdownShapingError::Poisoned)?;
        let plan = payload
            .frontier_plan
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        if support != job.source_utf16
            || source.start < support.start
            || source.end > support.end
            || source.start >= source.end
            || glyphs.start > glyphs.end
            || glyphs.end > payload.buffer.len()
        {
            return Err(MarkdownShapingError::InvalidRange);
        }
        Ok(Self {
            owner: Arc::downgrade(owner),
            job: Arc::downgrade(&job.identity),
            plan: Arc::downgrade(plan),
            source,
            glyphs,
            support,
        })
    }
    pub(in crate::markdown_shaping) fn require(&self, job: &MarkdownShapingJob) -> Result<()> {
        let payload = job.payload.as_ref().ok_or(MarkdownShapingError::Poisoned)?;
        if self.owner.strong_count() == 0
            || !Weak::ptr_eq(&self.job, &Arc::downgrade(&job.identity))
            || payload
                .frontier_plan
                .as_ref()
                .is_none_or(|p| !Weak::ptr_eq(&self.plan, &Arc::downgrade(p)))
            || self.support != job.source_utf16
            || self.source.start < self.support.start
            || self.source.end > self.support.end
            || self.glyphs.end > payload.buffer.len()
        {
            return Err(MarkdownShapingError::InvalidRange);
        }
        Ok(()) // Source/Font/原取消和真实分页错误由原Job guard先后复核，不另设权限源。
    }
}

impl MarkdownShapingFrontierJob {
    pub(super) fn local_plan(&self) -> bool {
        self.payload
            .as_ref()
            .and_then(|p| p.plan.as_ref())
            .is_some_and(|p| p.requires_local_certificate())
    }
    pub(super) fn prepare_local(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        budget: MarkdownShapingBudget,
        metadata_budget: usize,
        step: &mut MarkdownShapingFrontierStep,
    ) -> FrontierResult<bool> {
        self.guard(source, font)?;
        if !self.local_source_checked {
            if step.source_bytes != 0
                || step.input_bytes != 0
                || step.plan_records != 0
                || step.metadata_glyphs != 0
                || step.boundary_records != 0
            {
                return Ok(false); // 范围证明独占原量子；本回合已有扫描便只保原候选，下一轮继续。
            }
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingFrontierError::Poisoned)?;
            if self.local_source_cursor.is_none() {
                self.local_source_cursor = Some(
                    payload
                        .source
                        .begin_printable_ascii_suffix(self.original.clone())
                        .map_err(MarkdownShapingError::from)?,
                );
            }
            let progress = payload
                .source
                .advance_printable_ascii_suffix(
                    self.local_source_cursor.as_mut().expect("原完整范围游标"),
                    budget.bytes,
                    budget.scalars,
                    metadata_budget,
                )
                .map_err(MarkdownShapingError::from)?;
            step.source_bytes = progress.bytes;
            step.scalars = progress.scalars;
            step.plan_records = progress.metadata;
            self.guard(source, font)?; // 即使聚合拒绝，原lease归还的真实errno仍先于字符域能力结果。
            match progress.answer {
                None => return Ok(false),
                Some(false) => {
                    self.local_source_cursor.take();
                    self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                        MarkdownShapingFrontierReason::NonAscii,
                    );
                    return Ok(false);
                }
                Some(true) => {
                    self.local_source_checked = true;
                    self.local_source_bounds = Some((progress.minimum, progress.maximum));
                    self.local_source_cursor.take(); // 真正结束的路径Arc立即归还；仅保同owner的两个域标量。
                    return Ok(false); // 元数据/正文已花原量子，Font闭包在下一轮开始，不另发第二份预算。
                }
            }
        }
        if self.local_capability.is_some() {
            return Ok(true);
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        let plan = payload
            .plan
            .as_ref()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        if self.local_scan.is_none() {
            self.local_scan = Some(
                plan.local_job(
                    &payload.font,
                    self.local_source_bounds.expect("完整原范围的ASCII域"),
                )
                .map_err(|_| MarkdownShapingFrontierError::Poisoned)?,
            );
        }
        let scan = self
            .local_scan
            .as_mut()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        let before = scan.checked_records();
        let answer = scan.step(&plan.plan(), payload.font.font(), metadata_budget);
        step.plan_records = scan.checked_records() - before;
        self.guard(source, font)?; // 即使字体元数据拒绝，真实Source首errno仍比不适用优先。
        match answer {
            Ok(Some(capability)) => {
                self.local_capability = Some(capability);
                self.local_certificate = Some(
                    self.local_scan
                        .take()
                        .ok_or(MarkdownShapingFrontierError::Poisoned)?
                        .into_certificate()
                        .map_err(|_| MarkdownShapingFrontierError::Poisoned)?,
                );
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(reason) => {
                self.answer = MarkdownShapingFrontierAnswer::NeedsFrontier(
                    MarkdownShapingFrontierReason::Plan(reason),
                );
                self.local_scan = None;
                Ok(false)
            }
        }
    }
    pub(super) fn support_range(&self, end: usize) -> FrontierResult<Range<usize>> {
        let Some(capability) = self.local_capability else {
            return Ok(self.next..end);
        };
        let left = self
            .next
            .saturating_sub(capability.support_scalars)
            .max(self.original.start);
        let right = end
            .saturating_add(capability.support_scalars)
            .min(self.original.end);
        Ok(left..right) // 字符域已完整证明ASCII，故UTF16/标量/字节偏移一一对应；不跨真实item。
    }
    pub(super) fn seal_center(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        end: usize,
        metadata_budget: usize,
        step: &mut MarkdownShapingFrontierStep,
    ) -> FrontierResult<bool> {
        if !self.local_plan() {
            return Ok(true);
        }
        let child = self
            .child
            .as_mut()
            .ok_or(MarkdownShapingFrontierError::Poisoned)?;
        let payload = child
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        if payload.frontier_view.is_some() {
            return Ok(true);
        }
        if payload.buffer.len() != child.source_utf16.len() {
            return Err(MarkdownShapingError::InvalidRange.into());
        }
        let remaining = metadata_budget
            .checked_sub(step.plan_records + step.metadata_glyphs)
            .ok_or(MarkdownShapingError::InvalidOptions)?;
        for _ in 0..remaining {
            if self.local_verified == payload.buffer.len() {
                break;
            }
            let (info, _) = payload
                .buffer
                .paged_glyph_record(self.local_verified)
                .ok_or(MarkdownShapingError::ShaperStorage)?;
            if info.glyph_id == 0
                || info.cluster as usize != child.source_utf16.start + self.local_verified
            {
                return Err(MarkdownShapingError::InvalidRange.into());
            }
            self.local_verified += 1;
            step.metadata_glyphs += 1;
        } // 支持区真实整形也逐条核1→1与全局cluster；不能从表证书直接伪造实际输出。
        child.guard(source, font)?;
        let payload = child
            .payload
            .as_mut()
            .ok_or(MarkdownShapingError::Poisoned)?;
        if self.local_verified != payload.buffer.len() {
            return Ok(false);
        }
        let support = child.source_utf16.clone();
        let glyphs = (self.next - support.start)..(end - support.start);
        payload.frontier_view = Some(CertifiedCenterView {
            owner: Arc::downgrade(&self.identity),
            job: Arc::downgrade(&child.identity),
            plan: Arc::downgrade(
                payload
                    .frontier_plan
                    .as_ref()
                    .ok_or(MarkdownShapingError::Poisoned)?,
            ),
            source: self.next..end,
            glyphs,
            support,
        });
        child.guard(source, font)?;
        self.guard(source, font)?;
        Ok(true)
    }
}
