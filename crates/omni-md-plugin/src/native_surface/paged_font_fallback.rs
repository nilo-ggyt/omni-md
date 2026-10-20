//! 一个真实GCB的实际整形回退；Native候选构造与纯Core worker试次分开持有。
//!
//! 本模块不创建线程。调用方将trial packet移入真实worker并join后，才在主线程消费结果。
//! 试次只捕获同一Source/CoreFont；!Send的ShapeJob在worker内部构造和析构，绝不跨线程移动。
use std::{ops::Range, rc::Rc, sync::Arc};

use objc2_foundation::MainThreadMarker;
use omni_markdown_core::{
    MARKDOWN_SHAPING_MAX_FEATURES, MarkdownDocumentBufferError, MarkdownGraphemeAnswer,
    MarkdownGraphemeError, MarkdownGraphemeJob, MarkdownGraphemeQuery, MarkdownGraphemeUnit,
    MarkdownLayoutFont, MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownOriginalBackingObserver, MarkdownScriptAnswer,
    MarkdownScriptError, MarkdownScriptJob, MarkdownScriptUnit, MarkdownShapingCancellation,
    MarkdownShapingError, MarkdownShapingIoError, MarkdownShapingOptions,
    MarkdownShapingResourceSnapshot,
};
use thiserror::Error;

use super::{
    paged_font_cascade::{
        NativeMarkdownFontCascade, NativeMarkdownFontCascadeBudget, NativeMarkdownFontCascadeError,
    },
    paged_fonts::{
        NativeMarkdownFontCapture, NativeMarkdownPagedFont, NativeMarkdownPagedFontError,
    },
    paged_line_fonts::NativeMarkdownLineFont,
};

// 这里只拦住Core原语言标签预算之外的复制；64字节以内的语法仍由Core唯一校验器决定。
// 该上界与MarkdownShapingOptions的既有语言预算一致，不截断标签或改成默认语言。
const MAX_TRIAL_LANGUAGE_BYTES: usize = 64;

