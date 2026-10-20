//! 原测量选择的实际单item重整形；软行不是新的段落，正文始终来自同一冻结Source。
//!
//! 原cluster证明安全的候选可先复用精确advance；unsafe或cluster内部候选必须实际shape。
//! 最终前缀与余项均保存真实新job，调用方在同一worker推进；本模块不创建线程或Native对象。
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use thiserror::Error;

#[cfg(feature = "paged-safe-glyph-slices")]
mod emergency;
mod resolved_shape;
#[cfg(feature = "paged-line-compose")]
pub(crate) mod stored;
#[cfg(feature = "paged-safe-glyph-slices")]
pub use emergency::{
    MarkdownEmergencyGlyphSliceAnswer, MarkdownEmergencyGlyphSliceBoundary,
    MarkdownEmergencyGlyphSliceJob, MarkdownEmergencyGlyphSliceLine,
    MarkdownEmergencyGlyphSliceStep, MarkdownEmergencyGlyphSliceWork,
    MarkdownEmergencyShapedPrefixBoundary,
};
#[cfg(feature = "paged-safe-glyph-slices")]
pub(crate) use resolved_shape::MarkdownEmergencyResolvedChoice;
pub(crate) use resolved_shape::MarkdownResolvedLineStart;
pub use resolved_shape::{MarkdownSoftLineResolvedParts, MarkdownSoftLineResolvedShape};

use crate::{
    MarkdownDocumentBufferError, MarkdownGlyphClusterAnswer, MarkdownGlyphClusterError,
    MarkdownGlyphClusterIndex, MarkdownGraphemeAnswer, MarkdownGraphemeQuery,
    MarkdownLayoutBreakError, MarkdownLayoutBreakJob, MarkdownLayoutTextSourceObserver,
    MarkdownOriginalBackingObserver, MarkdownShapedGlyph, MarkdownShapingBudget,
    MarkdownShapingCancellation, MarkdownShapingError, MarkdownShapingJob,
    MarkdownShapingResourceObserver, MarkdownShapingStatus, MarkdownSoftLineMeasureCancellation,
    MarkdownSoftLineMeasureChoice, MarkdownSoftLineMeasureError, MarkdownSoftLineMeasureJob,
    markdown_soft_line_measure::{
        MarkdownSoftLineResolveSeed,
        provider::{JobGlyphProvider, ResolutionProvider},
    },
};

pub const MARKDOWN_SOFT_LINE_RESOLVE_GLYPHS: usize = 256;
pub const MARKDOWN_SOFT_LINE_RESOLVE_CANDIDATES: usize = 64;
const UNSAFE: u32 = harfrust::GlyphFlags::UNSAFE_TO_BREAK.to_bits();

