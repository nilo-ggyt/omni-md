//! 原同源字形item的有限断点测量；结果只属于宽度选择阶段，不是Native软行Ready。
//!
//! 顺序消费真实cluster与GCB∩UAX14机会，采用首个超宽机会之前的最后合格候选。
//! 有符号advance不单调，因此不对宽度做二分，也不跳过中间cluster。unsafe边界、缺字、
//! 词典和跨item会明确交还后继阶段；本模块不新建shaper、不设置窗口BOT/EOT。
use std::{
    ops::Range,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use thiserror::Error;
pub(crate) mod provider;
use provider::{GlyphProvider, GlyphProviderOrigin, JobGlyphProvider};
#[cfg(feature = "paged-shaping-frontier")]
mod frontier;

use crate::{
    MarkdownDocumentBufferError, MarkdownGlyphClusterAnswer, MarkdownGlyphClusterError,
    MarkdownGlyphClusterIndex, MarkdownLayoutBreakAnswer, MarkdownLayoutBreakCursor,
    MarkdownLayoutBreakError, MarkdownLayoutBreakJob, MarkdownLayoutBreakQuery,
    MarkdownLayoutBreakUnit, MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownLayoutTextSourceObserver, MarkdownLineBreakBoundary,
    MarkdownLineBreakKind, MarkdownOriginalBackingObserver, MarkdownShapingDirection,
    MarkdownShapingError, MarkdownShapingJob,
};

pub const MARKDOWN_SOFT_LINE_BREAK_QUERIES: usize = 64;
pub const MARKDOWN_SOFT_LINE_CLUSTERS: usize = 256;
const UNSAFE_TO_BREAK: u32 = harfrust::GlyphFlags::UNSAFE_TO_BREAK.to_bits();

#[derive(Clone, Default)]
pub struct MarkdownSoftLineMeasureCancellation(Arc<AtomicBool>);
impl MarkdownSoftLineMeasureCancellation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    /// 唯一Store只接同一个取消请求，不能以当前同为false冒充同一长期撤权位。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn same_request(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// 行首必须来自已选真实机会；本request不表达新的段落或shaper上下文。
#[derive(Clone)]
pub struct MarkdownSoftLineMeasureRequest {
    pub start_utf16: usize,
    pub width_26_6: i64,
    pub cancellation: MarkdownSoftLineMeasureCancellation,
}

#[derive(Debug, Error)]
pub enum MarkdownSoftLineMeasureError {
    #[error("软行测量源权限不匹配")]
    SourceMismatch,
    #[error("软行测量不属于原字形任务")]
    JobMismatch,
    #[error("软行测量范围、方向或配额无效")]
    InvalidRequest,
    #[error("软行测量坐标或advance溢出")]
    Overflow,
    #[error("软行机会与原字形覆盖不一致")]
    InconsistentCoverage,
    #[error("软行测量已取消")]
    Cancelled,
    #[error("软行测量已永久撤权")]
    Poisoned,
    #[error("软行原源已失败")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("软行原字形任务失败")]
    Shaping(#[from] MarkdownShapingError),
    #[error("软行cluster索引失败")]
    Cluster(#[from] MarkdownGlyphClusterError),
    #[error("软行断点索引失败")]
    Break(#[from] MarkdownLayoutBreakError),
    #[cfg(feature = "paged-line-compose")]
    #[error("已认证的存储字形来源失败")]
    Stored(#[source] Box<crate::MarkdownLineComposeError>),
}
type Result<T> = std::result::Result<T, MarkdownSoftLineMeasureError>;

/// 两种索引访问分别限额；一次断点查询只允许原交集检查一个实际候选。
#[derive(Clone, Copy, Debug)]
pub struct MarkdownSoftLineMeasureBudget {
    pub break_queries: usize,
    pub clusters: usize,
}
impl Default for MarkdownSoftLineMeasureBudget {
    fn default() -> Self {
        Self {
            break_queries: MARKDOWN_SOFT_LINE_BREAK_QUERIES,
            clusters: MARKDOWN_SOFT_LINE_CLUSTERS,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSoftLineMeasuredBreak {
    pub source_utf16: Range<usize>,
    pub x_advance_26_6: i64,
    pub boundary: MarkdownLineBreakBoundary,
    /// 位于候选后继cluster起点的原flags；不能把已选前缀中其他glyph的flags混入此值。
    pub boundary_flags: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineReshapeReason {
    LineStartInsideCluster,
    BoundaryInsideCluster,
    UnsafeToBreak,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineMeasureChoice {
    MeasuredBreak(MarkdownSoftLineMeasuredBreak),
    NeedsReshape {
        source_utf16: Range<usize>,
        boundary_utf16: usize,
        boundary_cluster_utf16: Range<usize>,
        boundary_flags: u32,
        measured_prefix_26_6: i64,
        full_width_26_6: Option<i64>,
        reason: MarkdownSoftLineReshapeReason,
    },
    NeedsFontFallback {
        cluster_utf16: Range<usize>,
        missing_glyphs: usize,
    },
    NeedsDictionary(MarkdownLineBreakBoundary),
    NeedsEmergencyBreak(MarkdownSoftLineMeasuredBreak),
    /// 真实认证前缀已经超宽；word_end仅是原已闭合词项，不是当前glyphReady范围或UAX机会。
    #[cfg(feature = "paged-shaping-frontier")]
    NeedsFrontierEmergencyPrefix {
        source_utf16: Range<usize>,
        word_end_utf16: usize,
        measured_prefix_26_6: i64,
    },
    /// 原状态机逐GCB和真实后继cluster flags确认的安全紧急切片；没有UAX自然机会。
    #[cfg(feature = "paged-shaping-frontier")]
    MeasuredEmergencyBreak {
        source_utf16: Range<usize>,
        x_advance_26_6: i64,
        boundary_flags: u32,
        overflowing_grapheme: bool,
    },
    NeedNextItem {
        source_utf16: Range<usize>,
        x_advance_26_6: i64,
        last_fitting: Option<MarkdownSoftLineMeasuredBreak>,
    },
    EmptySource,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownSoftLineMeasureAnswer {
    PendingDependencies,
    Yielded,
    Selected(MarkdownSoftLineMeasureChoice),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSoftLineMeasureStep {
    pub break_queries: usize,
    pub clusters: usize,
    pub answer: MarkdownSoftLineMeasureAnswer,
}

struct Fit {
    measured: MarkdownSoftLineMeasuredBreak,
    boundary_cluster: Range<usize>,
}
struct Payload {
    source: MarkdownLayoutTextSource,
    start_cursor: Option<MarkdownLayoutBreakCursor>,
    cursor: Option<MarkdownLayoutBreakCursor>,
    candidate: Option<MarkdownLineBreakBoundary>,
    next_query: usize,
    scan: usize,
    advance: i64,
    fitting: Option<Fit>,
    selected: Option<MarkdownSoftLineMeasureChoice>,
    #[cfg(feature = "paged-shaping-frontier")]
    word_probe: Option<frontier::WordClusterProbe>,
    #[cfg(feature = "paged-shaping-frontier")]
    word_safe: Option<frontier::WordSafeState>,
}

/// 只能由原测量内部选择派生；公开Choice本身不是重排权限，不能由调用方拼字段冒充。
#[cfg(feature = "paged-soft-line-resolve")]
pub(crate) struct MarkdownSoftLineResolveSeed {
    pub source: MarkdownLayoutTextSource,
    pub authority: MarkdownLayoutTextAuthority,
    pub font: MarkdownLayoutFontAuthority,
    pub owner: Weak<()>,
    pub origin: GlyphProviderOrigin,
    #[cfg(feature = "paged-safe-glyph-slices")]
    pub revoked: Arc<AtomicBool>,
    pub item: Range<usize>,
    pub start: usize,
    pub width: i64,
    pub choice: MarkdownSoftLineMeasureChoice,
    pub cancellation: MarkdownSoftLineMeasureCancellation,
}

/// 不保原ShapeJob、cluster/断点索引或Font强边；每次调用仍借回原实际owner核验。
pub struct MarkdownSoftLineMeasureJob {
    #[cfg(feature = "paged-soft-line-resolve")]
    identity: Arc<()>,
    #[cfg(feature = "paged-safe-glyph-slices")]
    proof_revoked: Arc<AtomicBool>,
    #[cfg(feature = "paged-safe-glyph-slices")]
    line_alive: Arc<AtomicBool>,
    payload: Option<Payload>,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    origin: GlyphProviderOrigin,
    generation: u64,
    direction: MarkdownShapingDirection,
    backing: Option<MarkdownOriginalBackingObserver>,
    glyph_count: usize,
    item: Range<usize>,
    start: usize,
    width: i64,
    cancellation: MarkdownSoftLineMeasureCancellation,
    cancelled: bool,
    resources: MarkdownLayoutTextSourceObserver,
    #[cfg(feature = "paged-line-compose")]
    start_issued: std::cell::Cell<bool>,
}
impl MarkdownSoftLineMeasureJob {
    /// 只消费上一真实LineEnd提交后派生的单次行首票；不把普通GCB或公开Range当成UAX机会。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn after_selected_start(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        provider: &mut dyn GlyphProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        request: MarkdownSoftLineMeasureRequest,
        proof: provider::SelectedLineStart,
    ) -> Result<Self> {
        proof.require(
            &source,
            authority,
            font,
            provider,
            request.start_utf16,
            &request.cancellation,
        )?;
        let mut job = Self::new_with_provider(source, authority, font, provider, breaks, request)?;
        let payload = job
            .payload
            .as_mut()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
        // 原new的实际Source/Font/Ready、标量端点、代次和取消门全部保留。只有已经真实
        // 选择且写入同Store末尾LineEnd的GCB可跳过“必须为自然机会”的检查；票已按值消费。
        payload.source.health()?;
        payload.start_cursor = None;
        Ok(job)
    }

    /// 只接收解析阶段私有构造的真实GCB行首；不把紧急换行写成原UAX14允许机会。
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn after_resolved_start(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        shape: &mut MarkdownShapingJob,
        breaks: &mut MarkdownLayoutBreakJob,
        request: MarkdownSoftLineMeasureRequest,
        proof: &crate::markdown_soft_line_resolve::MarkdownResolvedLineStart,
    ) -> Result<Self> {
        proof.require(&source, authority, font, shape, breaks, request.start_utf16)?;
        let mut job = Self::new(source, authority, font, shape, breaks, request)?;
        job.payload
            .as_mut()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
            .start_cursor = None;
        // new的端点/方向/实际Ready检查保留；唯一跳过的是原Unicode机会检查，因为这里只
        // 接受已经实际解析、完整GCB且同实际tail job的sealed proof，不接受外部坐标或bool。
        Ok(job)
    }

    /// 绑定已经真实Ready的水平item；不会为有限窗口创建新run或推断段落边界。
    ///
    /// # Errors
    /// 非原Source/Font、非标量端点、空item伪尾部、非水平item或无效宽度均明确拒绝。
    pub fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        shape: &mut MarkdownShapingJob,
        breaks: &mut MarkdownLayoutBreakJob,
        request: MarkdownSoftLineMeasureRequest,
    ) -> Result<Self> {
        Self::new_with_provider(
            source,
            authority,
            font,
            &mut JobGlyphProvider::new(shape, None),
            breaks,
            request,
        )
    }

    /// 封闭Store适配器只借已经实际签入的数据；外部crate不能从Range实现provider。
    pub(crate) fn new_with_provider(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        provider: &mut dyn GlyphProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        request: MarkdownSoftLineMeasureRequest,
    ) -> Result<Self> {
        let MarkdownSoftLineMeasureRequest {
            start_utf16,
            width_26_6,
            cancellation,
        } = request;
        source.health()?;
        if !source.permits(authority) {
            return Err(MarkdownSoftLineMeasureError::SourceMismatch);
        }
        if cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        let profile = provider.require(authority, font)?; // 实际Source/Font/EOF门，Stored不伪造Job。
        breaks.require_source(authority)?;
        if profile.generation != source.generation() {
            return Err(MarkdownSoftLineMeasureError::SourceMismatch);
        }
        let item = profile.source_utf16.clone();
        if width_26_6 <= 0
            || !matches!(
                profile.direction,
                MarkdownShapingDirection::LeftToRight | MarkdownShapingDirection::RightToLeft
            )
            || start_utf16 < item.start
            || start_utf16 > item.end
            || (start_utf16 == item.end && source.utf16_len() != 0)
        {
            return Err(MarkdownSoftLineMeasureError::InvalidRequest);
        }
        source.byte_offset_for_utf16(start_utf16)?; // 低代理项不是行首；不能用长度元数据伪造合法标量端点。
        let start_cursor = if start_utf16 == 0 {
            None // 真文档起点是行首；不是任意piece或shaper窗口的起点。
        } else {
            Some(breaks.cursor(
                MarkdownLayoutBreakUnit::Utf16,
                start_utf16,
                MarkdownLayoutBreakQuery::At,
            )?)
        };
        let resources = source.resource_observer();
        let backing = source.original_backing_observer();
        source.health()?;
        if cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        Ok(Self {
            #[cfg(feature = "paged-soft-line-resolve")]
            identity: Arc::new(()),
            #[cfg(feature = "paged-safe-glyph-slices")]
            proof_revoked: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "paged-safe-glyph-slices")]
            line_alive: Arc::new(AtomicBool::new(true)),
            payload: Some(Payload {
                source,
                start_cursor,
                cursor: None,
                candidate: None,
                next_query: start_utf16,
                scan: start_utf16,
                advance: 0,
                fitting: None,
                selected: None,
                #[cfg(feature = "paged-shaping-frontier")]
                word_probe: None,
                #[cfg(feature = "paged-shaping-frontier")]
                word_safe: None,
            }),
            source: authority.clone(),
            font: font.clone(),
            origin: profile.origin,
            generation: profile.generation,
            direction: profile.direction,
            backing,
            glyph_count: profile.glyphs,
            item,
            start: start_utf16,
            width: width_26_6,
            cancellation,
            cancelled: false,
            resources,
            #[cfg(feature = "paged-line-compose")]
            start_issued: std::cell::Cell::new(false),
        })
    }

    fn guard_provider(&self, provider: &mut dyn GlyphProvider) -> Result<()> {
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        if let Some(payload) = &self.payload {
            payload.source.health()?;
        }
        if self.origin != provider.origin() {
            return Err(MarkdownSoftLineMeasureError::JobMismatch);
        } // 先核原opaque来源，避免外来Job字体错误抢走原身份错误；Source真实errno已在前面检查。
        let facts = provider.require(&self.source, &self.font)?;
        if self.origin != facts.origin
            || self.item != facts.source_utf16
            || self.glyph_count != facts.glyphs
            || self.generation != facts.generation
            || self.direction != facts.direction
        {
            return Err(MarkdownSoftLineMeasureError::JobMismatch);
        }
        if self.cancelled || self.cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        if self.payload.is_none() {
            return Err(MarkdownSoftLineMeasureError::Poisoned);
        }
        Ok(()) // 身份/全部事实与实际健康门均要匹配，不能从旧目录长度或同名字体重新签票。
    }
    fn fail(&mut self, error: &MarkdownSoftLineMeasureError) {
        #[cfg(feature = "paged-safe-glyph-slices")]
        {
            self.proof_revoked.store(true, Ordering::Release);
            self.line_alive.store(false, Ordering::Release);
        } // 失败同时撤销当前item与整行历史收据；合法continue只撤当前item票。
        self.cancelled |= matches!(error, MarkdownSoftLineMeasureError::Cancelled);
        drop(self.payload.take()); // 本层仅有一个Source强claim；失败即释放，不藏在返回结果或cursor中。
    }

    /// 由同一个真实已选结果签发下一阶段；未选择、取消或原job失权不能生成新seed。
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn resolve_seed(
        &self,
        shape: &mut MarkdownShapingJob,
    ) -> Result<MarkdownSoftLineResolveSeed> {
        self.resolve_seed_provider(&mut JobGlyphProvider::new(shape, None))
    }

    /// 共用内部选项复制；调用者仍须持原provider，不能把相同字段的新目录升级成原请求。
    #[cfg(feature = "paged-soft-line-resolve")]
    fn resolve_seed_provider(
        &self,
        provider: &mut dyn GlyphProvider,
    ) -> Result<MarkdownSoftLineResolveSeed> {
        self.guard_provider(provider)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
        let choice = payload
            .selected
            .clone()
            .ok_or(MarkdownSoftLineMeasureError::InvalidRequest)?;
        Ok(MarkdownSoftLineResolveSeed {
            source: payload.source.clone(),
            authority: self.source.clone(),
            font: self.font.clone(),
            owner: Arc::downgrade(&self.identity),
            origin: self.origin.clone(),
            #[cfg(feature = "paged-safe-glyph-slices")]
            revoked: Arc::clone(&self.proof_revoked),
            item: self.item.clone(),
            start: self.start,
            width: self.width,
            choice,
            cancellation: self.cancellation.clone(),
        })
    }

    /// 每次解析操作重新借原Measure与Shape核验；同字节、同宽度的新测量也不能替换nonce。
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn require_resolve(
        &self,
        owner: &Weak<()>,
        shape: &mut MarkdownShapingJob,
    ) -> Result<()> {
        self.require_resolve_provider(owner, &mut JobGlyphProvider::new(shape, None))
    }

    /// Job与Store走同一Source先行、nonce、真实provider门；不会重签原output身份。
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn require_resolve_provider(
        &self,
        owner: &Weak<()>,
        provider: &mut dyn GlyphProvider,
    ) -> Result<()> {
        if let Some(payload) = &self.payload {
            payload.source.health()?; // 原IO错误优先，不能被外来nonce或取消的表层错误盖住。
        }
        if !Weak::ptr_eq(owner, &Arc::downgrade(&self.identity)) {
            return Err(MarkdownSoftLineMeasureError::JobMismatch);
        }
        self.guard_provider(provider)
    }

    /// 只有原内部unsafe或紧急选择能交给受控重排；安全已选行继续走既有selected许可。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn resolution_permit<'a>(
        &'a self,
        provider: &mut dyn GlyphProvider,
    ) -> Result<provider::ResolutionMeasurePermit<'a>> {
        self.guard_provider(provider)?;
        self.require_resolution()?;
        self.require_live_line()?;
        Ok(provider::ResolutionMeasurePermit::new(self))
    }

    #[cfg(feature = "paged-line-compose")]
    fn require_resolution(&self) -> Result<()> {
        match self
            .payload
            .as_ref()
            .and_then(|payload| payload.selected.as_ref())
        {
            Some(
                MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(_)
                | MarkdownSoftLineMeasureChoice::NeedsReshape { .. },
            ) => Ok(()),
            #[cfg(feature = "paged-shaping-frontier")]
            Some(MarkdownSoftLineMeasureChoice::NeedsFrontierEmergencyPrefix { .. }) => Ok(()),
            _ => Err(MarkdownSoftLineMeasureError::InvalidRequest),
        } // 字体、词典、未完成和NeedNextItem都不能借该入口变成已解决的软行。
    }

    /// 在明确请求后继item时继续原软行；方向、字体或样式的边界不会重置原累计宽度。
    ///
    /// 两个真实Ready任务必须在同一Source上严格相邻。原任务只借本次核验，成功后不再
    /// 保活它；跨item的glyph/spool仍由上层实际owner保存，本方法不伪造绘制或整行Ready。
    ///
    /// # Errors
    /// 非原前项、缺口、重叠、外来Source/Font、非水平item或错误阶段均永久撤销本测量。
    pub fn continue_item(
        &mut self,
        previous: &mut MarkdownShapingJob,
        next: &mut MarkdownShapingJob,
        next_font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
    ) -> Result<()> {
        self.prepare_continue(&mut JobGlyphProvider::new(previous, None))?
            .continue_with(&mut JobGlyphProvider::new(next, None), next_font, breaks)
    }

    /// 先核真实前项，再独占借本Measure。令牌不借provider，因而上层可先归还同一个
    /// Store的旧item借用，再借后项；不需要两个同时可变Store引用或共享锁。
    pub(crate) fn prepare_continue(
        &mut self,
        previous: &mut dyn GlyphProvider,
    ) -> Result<provider::PreparedMeasureContinuation<'_>> {
        let result = self
            .guard_provider(previous)
            .and_then(|()| self.require_next_item());
        if let Err(error) = result {
            self.fail(&error);
            return Err(error);
        }
        Ok(provider::PreparedMeasureContinuation::new(self))
    }

    /// 该阶段检查由临时permit与交接共用；公开Choice的Copy字段不能作为消费前项的证明。
    fn require_next_item(&self) -> Result<()> {
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
        match &payload.selected {
            Some(MarkdownSoftLineMeasureChoice::NeedNextItem {
                source_utf16,
                x_advance_26_6,
                ..
            }) if source_utf16 == &(self.start..self.item.end)
                && *x_advance_26_6 == payload.advance
                && payload.scan == self.item.end
                && !self.item.is_empty() =>
            {
                Ok(())
            }
            _ => Err(MarkdownSoftLineMeasureError::InvalidRequest),
        } // 未测完、最终机会、重形和缺字阶段均不能被后项覆盖。
    }

    fn continue_prepared(
        &mut self,
        next: &mut dyn GlyphProvider,
        next_font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
    ) -> Result<()> {
        let result = (|| {
            // prepare之后仅令牌独占借本状态；仍检查真正Source健康与取消，不能让等待
            // 下一次Store借用期间出现的IO失败被合法相邻范围掩盖。
            self.require_live_line()?;
            self.require_next_item()?;
            breaks.require_source(&self.source)?;
            let profile = next.require(&self.source, next_font)?;
            if !self.origin.same_provider_family(&profile.origin) {
                return Err(MarkdownSoftLineMeasureError::JobMismatch);
            } // Store目录只在原同一个Store内交接；同源的第二文件不是已认证历史的延续。
            let range = profile.source_utf16.clone();
            let source_len = self
                .payload
                .as_ref()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
                .source
                .utf16_len();
            if range.start != self.item.end
                || range.is_empty()
                || range.end > source_len
                || profile.generation != self.generation
                || !matches!(
                    profile.direction,
                    MarkdownShapingDirection::LeftToRight | MarkdownShapingDirection::RightToLeft
                )
            {
                return Err(MarkdownSoftLineMeasureError::InvalidRequest);
            } // 真实全局相邻坐标不得有缺口或重叠，item末尾仍不等于段落EOF。
            self.require_live_line()?;
            #[cfg(feature = "paged-safe-glyph-slices")]
            {
                self.proof_revoked.store(true, Ordering::Release);
                self.proof_revoked = Arc::new(AtomicBool::new(false));
            } // 只撤当前item票，真正整行scope保留；失败或Drop才撤整行。
            self.font = next_font.clone();
            self.origin = profile.origin;
            self.glyph_count = profile.glyphs;
            self.direction = profile.direction;
            self.item = range;
            #[cfg(feature = "paged-shaping-frontier")]
            {
                self.payload
                    .as_mut()
                    .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
                    .word_probe = None;
            }
            self.payload
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
                .selected = None;
            // cursor、候选、累计advance及last fitting不变。接缝的真实后继cluster flags
            // 仍在下一step中读取，不把item切分位置当安全断点。
            self.guard_provider(next)
        })();
        let result = match self.require_live_line() {
            Err(MarkdownSoftLineMeasureError::Source(error)) => Err(error.into()),
            _ => result,
        };
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    /// 只核本行已有源与纯取消标量；不能用此方法代替provider对Font/EOF的真实核验。
    fn require_live_line(&self) -> Result<()> {
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        self.payload
            .as_ref()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?
            .source
            .health()?;
        if self.cancelled || self.cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        Ok(())
    }

    /// 本临时许可只能在实际NeedNextItem签发，且借住原Measure直到producer消费完。
    #[cfg(feature = "paged-safe-glyph-slices")]
    pub(crate) fn measured_item_permit<'a>(
        &'a self,
        provider: &mut dyn GlyphProvider,
    ) -> Result<provider::MeasuredItemPermit<'a>> {
        self.guard_provider(provider)?;
        self.require_next_item()?;
        Ok(provider::MeasuredItemPermit::new(self))
    }

    /// 仅原状态机实际选中的安全自然/紧急断点可签临时选择票；公开Choice副本不能重建权限。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn selected_permit<'a>(
        &'a self,
        provider: &mut dyn GlyphProvider,
    ) -> Result<provider::SelectedMeasurePermit<'a>> {
        self.guard_provider(provider)?;
        self.require_selected()?;
        Ok(provider::SelectedMeasurePermit::new(self))
    }

    #[cfg(feature = "paged-line-compose")]
    fn require_selected(&self) -> Result<provider::SelectedMeasureSelection> {
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
        match payload.selected.as_ref() {
            Some(MarkdownSoftLineMeasureChoice::MeasuredBreak(selected))
                if selected.source_utf16.start == self.start
                    && selected.source_utf16.end > self.start
                    && selected.source_utf16.end <= self.item.end
                    && selected.boundary.utf16 == selected.source_utf16.end
                    && selected.boundary_flags & UNSAFE_TO_BREAK == 0 =>
            {
                Ok(provider::SelectedMeasureSelection {
                    source_utf16: selected.source_utf16.clone(),
                    x_advance_26_6: selected.x_advance_26_6,
                    boundary_flags: selected.boundary_flags,
                    kind: provider::SelectedMeasureKind::Natural(selected.boundary),
                })
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Some(MarkdownSoftLineMeasureChoice::MeasuredEmergencyBreak {
                source_utf16,
                x_advance_26_6,
                boundary_flags,
                overflowing_grapheme,
            }) if source_utf16.start == self.start
                && source_utf16.end > self.start
                && source_utf16.end <= self.item.end
                && boundary_flags & UNSAFE_TO_BREAK == 0 =>
            {
                Ok(provider::SelectedMeasureSelection {
                    source_utf16: source_utf16.clone(),
                    x_advance_26_6: *x_advance_26_6,
                    boundary_flags: *boundary_flags,
                    kind: provider::SelectedMeasureKind::Emergency {
                        overflowing_grapheme: *overflowing_grapheme,
                    },
                })
            }
            _ => Err(MarkdownSoftLineMeasureError::InvalidRequest),
        } // 缺字、词典、跨item未决、unsafe和内部cluster均不因已有宽度而成为安全行尾。
    }

    /// 固定量子只读原索引；Pending要求原依赖真正推进，Yielded保留本任务实际扫描位置。
    pub fn step(
        &mut self,
        shape: &mut MarkdownShapingJob,
        clusters: &mut MarkdownGlyphClusterIndex,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineMeasureBudget,
    ) -> Result<MarkdownSoftLineMeasureStep> {
        self.step_with_provider(
            &mut JobGlyphProvider::new(shape, Some(clusters)),
            breaks,
            budget,
        )
    }
    pub(crate) fn step_with_provider(
        &mut self,
        provider: &mut dyn GlyphProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineMeasureBudget,
    ) -> Result<MarkdownSoftLineMeasureStep> {
        let result = self.advance(provider, breaks, budget);
        let result = match self.payload.as_ref().map(|payload| payload.source.health()) {
            Some(Err(error)) => Err(error.into()), // 真实共享源错误优先于本次metadata/取消出口。
            _ => result.and_then(|value| {
                self.guard_provider(provider)?;
                Ok(value)
            }),
        };
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    fn advance(
        &mut self,
        provider: &mut dyn GlyphProvider,
        breaks: &mut MarkdownLayoutBreakJob,
        budget: MarkdownSoftLineMeasureBudget,
    ) -> Result<MarkdownSoftLineMeasureStep> {
        self.guard_provider(provider)?;
        breaks.require_source(&self.source)?;
        if budget.break_queries == 0 || budget.clusters == 0 {
            return Err(MarkdownSoftLineMeasureError::InvalidRequest);
        }
        let mut step = MarkdownSoftLineMeasureStep {
            break_queries: 0,
            clusters: 0,
            answer: MarkdownSoftLineMeasureAnswer::Yielded,
        };
        loop {
            self.guard_provider(provider)?; // 每个固定记录操作前检查取消；不等整个机会或长cluster结束才撤权。
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            if let Some(choice) = &payload.selected {
                step.answer = MarkdownSoftLineMeasureAnswer::Selected(choice.clone());
                return Ok(step);
            }
            if payload.source.utf16_len() == 0 {
                payload.selected = Some(MarkdownSoftLineMeasureChoice::EmptySource);
                continue; // 仅真空源，不把任意空item或未探索前缀包装成空文档。
            }
            if let Some(cursor) = &mut payload.start_cursor {
                if step.break_queries == budget.break_queries.min(MARKDOWN_SOFT_LINE_BREAK_QUERIES)
                {
                    return Ok(step);
                }
                step.break_queries += 1;
                match breaks.query(cursor, 1)? {
                    MarkdownLayoutBreakAnswer::PendingSource => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(step);
                    }
                    MarkdownLayoutBreakAnswer::Yielded => continue,
                    MarkdownLayoutBreakAnswer::Known(Some(_)) => payload.start_cursor = None,
                    MarkdownLayoutBreakAnswer::Known(None) => {
                        return Err(MarkdownSoftLineMeasureError::InvalidRequest);
                    }
                } // 非零起点必须是实际已确认机会，不能把script/font/window边界当已选行首。
                continue;
            }
            #[cfg(feature = "paged-shaping-frontier")]
            if self.advance_word_frontier(provider, breaks, budget, &mut step)? {
                return Ok(step);
            }
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownSoftLineMeasureError::Poisoned)?;
            if payload.candidate.is_none() {
                if step.break_queries == budget.break_queries.min(MARKDOWN_SOFT_LINE_BREAK_QUERIES)
                {
                    return Ok(step);
                }
                if payload.cursor.is_none() {
                    payload.cursor = Some(breaks.cursor(
                        MarkdownLayoutBreakUnit::Utf16,
                        payload.next_query,
                        MarkdownLayoutBreakQuery::Next,
                    )?);
                }
                step.break_queries += 1;
                match breaks.query(
                    payload
                        .cursor
                        .as_mut()
                        .ok_or(MarkdownSoftLineMeasureError::InconsistentCoverage)?,
                    1,
                )? {
                    MarkdownLayoutBreakAnswer::PendingSource => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(step);
                    }
                    MarkdownLayoutBreakAnswer::Yielded => continue,
                    MarkdownLayoutBreakAnswer::Known(Some(candidate)) => {
                        if candidate.utf16 <= payload.next_query
                            || candidate.utf16 > payload.source.utf16_len()
                        {
                            return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                        }
                        payload.cursor = None;
                        payload.candidate = Some(candidate);
                    }
                    MarkdownLayoutBreakAnswer::Known(None) => {
                        return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                    }
                }
                continue;
            }
            let candidate = payload
                .candidate
                .ok_or(MarkdownSoftLineMeasureError::InconsistentCoverage)?;
            if candidate.needs_dictionary {
                payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedsDictionary(candidate));
                continue; // 默认SA机会不是字典已完成的证明，不能继续签一条完整软行。
            }
            let target = candidate.utf16.min(self.item.end);
            if step.clusters == budget.clusters.min(MARKDOWN_SOFT_LINE_CLUSTERS) {
                return Ok(step);
            }
            if payload.scan < target {
                step.clusters += 1;
                let value = match provider.cluster(&self.source, &self.font, payload.scan)? {
                    MarkdownGlyphClusterAnswer::Pending => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(step);
                    }
                    MarkdownGlyphClusterAnswer::Known(Some(value)) => value,
                    MarkdownGlyphClusterAnswer::Known(None) => {
                        return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                    }
                };
                if value.source_utf16.start > payload.scan
                    || value.source_utf16.end <= payload.scan
                    || value.source_utf16.end > self.item.end
                {
                    return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
                } // 真实索引有洞是数据错误，不能伪装成一个可重形的普通边界。
                if value.source_utf16.start != payload.scan || value.source_utf16.end > target {
                    let at_start = value.source_utf16.start < payload.scan;
                    payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedsReshape {
                        source_utf16: self.start..target,
                        boundary_utf16: if at_start { self.start } else { target },
                        boundary_cluster_utf16: value.source_utf16,
                        boundary_flags: value.flags,
                        measured_prefix_26_6: payload.advance,
                        full_width_26_6: None,
                        reason: if at_start {
                            MarkdownSoftLineReshapeReason::LineStartInsideCluster
                        } else {
                            MarkdownSoftLineReshapeReason::BoundaryInsideCluster
                        },
                    });
                    continue; // 真实机会在HB cluster内部，已有聚合宽度不是完整候选宽度。
                }
                if value.missing_glyphs > 0 {
                    payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedsFontFallback {
                        cluster_utf16: value.source_utf16,
                        missing_glyphs: value.missing_glyphs,
                    });
                    continue; // glyph0的advance不能代替实际回退字体的宽度。
                }
                payload.advance = payload
                    .advance
                    .checked_add(value.x_advance_26_6)
                    .ok_or(MarkdownSoftLineMeasureError::Overflow)?;
                payload.scan = value.source_utf16.end;
                continue;
            }
            if candidate.utf16 > self.item.end
                || (candidate.utf16 == self.item.end && self.item.end != payload.source.utf16_len())
            {
                if payload.advance > self.width
                    && let Some(fit) = payload.fitting.take()
                {
                    payload.selected = Some(selected(fit));
                } else {
                    payload.selected = Some(MarkdownSoftLineMeasureChoice::NeedNextItem {
                        source_utf16: self.start..self.item.end,
                        x_advance_26_6: payload.advance,
                        last_fitting: payload.fitting.as_ref().map(|fit| fit.measured.clone()),
                    });
                }
                continue; // item结束不是段落EOF；没有后继flags和完整宽度就不签新断点。
            }
            let (flags, boundary_cluster) = if target < self.item.end {
                step.clusters += 1;
                match provider.cluster(&self.source, &self.font, target)? {
                    MarkdownGlyphClusterAnswer::Pending => {
                        step.answer = MarkdownSoftLineMeasureAnswer::PendingDependencies;
                        return Ok(step);
                    }
                    MarkdownGlyphClusterAnswer::Known(Some(value))
                        if value.source_utf16.start == target =>
                    {
                        (value.flags, value.source_utf16)
                    }
                    _ => return Err(MarkdownSoftLineMeasureError::InconsistentCoverage),
                }
            } else {
                (0, target..target) // 仅实际文档EOF；item末尾即使是mandatory也没有原后继flags证明。
            };
            let fit = Fit {
                measured: MarkdownSoftLineMeasuredBreak {
                    source_utf16: self.start..candidate.utf16,
                    x_advance_26_6: payload.advance,
                    boundary: candidate,
                    boundary_flags: flags,
                },
                boundary_cluster,
            };
            if payload.advance > self.width {
                payload.selected = Some(match payload.fitting.take() {
                    Some(previous) => selected(previous),
                    None => MarkdownSoftLineMeasureChoice::NeedsEmergencyBreak(fit.measured),
                }); // 不按字符强切长词或宣称一个超宽词已经适合窗口。
                continue;
            }
            if candidate.kind == MarkdownLineBreakKind::Mandatory {
                payload.selected = Some(selected(fit));
                continue;
            }
            payload.fitting = Some(fit);
            payload.next_query = candidate.utf16;
            payload.candidate = None; // 下一机会继续原逻辑前缀，不重读前面的cluster或正文。
        }
    }

    /// 仅观察同一Source payload的真实析构；不保活本job、外部索引或字体。
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownLayoutTextSourceObserver {
        self.resources.clone()
    }
    /// 取消只交还本层Source claim，不关闭由调用方独立拥有的原shaper与索引。
    pub fn close(self) {
        drop(self);
    }
}
#[cfg(feature = "paged-safe-glyph-slices")]
impl Drop for MarkdownSoftLineMeasureJob {
    fn drop(&mut self) {
        // 只撤销派生许可；不清空任何已封口spool或原不可变字形，也不冒称资源已经析构。
        self.proof_revoked.store(true, Ordering::Release);
        self.line_alive.store(false, Ordering::Release);
    }
}
fn selected(fit: Fit) -> MarkdownSoftLineMeasureChoice {
    if fit.measured.boundary_flags & UNSAFE_TO_BREAK != 0 {
        MarkdownSoftLineMeasureChoice::NeedsReshape {
            boundary_utf16: fit.measured.source_utf16.end,
            source_utf16: fit.measured.source_utf16,
            boundary_cluster_utf16: fit.boundary_cluster,
            boundary_flags: fit.measured.boundary_flags,
            measured_prefix_26_6: fit.measured.x_advance_26_6,
            full_width_26_6: Some(fit.measured.x_advance_26_6),
            reason: MarkdownSoftLineReshapeReason::UnsafeToBreak,
        }
    } else {
        MarkdownSoftLineMeasureChoice::MeasuredBreak(fit.measured)
    }
}

#[cfg(test)]
mod tests;