#[cfg(feature = "native-paged-production")]
mod compose;
mod trial;
#[cfg(feature = "native-paged-production")]
pub use compose::NativeMarkdownProbedFontFallbackRequest;
pub use trial::{
    NATIVE_MARKDOWN_FONT_TRIAL_GLYPHS, NativeMarkdownFontTrialBudget,
    NativeMarkdownFontTrialInputProgress, NativeMarkdownFontTrialStatus,
    NativeMarkdownFontTrialStep, NativeMarkdownFontTrialWorker,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownFontOrigin {
    Base,
    /// 原 Base 已经完整试次缺字后，系统针对同 GCB 首 scalar 选择的真实优先候选。
    #[cfg(feature = "native-paged-production")]
    ForString,
    Cascade(usize),
}
#[derive(Clone)]
pub struct NativeMarkdownFontFallbackRequest {
    pub source_utf16: Range<usize>,
    /// 保留真实itemization决定的script/direction/style/BOT/EOT；不从本GCB或4KiB窗口重猜。
    pub options: MarkdownShapingOptions,
    pub languages: &'static [&'static str],
    pub cascade_budget: NativeMarkdownFontCascadeBudget,
    pub maximum_font_bytes: u64,
}
#[derive(Debug, Error)]
pub enum NativeMarkdownFontFallbackError {
    #[cfg(feature = "native-paged-production")]
    #[error("原组合器字体试次权限失效：{0}")]
    Compose(#[source] omni_markdown_core::MarkdownLineComposeError),
    #[error("原字体回退权限已撤销")]
    Revoked,
    #[error("字体回退已停止或仍有未完成试次")]
    InvalidState,
    #[error("字体回退源权限不匹配")]
    SourceMismatch,
    #[error("字体回退原字体权限不匹配")]
    FontMismatch,
    #[error("请求不是同脚本样式的一个完整字素")]
    InvalidCluster,
    #[error("字体试次与原签发身份不匹配")]
    TrialMismatch,
    #[error("字体试次被错误地放在主线程执行")]
    MainThreadShape,
    #[error("字体试次字形资源没有实际完整释放")]
    GlyphStoresNotReleased,
    #[error("字体试次字形分页首次I/O失败：{0:?}")]
    GlyphStoreIo(MarkdownShapingIoError),
    #[error("字体试次操作与资源归还同时失败：{cause}；另一失败：{cleanup}")]
    TrialCleanup {
        #[source]
        cause: Box<Self>,
        cleanup: Box<Self>,
    },
    #[error("拒绝字体的字节映射没有实际完整释放")]
    FontBytesNotReleased,
    #[error("字体回退计数溢出")]
    Overflow,
    #[error("冻结字体回退源已经失败")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("真实字素索引失败")]
    Grapheme(#[from] MarkdownGraphemeError),
    #[error("真实脚本索引失败")]
    Script(#[from] MarkdownScriptError),
    #[error("系统字体候选失败")]
    Cascade(#[from] NativeMarkdownFontCascadeError),
    #[error("系统字体候选{ordinal}无法捕获同源字体: {error}")]
    CandidateCapture {
        ordinal: usize,
        #[source]
        error: NativeMarkdownFontCascadeError,
    },
    #[cfg(feature = "native-paged-production")]
    #[error("系统首字符优先候选无法捕获同源字体：{0}")]
    ForStringCapture(#[source] NativeMarkdownPagedFontError),
    #[error("实际字体整形失败")]
    Shaping(#[from] MarkdownShapingError),
}
type Result<T> = std::result::Result<T, NativeMarkdownFontFallbackError>;

impl NativeMarkdownFontFallbackError {
    /// 仅同一个实际试次取消位已经置位时，精确旧权限/取消可以正常结束；不能扩大到任意Source。
    fn is_cancelled_retirement(&self, cancelled: bool) -> bool {
        if !cancelled {
            return false; // 未见本次取消事实时不把真实失败猜成用户已离开。
        }
        match self {
            Self::Revoked
            | Self::Shaping(MarkdownShapingError::Cancelled)
            | Self::Source(MarkdownDocumentBufferError::PreparedTextInputStale)
            | Self::Shaping(MarkdownShapingError::Source(
                MarkdownDocumentBufferError::PreparedTextInputStale,
            )) => true,
            #[cfg(feature = "native-paged-production")]
            Self::Compose(
                omni_markdown_core::MarkdownLineComposeError::Cancelled
                | omni_markdown_core::MarkdownLineComposeError::Source(
                    MarkdownDocumentBufferError::PreparedTextInputStale,
                ),
            ) => true,
            _ => false, // Poisoned、存储errno和其他Source错误继续失败，不允许任意兼容豁免。
        }
    }

    /// 只辨认链首实际原始文件errno；调用方还必须已证明两者属于同一个Result/原Source。
    fn original_backing_errno(&self) -> Option<i32> {
        match self {
            Self::Source(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno))
            | Self::Shaping(MarkdownShapingError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            )) => Some(*errno),
            #[cfg(feature = "native-paged-production")]
            Self::Compose(omni_markdown_core::MarkdownLineComposeError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
            )) => Some(*errno),
            Self::TrialCleanup { cause, .. } => cause.original_backing_errno(),
            _ => None, // 编辑存储或链中另一项错误不能冒充这个原文件observer的重复首因。
        }
    }

    /// 退休时只把真实存储errno提到取消/清理之前，旧Cell的权限失效不冒充磁盘故障。
    pub(in crate::native_surface) fn is_source_io(&self) -> bool {
        match self {
            Self::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
                | MarkdownDocumentBufferError::EditStorageUnavailable(_),
            )
            | Self::Shaping(MarkdownShapingError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
                | MarkdownDocumentBufferError::EditStorageUnavailable(_),
            )) => true,
            #[cfg(feature = "native-paged-production")]
            Self::Compose(omni_markdown_core::MarkdownLineComposeError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
                | MarkdownDocumentBufferError::EditStorageUnavailable(_),
            )) => true,
            Self::TrialCleanup { cause, cleanup } => cause.is_source_io() || cleanup.is_source_io(),
            _ => false, // 不检查字符串，也不把任意Source变体扩大为可忽略的正常撤权。
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeMarkdownFontGlyphFacts {
    pub glyph_count: usize,
    pub missing_glyphs: usize,
    pub flags_union: u32,
    pub input_scalars: usize,
    pub source_bytes_read: usize,
    pub source_borrow_max_bytes: usize,
}

/// 同一个不可变Native/Core字体的选择结果；它不携带字形数组或Source强边。
/// 相邻item必须由后继composer重新合并/整形，本结果不能冒充完整run或软行。
pub struct NativeMarkdownFontChoice {
    origin: NativeMarkdownFontOrigin,
    source_utf16: Range<usize>,
    facts: NativeMarkdownFontGlyphFacts,
    font: Option<NativeMarkdownPagedFont>,
    base: MarkdownLayoutFontAuthority,
    selected: MarkdownLayoutFontAuthority,
    source: MarkdownLayoutTextAuthority,
}
impl NativeMarkdownFontChoice {
    /// 只读原候选来源，不能修改结果内部的已认证身份。
    #[must_use]
    pub fn origin(&self) -> NativeMarkdownFontOrigin {
        self.origin
    }
    /// 返回原全局UTF16范围的副本；修改副本不会把字体证明移到别的正文。
    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        self.source_utf16.clone()
    }
    /// 真实整形后的固定事实；它不包含glyph数组或cmap推测。
    #[must_use]
    pub fn facts(&self) -> NativeMarkdownFontGlyphFacts {
        self.facts
    }
    /// 只借出实际通过试次的同一字体；Base结果继续要求调用方保留原base。
    pub fn font<'a>(
        &'a self,
        base: &'a NativeMarkdownPagedFont,
        source: &MarkdownLayoutTextSource,
    ) -> Result<&'a NativeMarkdownPagedFont> {
        // Choice 只能由原完整 worker 结果创建，两端已在同一不可变 Source 核准。
        // 消费时只需再次核真实健康与原 authority，不能为借字体重读远端 UTF16 起点的整条前缀。
        source.byte_offset_for_utf16(0)?;
        if source.authority() != self.source {
            return Err(NativeMarkdownFontFallbackError::SourceMismatch);
        }
        if !base.core_font().permits(&self.base) {
            return Err(NativeMarkdownFontFallbackError::FontMismatch);
        }
        let font = self.font.as_ref().unwrap_or(base);
        if !font.core_font().permits(&self.selected) {
            return Err(NativeMarkdownFontFallbackError::FontMismatch);
        }
        Ok(font)
    }

