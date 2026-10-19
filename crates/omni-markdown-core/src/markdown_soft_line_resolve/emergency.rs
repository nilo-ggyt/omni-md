//! 同一原Ready数组的紧急续行；共享原selector，不按剩余全文反复创建Measure或shaper。
use super::*;
use crate::{MarkdownSafeGlyphSlice, MarkdownSafeGlyphSliceError};

/// 只报告原数组切片与真实源范围；没有伪造的tail flags并集或Native布局Ready。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownEmergencyGlyphSliceBoundary {
    pub source_utf16: std::ops::Range<usize>,
    pub remainder_utf16: std::ops::Range<usize>,
    pub kind: MarkdownSoftLineResolvedKind,
    pub x_advance_26_6: i64,
    pub glyphs: usize,
}
/// 只描述已实际整形的前缀。remainder是尚待消费的源坐标，没有tail字形数或flags承诺。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownEmergencyShapedPrefixBoundary {
    pub source_utf16: std::ops::Range<usize>,
    pub remainder_utf16: std::ops::Range<usize>,
    pub kind: MarkdownSoftLineResolvedKind,
    pub x_advance_26_6: i64,
    pub glyphs: usize,
    pub prefix_flags_union: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownEmergencyGlyphSliceAnswer {
    Yielded,
    PendingDependencies,
    Pending(MarkdownSoftLineResolvePending),
    ReusableLine(MarkdownEmergencyGlyphSliceBoundary),
    Reshaped(MarkdownSoftLineResolvedBoundary),
    ShapedPrefix(MarkdownEmergencyShapedPrefixBoundary),
    PendingPrefixConsumer,
    PrefixComplete {
        source_utf16_end: usize,
        source_eof: bool,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownEmergencyGlyphSliceStep {
    pub input_bytes: usize,
    pub glyphs: usize,
    pub candidates: usize,
    pub shapes: usize,
    pub answer: MarkdownEmergencyGlyphSliceAnswer,
}
/// 原始item只整形一次；后续真实新输入和shape单独计数，不能把元数据查询冒充零工作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownEmergencyGlyphSliceWork {
    pub original_input_scalars: usize,
    pub original_glyphs: usize,
    pub new_shape_input_bytes: usize,
    pub shape_calls: usize,
    pub candidates: usize,
    pub lines: usize,
}
impl MarkdownEmergencyGlyphSliceWork {
    fn add(&mut self, step: &MarkdownSoftLineResolveStep) -> Result<()> {
        self.new_shape_input_bytes = self
            .new_shape_input_bytes
            .checked_add(step.input_bytes)
            .ok_or(MarkdownSoftLineResolveError::Overflow)?;
        self.shape_calls = self
            .shape_calls
            .checked_add(step.shapes)
            .ok_or(MarkdownSoftLineResolveError::Overflow)?;
        self.candidates = self
            .candidates
            .checked_add(step.candidates)
            .ok_or(MarkdownSoftLineResolveError::Overflow)?;
        Ok(())
    }
}
/// 非Clone；消费后才得到原安全切片。公开Boundary副本不能用作后继起点权限。
pub struct MarkdownEmergencyGlyphSliceLine {
    boundary: MarkdownEmergencyGlyphSliceBoundary,
    slice: MarkdownSafeGlyphSlice,
}
impl MarkdownEmergencyGlyphSliceLine {
    #[must_use]
    pub fn boundary(&self) -> MarkdownEmergencyGlyphSliceBoundary {
        self.boundary.clone()
    }
    #[must_use]
    pub fn into_parts(self) -> (MarkdownEmergencyGlyphSliceBoundary, MarkdownSafeGlyphSlice) {
        (self.boundary, self.slice)
    }
}

/// worker局部单item续行；只保一个待交付切片和原selector有限状态，不新建glyph索引。
pub struct MarkdownEmergencyGlyphSliceJob {
    inner: MarkdownSoftLineResolveJob,
    line: Option<MarkdownEmergencyGlyphSliceLine>,
    complete: Option<(usize, bool)>,
    prefix_outstanding: bool,
    work: MarkdownEmergencyGlyphSliceWork,
}
impl MarkdownEmergencyGlyphSliceJob {
    /// 原Measure必须实际选中NeedsEmergencyBreak；自然断点走已有安全工厂，不造任意Range。
    pub fn new(
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<Self> {
        let mut inner = MarkdownSoftLineResolveJob::new(measure, original)?;
        let payload = inner
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        let emergency = matches!(
            payload.seed.choice,
            MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(_)
        );
        #[cfg(feature = "paged-shaping-frontier")]
        let emergency = emergency
            || matches!(
                payload.seed.choice,
                MarkdownSoftLineMeasureChoice::NeedsFrontierEmergencyPrefix { .. }
            );
        if !emergency {
            return Err(MarkdownSoftLineResolveError::InvalidRequest);
        }
        payload.reuse_safe = true;
        let profile = original.profile()?;
        inner.guard(measure, original)?;
        Ok(Self {
            inner,
            line: None,
            complete: None,
            prefix_outstanding: false,
            work: MarkdownEmergencyGlyphSliceWork {
                original_input_scalars: profile.input_scalars,
                original_glyphs: profile.glyph_count,
                new_shape_input_bytes: 0,
                shape_calls: 0,
                candidates: 0,
                lines: 0,
            },
        })
    }
    /// 显式只交付真实行前缀；原new仍保留完整tail对照合同，不暗改旧调用方。
    pub fn new_prefix_only(
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<Self> {
        let mut job = Self::new(measure, original)?;
        job.inner
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?
            .defer_tail = true;
        Ok(job)
    }
    #[must_use]
    pub fn cancellation(&self) -> MarkdownSoftLineResolveCancellation {
        self.inner.cancellation()
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownSoftLineResolveResourceObserver {
        self.inner.resource_observer()
    }
    pub fn work(
        &self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<MarkdownEmergencyGlyphSliceWork> {
        self.inner.guard(measure, original)?;
        Ok(self.work)
    }
    fn fail<T>(&mut self, error: MarkdownSoftLineResolveError) -> Result<T> {
        drop(self.line.take());
        drop(self.inner.payload.take());
        if let Some(errno) = self
            .inner
            .backing
            .as_ref()
            .and_then(|value| value.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        Err(error)
    }
    pub fn step(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownEmergencyGlyphSliceStep> {
        let result = self.advance(measure, original, clusters, breaks, budget);
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.fail(error),
        }
    }
    fn advance(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownEmergencyGlyphSliceStep> {
        self.inner.guard(measure, original)?;
        if budget.input_bytes == 0
            || budget.input_scalars == 0
            || budget.glyphs == 0
            || budget.candidates == 0
        {
            return Err(MarkdownSoftLineResolveError::InvalidRequest);
        }
        if self.prefix_outstanding {
            if self.inner.resources.snapshot().active_jobs != 0 {
                return Ok(empty_step(
                    MarkdownEmergencyGlyphSliceAnswer::PendingPrefixConsumer,
                ));
            }
            self.prefix_outstanding = false;
        } // 只有实际child析构后才开始下一行；不保N个Job，也不把交接当资源已归还。
        if let Some(line) = self.line.as_mut() {
            line.slice.require(original)?;
            return Ok(empty_step(MarkdownEmergencyGlyphSliceAnswer::ReusableLine(
                line.boundary.clone(),
            )));
        } // 重复观察同一行不重复签票或累计计数；须take_line后才能推进内部起点。
        if let Some((end, eof)) = self.complete {
            return Ok(empty_step(
                MarkdownEmergencyGlyphSliceAnswer::PrefixComplete {
                    source_utf16_end: end,
                    source_eof: eof,
                },
            ));
        }
        let step = self
            .inner
            .step(measure, original, clusters, breaks, budget)?;
        self.work.add(&step)?;
        let payload = self
            .inner
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        let answer = if matches!(payload.stage, Stage::ShapedPrefix) {
            MarkdownEmergencyGlyphSliceAnswer::ShapedPrefix(prefix_boundary(payload)?)
        } else if matches!(payload.stage, Stage::Reusable) {
            let choice = MarkdownEmergencyResolvedChoice::new(payload)?;
            match MarkdownSafeGlyphSlice::from_emergency_selection(
                &choice, measure, original, breaks, clusters,
            ) {
                Err(MarkdownSafeGlyphSliceError::PendingDependencies) => {
                    MarkdownEmergencyGlyphSliceAnswer::PendingDependencies
                }
                Err(error) => return Err(error.into()),
                Ok(slice) => {
                    let facts = slice.facts();
                    let boundary = MarkdownEmergencyGlyphSliceBoundary {
                        source_utf16: facts.source_utf16,
                        remainder_utf16: payload.selected_end..payload.seed.item.end,
                        kind: payload.kind,
                        x_advance_26_6: facts.x_advance_26_6,
                        glyphs: facts.visual_glyphs.len(),
                    };
                    self.line = Some(MarkdownEmergencyGlyphSliceLine {
                        boundary: boundary.clone(),
                        slice,
                    });
                    MarkdownEmergencyGlyphSliceAnswer::ReusableLine(boundary)
                }
            }
        } else {
            match step.answer {
                MarkdownSoftLineResolveAnswer::Yielded => {
                    MarkdownEmergencyGlyphSliceAnswer::Yielded
                }
                MarkdownSoftLineResolveAnswer::PendingDependencies => {
                    MarkdownEmergencyGlyphSliceAnswer::PendingDependencies
                }
                MarkdownSoftLineResolveAnswer::Pending(reason) => {
                    MarkdownEmergencyGlyphSliceAnswer::Pending(reason)
                }
                MarkdownSoftLineResolveAnswer::Resolved(boundary) => {
                    MarkdownEmergencyGlyphSliceAnswer::Reshaped(boundary)
                }
                MarkdownSoftLineResolveAnswer::EmptySource => {
                    return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
                }
            } // unsafe/cluster内部沿原真实prefix+tail路径，不能包装成安全复用或声称线性工作。
        };
        self.inner.guard(measure, original)?;
        Ok(MarkdownEmergencyGlyphSliceStep {
            input_bytes: step.input_bytes,
            glyphs: step.glyphs,
            candidates: step.candidates,
            shapes: step.shapes,
            answer,
        })
    }
    /// FIFO消费当前已核GCB行。下一起点仅在此内部更新，不接受公开Range或外部Start票。
    pub fn take_line(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<MarkdownEmergencyGlyphSliceLine> {
        let result = (|| {
            self.inner.guard(measure, original)?;
            let line = self
                .line
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::InvalidRequest)?;
            line.slice.require(original)?;
            let payload = self
                .inner
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
            let end = line.boundary.source_utf16.end;
            if !matches!(payload.stage, Stage::Reusable)
                || end != payload.selected_end
                || self.inner.resources.snapshot().active_jobs != 0
            {
                return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
            }
            self.work.lines = self
                .work
                .lines
                .checked_add(1)
                .ok_or(MarkdownSoftLineResolveError::Overflow)?;
            self.complete = advance_source_start(payload, end)?;
            self.inner.guard(measure, original)?;
            self.line
                .take()
                .ok_or(MarkdownSoftLineResolveError::InvalidRequest)
        })();
        match result {
            Ok(line) => Ok(line),
            Err(error) => self.fail(error),
        }
    }
    /// 取走真正Ready的前缀Job。下一步须先消费并释放它，余文仍由本selector私有cursor推进。
    pub fn take_shaped_prefix(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<(
        MarkdownEmergencyShapedPrefixBoundary,
        MarkdownSoftLineResolvedShape,
    )> {
        let result = (|| {
            self.inner.guard(measure, original)?;
            if self.prefix_outstanding || self.line.is_some() {
                return Err(MarkdownSoftLineResolveError::InvalidRequest);
            }
            let payload = self
                .inner
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
            let boundary = prefix_boundary(payload)?;
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
            self.work.lines = self
                .work
                .lines
                .checked_add(1)
                .ok_or(MarkdownSoftLineResolveError::Overflow)?;
            self.complete = advance_source_start(payload, boundary.source_utf16.end)?;
            self.prefix_outstanding = true;
            self.inner.guard(measure, original)?;
            Ok((boundary, line))
        })();
        match result {
            Ok(line) => Ok(line),
            Err(error) => self.fail(error),
        }
    }
    /// 仅真实Reshaped回答可交接；其余阶段不能用坐标副本构造伪新Job。
    pub fn take_reshaped_parts(
        self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<MarkdownSoftLineResolvedParts> {
        self.inner.take_parts(measure, original)
    }
    pub fn close(self) -> Result<MarkdownSoftLineResolveResources> {
        drop(self.line);
        self.inner.close()
    }
}
fn empty_step(answer: MarkdownEmergencyGlyphSliceAnswer) -> MarkdownEmergencyGlyphSliceStep {
    MarkdownEmergencyGlyphSliceStep {
        input_bytes: 0,
        glyphs: 0,
        candidates: 0,
        shapes: 0,
        answer,
    }
}

/// 相同原Job的封闭FIFO起点；来自真实GCB已选端点，不由公开Range或窗口边界恢复。
pub(super) fn advance_source_start(
    payload: &mut Payload,
    end: usize,
) -> Result<Option<(usize, bool)>> {
    if end <= payload.seed.start || end > payload.limit || end != payload.selected_end {
        return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
    }
    if end == payload.limit {
        payload.stage = Stage::Finished;
        return Ok(Some((end, end == payload.seed.source.utf16_len())));
    }
    payload.seed.start = end;
    payload.next = end;
    payload.candidate = end;
    payload.selected_end = end;
    payload.expected_width = None;
    payload.stage = Stage::Choose;
    payload.kind = MarkdownSoftLineResolvedKind::Natural;
    payload.scan = GlyphFacts::default();
    payload.line_facts = GlyphFacts::default();
    // 保留Mode::Emergency、原item/font/features/取消与nonce。原Source邻接context仍由原
    // reshape_subrange读取；软行不产生新BOT/EOT，真实未消费tail也不产生Ready或伪GlyphProvider。
    Ok(None)
}
pub(super) fn prefix_boundary(payload: &Payload) -> Result<MarkdownEmergencyShapedPrefixBoundary> {
    if !payload.defer_tail
        || !matches!(payload.stage, Stage::ShapedPrefix)
        || payload.line.is_none()
        || payload.current.is_some()
        || payload.best.is_some()
        || payload.tail.is_some()
        || payload.selected_end <= payload.seed.start
        || payload.selected_end > payload.limit
    {
        return Err(MarkdownSoftLineResolveError::InvalidRequest);
    }
    Ok(MarkdownEmergencyShapedPrefixBoundary {
        source_utf16: payload.seed.start..payload.selected_end,
        remainder_utf16: payload.selected_end..payload.seed.item.end,
        kind: payload.kind,
        x_advance_26_6: payload.line_facts.width,
        glyphs: payload.line_facts.count,
        prefix_flags_union: payload.line_facts.flags,
    })
}
