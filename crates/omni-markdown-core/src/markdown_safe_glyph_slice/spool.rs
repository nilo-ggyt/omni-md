//! 历史item必须先由仍活的原测量/原Job签收据；最终仅借原sealed文件，不复制整个spool。
use super::*;
use crate::{
    MarkdownShapedGlyph, MarkdownShapedSpool, MarkdownShapedSpoolError,
    MarkdownShapingCancellation, MarkdownShapingDirection,
    markdown_shaping::MarkdownSpoolSliceOrigin,
};

pub const MARKDOWN_SAFE_SPOOL_SLICE_GLYPHS: usize = 256;
#[derive(Debug, Error)]
pub enum MarkdownSafeSpoolSliceError {
    #[error("历史item尚未被原测量完整消费，或最终机会仍需重整形")]
    NeedsResolution,
    #[error("历史item不在原已选行内")]
    OutsideSelectedLine,
    #[error("历史item字形范围、配额或累计值不一致")]
    InconsistentCoverage,
    #[error("历史item字形坐标或advance溢出")]
    Overflow,
    #[error("历史item收据已经撤权")]
    Poisoned,
    #[error("历史item原测量已取消")]
    Cancelled,
    #[error("历史item原源失败")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("历史item原测量失败")]
    Measure(#[from] MarkdownSoftLineMeasureError),
    #[error("历史item原字形任务失败")]
    Shaping(#[from] MarkdownShapingError),
    #[error("历史item原断点索引失败")]
    Break(#[from] MarkdownLayoutBreakError),
    #[error("历史item原只读输出失败")]
    Spool(#[from] MarkdownShapedSpoolError),
}
type Result<T> = std::result::Result<T, MarkdownSafeSpoolSliceError>;

/// 只保整行弱票与取消/撤权标量；合法continue不撤此票，失败或Drop立刻撤销。
struct LineProof {
    measure: Weak<()>,
    alive: Arc<AtomicBool>,
    cancellation: MarkdownSoftLineMeasureCancellation,
}
impl LineProof {
    fn require(&self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(MarkdownSafeSpoolSliceError::Cancelled);
        }
        if !self.alive.load(Ordering::Acquire) || self.measure.upgrade().is_none() {
            return Err(MarkdownSafeSpoolSliceError::Poisoned);
        }
        Ok(())
    }
}

/// 不可Clone的历史收据；必须在NeedNextItem、原Job尚活时取得，不能事后回采同源字体。
pub struct MarkdownMeasuredItemSpoolProof {
    origin: MarkdownSpoolSliceOrigin,
    line: LineProof,
    measured: Range<usize>,
}
impl MarkdownMeasuredItemSpoolProof {
    pub fn capture(
        measure: &MarkdownSoftLineMeasureJob,
        job: &mut MarkdownShapingJob,
        spool: &mut MarkdownShapedSpool,
    ) -> Result<Self> {
        use crate::markdown_soft_line_measure::provider::JobGlyphProvider;
        let permit = measure
            .measured_item_permit(&mut JobGlyphProvider::new(job, None))
            .map_err(|error| match error {
                MarkdownSoftLineMeasureError::InvalidRequest => {
                    MarkdownSafeSpoolSliceError::NeedsResolution
                }
                other => MarkdownSafeSpoolSliceError::Measure(other),
            })?; // 维持公开旧API对“真实尚非NeedNextItem”的阶段错误，不改变原调用方恢复语义。
        let facts = permit.require(&mut JobGlyphProvider::new(job, None))?;
        let origin = spool.safe_slice_origin(&facts.source)?;
        spool.require_safe_slice_job(&origin, job)?;
        if origin.range != facts.source_utf16
            || origin.count != facts.glyphs
            || origin.generation != facts.generation
            || origin.direction != facts.direction
            || facts.origin
                != crate::markdown_soft_line_measure::provider::GlyphProviderOrigin::Job(
                    job.index_identity(),
                )
        {
            return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
        }
        job.glyph(&facts.source, &facts.font, facts.glyphs)?;
        permit.require(&mut JobGlyphProvider::new(job, None))?;
        Ok(Self {
            origin,
            measured: facts.measured_utf16,
            line: LineProof {
                measure: facts.line.owner,
                alive: facts.line.alive,
                cancellation: facts.line.cancellation,
            },
        }) // 与Store共用真实NeedNextItem门；不再为了签固定事实临时clone整份Source。
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownSafeSpoolSliceStep {
    pub glyphs: usize,
    pub ready: bool,
    pub pending_dependencies: bool,
}
#[derive(Clone, Copy)]
enum Phase {
    StartGcb,
    EndGcb,
    SeekStart { low: usize, high: usize },
    SeekEnd { low: usize, high: usize },
    CheckStart,
    CheckEnd,
    Accumulate { rank: usize },
    CheckAfter { rank: usize },
    Ready,
}
enum Progress {
    Metadata,
    Read,
    Pending,
}

/// 每次至多读256个真实record；二分、flags和advance都在原文件上有限推进，不建索引镜像。
pub struct MarkdownSafeSpoolSliceJob {
    proof: Option<MarkdownMeasuredItemSpoolProof>,
    phase: Phase,
    range: Range<usize>,
    first: usize,
    last: usize,
    x: i64,
    y: i64,
    poisoned: bool,
    shape_cancellation: MarkdownShapingCancellation,
}
impl MarkdownSafeSpoolSliceJob {
    pub fn new(
        measure: &MarkdownSoftLineMeasureJob,
        current: &mut MarkdownShapingJob,
        spool: &mut MarkdownShapedSpool,
        proof: MarkdownMeasuredItemSpoolProof,
    ) -> Result<Self> {
        let seed = measure.resolve_seed(current)?;
        spool.require_safe_slice_origin(&proof.origin)?;
        proof.line.require()?;
        if !Weak::ptr_eq(&seed.owner, &proof.line.measure) || seed.authority != proof.origin.source
        {
            return Err(MarkdownSafeSpoolSliceError::Poisoned);
        }
        let MarkdownSoftLineMeasureChoice::MeasuredBreak(selected) = &seed.choice else {
            return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
        };
        let range = selected.source_utf16.start.max(proof.origin.range.start)
            ..selected.source_utf16.end.min(proof.origin.range.end);
        if range.is_empty() {
            return Err(MarkdownSafeSpoolSliceError::OutsideSelectedLine);
        }
        if range.start < proof.measured.start || range.end > proof.measured.end {
            return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
        }
        seed.source.health()?;
        Ok(Self {
            proof: Some(proof),
            phase: Phase::StartGcb,
            range,
            first: 0,
            last: 0,
            x: 0,
            y: 0,
            poisoned: false,
            shape_cancellation: current.cancellation(),
        })
    }
    fn proof(&self) -> Result<&MarkdownMeasuredItemSpoolProof> {
        self.proof
            .as_ref()
            .ok_or(MarkdownSafeSpoolSliceError::Poisoned)
    }
    fn guard(
        &self,
        measure: &MarkdownSoftLineMeasureJob,
        current: &mut MarkdownShapingJob,
        spool: &mut MarkdownShapedSpool,
    ) -> Result<()> {
        let proof = self.proof()?;
        spool.require_safe_slice_origin(&proof.origin)?; // 原Source/IO优先于元数据撤权与取消。
        measure.require_resolve(&proof.line.measure, current)?;
        proof.line.require()?;
        if self.poisoned {
            return Err(MarkdownSafeSpoolSliceError::Poisoned);
        }
        Ok(())
    }
    fn glyph(&self, spool: &mut MarkdownShapedSpool, rank: usize) -> Result<MarkdownShapedGlyph> {
        let origin = &self.proof()?.origin;
        let index = visual_index(origin, rank)?;
        spool
            .glyph(&origin.authority, &origin.source, &origin.font, index)?
            .ok_or(MarkdownSafeSpoolSliceError::InconsistentCoverage)
    }
    /// 逻辑rank按源递增；RTL只反向选原视觉下标，不反转glyph或改变源cluster。
    fn advance_one(
        &mut self,
        spool: &mut MarkdownShapedSpool,
        breaks: &mut MarkdownLayoutBreakJob,
    ) -> Result<Progress> {
        let origin = self.proof()?.origin.clone();
        match self.phase {
            Phase::StartGcb | Phase::EndGcb => {
                let start = matches!(self.phase, Phase::StartGcb);
                let at = if start {
                    self.range.start
                } else {
                    self.range.end
                };
                match breaks.grapheme_query(
                    &origin.source,
                    at,
                    MarkdownGraphemeQuery::IsBoundary,
                )? {
                    MarkdownGraphemeAnswer::Pending => return Ok(Progress::Pending),
                    MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == at => {}
                    _ => return Err(MarkdownSafeSpoolSliceError::NeedsResolution),
                }
                self.phase = if start {
                    Phase::EndGcb
                } else {
                    Phase::SeekStart {
                        low: 0,
                        high: origin.count,
                    }
                };
                Ok(Progress::Metadata)
            }
            Phase::SeekStart { low, high } | Phase::SeekEnd { low, high } => {
                let start = matches!(self.phase, Phase::SeekStart { .. });
                let target = if start {
                    self.range.start
                } else {
                    self.range.end
                };
                if low == high {
                    if start {
                        self.first = low;
                        self.phase = Phase::SeekEnd {
                            low: 0,
                            high: origin.count,
                        };
                    } else {
                        self.last = low;
                        self.phase = Phase::CheckStart;
                    }
                    return Ok(Progress::Metadata);
                }
                let middle = low + (high - low) / 2;
                let glyph = self.glyph(spool, middle)?;
                let (low, high) = if glyph.source_utf16_cluster < target {
                    (middle + 1, high)
                } else {
                    (low, middle)
                };
                self.phase = if start {
                    Phase::SeekStart { low, high }
                } else {
                    Phase::SeekEnd { low, high }
                };
                Ok(Progress::Read)
            }
            Phase::CheckStart => {
                if self.first > self.last || self.last > origin.count {
                    return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
                }
                self.phase = Phase::CheckEnd;
                if self.range.start != origin.range.start {
                    if self.first == origin.count
                        || self.glyph(spool, self.first)?.source_utf16_cluster != self.range.start
                    {
                        return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
                    }
                    Ok(Progress::Read)
                } else {
                    Ok(Progress::Metadata)
                }
            }
            Phase::CheckEnd => {
                self.phase = Phase::Accumulate { rank: self.first };
                if self.range.end != origin.range.end {
                    if self.last == origin.count
                        || self.glyph(spool, self.last)?.source_utf16_cluster != self.range.end
                    {
                        return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
                    }
                    Ok(Progress::Read)
                } else {
                    Ok(Progress::Metadata)
                }
            }
            Phase::Accumulate { rank } => {
                if rank == self.last {
                    self.phase = Phase::CheckAfter { rank: self.last };
                    return Ok(Progress::Metadata);
                }
                let glyph = self.glyph(spool, rank)?;
                if !self.range.contains(&glyph.source_utf16_cluster) || glyph.glyph_id == 0 {
                    return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
                }
                if self.range.start != origin.range.start
                    && glyph.source_utf16_cluster == self.range.start
                    && glyph.flags & UNSAFE != 0
                {
                    return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
                } // 起点整个cluster的flags均须检查，不能把首glyph的0当作其他glyph的证明。
                self.x = self
                    .x
                    .checked_add(i64::from(glyph.x_advance))
                    .ok_or(MarkdownSafeSpoolSliceError::Overflow)?;
                self.y = self
                    .y
                    .checked_add(i64::from(glyph.y_advance))
                    .ok_or(MarkdownSafeSpoolSliceError::Overflow)?;
                self.phase = Phase::Accumulate { rank: rank + 1 };
                Ok(Progress::Read)
            }
            Phase::CheckAfter { rank } => {
                if self.range.end == origin.range.end || rank == origin.count {
                    self.phase = Phase::Ready;
                    return Ok(Progress::Metadata);
                }
                let glyph = self.glyph(spool, rank)?;
                if glyph.source_utf16_cluster != self.range.end {
                    self.phase = Phase::Ready;
                } else {
                    if glyph.flags & UNSAFE != 0 {
                        return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
                    }
                    self.phase = Phase::CheckAfter { rank: rank + 1 };
                } // 末端属于后继cluster；该cluster很长时持续真实Pending工作，不截取首glyph冒Ready。
                Ok(Progress::Read)
            }
            Phase::Ready => Ok(Progress::Metadata),
        }
    }
    pub fn step(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        current: &mut MarkdownShapingJob,
        spool: &mut MarkdownShapedSpool,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: usize,
    ) -> Result<MarkdownSafeSpoolSliceStep> {
        let result = (|| {
            self.guard(measure, current, spool)?;
            if budget == 0 || budget > MARKDOWN_SAFE_SPOOL_SLICE_GLYPHS {
                return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
            }
            let mut glyphs = 0;
            let mut pending_dependencies = false;
            while glyphs < budget && !matches!(self.phase, Phase::Ready) {
                match self.advance_one(spool, breaks)? {
                    Progress::Read => glyphs += 1,
                    Progress::Metadata => {}
                    Progress::Pending => {
                        pending_dependencies = true;
                        break;
                    }
                } // 无字形操作的状态转移总数固定；只有真实record读取消耗glyph配额。
            }
            self.guard(measure, current, spool)?;
            Ok(MarkdownSafeSpoolSliceStep {
                glyphs,
                ready: matches!(self.phase, Phase::Ready),
                pending_dependencies,
            })
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    /// 完整核验后消费候选；输出只是借原spool的读票，不新建FD或复制原glyph文件。
    pub fn seal(
        mut self,
        measure: &MarkdownSoftLineMeasureJob,
        current: &mut MarkdownShapingJob,
        spool: &mut MarkdownShapedSpool,
    ) -> Result<MarkdownSafeSpoolSlice> {
        self.guard(measure, current, spool)?;
        if !matches!(self.phase, Phase::Ready) {
            return Err(MarkdownSafeSpoolSliceError::NeedsResolution);
        }
        let proof = self
            .proof
            .take()
            .ok_or(MarkdownSafeSpoolSliceError::Poisoned)?;
        let backwards = backwards(&proof.origin);
        let visual = if backwards {
            proof.origin.count - self.last..proof.origin.count - self.first
        } else {
            self.first..self.last
        };
        Ok(MarkdownSafeSpoolSlice {
            proof,
            cancellation: self.shape_cancellation,
            facts: MarkdownSafeGlyphSliceFacts {
                source_utf16: self.range,
                visual_glyphs: visual,
                x_advance_26_6: self.x,
                y_advance_26_6: self.y,
            },
            poisoned: false,
        })
    }
}
fn backwards(origin: &MarkdownSpoolSliceOrigin) -> bool {
    matches!(origin.direction, MarkdownShapingDirection::RightToLeft)
}
fn visual_index(origin: &MarkdownSpoolSliceOrigin, rank: usize) -> Result<usize> {
    if rank >= origin.count {
        return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
    }
    Ok(if backwards(origin) {
        origin.count - rank - 1
    } else {
        rank
    })
}

/// 必须由上面完整job签发；公开Facts不能构造或替换这张具体spool读票。
pub struct MarkdownSafeSpoolSlice {
    proof: MarkdownMeasuredItemSpoolProof,
    cancellation: MarkdownShapingCancellation,
    facts: MarkdownSafeGlyphSliceFacts,
    poisoned: bool,
}
impl MarkdownSafeSpoolSlice {
    #[must_use]
    pub fn facts(&self) -> MarkdownSafeGlyphSliceFacts {
        self.facts.clone()
    }
    fn require(&mut self, spool: &mut MarkdownShapedSpool) -> Result<()> {
        let result = (|| {
            spool.require_safe_slice_origin(&self.proof.origin)?;
            self.proof.line.require()?;
            if self.cancellation.requested() {
                return Err(MarkdownSafeSpoolSliceError::Cancelled);
            }
            if self.poisoned {
                return Err(MarkdownSafeSpoolSliceError::Poisoned);
            }
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    pub fn view(
        mut self,
        spool: &mut MarkdownShapedSpool,
    ) -> Result<MarkdownSafeSpoolSliceView<'_>> {
        self.require(spool)?;
        Ok(MarkdownSafeSpoolSliceView { slice: self, spool })
    }
}
/// Rust独占借用阻止读取期间close/换掉原文件；没有第二reader owner、映射或FD。
pub struct MarkdownSafeSpoolSliceView<'a> {
    slice: MarkdownSafeSpoolSlice,
    spool: &'a mut MarkdownShapedSpool,
}
impl MarkdownSafeSpoolSliceView<'_> {
    #[must_use]
    pub fn facts(&self) -> MarkdownSafeGlyphSliceFacts {
        self.slice.facts()
    }
    pub fn read_batch(
        &mut self,
        range: Range<usize>,
        output: &mut [MarkdownShapedGlyph],
    ) -> Result<usize> {
        let result = (|| {
            self.slice.require(self.spool)?;
            let facts = &self.slice.facts;
            if range.start > range.end
                || range.end > facts.visual_glyphs.len()
                || range.len() != output.len()
                || range.len() > MARKDOWN_SAFE_SPOOL_SLICE_GLYPHS
            {
                return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
            }
            let origin = &self.slice.proof.origin;
            let absolute =
                facts.visual_glyphs.start + range.start..facts.visual_glyphs.start + range.end;
            self.spool.read_batch(
                &origin.authority,
                &origin.source,
                &origin.font,
                absolute,
                output,
            )?;
            if output
                .iter()
                .any(|glyph| !facts.source_utf16.contains(&glyph.source_utf16_cluster))
            {
                return Err(MarkdownSafeSpoolSliceError::InconsistentCoverage);
            }
            self.slice.require(self.spool)?;
            Ok(output.len())
        })();
        if result.is_err() {
            self.slice.poisoned = true;
        }
        result // Err时output候选全部作废；读取过一部分不构成可交付前缀。
    }
}