    /// 消费选择结果，把真实fallback capture移交当前会话palette；Base返回None以复用原对象。
    /// 这只转移字体所有权，仍仅证明原单GCB试次，不授予整item无缺字或WholeWord证书。
    ///
    /// # Errors
    /// 原Source、base/selected字体身份或调用票变化时拒绝，不按名称重建替代字体。
    pub fn into_captured_font(
        mut self,
        base: &NativeMarkdownPagedFont,
        source: &MarkdownLayoutTextSource,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Option<NativeMarkdownPagedFont>> {
        self.font(base, source)?; // 真实Source首错优先；外来同长度正文不能取得原capture。
        let permitted = current();
        self.font(base, source)?; // current可进入Native并重入，之后必须重新核原来源与字体。
        if !permitted {
            return Err(NativeMarkdownFontFallbackError::Revoked);
        }
        Ok(self.font.take()) // 这里只移动原实际对象；没有新的Native getter、Source或字形缓存。
    }
}

/// Send packet只含原纯Core值和固定试次身份；没有CTFont、Native闭包或ShapeJob。
pub struct NativeMarkdownFontTrial {
    #[cfg(feature = "native-paged-production")]
    probe: Option<omni_markdown_core::MarkdownLineFontProbe>,
    source: MarkdownLayoutTextSource,
    source_authority: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFont,
    font_authority: MarkdownLayoutFontAuthority,
    range: Range<usize>,
    options: MarkdownShapingOptions,
    cancellation: MarkdownShapingCancellation,
    identity: Arc<()>,
}
pub struct NativeMarkdownFontTrialResult {
    identity: Arc<()>,
    outcome: Result<NativeMarkdownFontGlyphFacts>,
    resources: Option<MarkdownShapingResourceSnapshot>,
    // 原worker最后的probe/Source可能在结果形成后才Drop；只保原子观察与同次取消位，不保正文。
    backing: Option<MarkdownOriginalBackingObserver>,
    cancellation: MarkdownShapingCancellation,
}
impl NativeMarkdownFontTrialResult {
    /// None明确表示构造未完成，没有资源收据；不能把它当成“零资源已通过”。
    #[must_use]
    pub fn resources(&self) -> Option<MarkdownShapingResourceSnapshot> {
        self.resources
    }
    /// 任何退出都消费同一真实快照；只有拥有者主动退休时才可忽略其已请求的正常取消。
    /// 活动候选接纳失败必须保留原 TypedErr，包括取消，不能把结果错误伪装成成功退休。
    pub(in crate::native_surface) fn retire(self, preserve_cancelled: bool) -> Result<()> {
        let cancelled = self.cancellation.requested();
        let mut cause = self
            .outcome
            .err()
            .filter(|error| preserve_cancelled || !error.is_cancelled_retirement(cancelled));
        // 先完整读取本次真实Job的资源事实；Source已经失败也不能提前返回、漏掉不在Composer账内的试次。
        let cleanup = self.resources.and_then(|resources| {
            let unreleased = resources.files_open != 0
                || resources.mappings_open != 0
                || resources.mapped_bytes != 0
                || resources.work_bytes != 0
                || resources.cleanup_failed
                || !resources.shaper_access_certified;
            match resources.first_error {
                Some(error) => Some(NativeMarkdownFontFallbackError::GlyphStoreIo(error)),
                None if unreleased => Some(NativeMarkdownFontFallbackError::GlyphStoresNotReleased),
                None => None, // 原快照证明真实归还；未创建Job的外层None始终不合成全零快照。
            }
        });
        if let Some(cleanup) = cleanup {
            cause = Some(match cause {
                None => cleanup,
                Some(original)
                    if matches!((&original, &cleanup),
                        (NativeMarkdownFontFallbackError::GlyphStoreIo(first), NativeMarkdownFontFallbackError::GlyphStoreIo(second)) if first == second
                    ) || matches!(
                        (&original, &cleanup),
                        (
                            NativeMarkdownFontFallbackError::GlyphStoresNotReleased,
                            NativeMarkdownFontFallbackError::GlyphStoresNotReleased
                        )
                    ) =>
                {
                    original
                } // 同一Result原outcome已由同一快照签出的同一资源错误，只记一次。
                Some(original) => {
                    let (cause, cleanup) = if original.is_source_io() {
                        (original, cleanup) // 已有真实Source首因仍排第一，字形资源失败完整作为次因保存。
                    } else {
                        (cleanup, original) // 资源首错优先于其余真正操作失败，两者都不能被退休吞掉。
                    };
                    NativeMarkdownFontFallbackError::TrialCleanup {
                        cause: Box::new(cause),
                        cleanup: Box::new(cleanup),
                    }
                }
            });
        }
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|backing| backing.first_errno())
        {
            let already_recorded = cause
                .as_ref()
                .is_some_and(|cause| cause.original_backing_errno() == Some(errno));
            if !already_recorded {
                // 此observer属于同一个实际Result；最后Source Drop的errno成为链首，前面已读取的
                // shaper分页/归还错误仍保留。只对同一原子首因去重，不按文本或相同整数合并外来结果。
                let source = NativeMarkdownFontFallbackError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(errno),
                );
                cause = Some(match cause {
                    Some(cleanup) => NativeMarkdownFontFallbackError::TrialCleanup {
                        cause: Box::new(source),
                        cleanup: Box::new(cleanup),
                    },
                    None => source,
                });
            }
        }
        cause.map_or(Ok(()), Err)
    }

    /// 当前选择入口拒绝了结果也先退休它；真实资源次因不能被正常撤权或主错误覆盖。
    fn retire_after(
        self,
        cause: NativeMarkdownFontFallbackError,
        same_trial: bool,
    ) -> NativeMarkdownFontFallbackError {
        let cancelled = self.cancellation.requested();
        match self.retire(false) {
            Ok(()) => cause,
            Err(cleanup) => {
                if cause.is_cancelled_retirement(cancelled) {
                    return cleanup; // 正常撤权不是第二笔失败，真实资源错误必须成为唯一可报告首因。
                }
                if same_trial {
                    let repeated_source = cause
                        .original_backing_errno()
                        .is_some_and(|errno| cleanup.original_backing_errno() == Some(errno));
                    if repeated_source {
                        return cleanup; // 保留退休已组成的完整链，不能为去掉重复Source而丢掉真实shaper次因。
                    }
                    #[cfg(feature = "native-paged-production")]
                    if matches!(
                        (&cause, &cleanup),
                        (
                            NativeMarkdownFontFallbackError::Compose(
                                omni_markdown_core::MarkdownLineComposeError::Poisoned
                            ),
                            NativeMarkdownFontFallbackError::Compose(
                                omni_markdown_core::MarkdownLineComposeError::Poisoned
                            )
                        )
                    ) {
                        // 同nonce的packet与接纳门都在原probe消费后观察同一Poisoned，且退休没有其他次因。
                        // 只去掉这次重复报告，仍返回原失败；外来试次或复合清理错误均不走此分支。
                        return cleanup;
                    }
                }
                let (cause, cleanup) = if cleanup.is_source_io() && !cause.is_source_io() {
                    (cleanup, cause) // 最后Source Drop的真实errno成为链首，原操作错误仍保存在同一因果中。
                } else {
                    (cause, cleanup) // 已有Source首因或两个真正操作错误时保持原主因。
                };
                NativeMarkdownFontFallbackError::TrialCleanup {
                    cause: Box::new(cause),
                    cleanup: Box::new(cleanup),
                }
            }
        }
    }

    /// 只读真实worker结果供错误呈现/控制使用；字段私有，不能人工构造缺字通过票。
    pub fn outcome(
        &self,
    ) -> std::result::Result<&NativeMarkdownFontGlyphFacts, &NativeMarkdownFontFallbackError> {
        self.outcome.as_ref()
    }
}
pub enum NativeMarkdownFontFallbackStep {
    PendingDependencies,
    /// 原系统候选没有可供本Core整形器捕获的文件；它没有被shape，更没有缺字结论。
    /// 调用方必须显式决定是否按原cursor继续；成功策略仅能称首个可捕获且无缺字的字体。
    UnavailableCandidate {
        ordinal: usize,
        cause: NativeMarkdownFontCascadeError,
    },
    /// 优先候选没有本地文件，尚未形成任何完整 GCB 覆盖结论；下一量子才进入原全局级联。
    #[cfg(feature = "native-paged-production")]
    UnavailableForString {
        cause: NativeMarkdownPagedFontError,
    },
    /// 仅在真正启动一次试次时分配其独占包；待决和耗尽状态不占整份整形输入的栈空间。
    Trial(Box<NativeMarkdownFontTrial>),
    Exhausted {
        tried: usize,
    },
}
pub enum NativeMarkdownFontFallbackCompletion {
    Rejected {
        origin: NativeMarkdownFontOrigin,
        facts: NativeMarkdownFontGlyphFacts,
    },
    Selected(NativeMarkdownFontChoice),
}
struct Active {
    identity: Arc<()>,
    cancellation: MarkdownShapingCancellation,
    font: Option<NativeMarkdownFontCapture>,
    authority: MarkdownLayoutFontAuthority,
    origin: NativeMarkdownFontOrigin,
    #[cfg(feature = "native-paged-production")]
    phase: ProbeTrialPhase,
    #[cfg(feature = "native-paged-production")]
    facts: Option<NativeMarkdownFontGlyphFacts>,
}