#[derive(Debug, Error)]
pub enum MarkdownSoftLineResolveError {
    #[error("软行解析阶段或配额无效")]
    InvalidRequest,
    #[error("软行候选、原glyph或实际重排结果不一致")]
    InconsistentCoverage,
    #[error("软行解析坐标或advance溢出")]
    Overflow,
    #[error("软行解析已永久撤权")]
    Poisoned,
    #[error("软行字形资源未实际完整归还")]
    ResourceCleanup,
    #[error("软行原冻结源失败")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("软行原测量已失权")]
    Measure(#[from] MarkdownSoftLineMeasureError),
    #[error("软行实际整形失败")]
    Shaping(#[from] MarkdownShapingError),
    #[error("软行原cluster索引失败")]
    Cluster(#[from] MarkdownGlyphClusterError),
    #[error("软行原断点索引失败")]
    Break(#[from] MarkdownLayoutBreakError),
    #[error("软行只读字形交付失败")]
    Spool(#[from] crate::MarkdownShapedSpoolError),
    #[cfg(feature = "paged-safe-glyph-slices")]
    #[error("原紧急安全字形切片失败")]
    SafeSlice(#[from] crate::MarkdownSafeGlyphSliceError),
}
type Result<T> = std::result::Result<T, MarkdownSoftLineResolveError>;

/// 一个解析阶段同时请求原测量位与原shaper位；所有试次沿同一原shaper位，不逐候选复位。
#[derive(Clone)]
pub struct MarkdownSoftLineResolveCancellation {
    measure: MarkdownSoftLineMeasureCancellation,
    shape: MarkdownShapingCancellation,
}
impl MarkdownSoftLineResolveCancellation {
    pub fn cancel(&self) {
        self.measure.cancel();
        self.shape.request(); // 真正深层shape观察这个原位；旧Measure位单独请求仍在量子边界核验。
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MarkdownSoftLineResolveBudget {
    pub input_bytes: usize,
    pub input_scalars: usize,
    pub glyphs: usize,
    pub candidates: usize,
}
impl Default for MarkdownSoftLineResolveBudget {
    fn default() -> Self {
        Self {
            input_bytes: 4096,
            input_scalars: 256,
            glyphs: MARKDOWN_SOFT_LINE_RESOLVE_GLYPHS,
            candidates: MARKDOWN_SOFT_LINE_RESOLVE_CANDIDATES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineResolvePending {
    FontFallback {
        source_utf16: Range<usize>,
        missing_glyphs: usize,
    },
    Dictionary,
    NextItem,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineResolvedKind {
    Natural,
    Emergency,
    OverflowingGrapheme,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineResolvedSide {
    Line,
    Remainder,
}

/// 仅真实已shape的源范围/字形事实；Native几何、跨item合成和BiDi行重排仍在后继阶段。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSoftLineResolvedBoundary {
    pub source_utf16: Range<usize>,
    pub remainder_utf16: Range<usize>,
    pub kind: MarkdownSoftLineResolvedKind,
    pub x_advance_26_6: i64,
    pub line_glyphs: usize,
    pub remainder_glyphs: usize,
    pub line_flags_union: u32,
    pub remainder_flags_union: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineResolveAnswer {
    EmptySource,
    PendingDependencies,
    Yielded,
    Pending(MarkdownSoftLineResolvePending),
    Resolved(MarkdownSoftLineResolvedBoundary),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSoftLineResolveStep {
    pub input_bytes: usize,
    /// 原整形输入本次实际消费的标量；与正文 bytes 分开传递，不由 UTF-8 长度估算。
    pub input_scalars: usize,
    /// 本次新建实际重排 Job 时读取的相邻上下文；不与后继正文输入重复结算。
    pub context_bytes: usize,
    pub glyphs: usize,
    pub candidates: usize,
    pub shapes: usize,
    pub answer: MarkdownSoftLineResolveAnswer,
}

#[derive(Default)]
struct ResourceFacts {
    active: AtomicUsize,
    peak: AtomicUsize,
    created: AtomicUsize,
    released: AtomicUsize,
    failed: AtomicBool,
}
#[derive(Clone)]
pub struct MarkdownSoftLineResolveResourceObserver {
    source: MarkdownLayoutTextSourceObserver,
    facts: Arc<ResourceFacts>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownSoftLineResolveResources {
    pub source_payload_released: bool,
    pub active_jobs: usize,
    pub peak_active_jobs: usize,
    pub jobs_created: usize,
    pub jobs_released: usize,
    pub cleanup_failed: bool,
}
impl MarkdownSoftLineResolveResourceObserver {
    #[must_use]
    pub fn snapshot(&self) -> MarkdownSoftLineResolveResources {
        MarkdownSoftLineResolveResources {
            source_payload_released: self.source.payload_released(),
            active_jobs: self.facts.active.load(Ordering::Acquire),
            peak_active_jobs: self.facts.peak.load(Ordering::Acquire),
            jobs_created: self.facts.created.load(Ordering::Acquire),
            jobs_released: self.facts.released.load(Ordering::Acquire),
            cleanup_failed: self.facts.failed.load(Ordering::Acquire),
        }
    }
}

/// 只有这个局部owner能结算子job；先真正Drop，再读取原observer，不提供手动签成功入口。
struct OwnedShape {
    job: Option<MarkdownShapingJob>,
    observer: MarkdownShapingResourceObserver,
    resources: Arc<ResourceFacts>,
}
impl OwnedShape {
    fn new(job: MarkdownShapingJob, resources: &Arc<ResourceFacts>) -> Result<Self> {
        if resources
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .is_err()
            || resources
                .created
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
                .is_err()
        {
            resources.failed.store(true, Ordering::Release);
            return Err(MarkdownSoftLineResolveError::Overflow);
        }
        resources
            .peak
            .fetch_max(resources.active.load(Ordering::Acquire), Ordering::AcqRel);
        let observer = job.resource_observer();
        Ok(Self {
            job: Some(job),
            observer,
            resources: resources.clone(),
        })
    }
    fn job(&mut self) -> Result<&mut MarkdownShapingJob> {
        self.job
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)
    }
}
impl Drop for OwnedShape {
    fn drop(&mut self) {
        drop(self.job.take());
        let observed = self.observer.snapshot();
        if observed.files_open != 0
            || observed.mappings_open != 0
            || observed.mapped_bytes != 0
            || observed.cleanup_failed
            || observed.first_error.is_some()
        {
            self.resources.failed.store(true, Ordering::Release);
        } // 字体/Source仍可由原owner合法共享；这里只结算本job真实glyph映射与文件。
        if self
            .resources
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .is_err()
            || self
                .resources
                .released
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
                .is_err()
        {
            self.resources.failed.store(true, Ordering::Release); // Drop不panic；计数异常永远不能签资源成功。
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Exact,
    Emergency,
}
#[derive(Clone, Copy)]
enum Stage {
    Choose,
    Input,
    Shape,
    Scan,
    TailInput,
    TailShape,
    TailScan,
    Finished,
    #[cfg(feature = "paged-safe-glyph-slices")]
    Reusable,
    #[cfg(feature = "paged-safe-glyph-slices")]
    ShapedPrefix,
}
#[derive(Clone, Copy, Default)]
struct GlyphFacts {
    count: usize,
    width: i64,
    flags: u32,
    missing: usize,
}
struct Fit {
    end: usize,
    width: i64,
    shape: Option<OwnedShape>,
    facts: Option<GlyphFacts>,
}
struct Payload {
    seed: MarkdownSoftLineResolveSeed,
    mode: Mode,
    limit: usize,
    next: usize,
    candidate: usize,
    stage: Stage,
    current: Option<OwnedShape>,
    best: Option<Fit>,
    line: Option<OwnedShape>,
    tail: Option<OwnedShape>,
    scan: GlyphFacts,
    line_facts: GlyphFacts,
    selected_end: usize,
    expected_width: Option<i64>,
    kind: MarkdownSoftLineResolvedKind,
    answer: Option<MarkdownSoftLineResolveAnswer>,
    final_prefix: bool,
    #[cfg(feature = "paged-safe-glyph-slices")]
    reuse_safe: bool,
    #[cfg(feature = "paged-safe-glyph-slices")]
    defer_tail: bool,
}

/// 原Measure/Shape仅Weak签发；每次操作借回真实owner，本层最多保留两个实际分页子job。
pub struct MarkdownSoftLineResolveJob {
    payload: Option<Payload>,
    cancellation: MarkdownSoftLineResolveCancellation,
    resources: MarkdownSoftLineResolveResourceObserver,
    backing: Option<MarkdownOriginalBackingObserver>,
}
impl MarkdownSoftLineResolveJob {
    /// 不创建新shaper；实际子job仅在worker调用step时构造。
    pub fn new(
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<Self> {
        let seed = measure.resolve_seed(original)?;
        Self::from_seed(
            seed,
            JobGlyphProvider::new(original, None).shaping_cancellation(),
        ) // 同原Job的取消位，所有producer都从这一能力边界交给共用selector。
    }

    /// 已选原seed和真实producer的原取消位共用唯一装配，Stored不伪造原Ready Job。
    fn from_seed(
        seed: MarkdownSoftLineResolveSeed,
        shape: MarkdownShapingCancellation,
    ) -> Result<Self> {
        let cancellation = MarkdownSoftLineResolveCancellation {
            measure: seed.cancellation.clone(),
            shape,
        };
        let resources = MarkdownSoftLineResolveResourceObserver {
            source: seed.source.resource_observer(),
            facts: Arc::new(ResourceFacts::default()),
        };
        let backing = seed.source.original_backing_observer();
        let (mode, limit, pending) = match &seed.choice {
            MarkdownSoftLineMeasureChoice::MeasuredBreak(value) => {
                (Mode::Exact, value.source_utf16.end, None)
            }
            MarkdownSoftLineMeasureChoice::NeedsReshape { source_utf16, .. } => {
                (Mode::Exact, source_utf16.end, None)
            }
            MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(value) => {
                (Mode::Emergency, value.source_utf16.end, None)
            }
            #[cfg(feature = "paged-shaping-frontier")]
            MarkdownSoftLineMeasureChoice::NeedsFrontierEmergencyPrefix {
                source_utf16, ..
            } => (Mode::Emergency, source_utf16.end, None), // 只解析实际provider前缀；原word_end从未成为可shape的范围或伪UAX机会。
            #[cfg(feature = "paged-shaping-frontier")]
            MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak { source_utf16, .. } => {
                (Mode::Emergency, source_utf16.end, None)
            } // 旧显式resolver仍可重核原选择；生产Store直接消费封闭安全票。
            MarkdownSoftLineMeasureChoice::NeedsFontFallback {
                cluster_utf16,
                missing_glyphs,
            } => (
                Mode::Exact,
                seed.start,
                Some(MarkdownSoftLineResolvePending::FontFallback {
                    source_utf16: cluster_utf16.clone(),
                    missing_glyphs: *missing_glyphs,
                }),
            ),
            MarkdownSoftLineMeasureChoice::NeedsDictionary(_) => (
                Mode::Exact,
                seed.start,
                Some(MarkdownSoftLineResolvePending::Dictionary),
            ),
            MarkdownSoftLineMeasureChoice::NeedNextItem { .. } => (
                Mode::Exact,
                seed.start,
                Some(MarkdownSoftLineResolvePending::NextItem),
            ),
            MarkdownSoftLineMeasureChoice::EmptySource => (Mode::Exact, seed.start, None),
        };
        let pending = pending.or_else(|| {
            (seed.start < seed.item.start || limit > seed.item.end)
                .then_some(MarkdownSoftLineResolvePending::NextItem)
        });
        let answer = if matches!(seed.choice, MarkdownSoftLineMeasureChoice::EmptySource) {
            Some(MarkdownSoftLineResolveAnswer::EmptySource)
        } else {
            pending.map(MarkdownSoftLineResolveAnswer::Pending)
        };
        let next = seed.start;
        Ok(Self {
            payload: Some(Payload {
                seed,
                mode,
                limit,
                next,
                candidate: next,
                stage: if answer.is_some() {
                    Stage::Finished
                } else {
                    Stage::Choose
                },
                current: None,
                best: None,
                line: None,
                tail: None,
                scan: GlyphFacts::default(),
                line_facts: GlyphFacts::default(),
                selected_end: next,
                expected_width: None,
                kind: MarkdownSoftLineResolvedKind::Natural,
                answer,
                final_prefix: false,
                #[cfg(feature = "paged-safe-glyph-slices")]
                reuse_safe: false,
                #[cfg(feature = "paged-safe-glyph-slices")]
                defer_tail: false,
            }),
            cancellation,
            resources,
            backing,
        })
    }

    #[must_use]
    pub fn cancellation(&self) -> MarkdownSoftLineResolveCancellation {
        self.cancellation.clone()
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownSoftLineResolveResourceObserver {
        self.resources.clone()
    }

    /// 消费原解析owner，转交两个真实job；公开Copy坐标不能构造后继行首权限。
    ///
    /// 转交后原Measure可归还；每个具体Shape仍由自己的Source/Font/nonce与原取消位守护。
    pub fn take_parts(
        mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<MarkdownSoftLineResolvedParts> {
        self.guard(measure, original)?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        let boundary = match &payload.answer {
            Some(MarkdownSoftLineResolveAnswer::Resolved(boundary)) => boundary.clone(),
            _ => return Err(MarkdownSoftLineResolveError::InvalidRequest),
        };
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
        let remainder = match payload.tail.take() {
            Some(tail) => Some(MarkdownSoftLineResolvedShape::new(
                tail,
                payload.seed.source.clone(),
                payload.seed.authority.clone(),
                payload.seed.font.clone(),
                true,
            )?),
            None => None, // 真空余项不制造第二个用户文档或假Job。
        };
        let parts = MarkdownSoftLineResolvedParts::new(boundary, line, remainder);
        self.guard(measure, original)?; // 所有转移完成后仍需原票；失败时候选parts真实Drop，不发布部分前缀。
        drop(self.payload.take());
        Ok(parts)
    }

    fn guard(
        &self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
    ) -> Result<()> {
        self.guard_provider(measure, &mut JobGlyphProvider::new(original, None))
    }

    fn guard_provider(
        &self,
        measure: &MarkdownSoftLineMeasureJob,
        provider: &mut dyn ResolutionProvider,
    ) -> Result<()> {
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        payload.seed.source.health()?;
        if payload.seed.origin != provider.origin() {
            return Err(MarkdownSoftLineMeasureError::JobMismatch.into());
        } // 同一Measure合法交接后仍是原nonce，但旧selector不能借后项目录重签原字体/范围。
        measure.require_resolve_provider(&payload.seed.owner, provider)?;
        if self.resources.facts.failed.load(Ordering::Acquire) {
            return Err(MarkdownSoftLineResolveError::ResourceCleanup);
        }
        Ok(())
    }

    /// 每次最多一个真实shape；它是worker同步算法，不冒称主线程有限时延。
    pub fn step(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownSoftLineResolveStep> {
        self.step_provider(
            measure,
            &mut JobGlyphProvider::new(original, Some(clusters)),
            breaks,
            budget,
        )
    }

    fn step_provider(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        provider: &mut dyn ResolutionProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownSoftLineResolveStep> {
        let result = self.advance(measure, provider, breaks, budget);
        let result = match self.payload.as_ref().map(|p| p.seed.source.health()) {
            Some(Err(error)) => Err(error.into()),
            _ => result.and_then(|answer| {
                self.guard_provider(measure, provider)?;
                Ok(answer)
            }),
        };
        if result.is_err() {
            drop(self.payload.take());
        }
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        } // 包括最后Source归还，真实errno不被候选/取消错误或控制占位错误吞掉。
        result
    }

    fn advance(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        provider: &mut dyn ResolutionProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineResolveBudget,
    ) -> Result<MarkdownSoftLineResolveStep> {
        self.guard_provider(measure, provider)?;
        if budget.input_bytes == 0
            || budget.input_scalars == 0
            || budget.glyphs == 0
            || budget.candidates == 0
        {
            return Err(MarkdownSoftLineResolveError::InvalidRequest);
        }
        let mut step = MarkdownSoftLineResolveStep {
            input_bytes: 0,
            input_scalars: 0,
            context_bytes: 0,
            glyphs: 0,
            candidates: 0,
            shapes: 0,
            answer: MarkdownSoftLineResolveAnswer::Yielded,
        };
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
        breaks.require_source(&payload.seed.authority)?;
        if let Some(answer) = &payload.answer {
            step.answer = answer.clone();
            return Ok(step);
        }
        match payload.stage {
            Stage::Choose => {
                for _ in 0..budget.candidates.min(MARKDOWN_SOFT_LINE_RESOLVE_CANDIDATES) {
                    let end = match payload.mode {
                        Mode::Exact => payload.limit,
                        Mode::Emergency => match breaks.grapheme_query(
                            &payload.seed.authority,
                            payload.next,
                            MarkdownGraphemeQuery::Next,
                        )? {
                            MarkdownGraphemeAnswer::Pending => {
                                step.answer = MarkdownSoftLineResolveAnswer::PendingDependencies;
                                return Ok(step);
                            }
                            MarkdownGraphemeAnswer::Known(Some(value))
                                if value.utf16 > payload.next && value.utf16 <= payload.limit =>
                            {
                                value.utf16
                            }
                            _ => return Err(MarkdownSoftLineResolveError::InconsistentCoverage),
                        },
                    };
                    match breaks.grapheme_query(
                        &payload.seed.authority,
                        end,
                        MarkdownGraphemeQuery::IsBoundary,
                    )? {
                        MarkdownGraphemeAnswer::Pending => {
                            step.answer = MarkdownSoftLineResolveAnswer::PendingDependencies;
                            return Ok(step);
                        }
                        MarkdownGraphemeAnswer::Known(Some(_)) => {}
                        _ => return Err(MarkdownSoftLineResolveError::InconsistentCoverage),
                    } // 真实选择必须仍是同一原GCB边界，低代理项或字素内部从不获得授权。
                    if end == payload.seed.item.end && end != payload.seed.source.utf16_len() {
                        let following = provider
                            .following_item_flags(&payload.seed.authority, &payload.seed.font)?;
                        if following.is_none_or(|flags| flags & UNSAFE != 0) {
                            pending(payload, MarkdownSoftLineResolvePending::NextItem);
                            break; // 没有真实安全后继仍拒绝；不能用Store/Script末端伪造段落EOF或跳过unsafe边界。
                        } // 同Store已存后项的首cluster提供实际右侧见证；原Source、Font、GCB及上下文合同均不变。
                    }
                    step.candidates += 1;
                    payload.candidate = end;
                    payload.next = end;
                    if matches!(payload.mode, Mode::Emergency)
                        && let Some(width) = safe_width(payload, provider, end)?
                    {
                        if width <= payload.seed.width {
                            payload.best = Some(Fit {
                                end,
                                width,
                                shape: None,
                                facts: None,
                            });
                            if end == payload.limit {
                                step.context_bytes =
                                    choose_best(payload, provider, &self.resources.facts)?;
                                break;
                            }
                            continue; // 同一原数组、完整cluster端点和安全flags已证明精确有符号advance。
                        }
                        #[cfg(feature = "paged-safe-glyph-slices")]
                        if payload.reuse_safe {
                            if payload.best.is_none() {
                                payload.kind = MarkdownSoftLineResolvedKind::OverflowingGrapheme;
                                payload.best = Some(Fit {
                                    end,
                                    width,
                                    shape: None,
                                    facts: None,
                                });
                            } // 第一个完整GCB也超宽时保留整个GCB，并明确overflow；绝不切开组合符。
                            step.context_bytes =
                                choose_best(payload, provider, &self.resources.facts)?;
                            break; // 安全超宽候选的真实重形宽度相同，不再造一个相同前缀来确认。
                        }
                    }
                    step.context_bytes =
                        start_current(payload, provider, &self.resources.facts, false)?;
                    break; // unsafe/cluster内部或超宽候选先实际shape，不用估算宽度终止搜索。
                }
            }
            Stage::Input | Stage::TailInput => {
                let tail = matches!(payload.stage, Stage::TailInput);
                let owned = if tail {
                    payload.tail.as_mut()
                } else {
                    payload.current.as_mut()
                }
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
                let input = owned.job()?.step(
                    &payload.seed.authority,
                    &payload.seed.font,
                    MarkdownShapingBudget {
                        bytes: budget.input_bytes.min(4096),
                        scalars: budget.input_scalars,
                    },
                )?;
                step.input_bytes = input.bytes;
                step.input_scalars = input.scalars;
                if input.status == MarkdownShapingStatus::AwaitingShape {
                    payload.stage = if tail { Stage::TailShape } else { Stage::Shape };
                }
            }
            Stage::Shape | Stage::TailShape => {
                let tail = matches!(payload.stage, Stage::TailShape);
                let owned = if tail {
                    payload.tail.as_mut()
                } else {
                    payload.current.as_mut()
                }
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
                owned
                    .job()?
                    .shape(&payload.seed.authority, &payload.seed.font)?;
                payload.scan = GlyphFacts::default();
                payload.stage = if tail { Stage::TailScan } else { Stage::Scan };
                step.shapes = 1;
            }
            Stage::Scan | Stage::TailScan => {
                let tail = matches!(payload.stage, Stage::TailScan);
                let owned = if tail {
                    payload.tail.as_mut()
                } else {
                    payload.current.as_mut()
                }
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
                let job = owned.job()?;
                let profile = job.profile()?;
                for _ in 0..budget.glyphs.min(MARKDOWN_SOFT_LINE_RESOLVE_GLYPHS) {
                    let Some(glyph) = job.glyph(
                        &payload.seed.authority,
                        &payload.seed.font,
                        payload.scan.count,
                    )?
                    else {
                        break;
                    };
                    payload.scan.count += 1;
                    payload.scan.width = payload
                        .scan
                        .width
                        .checked_add(i64::from(glyph.x_advance))
                        .ok_or(MarkdownSoftLineResolveError::Overflow)?;
                    payload.scan.flags |= glyph.flags;
                    payload.scan.missing += usize::from(glyph.glyph_id == 0);
                    step.glyphs += 1;
                } // 逐实际glyph汇总，不能用输入字符数、cmap或原prefix估计代替真实结果。
                if payload.scan.count == profile.glyph_count {
                    if job
                        .glyph(
                            &payload.seed.authority,
                            &payload.seed.font,
                            profile.glyph_count,
                        )?
                        .is_some()
                        || !profile.shaper_access_certified
                        || profile.source_borrow_max_bytes > 4096
                    {
                        return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
                    }
                    if payload.scan.missing > 0 {
                        let range = job.source_range();
                        pending(
                            payload,
                            MarkdownSoftLineResolvePending::FontFallback {
                                source_utf16: range,
                                missing_glyphs: payload.scan.missing,
                            },
                        );
                    } else if tail {
                        finish(payload)?;
                    } else {
                        step.context_bytes =
                            accept_current(payload, provider, &self.resources.facts)?;
                    }
                }
            }
            #[cfg(feature = "paged-safe-glyph-slices")]
            Stage::Reusable | Stage::ShapedPrefix => {} // 仅显式wrapper消费；旧API不把未解析余文报Ready。
            Stage::Finished => return Err(MarkdownSoftLineResolveError::InconsistentCoverage),
        }
        if let Some(answer) = &payload.answer {
            step.answer = answer.clone();
        }
        Ok(step)
    }

    /// 只读一个真实新glyph；必须仍借原Measure与Shape，不可用同字节新任务重签。
    pub fn glyph(
        &mut self,
        measure: &MarkdownSoftLineMeasureJob,
        original: &mut MarkdownShapingJob,
        side: MarkdownSoftLineResolvedSide,
        ordinal: usize,
    ) -> Result<Option<MarkdownShapedGlyph>> {
        let result = (|| {
            self.guard(measure, original)?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineResolveError::Poisoned)?;
            if !matches!(
                payload.answer,
                Some(MarkdownSoftLineResolveAnswer::Resolved(_))
            ) {
                return Err(MarkdownSoftLineResolveError::InvalidRequest);
            }
            let job = match side {
                MarkdownSoftLineResolvedSide::Line => payload.line.as_mut(),
                MarkdownSoftLineResolvedSide::Remainder => payload.tail.as_mut(),
            };
            match job {
                Some(job) => {
                    Ok(job
                        .job()?
                        .glyph(&payload.seed.authority, &payload.seed.font, ordinal)?)
                }
                None if side == MarkdownSoftLineResolvedSide::Remainder && ordinal == 0 => Ok(None),
                None => Err(MarkdownSoftLineResolveError::InvalidRequest),
            }
        })();
        let result = match self
            .payload
            .as_ref()
            .map(|payload| payload.seed.source.health())
        {
            Some(Err(error)) => Err(error.into()),
            _ => result.and_then(|glyph| {
                self.guard(measure, original)?;
                Ok(glyph)
            }),
        };
        if result.is_err() {
            drop(self.payload.take());
        }
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        result
    }

    /// 归还所有真实子job再结算；原Source可能仍由调用方的原shape/索引合法保活。
    pub fn close(mut self) -> Result<MarkdownSoftLineResolveResources> {
        drop(self.payload.take());
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let facts = self.resources.snapshot();
        if facts.active_jobs != 0
            || facts.jobs_created != facts.jobs_released
            || facts.cleanup_failed
        {
            return Err(MarkdownSoftLineResolveError::ResourceCleanup);
        }
        Ok(facts)
    }
}

fn pending(payload: &mut Payload, reason: MarkdownSoftLineResolvePending) {
    drop(payload.current.take());
    drop(payload.best.take());
    drop(payload.line.take());
    drop(payload.tail.take());
    payload.answer = Some(MarkdownSoftLineResolveAnswer::Pending(reason));
    payload.stage = Stage::Finished; // 未解决阶段没有glyph Ready；旧字形不能跨过缺失字体或词典阶段。
}
fn safe_width(
    payload: &Payload,
    provider: &mut dyn ResolutionProvider,
    end: usize,
) -> Result<Option<i64>> {
    let start = match provider
        .cluster(
            &payload.seed.authority,
            &payload.seed.font,
            payload.seed.start,
        )
        .map_err(provider_error)?
    {
        MarkdownGlyphClusterAnswer::Known(Some(cluster))
            if cluster.source_utf16.start == payload.seed.start
                && (payload.seed.start == payload.seed.item.start
                    || cluster.flags & UNSAFE == 0) =>
        {
            cluster
        }
        _ => return Ok(None),
    };
    let edge = if end == payload.seed.item.end {
        match provider
            .cluster(&payload.seed.authority, &payload.seed.font, end - 1)
            .map_err(provider_error)?
        {
            MarkdownGlyphClusterAnswer::Known(Some(cluster)) if cluster.source_utf16.end == end => {
                cluster
                    .prefix_x_26_6
                    .checked_add(cluster.x_advance_26_6)
                    .ok_or(MarkdownSoftLineResolveError::Overflow)?
            }
            _ => return Ok(None),
        }
    } else {
        match provider
            .cluster(&payload.seed.authority, &payload.seed.font, end)
            .map_err(provider_error)?
        {
            MarkdownGlyphClusterAnswer::Known(Some(cluster))
                if cluster.source_utf16.start == end && cluster.flags & UNSAFE == 0 =>
            {
                cluster.prefix_x_26_6
            }
            _ => return Ok(None),
        }
    };
    Ok(Some(
        edge.checked_sub(start.prefix_x_26_6)
            .ok_or(MarkdownSoftLineResolveError::Overflow)?,
    ))
}
fn start_current(
    payload: &mut Payload,
    provider: &mut dyn ResolutionProvider,
    resources: &Arc<ResourceFacts>,
    final_prefix: bool,
) -> Result<usize> {
    let mut job = provider.reshape_subrange(
        &payload.seed.authority,
        &payload.seed.font,
        payload.seed.start..payload.candidate,
    )?;
    // 构造成功且尚未 ingest；原 Job 的实读总数此刻只含本次真实相邻上下文。
    // 失败沿原错误立即结束，不能拿请求范围或固定 halo 大小补造成功工作量。
    let context_bytes = job.profile()?.source_bytes_read;
    let status = job.status();
    payload.current = Some(OwnedShape::new(job, resources)?);
    payload.final_prefix = final_prefix;
    payload.stage = if status == MarkdownShapingStatus::Ingesting {
        Stage::Input
    } else {
        Stage::Shape
    };
    Ok(context_bytes)
}
fn accept_current(
    payload: &mut Payload,
    provider: &mut dyn ResolutionProvider,
    resources: &Arc<ResourceFacts>,
) -> Result<usize> {
    if payload.final_prefix {
        if payload.expected_width != Some(payload.scan.width) {
            return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
        }
        payload.line = payload.current.take();
        payload.line_facts = payload.scan;
        return start_tail(payload, provider, resources);
    }
    let fit = payload.scan.width <= payload.seed.width;
    if fit {
        payload.best = Some(Fit {
            end: payload.candidate,
            width: payload.scan.width,
            shape: payload.current.take(),
            facts: Some(payload.scan),
        });
        if matches!(payload.mode, Mode::Exact) || payload.candidate == payload.limit {
            return choose_best(payload, provider, resources);
        }
        payload.stage = Stage::Choose;
    } else if matches!(payload.mode, Mode::Exact) {
        drop(payload.current.take());
        payload.mode = Mode::Emergency;
        payload.next = payload.seed.start;
        payload.stage = Stage::Choose; // 实际重排超宽后重新按真实GCB搜索，不沿原估计签行。
    } else if payload.best.is_some() {
        drop(payload.current.take());
        return choose_best(payload, provider, resources);
    } else {
        payload.kind = MarkdownSoftLineResolvedKind::OverflowingGrapheme;
        payload.best = Some(Fit {
            end: payload.candidate,
            width: payload.scan.width,
            shape: payload.current.take(),
            facts: Some(payload.scan),
        });
        // 第一整个 GCB 也超宽时保留完整字素；若启动新 Job，原样交回它的真实上下文费用。
        return choose_best(payload, provider, resources);
    }
    Ok(0) // 只移交已有候选或改变选择阶段，没有新 Job，也没有额外上下文读取。
}
fn choose_best(
    payload: &mut Payload,
    provider: &mut dyn ResolutionProvider,
    resources: &Arc<ResourceFacts>,
) -> Result<usize> {
    let mut best = payload
        .best
        .take()
        .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
    payload.selected_end = best.end;
    if payload.kind != MarkdownSoftLineResolvedKind::OverflowingGrapheme {
        payload.kind = if best.end == payload.limit {
            MarkdownSoftLineResolvedKind::Natural
        } else {
            MarkdownSoftLineResolvedKind::Emergency
        };
    }
    #[cfg(feature = "paged-safe-glyph-slices")]
    if payload.reuse_safe && best.shape.is_none() {
        payload.expected_width = Some(best.width);
        payload.stage = Stage::Reusable;
        return Ok(0); // 只移交原候选权限；wrapper 仍核真实 GCB/索引，不读取新的整形上下文。
    }
    match best.shape.take() {
        Some(shape) => {
            payload.line = Some(shape);
            payload.line_facts = best
                .facts
                .ok_or(MarkdownSoftLineResolveError::InconsistentCoverage)?;
            start_tail(payload, provider, resources)
        }
        None => {
            payload.candidate = best.end;
            payload.expected_width = Some(best.width);
            start_current(payload, provider, resources, true) // 最终安全候选也保存真实prefix，供后继逐glyph消费。
        }
    }
}
fn start_tail(
    payload: &mut Payload,
    provider: &mut dyn ResolutionProvider,
    resources: &Arc<ResourceFacts>,
) -> Result<usize> {
    #[cfg(feature = "paged-safe-glyph-slices")]
    if payload.defer_tail {
        if payload.line.is_none() || payload.tail.is_some() {
            return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
        }
        payload.stage = Stage::ShapedPrefix;
        return Ok(0); // 只交真实已整形 prefix；余文仍是私有源 cursor，没有新上下文读取。
    }
    if payload.selected_end == payload.seed.item.end {
        payload.scan = GlyphFacts::default();
        return finish(payload).map(|()| 0); // 真空余项只封存已有前缀，不创建 Job 或上下文。
    }
    let mut job = provider.reshape_subrange(
        &payload.seed.authority,
        &payload.seed.font,
        payload.selected_end..payload.seed.item.end,
    )?;
    // 余项 Job 的构造读取也属于当前步骤；后续 ingest 的 bytes/scalars 由其实际 step 返回。
    let context_bytes = job.profile()?.source_bytes_read;
    let status = job.status();
    payload.tail = Some(OwnedShape::new(job, resources)?);
    payload.stage = if status == MarkdownShapingStatus::Ingesting {
        Stage::TailInput
    } else {
        Stage::TailShape
    };
    Ok(context_bytes)
}
fn finish(payload: &mut Payload) -> Result<()> {
    if payload.selected_end <= payload.seed.start || payload.selected_end > payload.seed.item.end {
        return Err(MarkdownSoftLineResolveError::InconsistentCoverage);
    }
    payload.answer = Some(MarkdownSoftLineResolveAnswer::Resolved(
        MarkdownSoftLineResolvedBoundary {
            source_utf16: payload.seed.start..payload.selected_end,
            remainder_utf16: payload.selected_end..payload.seed.item.end,
            kind: payload.kind,
            x_advance_26_6: payload.line_facts.width,
            line_glyphs: payload.line_facts.count,
            remainder_glyphs: payload.scan.count,
            line_flags_union: payload.line_facts.flags,
            remainder_flags_union: payload.scan.flags,
        },
    ));
    payload.stage = Stage::Finished;
    Ok(())
}

#[cfg(test)]
mod tests;

/// 原Job适配不改变已有错误层次；Store专有首因仍保留在Measure::Stored中。
fn provider_error(error: MarkdownSoftLineMeasureError) -> MarkdownSoftLineResolveError {
    match error {
        MarkdownSoftLineMeasureError::Source(error) => error.into(),
        MarkdownSoftLineMeasureError::Shaping(error) => error.into(),
        MarkdownSoftLineMeasureError::Cluster(error) => error.into(),
        other => other.into(),
    }
}