/// 原候选只有一个真实段在途；成功段归还后才允许原协调器前进，不能并行保留多个 Job。
#[cfg(feature = "native-paged-production")]
#[derive(Clone, Copy, Eq, PartialEq)]
enum ProbeTrialPhase {
    Worker,
    NextSegment,
    SegmentReady,
}

/// 一个显式激活阶段的候选消费器；只有一个实际试次在途，不缓存所有Font。
pub struct NativeMarkdownFontFallback {
    #[cfg(feature = "native-paged-production")]
    probe: Option<omni_markdown_core::MarkdownLineFontProbe>,
    source: Option<MarkdownLayoutTextSource>,
    authority: MarkdownLayoutTextAuthority,
    base: MarkdownLayoutFontAuthority,
    backing: Option<MarkdownOriginalBackingObserver>,
    request: NativeMarkdownFontFallbackRequest,
    cascade: Option<NativeMarkdownFontCascade>,
    active: Option<Active>,
    tried: usize,
    // 仅本 owner 实际首 scalar 读取尚未交回的工作量；错误或 unavailable 出口同样由 Producer 领取。
    #[cfg(feature = "native-paged-production")]
    probe_source_bytes: usize,
    stopped: bool,
    mtm: MainThreadMarker,
}
impl NativeMarkdownFontFallback {
    /// 捕获原Source/Font身份；字素与脚本待决时不创建任何级联候选。
    pub fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        base: &NativeMarkdownPagedFont,
        base_authority: &MarkdownLayoutFontAuthority,
        request: NativeMarkdownFontFallbackRequest,
        mtm: MainThreadMarker,
    ) -> Result<Self> {
        source.byte_offset_for_utf16(request.source_utf16.start)?;
        if source.authority() != *authority {
            return Err(NativeMarkdownFontFallbackError::SourceMismatch);
        }
        if !base.core_font().permits(base_authority) {
            return Err(NativeMarkdownFontFallbackError::FontMismatch);
        }
        if request
            .options
            .language
            .as_ref()
            .is_some_and(|language| language.len() > MAX_TRIAL_LANGUAGE_BYTES)
        {
            // 在issue克隆options和创建任何试次之前拒绝超预算标签；此处不复制Core语法规则。
            return Err(MarkdownShapingError::InvalidOptions.into());
        }
        if request.source_utf16.start >= request.source_utf16.end
            || request.source_utf16.end > source.utf16_len()
            || !base.accepts_shaping_size(request.options.points)
            || request.options.features.len() > MARKDOWN_SHAPING_MAX_FEATURES
        {
            // 字号沿同一Font payload核实际双精度绑定和唯一f32编码，不用有损回转误拒14.3等尺寸。
            return Err(NativeMarkdownFontFallbackError::InvalidCluster);
        }
        source.byte_offset_for_utf16(request.source_utf16.end)?; // 真实标量端点，不接受低代理项。
        let backing = source.original_backing_observer();
        Ok(Self {
            #[cfg(feature = "native-paged-production")]
            probe: None,
            source: Some(source),
            authority: authority.clone(),
            base: base_authority.clone(),
            backing,
            request,
            cascade: None,
            active: None,
            tried: 0,
            #[cfg(feature = "native-paged-production")]
            probe_source_bytes: 0,
            stopped: false,
            mtm,
        })
    }
    fn guard(
        &self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<()> {
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let source = self
            .source
            .as_ref()
            .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
        // 构造时已在同一不可变Source核过真实两端；此处只问健康，不再借远端叶解码同一位置。
        source.byte_offset_for_utf16(0)?;
        if source.authority() != self.authority {
            return Err(NativeMarkdownFontFallbackError::SourceMismatch);
        }
        #[cfg(feature = "native-paged-production")]
        if let Some(probe) = &self.probe {
            probe.validate().map_err(compose::error)?;
        } // 原Source健康先行；已经消费的Style scope不能借仍存活的Source继续字体试次。
        if !base.core_font().permits(&self.base) || !base.core_font().permits(authority) {
            return Err(NativeMarkdownFontFallbackError::FontMismatch);
        }
        let valid = !self.stopped && current();
        // current可重入并消费原样式票；回调返回后必须重新核原Source及scope，不能只沿旧bool。
        source.byte_offset_for_utf16(0)?;
        #[cfg(feature = "native-paged-production")]
        if let Some(probe) = &self.probe {
            probe.validate().map_err(compose::error)?;
        }
        if !valid {
            return Err(NativeMarkdownFontFallbackError::Revoked);
        }
        Ok(())
    }
    fn cluster(
        &self,
        dependencies: Option<(&mut MarkdownGraphemeJob, &mut MarkdownScriptJob)>,
    ) -> Result<bool> {
        #[cfg(feature = "native-paged-production")]
        if let Some(probe) = &self.probe {
            probe.validate().map_err(compose::error)?;
            return Ok(true); // 不重新扫描GCB/Script，私有构造已绑定原协调器同一真实单字素票。
        }
        let (graphemes, scripts) =
            dependencies.ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
        let range = &self.request.source_utf16;
        match graphemes.query(
            &self.authority,
            MarkdownGraphemeUnit::Utf16,
            range.start,
            MarkdownGraphemeQuery::IsBoundary,
        )? {
            MarkdownGraphemeAnswer::Pending => return Ok(false),
            MarkdownGraphemeAnswer::Known(None) => {
                return Err(NativeMarkdownFontFallbackError::InvalidCluster);
            }
            MarkdownGraphemeAnswer::Known(Some(_)) => {}
        }
        match graphemes.query(
            &self.authority,
            MarkdownGraphemeUnit::Utf16,
            range.start,
            MarkdownGraphemeQuery::Next,
        )? {
            MarkdownGraphemeAnswer::Pending => return Ok(false),
            MarkdownGraphemeAnswer::Known(Some(end)) if end.utf16 == range.end => {}
            _ => return Err(NativeMarkdownFontFallbackError::InvalidCluster),
        } // 两端是边界还不够，必须是严格下一个边界，不能把多个GCB拼成试次。
        match scripts.run_at(&self.authority, MarkdownScriptUnit::Utf16, range.start)? {
            MarkdownScriptAnswer::Pending => Ok(false),
            MarkdownScriptAnswer::Known(Some(run))
                if run.start_utf16 <= range.start
                    && run.end_utf16 >= range.end
                    && run.script.iso15924_tag() == self.request.options.script =>
            {
                Ok(true)
            }
            _ => Err(NativeMarkdownFontFallbackError::InvalidCluster),
        } // 完整GCB必须属于原script run；不从第一个scalar的cmap或script单独猜测。
    }
    fn stop(&mut self) {
        self.stopped = true;
        if let Some(active) = self.active.take() {
            active.cancellation.request();
            drop(active);
        }
        drop(self.cascade.take());
        #[cfg(feature = "native-paged-production")]
        drop(self.probe.take());
        drop(self.source.take()); // worker仅持纯Core clone；调用方仍须join，不能在这里声称已释放。
    }

    /// 主线程只准备一个候选；实际shape必须消费返回的packet，不在此隐藏同步长算法。
    pub fn next_trial(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        graphemes: &mut MarkdownGraphemeJob,
        scripts: &mut MarkdownScriptJob,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackStep> {
        let result = self.issue(base, authority, Some((graphemes, scripts)), &[], current);
        if result.is_err() {
            self.stop();
            if let Some(errno) = self
                .backing
                .as_ref()
                .and_then(|observer| observer.first_errno())
            {
                return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
            } // 最后Source归还的真实失败也优先于表层失权，不以错误出口吞掉cleanup首因。
        }
        result
    }
    fn issue(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        dependencies: Option<(&mut MarkdownGraphemeJob, &mut MarkdownScriptJob)>,
        palette: &[Rc<NativeMarkdownLineFont>],
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackStep> {
        self.guard(base, authority, current)?;
        if self.active.is_some() {
            #[cfg(feature = "native-paged-production")]
            if self.probe.is_some() {
                return self.continue_trial(base, authority, current);
            }
            return Err(NativeMarkdownFontFallbackError::InvalidState);
        }
        if !self.cluster(dependencies)? {
            return Ok(NativeMarkdownFontFallbackStep::PendingDependencies);
        }
        self.guard(base, authority, current)?;
        #[cfg(feature = "native-paged-production")]
        if self.tried == 1 && self.probe.is_some() {
            // tried 只在真实试次结束或明确不可捕获后前进；Base 首次缺字才轮到系统原首字符候选。
            // 首 scalar 不证明完整 GCB，后面的 start_trial 仍发送同一完整原范围到唯一 worker。
            let font = match self.for_string_font(base, authority, palette, current) {
                Ok(font) => font,
                Err(NativeMarkdownFontFallbackError::ForStringCapture(
                    cause @ NativeMarkdownPagedFontError::FileSourceUnavailable,
                )) => {
                    self.tried = self
                        .tried
                        .checked_add(1)
                        .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
                    self.guard(base, authority, current)?;
                    return Ok(NativeMarkdownFontFallbackStep::UnavailableForString { cause });
                }
                Err(cause) => return Err(cause),
            };
            return self.start_trial(
                base,
                authority,
                NativeMarkdownFontOrigin::ForString,
                Some(font),
                current,
            );
        }
        let (origin, font) = if self.tried == 0 {
            (NativeMarkdownFontOrigin::Base, None)
        } else {
            if self.cascade.is_none() {
                let valid = || self.guard(base, authority, current).is_ok();
                let result = NativeMarkdownFontCascade::capture(
                    base,
                    authority,
                    self.request.languages,
                    self.request.cascade_budget,
                    self.mtm,
                    &valid,
                );
                self.guard(base, authority, current)?;
                self.cascade = Some(result?);
            }
            // 临时移出本级联owner，结束self.cascade的可变借用后才调用Native。
            // 每个原语的current闭包都可复用完整Source端点健康门，不用仅observer替代。
            let mut cascade = self
                .cascade
                .take()
                .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
            let valid = || self.guard(base, authority, current).is_ok();
            let candidate = cascade.next(base, authority, &valid);
            self.guard(base, authority, current)?;
            let Some(candidate) = candidate? else {
                let tried = self.tried;
                self.stop();
                return Ok(NativeMarkdownFontFallbackStep::Exhausted { tried });
            };
            let font = candidate.capture_reusing(
                base,
                authority,
                palette,
                self.request.maximum_font_bytes,
                &valid,
            );
            self.guard(base, authority, current)?;
            let font = match font {
                Ok(font) => font,
                Err(
                    cause @ NativeMarkdownFontCascadeError::Font(
                        NativeMarkdownPagedFontError::FileSourceUnavailable,
                    ),
                ) => {
                    let ordinal = candidate.ordinal();
                    self.cascade = Some(cascade);
                    self.tried = self
                        .tried
                        .checked_add(1)
                        .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
                    self.guard(base, authority, current)?;
                    return Ok(NativeMarkdownFontFallbackStep::UnavailableCandidate {
                        ordinal,
                        cause,
                    }); // 没有创建Job或缺字收据；不自动重试、重建级联或把另一字体改成该身份。
                }
                Err(error) => {
                    return Err(NativeMarkdownFontFallbackError::CandidateCapture {
                        ordinal: candidate.ordinal(),
                        error,
                    }); // 其他来源、解析与IO错误仍立即撤权，不能以“候选不可用”吞掉失败。
                }
            };
            self.cascade = Some(cascade); // 原签发者持续存活；不重建系统列表或另领候选顺序。
            (
                NativeMarkdownFontOrigin::Cascade(candidate.ordinal()),
                Some(font),
            )
        };
        self.start_trial(base, authority, origin, font, current)
    }

    /// 一个实际候选占据原唯一在途槽；来源差异只影响候选顺序，不绕过完整 GCB 整形或资源收据。
    fn start_trial(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        origin: NativeMarkdownFontOrigin,
        font: Option<NativeMarkdownFontCapture>,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackStep> {
        let selected = font
            .as_ref()
            .map(NativeMarkdownFontCapture::font)
            .unwrap_or(base);
        let font_authority = selected.core_font().authority();
        let identity = Arc::new(());
        let cancellation = MarkdownShapingCancellation::default();
        let trial = NativeMarkdownFontTrial {
            #[cfg(feature = "native-paged-production")]
            probe: self.probe.clone(),
            source: self
                .source
                .as_ref()
                .ok_or(NativeMarkdownFontFallbackError::InvalidState)?
                .clone(),
            source_authority: self.authority.clone(),
            font: selected.core_font().clone(),
            font_authority: font_authority.clone(),
            range: self.request.source_utf16.clone(),
            options: self.request.options.clone(),
            cancellation: cancellation.clone(),
            identity: identity.clone(),
        };
        self.active = Some(Active {
            identity,
            cancellation,
            font,
            authority: font_authority,
            origin,
            #[cfg(feature = "native-paged-production")]
            phase: ProbeTrialPhase::Worker,
            #[cfg(feature = "native-paged-production")]
            facts: None,
        });
        self.tried = self
            .tried
            .checked_add(1)
            .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
        self.guard(base, authority, current)?;
        Ok(NativeMarkdownFontFallbackStep::Trial(Box::new(trial)))
    }

    /// Expose only the active Cascade capture's existing fixed resource observer for native probes.
    /// Base/no capture returns None; the observer retains no Font, Source, descriptor, or mapping.
    #[cfg(feature = "native-paged-font-fallback-probe")]
    #[must_use]
    pub fn active_font_bytes_observer(
        &self,
    ) -> Option<omni_markdown_core::MarkdownLayoutFontBytesResourceObserver> {
        self.active
            .as_ref()
            .and_then(|active| active.font.as_ref())
            .map(|capture| capture.font().bytes_observer())
    }

    /// 必须在真实worker完成/join后调用；结果只授权这个原GCB的字体选择。
    pub fn complete_trial(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        graphemes: &mut MarkdownGraphemeJob,
        scripts: &mut MarkdownScriptJob,
        result: NativeMarkdownFontTrialResult,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackCompletion> {
        let completion = self
            .complete(base, authority, Some((graphemes, scripts)), result, current)
            .and_then(|completion| completion.ok_or(NativeMarkdownFontFallbackError::InvalidState));
        completion.map_err(|cause| self.stop_preserving_error(cause))
    }
    /// 同一字体入口失败后结束原Native字段；已携真实Source的完整错误链必须原样保留。
    fn stop_preserving_error(
        &mut self,
        cause: NativeMarkdownFontFallbackError,
    ) -> NativeMarkdownFontFallbackError {
        self.stop();
        if cause.is_source_io() {
            return cause; // TrialCleanup里的真实shaper次因不能被外层再次覆盖成单一Source。
        }
        match self
            .backing
            .as_ref()
            .and_then(|backing| backing.first_errno())
        {
            Some(errno) => NativeMarkdownFontFallbackError::TrialCleanup {
                cause: Box::new(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
                ),
                cleanup: Box::new(cause),
            }, // 仅原stop之后新出现的Source首因提升优先级；原操作失败仍完整保留。
            None => cause,
        }
    }

    fn complete(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        dependencies: Option<(&mut MarkdownGraphemeJob, &mut MarkdownScriptJob)>,
        result: NativeMarkdownFontTrialResult,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Option<NativeMarkdownFontFallbackCompletion>> {
        // 首个Native/current前固定原试次身份；只有同nonce才证明后续两个Source错误来自同一观察。
        let same_trial = self
            .active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.identity, &result.identity));
        // Fresh 的唯一新映射须随失败试次归还；Reused 只退出本次强边，最终释放仍由原 Body 严格验收。
        let bytes = if same_trial {
            self.active
                .as_ref()
                .and_then(|active| active.font.as_ref())
                .and_then(NativeMarkdownFontCapture::fresh_bytes_observer)
        } else {
            None // A foreign result cannot prove that this active packet's legitimate font owners have ended.
        };
        let completion =
            self.complete_trial_inner(base, authority, dependencies, result, current, same_trial);
        let Err(cause) = completion else {
            return completion; // Selected transfers this capture to Choice; its legitimate owner must remain live.
        };
        let already_font_failure = matches!(
            &cause,
            NativeMarkdownFontFallbackError::FontBytesNotReleased
        );
        let cause = self.stop_preserving_error(cause); // Drop the original active/cascade/probe before reading its observer.
        if let Some(bytes) = bytes
            && (!bytes.mapping_drop_completed()
                || !bytes.unmap_succeeded()
                || bytes.mapped_bytes() != 0
                || !bytes.source_descriptor_closed()
                || !bytes.backing_descriptor_closed()
                || bytes.failed())
            && !already_font_failure
        {
            // Keep the original Source/operation chain first and attach the actual font cleanup failure.
            // Rejected may already report this capture's FontBytesNotReleased; do not duplicate that failure.
            return Err(NativeMarkdownFontFallbackError::TrialCleanup {
                cause: Box::new(cause),
                cleanup: Box::new(NativeMarkdownFontFallbackError::FontBytesNotReleased),
            });
        }
        Err(cause)
    }

    /// Admit the original nonce, consume glyph facts, and transfer ownership; errors return to the real stop boundary.
    fn complete_trial_inner(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        dependencies: Option<(&mut MarkdownGraphemeJob, &mut MarkdownScriptJob)>,
        result: NativeMarkdownFontTrialResult,
        current: &(impl Fn() -> bool + ?Sized),
        same_trial: bool,
    ) -> Result<Option<NativeMarkdownFontFallbackCompletion>> {
        let admission = (|| {
            self.guard(base, authority, current)?;
            if !self.cluster(dependencies)? {
                return Err(NativeMarkdownFontFallbackError::InvalidCluster);
            }
            let active = self
                .active
                .as_ref()
                .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
            if !Arc::ptr_eq(&active.identity, &result.identity) {
                return Err(NativeMarkdownFontFallbackError::TrialMismatch);
            }
            Ok(())
        })();
        if let Err(cause) = admission {
            self.stop(); // 原同次取消先置位，旧Cell/Style的精确正常撤权才能在退休时被辨认。
            return Err(result.retire_after(cause, same_trial));
        }
        if result.outcome.is_err() {
            // 同结果的资源快照必须先完整消费；Source 首因和实际 glyph 归还次因沿原退休路径组合。
            // preserve_cancelled=true 保证这里已观察到的错误不能被“主动退休取消”规则过滤为 Ok。
            return Err(result
                .retire(true)
                .expect_err("保留原错误的结果退休不能成功"));
        }
        let facts = result.outcome?;
        #[cfg(feature = "native-paged-production")]
        let mut facts = facts;
        let resources = result
            .resources
            .ok_or(NativeMarkdownFontFallbackError::GlyphStoresNotReleased)?;
        if resources.files_open != 0
            || resources.mappings_open != 0
            || resources.mapped_bytes != 0
            || resources.work_bytes != 0
            || resources.cleanup_failed
            || resources.first_error.is_some()
            || !resources.shaper_access_certified
        {
            return Err(NativeMarkdownFontFallbackError::GlyphStoresNotReleased);
        } // 不以worker返回、shape Ok或假零值代替真实数组/FD释放。
        self.guard(base, authority, current)?;
        let active = self
            .active
            .take()
            .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
        #[cfg(feature = "native-paged-production")]
        let mut active = active;
        #[cfg(feature = "native-paged-production")]
        if self.probe.is_some() {
            if active.phase != ProbeTrialPhase::Worker {
                return Err(NativeMarkdownFontFallbackError::InvalidState);
            }
            if let Some(previous) = active.facts {
                // 累计的每项都来自已经真实检查并释放的段；不是把首段结果伪装成整个外 GCB。
                for (value, addition) in [
                    (&mut facts.glyph_count, previous.glyph_count),
                    (&mut facts.missing_glyphs, previous.missing_glyphs),
                    (&mut facts.input_scalars, previous.input_scalars),
                    (&mut facts.source_bytes_read, previous.source_bytes_read),
                ] {
                    *value = value
                        .checked_add(addition)
                        .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
                }
                facts.flags_union |= previous.flags_union;
                facts.source_borrow_max_bytes = facts
                    .source_borrow_max_bytes
                    .max(previous.source_borrow_max_bytes);
            }
        }
        if facts.missing_glyphs > 0 {
            // 真实缺字仍推进原有序级联；共享原字体也要重新 shape，不能把旧 GCB 的成功挪到这里。
            let bytes = active
                .font
                .as_ref()
                .and_then(NativeMarkdownFontCapture::fresh_bytes_observer);
            let origin = active.origin;
            drop(active);
            if let Some(bytes) = bytes
                && (!bytes.unmap_succeeded()
                    || !bytes.mapping_drop_completed()
                    || bytes.mapped_bytes() != 0
                    || !bytes.source_descriptor_closed()
                    || !bytes.backing_descriptor_closed()
                    || bytes.failed())
            {
                return Err(NativeMarkdownFontFallbackError::FontBytesNotReleased);
            }
            self.guard(base, authority, current)?;
            return Ok(Some(NativeMarkdownFontFallbackCompletion::Rejected {
                origin,
                facts,
            }));
        }
        #[cfg(feature = "native-paged-production")]
        if let Some(probe) = &self.probe
            && !probe.complete_single_segment().map_err(compose::error)?
        {
            active.facts = Some(facts);
            active.phase = ProbeTrialPhase::NextSegment;
            self.active = Some(active);
            return Ok(None); // 外 GCB 仍有同候选真实段，不能在这里签 Choice 或切下一 Font。
        }
        self.select_choice(base, active, facts, current).map(Some)
    }

    /// 只有整个候选的所有真实段都无缺字并已归还 Job 后，才转移原 Font capture。
    fn select_choice(
        &mut self,
        base: &NativeMarkdownPagedFont,
        active: Active,
        facts: NativeMarkdownFontGlyphFacts,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackCompletion> {
        let choice = NativeMarkdownFontChoice {
            origin: active.origin,
            source_utf16: self.request.source_utf16.clone(),
            facts,
            font: active.font.map(NativeMarkdownFontCapture::into_font),
            base: self.base.clone(),
            selected: active.authority,
            source: self.authority.clone(),
        };
        let source = self
            .source
            .take()
            .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
        #[cfg(feature = "native-paged-production")]
        let probe = self.probe.take(); // 最后Native/current回调以后还要核原票，不能先丢最后Source读claim。
        self.stop(); // 已选择字体转交结果；先归还级联，再核原Source与调用票，不在idle藏业务缓存。
        source.byte_offset_for_utf16(0)?;
        let valid = current() && base.core_font().permits(&self.base);
        source.byte_offset_for_utf16(0)?; // 原两端已认证；这里只核健康，不重复解码远端 GCB 前缀。
        #[cfg(feature = "native-paged-production")]
        if let Some(probe) = &probe {
            probe.validate().map_err(compose::error)?;
        }
        if !valid {
            return Err(NativeMarkdownFontFallbackError::Revoked);
        }
        drop(source);
        #[cfg(feature = "native-paged-production")]
        drop(probe);
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        } // 真实最后Source Drop的I/O事实不因已有无缺字结果而被抹掉。
        Ok(NativeMarkdownFontFallbackCompletion::Selected(choice))
    }
    /// 只请求原试次取消并交还Native字段；调用方必须join后核真实worker资源收据。
    pub fn close(mut self) {
        self.stop();
    }
}
impl Drop for NativeMarkdownFontFallback {
    fn drop(&mut self) {
        self.stop();
    }
}
