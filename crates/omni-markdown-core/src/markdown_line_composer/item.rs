//! 单个worker内的真实分项协调；请求只携带弱源票和固定事实，不把文字或Native对象送入响应。

use std::{ops::Range, sync::Arc};

use thiserror::Error;

use super::{MarkdownLineFontRegistration, MarkdownLineRegisteredFont};
use crate::markdown_bidi::{
    MarkdownBidiPrepareBudget, MarkdownBidiPrepareJob, MarkdownBidiPrepareStatus,
};

use crate::{
    BidiBaseDirection, BidiError, BidiMemoryBudget, MarkdownBidiCancellation,
    MarkdownBidiParagraph, MarkdownDocumentBufferError, MarkdownGraphemeAnswer,
    MarkdownGraphemeQuery, MarkdownLayoutBreakError, MarkdownLayoutBreakJob, MarkdownLayoutFont,
    MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownLineBreakBudget, MarkdownScriptAnswer, MarkdownScriptBudget,
    MarkdownScriptCancellation, MarkdownScriptError, MarkdownScriptJob, MarkdownScriptUnit,
    MarkdownShapedGlyph, MarkdownShapingBudget, MarkdownShapingCancellation,
    MarkdownShapingDirection, MarkdownShapingError, MarkdownShapingFeature, MarkdownShapingJob,
    MarkdownShapingOptions, MarkdownShapingStatus,
};

const METADATA_UNITS: usize = 256;
mod bidi_preparation;
mod font_probe;
mod shaping_boundary;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use shaping_boundary::CertifiedShapingEndpoint;
pub(crate) use shaping_boundary::ResolvedBidiShapingSeam;
#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-shaping-frontier"
))]
mod table_row;
pub use font_probe::{
    MarkdownLineFontProbe, MarkdownLineFontProbeSegmentAnswer, MarkdownLineFontProbeSegmentStep,
};
#[cfg(feature = "paged-shaping-frontier")]
pub(super) mod closed_item;
#[cfg(feature = "paged-shaping-frontier")]
mod frontier;
#[cfg(feature = "paged-shaping-frontier")]
mod paragraph;
#[cfg(all(test, feature = "paged-shaping-frontier"))]
mod paragraph_tests;
#[cfg(feature = "paged-shaping-frontier")]
mod prefix;
#[cfg(feature = "paged-shaping-frontier")]
mod style;
#[cfg(feature = "paged-shaping-frontier")]
pub use frontier::{MarkdownLineFrontierReason, MarkdownLineFrontierWork};

#[derive(Debug, Error)]
pub enum MarkdownLineComposeError {
    #[error("组合器请求不属于原冻结源或原样式请求")]
    SourceMismatch,
    #[error("组合器阶段、配额或样式范围无效")]
    InvalidRequest,
    #[error("组合器元数据覆盖不连续")]
    InconsistentCoverage,
    #[error("查询位置位于尚未展开的隐藏范围 {0:?}")]
    FoldedRange(Range<usize>),
    #[error("已发布输出尚未覆盖所查询的字素边界")]
    GraphemePrefixPending,
    #[error("表格块内部的字素与命中由原格显示源提供")]
    TableRange,
    #[error("本行必须按真实UBA视觉顺序继续组合多个字形片段")]
    NeedsBidiAssembly,
    #[error("组合器坐标或计数溢出")]
    Overflow,
    #[error("组合器已取消")]
    Cancelled,
    #[error("组合器已永久撤权")]
    Poisoned,
    #[cfg(feature = "paged-shaping-frontier")]
    #[error("样式重排失败：{cause}；原字形资源归还{operation:?}失败，errno={errno}")]
    StyleResetCleanup {
        #[source]
        cause: Box<MarkdownLineComposeError>,
        operation: crate::MarkdownShapingIoOperation,
        errno: i32,
    },
    #[cfg(feature = "paged-shaping-frontier")]
    #[error("已提交行的两份只读快照仍在使用，请先归还一份")]
    SnapshotBusy,
    #[error("实际字体注册表已失效或锁已失败")]
    FontCatalogUnavailable,
    #[error("实际字体注册槽预算已用完")]
    FontBudgetExceeded,
    #[error("原实际字体注册已经过期")]
    FontExpired,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    #[error("原相邻样式来源失效：{0:?}")]
    RootLeafBoundary(Box<crate::markdown_document_buffer::MarkdownTableRequestError>),
    #[error("原冻结源读取失败")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("原UBA元数据失败")]
    Bidi(#[from] BidiError),
    #[error("原Script元数据失败")]
    Script(#[from] MarkdownScriptError),
    #[error("原断点元数据失败")]
    Break(#[from] MarkdownLayoutBreakError),
    #[error("原实际整形失败")]
    Shaping(#[from] MarkdownShapingError),
    #[cfg(feature = "paged-shaping-frontier")]
    #[error("原字体前沿元数据认证失败")]
    FrontierMetadata(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("原实际有限前缀重排失败")]
    Resolve(#[from] crate::MarkdownSoftLineResolveError),
    #[error("原实际字形cluster索引失败")]
    Cluster(#[from] crate::MarkdownGlyphClusterError),
    #[error("组合器分页文件操作失败")]
    Storage(#[from] std::io::Error),
    #[error("原安全字形切片失效")]
    Safe(#[from] crate::MarkdownSafeGlyphSliceError),
    #[error("原软行测量失败")]
    Measure(#[from] crate::MarkdownSoftLineMeasureError),
}
pub(super) type Result<T> = std::result::Result<T, MarkdownLineComposeError>;

/// 非字形控制必须由后继真实段落/制表策略处理，不能作为缺字送入字体fallback。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineInlineControlKind {
    Tab,
    CarriageReturn,
    LineFeed,
    NextLine,
    LineSeparator,
    ParagraphSeparator,
    EmptyParagraph,
    /// 原折叠隐区的整段控制；没有字形，端点仍属于原完整 Source。
    FoldedSection,
    /// 原结构行的独立块；内部正文只由真实格显示源布局。
    TableRow,
}
#[derive(Clone)]
pub struct MarkdownLineInlineControl {
    pub source_utf16: std::ops::Range<usize>,
    pub kind: MarkdownLineInlineControlKind,
    pub(super) source: MarkdownLayoutTextAuthority,
    pub(super) nonce: Arc<()>,
}

/// 同一次组合的深层取消位；Clone只共享原位，不拥有正文、字体或调度线程。
#[derive(Clone, Default)]
pub struct MarkdownLineComposeCancellation {
    bidi: MarkdownBidiCancellation,
    script: MarkdownScriptCancellation,
    shape: MarkdownShapingCancellation,
    measure: crate::MarkdownSoftLineMeasureCancellation,
}
impl MarkdownLineComposeCancellation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.bidi.cancel();
        self.script.cancel();
        self.shape.request(); // 实际长GSUB/GPOS也观察原位；不是只在外层step之间取消。
        self.measure.cancel();
    }
    pub fn is_cancelled(&self) -> bool {
        self.bidi.is_cancelled()
    }
    pub(super) fn shaping(&self) -> MarkdownShapingCancellation {
        self.shape.clone()
    }
    pub(super) fn measure(&self) -> crate::MarkdownSoftLineMeasureCancellation {
        self.measure.clone()
    }
}

/// 单次只推进一个依赖或一份输入；同一个bytes配额不会给Script和断点各用一遍。
#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineComposeBudget {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub metadata_units: usize,
}
impl Default for MarkdownLineComposeBudget {
    fn default() -> Self {
        Self {
            bytes: 4096,
            scalars: 256,
            records: 256,
            metadata_units: METADATA_UNITS,
        }
    }
}

/// 平台响应样式和实际Font；方向、脚本及BOT/EOT由Core原事实确定，不接受响应伪造。
#[derive(Clone, Debug)]
pub struct MarkdownLineComposeStyle {
    /// 原语义来源的绘制位随同一字体响应封存；不改写字形、源坐标或断行权限。
    pub presentation: crate::MarkdownPresentationStyles,
    pub points: f32,
    pub language: Option<String>,
    pub features: Vec<MarkdownShapingFeature>,
}

/// 私有nonce绑定本次仍待响应的真实交集；相同坐标的新请求不能复用旧响应。
#[derive(Clone)]
pub struct MarkdownLineStyleRequest {
    nonce: Arc<()>,
    source: MarkdownLayoutTextAuthority,
    range: Range<usize>,
    scope: StyleScope,
    // 只有真实 UBA 自然分界可不为 GCB；不能从短 coverage 或源坐标自行构造。
    shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
    #[cfg(feature = "paged-shaping-frontier")]
    continuation_font: Option<MarkdownLayoutFontAuthority>,
    // 字体交接已经持原 GCB 两端票；保其真实字节范围供有限首 scalar 读取，不能再次远端定位。
    #[cfg(feature = "paged-shaping-frontier")]
    continuation_bytes: Option<Range<usize>>,
}

/// 普通文字的真实 Unicode 事实与已认证结构行互斥；表格握手不持伪造脚本、级别或段落。
#[derive(Clone)]
enum StyleScope {
    Text(TextStyleScope),
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    Table(crate::markdown_layout_source::table_rows::TableSpan),
}

#[derive(Clone)]
struct TextStyleScope {
    paragraph: Range<usize>,
    script: [u8; 4],
    level: u8,
}
impl MarkdownLineStyleRequest {
    pub fn source(&self) -> &MarkdownLayoutTextAuthority {
        &self.source
    }
    pub fn source_range(&self) -> Range<usize> {
        self.range.clone()
    }
    /// 仅文字作用域持原 Unicode 段落；结构行必须先交回真实行票，误用按调用契约 panic。
    pub fn paragraph_range(&self) -> Range<usize> {
        self.text_scope()
            .expect("结构表格请求没有文字段落权限")
            .paragraph
            .clone()
    }
    /// 只返回实际 Script 分析值；表格结构握手没有可借用的脚本，误用按调用契约 panic。
    pub fn script(&self) -> [u8; 4] {
        self.text_scope().expect("结构表格请求没有脚本权限").script
    }
    /// 只返回原 UBA 级别；结构行不经过文字 UBA，误用按调用契约 panic。
    pub fn level(&self) -> u8 {
        self.text_scope()
            .expect("结构表格请求没有 UBA 级别权限")
            .level
    }
    /// 字体/整形入口必须先取得真实 Text 作用域；结构请求只能交回原行票，不能降为文字。
    fn text_scope(&self) -> Result<&TextStyleScope> {
        match &self.scope {
            StyleScope::Text(text) => Ok(text),
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            StyleScope::Table(_) => Err(MarkdownLineComposeError::TableRange),
        }
    }
    /// 字体交接保留原item的Font上下文身份；普通样式请求没有续算身份。
    /// 这不是平台字体优先策略：完整GCB仍按真实语义base选择，不能让旧fallback吞掉已可回base的字符。
    #[cfg(feature = "paged-shaping-frontier")]
    pub fn continuation_font(&self) -> Option<&MarkdownLayoutFontAuthority> {
        self.continuation_font.as_ref()
    }
    /// 响应只能覆盖请求起点开始的一个真实样式段；GCB与当前nonce仍由接收方再次核验。
    pub fn respond(
        self,
        coverage: Range<usize>,
        font: MarkdownLayoutFont,
        style: MarkdownLineComposeStyle,
    ) -> Result<MarkdownLineStyleResponse> {
        self.text_scope()?; // 没有 Unicode/Font 权限的结构请求不得产生普通字体响应。
        if coverage.start != self.range.start
            || coverage.end <= coverage.start
            || coverage.end > self.range.end
        {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        Ok(MarkdownLineStyleResponse {
            request: self,
            coverage,
            font,
            style,
            registration: None,
            #[cfg(feature = "paged-shaping-frontier")]
            preferred_registration: None,
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            root_leaf_boundary: None,
            paragraph_inset_26_6: 0,
        })
    }

    /// 完整组合器使用实际palette仍持有的注册值；弱票不能在字体已归还后冒充可画字形。
    pub fn respond_registered(
        self,
        coverage: Range<usize>,
        font: &MarkdownLineRegisteredFont,
        style: MarkdownLineComposeStyle,
    ) -> Result<MarkdownLineStyleResponse> {
        let mut response = self.respond(coverage, font.font().clone(), style)?;
        response.registration = Some(font.registration());
        Ok(response)
    }
}

/// 没有公开字段；外部只能消费实际request产生响应，不能凭source range构造新item。
pub struct MarkdownLineStyleResponse {
    request: MarkdownLineStyleRequest,
    coverage: Range<usize>,
    font: MarkdownLayoutFont,
    style: MarkdownLineComposeStyle,
    registration: Option<MarkdownLineFontRegistration>,
    #[cfg(feature = "paged-shaping-frontier")]
    preferred_registration: Option<MarkdownLineFontRegistration>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    root_leaf_boundary: Option<crate::VerifiedMarkdownRootLeafStyleBoundary>,
    pub(super) paragraph_inset_26_6: i32,
}

impl MarkdownLineStyleResponse {
    /// 只附同一个真实请求签出的相邻语义票；短coverage本身没有闭项权。
    /// 后继仍须按原预算取得右侧完整GCB，并实测另一Font，颜色变化不能强行断开连字。
    ///
    /// # Errors
    /// 异源或另一次请求即使坐标相同也拒绝；票的左样式和本响应必须精确一致。
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub fn with_root_leaf_style_boundary(
        mut self,
        boundary: crate::VerifiedMarkdownRootLeafStyleBoundary,
    ) -> Result<Self> {
        if !Arc::ptr_eq(&self.request.nonce, &boundary.request().nonce)
            || self.request.source != *boundary.source()
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if self.coverage.end != boundary.left().utf16_range.end
            || self.coverage.start < boundary.left().utf16_range.start
            || self.style.presentation != boundary.left().styles
        {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        self.root_leaf_boundary = Some(boundary);
        Ok(self)
    }

    /// 原平台语义base随同本次nonce封存；这是字体选择的真实优先身份，不是范围或闭项许可。
    /// 选中fallback后，后继完整GCB仍须重新实际试base，不能凭fallback也覆盖空格而吞掉优先字体。
    ///
    /// # Errors
    /// 原注册已过期或绑定字号与本样式不一致时拒绝，不按同名字体重新注册替代权限。
    #[cfg(feature = "paged-shaping-frontier")]
    pub fn with_preferred_registered_font(
        mut self,
        preferred: &MarkdownLineRegisteredFont,
    ) -> Result<Self> {
        let registration = preferred.registration();
        let actual = registration.lease()?;
        if actual
            .font()
            .shaping_point_size()
            .is_some_and(|points| (points as f32).to_bits() != self.style.points.to_bits())
        {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        self.preferred_registration = Some(registration);
        Ok(self)
    }

    /// 左右段落缩进随原 nonce 响应封存；字面调用不设置该值，保持原零缩进行为。
    pub fn with_paragraph_inset(mut self, inset_26_6: i32) -> Result<Self> {
        if inset_26_6 < 0 {
            return Err(MarkdownLineComposeError::InvalidRequest);
        } // 负可用宽度不能等到绘图时裁掉；实际行宽在原测量入口继续核验。
        self.paragraph_inset_26_6 = inset_26_6;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MarkdownLineItemFacts {
    pub source_utf16: Range<usize>,
    pub paragraph_utf16: Range<usize>,
    pub script: [u8; 4],
    pub level: u8,
    pub direction: MarkdownShapingDirection,
    pub points: f32,
    pub beginning_of_text: bool,
    pub end_of_text: bool,
}

/// worker局部唯一真实job。分项Ready没有承诺软行、Native几何或字体回退已经完成。
pub struct MarkdownLineItem {
    pub(super) job: super::worker_resources::ShapeResourceOwner,
    pub(super) source: MarkdownLayoutTextSource,
    pub(super) authority: MarkdownLayoutTextAuthority,
    pub(super) font: MarkdownLayoutFontAuthority,
    pub(super) registration: Option<MarkdownLineFontRegistration>,
    // 只保上游已校验的小样式，不保正文；历史字形遇到真正unsafe边界时须用原选项重形。
    pub(super) replay_style: MarkdownLineComposeStyle,
    facts: MarkdownLineItemFacts,
    shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
}
impl MarkdownLineItem {
    /// Store 只借原分项签出的两张 scalar shaping seam；None 仍须核真实 GCB。
    /// 这两端不授予 LineEnd/caret；下游不得将视觉 part 的边缘冒充编辑边界。
    pub(in crate::markdown_line_composer) fn shaping_seams(
        &self,
    ) -> &[Option<ResolvedBidiShapingSeam>; 2] {
        &self.shaping_seams
    }
    pub fn facts(&self) -> &MarkdownLineItemFacts {
        &self.facts
    }
    pub fn font_registration(&self) -> Option<&MarkdownLineFontRegistration> {
        self.registration.as_ref()
    }
    /// 原job已经校验的小样式；后继unsafe重形须复用它，不重新查询较新的活文档样式。
    pub fn shaping_style(&self) -> &MarkdownLineComposeStyle {
        &self.replay_style
    }
    pub fn glyph(&mut self, ordinal: usize) -> Result<Option<MarkdownShapedGlyph>> {
        self.source.health()?;
        Ok(self.job.glyph(&self.authority, &self.font, ordinal)?)
    }
}

#[derive(Clone)]
pub enum MarkdownLineItemAnswer {
    Yielded,
    NeedStyle(MarkdownLineStyleRequest),
    ItemReady(MarkdownLineItemFacts),
    EndOfSource,
    NeedsControl(MarkdownLineInlineControl),
    #[cfg(feature = "paged-shaping-frontier")]
    NeedsFrontier(MarkdownLineFrontierReason),
}
#[derive(Clone)]
pub struct MarkdownLineItemStep {
    pub bytes: usize,
    /// 前沿child构造的原邻接上下文读取独立计数，不冒充中心输出或步进输入配额。
    #[cfg(feature = "paged-shaping-frontier")]
    pub context_bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub metadata_units: usize,
    pub shapes: usize,
    pub answer: MarkdownLineItemAnswer,
}

#[derive(Clone, Copy)]
enum Stage {
    Bidi,
    Dependencies,
    ItemStart,
    ItemScan,
    Style,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    TableDirectory,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    TableRow,
    #[cfg(feature = "paged-shaping-frontier")]
    StyleEndpoint,
    Input,
    #[cfg(feature = "paged-shaping-frontier")]
    ClosedInput,
    #[cfg(feature = "paged-shaping-frontier")]
    ClosedFonts,
    Shape,
    Ready,
    End,
    Control,
    #[cfg(feature = "paged-shaping-frontier")]
    Frontier,
}
struct Scan {
    start: usize,
    next: usize,
    limit: usize,
    paragraph: Range<usize>,
    script: [u8; 4],
    level: u8,
    shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
    last_bidi_unit: Option<crate::markdown_bidi::MarkdownBidiShapingUnit>,
    #[cfg(feature = "paged-shaping-frontier")]
    verified_endpoints: usize,
    #[cfg(feature = "paged-shaping-frontier")]
    endpoint_proofs: [Option<CertifiedShapingEndpoint>; 2],
}
struct Payload {
    source: MarkdownLayoutTextSource,
    folds: super::folds::Plan,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    tables: Option<crate::markdown_layout_source::table_rows::TablePlanWriter>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    table_checked_at: Option<usize>,
    bidi: Option<MarkdownBidiParagraph>,
    bidi_preparation: Option<MarkdownBidiPrepareJob>,
    #[cfg(feature = "paged-shaping-frontier")]
    bidi_visible_preparation: crate::markdown_bidi::MarkdownBidiVisiblePreparation,
    scripts: MarkdownScriptJob,
    breaks: MarkdownLayoutBreakJob,
    scripts_ready: bool,
    breaks_ready: bool,
    script_first: bool,
    cursor: usize,
    // 上一原 item 真消费完的右 seam 才能授权下一分项的相同起点；不凭裸 cursor 复建。
    next_shaping_seam: Option<ResolvedBidiShapingSeam>,
    scan: Option<Scan>,
    request: Option<MarkdownLineStyleRequest>,
    // 内部缩短coverage的响应只有一份；等待实际metadata量子时不持Native借用或新正文。
    #[cfg(feature = "paged-shaping-frontier")]
    pending_style: Option<MarkdownLineStyleResponse>,
    // 只有仍等待原样式响应的协调器持强边；外部请求副本不能延长字体试次权限。
    font_probe_scope: Option<Arc<()>>,
    item: Option<MarkdownLineItem>,
    #[cfg(feature = "paged-shaping-frontier")]
    closed_input: Option<closed_item::PendingItem>,
    #[cfg(feature = "paged-shaping-frontier")]
    closed_fonts: Option<closed_item::FontRuns>,
    stage: Stage,
    control: Option<MarkdownLineInlineControl>,
    resources: super::worker_resources::ComposerResourceScope,
    #[cfg(feature = "paged-shaping-frontier")]
    frontier: Option<frontier::PendingFrontier>,
    #[cfg(feature = "paged-shaping-frontier")]
    frontier_enabled: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    open_script: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    open_probe: Option<prefix::OpenPrefixProbe>,
    #[cfg(feature = "paged-shaping-frontier")]
    closed_script_at: Option<usize>,
    #[cfg(feature = "paged-shaping-frontier")]
    script_source_demand: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    frontier_work: MarkdownLineFrontierWork,
}

/// 只保一份待处理item；不会为长文建立`Vec<Job>`、`Vec<run>`或完整正文副本。
pub struct MarkdownLineItemCoordinator {
    payload: Option<Payload>,
    authority: MarkdownLayoutTextAuthority,
    cancellation: MarkdownLineComposeCancellation,
    backing: Option<crate::MarkdownOriginalBackingObserver>,
    resources: super::worker_resources::ComposerResourceScope,
}
impl MarkdownLineItemCoordinator {
    pub fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        cancellation: MarkdownLineComposeCancellation,
    ) -> Result<Self> {
        source.health()?;
        if !source.permits(authority) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if cancellation.is_cancelled() {
            return Err(MarkdownLineComposeError::Cancelled);
        }
        let backing = source.original_backing_observer();
        let scripts =
            MarkdownScriptJob::new(source.clone(), authority, cancellation.script.clone())?;
        let breaks = MarkdownLayoutBreakJob::new(source.clone(), authority)?;
        let resources = super::worker_resources::ComposerResourceScope::new(&source);
        resources.register_dependencies(scripts.resource_observer(), breaks.resource_observer());
        Ok(Self {
            payload: Some(Payload {
                source,
                folds: super::folds::Plan::default(),
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                tables: None,
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                table_checked_at: None,
                bidi: None,
                bidi_preparation: None,
                #[cfg(feature = "paged-shaping-frontier")]
                bidi_visible_preparation: Default::default(),
                scripts,
                breaks,
                scripts_ready: false,
                breaks_ready: false,
                script_first: true,
                cursor: 0,
                next_shaping_seam: None,
                scan: None,
                request: None,
                #[cfg(feature = "paged-shaping-frontier")]
                pending_style: None,
                font_probe_scope: None,
                item: None,
                #[cfg(feature = "paged-shaping-frontier")]
                closed_input: None,
                #[cfg(feature = "paged-shaping-frontier")]
                closed_fonts: None,
                stage: Stage::Bidi,
                control: None,
                resources: resources.clone(),
                #[cfg(feature = "paged-shaping-frontier")]
                frontier: None,
                #[cfg(feature = "paged-shaping-frontier")]
                frontier_enabled: false,
                #[cfg(feature = "paged-shaping-frontier")]
                open_script: false,
                #[cfg(feature = "paged-shaping-frontier")]
                open_probe: None,
                #[cfg(feature = "paged-shaping-frontier")]
                closed_script_at: None,
                #[cfg(feature = "paged-shaping-frontier")]
                script_source_demand: false,
                #[cfg(feature = "paged-shaping-frontier")]
                frontier_work: MarkdownLineFrontierWork::default(),
            }),
            authority: authority.clone(),
            cancellation,
            backing,
            resources,
        })
    }

    pub(super) fn resource_scope(&self) -> super::worker_resources::ComposerResourceScope {
        self.resources.clone()
    }

    /// 原词组/删除共享此有限 proof，不能把可见序列的相邻端点当作完整原 GCB 读取隐区。
    pub(in crate::markdown_line_composer) fn fold_plan(
        &self,
    ) -> Option<crate::markdown_layout_source::folds::FoldPlan> {
        self.payload
            .as_ref()
            .and_then(|payload| payload.folds.shared())
    }

    /// 折叠是构造期的显示意图；开始消费原 Source 后不能改写尚在用的控制范围。
    pub(super) fn install_folded_ranges(&mut self, ranges: &[Range<usize>]) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !matches!(payload.stage, Stage::Bidi) || payload.cursor != 0 {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            let plan = super::folds::Plan::new(&payload.source, ranges)?;
            if let Some(shared) = plan.shared() {
                payload.scripts.install_fold_plan(shared.clone())?;
                payload.breaks.install_fold_plan(shared)?;
            }
            payload.folds = plan;
            Ok(())
        })();
        self.finish(result)
    }

    /// 同一冻结源的显隐选择变化只撤销样式项；保留已收费的UBA、Script和GCB/LB索引。
    /// 旧Font试次须由Native先取消并归还，此处撤销nonce后不能再接受旧响应。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn reset_styles(&mut self) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            // 原样式的实际整形owner先退出；Source及语言索引保持同一allocation，不重新扫描原mmap。
            payload.font_probe_scope = None;
            payload.request = None;
            payload.pending_style = None;
            payload.item = None;
            payload.closed_input = None;
            payload.closed_fonts = None;
            payload.frontier = None;
            payload.scan = None;
            payload.control = None;
            payload.open_probe = None;
            payload.closed_script_at = None;
            payload.open_script = false;
            payload.script_source_demand = false;
            payload.cursor = 0;
            payload.next_shaping_seam = None;
            payload.stage = Stage::Bidi; // 已有Bidi直接复用；尚未完成的原准备续体沿原量子继续。

            // 新glyph writer有独立释放收据；复用依赖的实际observer，不能把旧writer登记覆盖成“已释放”。
            let resources = super::worker_resources::ComposerResourceScope::new(&payload.source);
            resources.register_dependencies(
                payload.scripts.resource_observer(),
                payload.breaks.resource_observer(),
            );
            #[cfg(feature = "s2-table-window-prototype")]
            if let Some(tables) = &mut payload.tables {
                tables.rewind();
                payload.table_checked_at = None;
                resources.register_table_directory(tables.plan().resource_observer());
            }
            if let Some(bidi) = &payload.bidi {
                resources.register_bidi(bidi.resource_observer());
            } else if let Some(preparation) = &payload.bidi_preparation {
                resources.register_bidi(preparation.resource_observer());
            } // 尚未创建UBA时不制造observer，真正创建仍由原bidi_preparation登记。
            payload.resources = resources.clone();
            self.resources = resources;
            Ok(())
        })();
        self.finish(result)
    }

    #[cfg(all(test, feature = "paged-shaping-frontier"))]
    /// 实际语言索引的累计源读取只用于重排回归；getter自身不推进任何Job。
    pub(super) fn style_dependency_source_reads(&mut self) -> (usize, usize) {
        let payload = self.payload.as_mut().expect("原索引仍由Coordinator持有");
        (
            payload.scripts.storage_profile().source_bytes_read,
            payload.breaks.line_source_bytes_read_for_test(),
        )
    }

    fn guard(&self) -> Result<()> {
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
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        payload.source.health()?;
        if !payload.source.permits(&self.authority) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if self.cancellation.is_cancelled() {
            return Err(MarkdownLineComposeError::Cancelled);
        }
        Ok(())
    }

    fn finish<T>(&mut self, answer: Result<T>) -> Result<T> {
        let answer = match self.guard() {
            Err(error) => {
                drop(answer); // 后置撤权时先归还尚未交付的实际item，再核析构产生的首I/O事实。
                Err(error)
            }
            Ok(()) => answer,
        };
        if answer.is_err() {
            drop(self.payload.take()); // 源、三个索引和实际job先归还，外部请求壳不能重新启用此owner。
        }
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        answer
    }

    /// UBA整源元数据计算是可取消的worker阶段，不宣称一次调用具有限主线程耗时。
    pub fn step(&mut self, budget: MarkdownLineComposeBudget) -> Result<MarkdownLineItemStep> {
        let result = self.advance(budget);
        self.finish(result)
    }

    /// 词组移动只推进原 Bidi 准备；整源证书和逐可见段职责明确区分，不顺带推进 GCB。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn prepare_word_move(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<(MarkdownLineItemStep, Option<super::MarkdownWordMoveScope>)> {
        let result = (|| {
            self.guard()?;
            let mut step = MarkdownLineItemStep {
                bytes: 0,
                context_bytes: 0,
                scalars: 0,
                records: 0,
                metadata_units: 0,
                shapes: 0,
                answer: MarkdownLineItemAnswer::Yielded,
            };
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if payload.bidi.is_none() {
                bidi_preparation::advance(
                    payload,
                    &self.authority,
                    &self.cancellation,
                    budget,
                    &mut step,
                )?;
            } // 已发布的原Paragraph直接借其事实，不为每个按键重复扫描或另建分析器。
            let proved = self.word_move_scope()?;
            Ok((step, proved))
        })();
        self.finish(result) // 能力结果不是样式或位置；只有真实 Source/取消/I/O 失败才沿原关闭边退休。
    }

    /// 已准备表示决定后续真实消费者；Visible 只允许逐 GCB 求原完整 B 段的真实零级顺序。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_line_composer) fn word_move_scope(
        &self,
    ) -> Result<Option<super::MarkdownWordMoveScope>> {
        self.guard()?;
        let Some(bidi) = self
            .payload
            .as_ref()
            .and_then(|payload| payload.bidi.as_ref())
        else {
            return Ok(None);
        };
        let scope = if bidi.whole_source_uniform_zero(&self.authority)? {
            super::MarkdownWordMoveScope::WholeUniformZero
        } else if bidi.uses_visible_paragraph_scopes(&self.authority)? {
            super::MarkdownWordMoveScope::VisibleParagraphScopes
        } else {
            super::MarkdownWordMoveScope::Unsupported
        };
        Ok(Some(scope))
    }

    /// 同一 Item 的原在途 owner 准备实际 GCB 所属完整 B 段；任何工作都结束本回合，不再兼花分类量子。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_line_composer) fn prepare_word_cluster(
        &mut self,
        range: Range<usize>,
        budget: MarkdownLineComposeBudget,
    ) -> Result<(MarkdownLineItemStep, Option<(bool, usize)>)> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let bidi = payload
                .bidi
                .as_mut()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            let progress = bidi.prepare_layout_paragraph_range(
                &self.authority,
                range.start,
                MarkdownBidiPrepareBudget {
                    bytes: budget.bytes,
                    scalars: budget.scalars,
                    metadata_ops: budget.metadata_units,
                },
                &mut payload.bidi_visible_preparation,
            )?;
            let mut step = MarkdownLineItemStep {
                bytes: progress.bytes,
                context_bytes: 0,
                scalars: progress.scalars,
                records: 0,
                metadata_units: progress.metadata,
                shapes: 0,
                answer: MarkdownLineItemAnswer::Yielded,
            };
            if !progress.ready || step.bytes != 0 || step.scalars != 0 || step.metadata_units != 0 {
                return Ok((step, None));
            } // 刚完成的真实段在下一回合借证书；原 4KiB 量子不会被两种角色重复使用。
            let (proof, through) = bidi.prepare_word_order(
                &self.authority,
                range,
                budget.records.min(budget.metadata_units),
            )?;
            step.records = proof.records;
            step.metadata_units = proof.metadata_units;
            Ok((step, proof.permitted.map(|permitted| (permitted, through))))
        })();
        self.finish(result)
    }

    /// 原布局游标仍在暂停中的真实 Item 起点；Word 只保存该标量，不保额外 B 段或 Source。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_line_composer) fn word_layout_position(&self) -> Result<usize> {
        self.guard()?;
        Ok(self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .cursor)
    }

    /// Word 可能换走两槽中的旧 B 段；发布终点前在同一在途 owner 中恢复原布局段，避免 Style/Scan 借失效段。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_line_composer) fn restore_word_layout_context(
        &mut self,
        position: usize,
        budget: MarkdownLineComposeBudget,
    ) -> Result<(MarkdownLineItemStep, bool)> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let progress = payload
                .bidi
                .as_mut()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                .prepare_layout_paragraph_range(
                    &self.authority,
                    position,
                    MarkdownBidiPrepareBudget {
                        bytes: budget.bytes,
                        scalars: budget.scalars,
                        metadata_ops: budget.metadata_units,
                    },
                    &mut payload.bidi_visible_preparation,
                )?;
            Ok((
                MarkdownLineItemStep {
                    bytes: progress.bytes,
                    context_bytes: 0,
                    scalars: progress.scalars,
                    records: 0,
                    metadata_units: progress.metadata,
                    shapes: 0,
                    answer: MarkdownLineItemAnswer::Yielded,
                },
                progress.ready,
            ))
        })();
        self.finish(result)
    }

    fn advance(&mut self, budget: MarkdownLineComposeBudget) -> Result<MarkdownLineItemStep> {
        self.guard()?;
        if budget.bytes == 0
            || budget.scalars == 0
            || budget.records == 0
            || budget.metadata_units == 0
        {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .breaks
            .take_line_source_demand(&self.authority)?
        {
            // 直接使用Coordinator的调用方也只在下一量子推进；Engine会在上个回合
            // 取走同一需求并调度，因此两种驱动方式不会把一次等待重复计费。
            return self.advance_pending_line_breaks(budget);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .script_source_demand
        {
            return self.advance_pending_scripts(budget);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .breaks
            .take_grapheme_source_demand(&self.authority)?
        {
            return self.advance_pending_graphemes(budget);
        }
        let mut step = MarkdownLineItemStep {
            bytes: 0,
            #[cfg(feature = "paged-shaping-frontier")]
            context_bytes: 0,
            scalars: 0,
            records: 0,
            metadata_units: 0,
            shapes: 0,
            answer: MarkdownLineItemAnswer::Yielded,
        };
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        #[cfg(all(
            feature = "s2-table-window-prototype",
            feature = "paged-shaping-frontier"
        ))]
        if matches!(payload.stage, Stage::Bidi | Stage::ItemStart)
            && table_row::known_row_request(payload, &self.authority, &mut step)?
        {
            return Ok(step); // 真实目录访问独占此量子，表格行首先交语义来源，不能先进入 Body UBA。
        }
        match payload.stage {
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            Stage::TableDirectory | Stage::TableRow => {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }

            Stage::Bidi => {
                #[cfg(feature = "paged-shaping-frontier")]
                if payload.bidi.is_some() {
                    if paragraph::prepare(payload, &self.authority, budget, &mut step)? {
                        payload.stage = Stage::Dependencies;
                    }
                    return Ok(step); // 真B段范围先独占原量子；后继Script/GCB不得再次消费同一预算。
                }
                bidi_preparation::advance(
                    payload,
                    &self.authority,
                    &self.cancellation,
                    budget,
                    &mut step,
                )?;
                #[cfg(not(feature = "paged-shaping-frontier"))]
                if payload.bidi.is_some() {
                    payload.stage = Stage::Dependencies;
                }
            }
            Stage::Dependencies => {
                // 原ASCII继续消费完整Dense/Script快路；普通LTR Unicode只在完整Uniform
                // 无控制证明下开始查询原稳定SCX/GCB。没有证明仍轮换原两个完整依赖。
                #[cfg(feature = "paged-shaping-frontier")]
                let prefix = payload.frontier_enabled
                    && (payload.breaks.ascii_prefix_ready(&self.authority)?
                        || prefix::unicode_prefix_allowed(payload, &self.authority)?);
                #[cfg(feature = "paged-shaping-frontier")]
                let start_from_prefix = if payload
                    .bidi
                    .as_ref()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                    .uses_prepared_paragraphs()
                {
                    // 原Source本身不超过一次既有借用域时保完整SCX批次；只有长源
                    // 才按当前真B段开放准备。这个额度不签SCX、Font、shape或行边界。
                    payload.source.byte_len() > crate::MARKDOWN_SCRIPT_SOURCE_BYTES
                } else {
                    prefix
                };
                #[cfg(feature = "paged-shaping-frontier")]
                if payload.frontier_enabled
                    && start_from_prefix
                    && !payload.breaks.ascii_prefix_ready(&self.authority)?
                {
                    payload.stage = Stage::ItemStart;
                    return Ok(step); // 原无B前缀手感照旧；长多段的真实控制仍先交initial_inline_control。
                }
                #[cfg(not(feature = "paged-shaping-frontier"))]
                let prefix = false;
                let scripts = !payload.scripts_ready
                    && (prefix || payload.script_first || payload.breaks_ready);
                if scripts {
                    let progress = payload.scripts.step(
                        &self.authority,
                        MarkdownScriptBudget {
                            bytes: budget.bytes.min(4096),
                            scalars: budget.scalars,
                            records: budget.records,
                            stack_ops: budget.records,
                        },
                    )?;
                    payload.scripts_ready = progress.status == crate::MarkdownScriptStatus::Ready;
                    step.bytes = progress.bytes;
                    step.scalars = progress.scalars;
                    step.records = progress.records;
                } else if !payload.breaks_ready && !prefix {
                    let progress = payload.breaks.step(
                        &self.authority,
                        MarkdownLineBreakBudget {
                            bytes: budget.bytes.min(4096),
                            scalars: budget.scalars,
                            records: budget.records,
                        },
                    )?;
                    payload.breaks_ready = progress.ready;
                    step.bytes = progress.bytes;
                    step.scalars = progress.scalars;
                    step.records = progress.records;
                }
                payload.script_first = !payload.script_first;
                if payload.scripts_ready && (payload.breaks_ready || prefix) {
                    payload.stage = Stage::ItemStart;
                }
            }
            Stage::ItemStart => {
                #[cfg(feature = "paged-shaping-frontier")]
                if !paragraph::prepare(payload, &self.authority, budget, &mut step)?
                    || step.bytes != 0
                    || step.scalars != 0
                    || step.metadata_units != 0
                {
                    return Ok(step); // 跨段先结束真实叶/目录查询，Ready同样不在这一量子再推进分项。
                }
                if let Some(range) = payload.folds.at(payload.cursor) {
                    // 隐藏行只交原控制 nonce，不进入 Script/Font/HarfBuzz；原源索引仍分步推进。
                    let control = MarkdownLineInlineControl::new(
                        self.authority.clone(),
                        range,
                        MarkdownLineInlineControlKind::FoldedSection,
                    );
                    payload.control = Some(control.clone());
                    payload.stage = Stage::Control;
                    step.answer = MarkdownLineItemAnswer::NeedsControl(control);
                } else if payload.cursor == payload.source.utf16_len() {
                    // 这里只报告分项输入的真实EOF。末尾空视觉行仍由后继行组合器按实际分隔符处理。
                    payload.stage = Stage::End;
                    step.answer = MarkdownLineItemAnswer::EndOfSource;
                } else if let Some(control) = initial_inline_control(
                    &payload.source,
                    &self.authority,
                    payload.bidi.as_ref(),
                    payload.cursor,
                )? {
                    // CRLF按真实两个源unit作为一项控制，绝不在其GCB内部签item或伪换段。
                    payload.control = Some(control.clone());
                    payload.stage = Stage::Control;
                    step.answer = MarkdownLineItemAnswer::NeedsControl(control);
                } else {
                    // 完成的Script run、真实UBA段和实际level必须来自同Source；查询不能补捕较新文档。
                    #[cfg(feature = "paged-shaping-frontier")]
                    let script_answer = {
                        step.metadata_units = 1;
                        prefix::script_at(payload, &self.authority)?
                    };
                    #[cfg(feature = "paged-shaping-frontier")]
                    let script = match script_answer {
                        Some(script) => script,
                        None => return Ok(step),
                    };
                    #[cfg(not(feature = "paged-shaping-frontier"))]
                    let script = match payload.scripts.run_at(
                        &self.authority,
                        MarkdownScriptUnit::Utf16,
                        payload.cursor,
                    )? {
                        MarkdownScriptAnswer::Known(Some(run))
                            if run.start_utf16 <= payload.cursor
                                && payload.cursor < run.end_utf16 =>
                        {
                            run
                        }
                        _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
                    };
                    let bidi = payload
                        .bidi
                        .as_mut()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                    bidi.require_layout_source(&self.authority)?;
                    let first_bidi_unit =
                        bidi.shaping_unit_at_utf16(&self.authority, payload.cursor)?;
                    #[cfg(feature = "paged-shaping-frontier")]
                    let (paragraph, level) = {
                        let end = payload
                            .cursor
                            .checked_add(1)
                            .ok_or(MarkdownLineComposeError::Overflow)?;
                        if let Some(span) =
                            bidi.uniform_control_free_span(&self.authority, payload.cursor..end)?
                        {
                            // 原完整单段证明已绑定分析范围和level0；起点只消费这两个事实，
                            // 不为checked_range的标量定位再次借正文，也不以此一unit当完整段。
                            // 原Script交集和下方GCB准入仍独立决定可交付item范围。
                            (span.paragraph_range(), span.level())
                        } else {
                            // 没有完整无控制证明时保持原段表/标量端点查询，不以level0猜范围。
                            (
                                bidi.paragraph_range_at_utf16(payload.cursor)?
                                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                                match &first_bidi_unit {
                                    Some(unit) => unit.level,
                                    None => bidi.level_at_utf16(payload.cursor)?,
                                },
                            )
                        }
                    };
                    #[cfg(not(feature = "paged-shaping-frontier"))]
                    let (paragraph, level) = (
                        bidi.paragraph_range_at_utf16(payload.cursor)?
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                        match &first_bidi_unit {
                            Some(unit) => unit.level,
                            None => bidi.level_at_utf16(payload.cursor)?,
                        },
                    );
                    payload.scan = Some(Scan {
                        start: payload.cursor,
                        next: payload
                            .cursor
                            .checked_add(1)
                            .ok_or(MarkdownLineComposeError::Overflow)?,
                        limit: script.end_utf16.min(paragraph.end),
                        paragraph,
                        script: script.script.iso15924_tag(),
                        level,
                        shaping_seams: [payload.next_shaping_seam.clone(), None],
                        last_bidi_unit: first_bidi_unit,
                        #[cfg(feature = "paged-shaping-frontier")]
                        verified_endpoints: 0,
                        #[cfg(feature = "paged-shaping-frontier")]
                        endpoint_proofs: [None, None],
                    });
                    step.metadata_units = 1; // 起点消费原段/level证明或原查询，均计一次真实元数据工作。
                    payload.stage = Stage::ItemScan;
                }
            }
            Stage::ItemScan => {
                // 逐UTF16元数据确定同level的实际末端；低代理项只读原重复level记录，
                // 只有完整GCB端点才会在下面成为可交付范围，不能把配额边界当新run。
                let scan = payload
                    .scan
                    .as_mut()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let bidi = payload
                    .bidi
                    .as_mut()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let mut complete = scan.next == scan.limit || scan.shaping_seams[1].is_some();
                #[cfg(feature = "paged-shaping-frontier")]
                if !complete
                    && let Some(span) =
                        bidi.uniform_control_free_span(&self.authority, scan.start..scan.limit)?
                {
                    // 原UBA已在完整扫描中证明此分析范围每单位level0且没有六种控制；
                    // 消费同Source真实Script/paragraph交集，不再逐UTF16重借中文/emoji正文。
                    // span只借原Paragraph且不证明端点；下方原GCB检查仍决定能否交付。
                    if scan.level != span.level() || scan.limit > payload.source.utf16_len() {
                        return Err(MarkdownLineComposeError::InconsistentCoverage);
                    }
                    scan.next = span.source_range().end;
                    complete = true;
                    step.metadata_units = 1;
                }
                #[cfg(feature = "paged-shaping-frontier")]
                if !complete
                    && payload
                        .source
                        .is_printable_ascii_range(0..payload.source.utf16_len())?
                {
                    // 保留原ASCII聚合路径；原冻结root在建叶/编辑时已验证全源U+0020..007E；
                    // Auto 基方向下它们的已解 UBA level 全为0，且没有 inline control。
                    // 直接消费原 Script/paragraph 的真实上界，避免为首屏重读整个超长项。
                    // 该证明只在本次纯 Core 借用内使用；外层前后 guard 仍核同 Source 健康，
                    // 不按字符抽样、generation 或已显示前缀推断未读尾部的性质。
                    if scan.level != 0 || scan.limit > payload.source.utf16_len() {
                        return Err(MarkdownLineComposeError::InconsistentCoverage);
                    }
                    scan.next = scan.limit;
                    complete = true;
                    step.metadata_units = 1; // 固定聚合查询也收费；没有逐 UTF16 level/正文探测。
                }
                for _ in 0..budget.metadata_units.min(METADATA_UNITS) {
                    if complete {
                        break;
                    }
                    if inline_control(&payload.source, scan.next)?.is_some() {
                        complete = true; // 控制之前的真实GCB封闭字形项；控制本身留给原下一阶段。
                        break;
                    }
                    let unit = bidi.shaping_unit_at_utf16(&self.authority, scan.next)?;
                    let level = match &unit {
                        Some(unit) => unit.level,
                        None => bidi.level_at_utf16(scan.next)?,
                    };
                    if level != scan.level {
                        let left = scan
                            .last_bidi_unit
                            .as_ref()
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                        let right = unit
                            .as_ref()
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                        scan.shaping_seams[1] = Some(ResolvedBidiShapingSeam::from_adjacent_units(
                            &self.authority,
                            left,
                            right,
                        )?);
                        step.metadata_units += 1; // 真正读到不同 level 的那条记录同样收费，不能藏在结束分支。
                        complete = true;
                        break;
                    }
                    scan.last_bidi_unit = unit;
                    scan.next = scan
                        .next
                        .checked_add(1)
                        .ok_or(MarkdownLineComposeError::Overflow)?;
                    step.metadata_units += 1;
                    complete = scan.next == scan.limit;
                }
                if complete {
                    if scan.next <= scan.start {
                        return Err(MarkdownLineComposeError::InconsistentCoverage);
                    }
                    for (ordinal, endpoint) in [scan.start, scan.next].into_iter().enumerate() {
                        let seam = scan.shaping_seams[ordinal].clone();
                        if let Some(seam) = &seam {
                            let (_, actual) = seam.boundary(&self.authority)?;
                            let (left, right) = seam.levels();
                            if actual != endpoint
                                || (ordinal == 0 && right != scan.level)
                                || (ordinal == 1 && left != scan.level)
                            {
                                return Err(MarkdownLineComposeError::InconsistentCoverage);
                            }
                        } // 只借原扫描确实读到的不同 level；script/样式/预算上界不能补签。
                        #[cfg(feature = "paged-shaping-frontier")]
                        {
                            if payload.frontier_enabled {
                                if ordinal < scan.verified_endpoints {
                                    continue;
                                }
                                if step.metadata_units == budget.metadata_units {
                                    return Ok(step);
                                }
                                step.metadata_units += 1;
                            }
                        }
                        #[cfg(not(feature = "paged-shaping-frontier"))]
                        let _ = ordinal;
                        #[cfg(feature = "paged-shaping-frontier")]
                        let (answer, proof) = payload
                            .breaks
                            .certified_grapheme_endpoint(&self.authority, endpoint)?;
                        #[cfg(not(feature = "paged-shaping-frontier"))]
                        let answer = payload.breaks.grapheme_query(
                            &self.authority,
                            endpoint,
                            MarkdownGraphemeQuery::IsBoundary,
                        )?;
                        match answer {
                            MarkdownGraphemeAnswer::Known(Some(boundary))
                                if boundary.utf16 == endpoint =>
                            {
                                #[cfg(feature = "paged-shaping-frontier")]
                                {
                                    scan.verified_endpoints += 1;
                                    scan.endpoint_proofs[ordinal] =
                                        Some(CertifiedShapingEndpoint::Grapheme {
                                            proof: proof.ok_or(
                                                MarkdownLineComposeError::InconsistentCoverage,
                                            )?,
                                            seam,
                                        });
                                }
                            }
                            MarkdownGraphemeAnswer::Known(None) if seam.is_some() => {
                                #[cfg(feature = "paged-shaping-frontier")]
                                {
                                    scan.verified_endpoints += 1;
                                    scan.endpoint_proofs[ordinal] =
                                        Some(CertifiedShapingEndpoint::Bidi(
                                            seam.expect("分支已核原实际 seam"),
                                        ));
                                }
                            }
                            #[cfg(feature = "paged-shaping-frontier")]
                            MarkdownGraphemeAnswer::Pending => return Ok(step),
                            _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
                        }
                    } // 非 GCB 只用于本地 shaping 分项，GCB 目录、caret 和软行机会保持原事实。
                    payload.request = Some(MarkdownLineStyleRequest {
                        nonce: Arc::new(()),
                        source: self.authority.clone(),
                        range: scan.start..scan.next,
                        scope: StyleScope::Text(TextStyleScope {
                            paragraph: scan.paragraph.clone(),
                            script: scan.script,
                            level: scan.level,
                        }),
                        shaping_seams: scan.shaping_seams.clone(),
                        #[cfg(feature = "paged-shaping-frontier")]
                        continuation_font: None,
                        #[cfg(feature = "paged-shaping-frontier")]
                        continuation_bytes: None,
                    });
                    payload.font_probe_scope = Some(Arc::new(()));
                    payload.stage = Stage::Style;
                    step.answer = MarkdownLineItemAnswer::NeedStyle(
                        payload
                            .request
                            .as_ref()
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                            .clone(),
                    );
                }
            }
            Stage::Style => {
                // 调度等待只返回原nonce的副本，不重复创建Font、文件、Source或另一份请求权限。
                step.answer = MarkdownLineItemAnswer::NeedStyle(
                    payload
                        .request
                        .as_ref()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                        .clone(),
                );
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Stage::StyleEndpoint => {
                let end = payload
                    .pending_style
                    .as_ref()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                    .coverage
                    .end;
                step.metadata_units = 1; // 查询实际发生前收费；Pending不会借下一个量子再查一次。
                let (answer, endpoint) = payload
                    .breaks
                    .certified_grapheme_endpoint(&self.authority, end)?;
                match answer {
                    MarkdownGraphemeAnswer::Pending => {} // 原GCB demand由Engine下一独立量子消费。
                    MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == end => {
                        let response = payload
                            .pending_style
                            .take()
                            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                        payload.stage = Stage::Style;
                        install_style(
                            payload,
                            &self.authority,
                            &self.cancellation,
                            response,
                            StyleCoordinates {
                                endpoint: endpoint.map(|proof| {
                                    CertifiedShapingEndpoint::Grapheme { proof, seam: None }
                                }),
                            },
                        )?;
                    }
                    _ => return Err(MarkdownLineComposeError::InvalidRequest),
                }
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Stage::Frontier => {
                step = frontier::advance(payload, &self.authority, &self.cancellation, budget)?;
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Stage::ClosedFonts => {
                step = closed_item::advance_fonts(payload, &self.authority, budget)?;
            }
            #[cfg(feature = "paged-shaping-frontier")]
            Stage::ClosedInput => {
                // queue_style只完成原票交接；邻接上下文在此按同一个bytes/scalars余额
                // 真实读取。context_bytes是本回合bytes的子集，没有同时再读中心正文。
                let progress = payload
                    .closed_input
                    .as_mut()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                    .shaping
                    .step(MarkdownShapingBudget {
                        bytes: budget.bytes.min(4096),
                        scalars: budget.scalars,
                    })?;
                step.bytes = progress.bytes;
                step.context_bytes = progress.bytes;
                step.scalars = progress.scalars;
                if let Some(job) = progress.job {
                    let pending = payload
                        .closed_input
                        .take()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                    payload.item = Some(MarkdownLineItem {
                        job: super::worker_resources::ShapeResourceOwner::new(
                            job,
                            &payload.resources,
                        ),
                        source: payload.source.clone(),
                        authority: self.authority.clone(),
                        font: pending.font,
                        registration: pending.registration,
                        replay_style: pending.replay_style,
                        facts: pending.facts,
                        shaping_seams: pending.shaping_seams,
                    });
                    payload.stage = Stage::Input;
                } // 固定上下文全部准备好才装配唯一Job；下一回合才输入中心正文，预算不复用。
            }
            Stage::Input => {
                // 原job逐批读取同一冻结源；单次原始正文请求仍不超过4KiB，不按item长短扩窗。
                let item = payload
                    .item
                    .as_mut()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                let progress = item.job.step(
                    &self.authority,
                    &item.font,
                    MarkdownShapingBudget {
                        bytes: budget.bytes.min(4096),
                        scalars: budget.scalars,
                    },
                )?;
                step.bytes = progress.bytes;
                step.scalars = progress.scalars;
                if progress.status == MarkdownShapingStatus::AwaitingShape {
                    payload.stage = Stage::Shape;
                }
            }
            Stage::Shape => {
                // 一次真实整形可包含长run的算法工作，必须在worker执行；深层取消沿原位传播。
                let item = payload
                    .item
                    .as_mut()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                item.job.shape(&self.authority, &item.font)?;
                step.shapes = 1;
                #[cfg(feature = "paged-shaping-frontier")]
                if closed_item::inspect_shaped(payload)? {
                    // 首字素试字成功不证明闭项余下内容无缺字；真正入Store前逐条检查原结果。
                    payload.stage = Stage::ClosedFonts;
                    return Ok(step);
                }
                payload.stage = Stage::Ready;
                step.answer = MarkdownLineItemAnswer::ItemReady(
                    payload
                        .item
                        .as_ref()
                        .expect("原完整Job仍在唯一槽内")
                        .facts
                        .clone(),
                );
            }
            Stage::Ready => {
                // 未被消费的唯一item只重复返回有限事实，不复制字形或让较新任务替代原job。
                step.answer = MarkdownLineItemAnswer::ItemReady(
                    payload
                        .item
                        .as_ref()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                        .facts
                        .clone(),
                );
            }
            Stage::Control => {
                step.answer = MarkdownLineItemAnswer::NeedsControl(
                    payload
                        .control
                        .as_ref()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                        .clone(),
                );
            }
            Stage::End => {
                step.answer = MarkdownLineItemAnswer::EndOfSource; // EOF重复查询仍经过原源健康与取消门。
            }
        }
        Ok(step)
    }

    /// 同步兼容入口保留原即时GCB验证；不把这里的查询虚记成后继step预算。
    pub fn provide_style(&mut self, response: MarkdownLineStyleResponse) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            install_style(
                payload,
                &self.authority,
                &self.cancellation,
                response,
                StyleCoordinates::default(),
            )
        })();
        self.finish(result)
    }

    /// 只消费当前真实控制nonce；Font和段落样式来自平台原对象，不创建字符替身。
    pub(super) fn consume_control(
        &mut self,
        response: super::MarkdownLineControlResponse,
    ) -> Result<super::controls::ValidatedControl> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            let request = payload
                .control
                .as_ref()
                .ok_or(MarkdownLineComposeError::InvalidRequest)?;
            if !matches!(payload.stage, Stage::Control) || !request.accepts(&response) {
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            for offset in [request.source_utf16.start, request.source_utf16.end] {
                if !matches!(payload.breaks.grapheme_query(&self.authority, offset, MarkdownGraphemeQuery::IsBoundary)?, MarkdownGraphemeAnswer::Known(Some(actual)) if actual.utf16 == offset)
                {
                    return Err(MarkdownLineComposeError::InconsistentCoverage);
                }
            }
            let paragraph = payload
                .source
                .snapshot_for_bidi()
                .text_checking_paragraph_range(request.source_utf16.clone())?;
            let level = payload
                .bidi
                .as_mut()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                .level_at_utf16(request.source_utf16.start)?;
            let range = request.source_utf16.clone();
            let facts = MarkdownLineItemFacts {
                source_utf16: range.clone(),
                paragraph_utf16: paragraph.clone(),
                script: *b"Zyyy",
                level,
                direction: if level & 1 == 0 {
                    MarkdownShapingDirection::LeftToRight
                } else {
                    MarkdownShapingDirection::RightToLeft
                },
                points: response.points,
                beginning_of_text: range.start == paragraph.start,
                end_of_text: range.end == paragraph.end,
            };
            response.registered.registration().lease()?;
            payload.cursor = range.end;
            payload.control = None;
            payload.stage = Stage::ItemStart;
            Ok(super::controls::ValidatedControl::from_response(
                response, facts,
            ))
        })();
        self.finish(result)
    }

    /// 组合worker只借原已认证依赖；开放模式仍由每个实际查询给出Known或真实Pending。
    pub(super) fn dependencies(
        &mut self,
    ) -> Result<(&mut MarkdownBidiParagraph, &mut MarkdownLayoutBreakJob)> {
        self.guard()?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        #[cfg(feature = "paged-shaping-frontier")]
        let prefix = payload.frontier_enabled
            && (payload
                .bidi
                .as_ref()
                .is_some_and(MarkdownBidiParagraph::uses_prepared_paragraphs)
                || payload.breaks.ascii_prefix_ready(&self.authority)?
                || prefix::unicode_prefix_allowed(payload, &self.authority)?);
        #[cfg(not(feature = "paged-shaping-frontier"))]
        let prefix = false;
        if (!payload.scripts_ready && !prefix) || (!payload.breaks_ready && !prefix) {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        // 真B段的控制起点同样可查询原LB/GCB的Pending；这里只借原依赖，不授无控制或SCX范围。
        Ok((
            payload
                .bidi
                .as_mut()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
            &mut payload.breaks,
        ))
    }

    /// 逻辑词组删除只借本worker原GCB依赖，不要求字体、脚本或视觉行已经完成。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_line_composer) fn word_breaks(
        &mut self,
    ) -> Result<&mut MarkdownLayoutBreakJob> {
        self.guard()?;
        Ok(&mut self
            .payload
            .as_mut()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .breaks)
    }

    /// 只取实际LB查询的等待；GCB/cluster未完成或候选配额用完不会请求源扫描。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn take_line_break_demand(&mut self) -> Result<bool> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            Ok(payload.breaks.take_line_source_demand(&self.authority)?)
        })();
        self.finish(result)
    }

    /// 消费方已结束本量子后，单独推进同一原LBjob；保留Item/Style/Measure的原身份与游标。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn advance_pending_line_breaks(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownLineItemStep> {
        self.advance_pending_break_dependency(budget, false)
    }

    /// 移出唯一Ready item供同worker后继行组合消费；不会把item末尾签成软行末尾。
    pub fn take_item(&mut self) -> Result<MarkdownLineItem> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !matches!(payload.stage, Stage::Ready) {
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            let mut item = payload
                .item
                .take()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            let profile = item.job.profile()?;
            item.job
                .glyph(&self.authority, &item.font, profile.glyph_count)?;
            if item.facts.source_utf16.start != payload.cursor {
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            #[cfg(feature = "paged-shaping-frontier")]
            frontier::lend_font_item(payload, &self.authority, &mut item)?;
            #[cfg(feature = "paged-shaping-frontier")]
            closed_item::lend_font_item(payload, &item)?;
            // 新Font段登记的是这一个真实Ready child；原Frontier在其实际归还前不能越过它。
            payload.cursor = item.facts.source_utf16.end;
            payload.next_shaping_seam = item.shaping_seams[1].clone();
            #[cfg(feature = "paged-shaping-frontier")]
            if payload.closed_fonts.is_some() {
                payload.stage = Stage::ClosedFonts;
            } else if payload.frontier.is_some() {
                payload.stage = Stage::Frontier;
            } else {
                payload.stage = Stage::ItemStart;
            }
            #[cfg(not(feature = "paged-shaping-frontier"))]
            {
                payload.stage = Stage::ItemStart;
            }
            Ok(item)
        })();
        self.finish(result)
    }
}

/// 只有内部已收费端点可以跳过同步查询；空值保持原公开入口的准确验证行为。
#[derive(Default)]
struct StyleCoordinates {
    #[cfg(feature = "paged-shaping-frontier")]
    endpoint: Option<CertifiedShapingEndpoint>,
}

fn install_style(
    payload: &mut Payload,
    authority: &MarkdownLayoutTextAuthority,
    cancellation: &MarkdownLineComposeCancellation,
    mut response: MarkdownLineStyleResponse,
    coordinates: StyleCoordinates,
) -> Result<()> {
    #[cfg(not(feature = "paged-shaping-frontier"))]
    let _ = coordinates;
    let request = payload
        .request
        .as_ref()
        .ok_or(MarkdownLineComposeError::InvalidRequest)?;
    if !matches!(payload.stage, Stage::Style)
        || !Arc::ptr_eq(&request.nonce, &response.request.nonce)
        || response.request.source != *authority
        || response.coverage.start != request.range.start
        || response.coverage.end <= response.coverage.start
        || response.coverage.end > request.range.end
    {
        // 相同字节、相同坐标、旧请求重放都不能替代实际仍待响应的唯一nonce。
        return Err(MarkdownLineComposeError::SourceMismatch);
    }
    let text = request.text_scope()?; // 真实样式安装只消费已分析文字，不能为结构行补造 Unicode 事实。
    let mut shaping_seams = request.shaping_seams.clone();
    if response.coverage.end != request.range.end {
        shaping_seams[1] = None; // 真实缩短样式段不能继承尚未到达的右 UBA seam。
    }
    #[cfg(feature = "paged-shaping-frontier")]
    if request.continuation_font.is_some() {
        #[cfg(feature = "s2-table-window-prototype")]
        if frontier::has_style_boundary_probe(payload) {
            return frontier::provide_boundary_style(payload, authority, cancellation, response);
        }
        // 两条路径都只发已取得原GCB端点的试次；不能让闭项响应误落到另一个Frontier owner。
        if payload.closed_fonts.is_some() {
            return closed_item::provide_font_style(payload, authority, response);
        }
        return frontier::provide_terminal_style(payload, authority, response);
    }
    #[cfg(feature = "paged-shaping-frontier")]
    let end_proof = if let Some(endpoint) = coordinates.endpoint {
        if endpoint.boundary(authority)?.utf16 != response.coverage.end {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        endpoint
    } else {
        let (answer, endpoint) = payload
            .breaks
            .certified_grapheme_endpoint(authority, response.coverage.end)?;
        match answer {
            MarkdownGraphemeAnswer::Known(Some(boundary))
                if boundary.utf16 == response.coverage.end =>
            {
                CertifiedShapingEndpoint::Grapheme {
                    proof: endpoint.ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
                    seam: if response.coverage.end == request.range.end {
                        request.shaping_seams[1].clone()
                    } else {
                        None
                    },
                }
            }
            MarkdownGraphemeAnswer::Known(None) if response.coverage.end == request.range.end => {
                let seam = request.shaping_seams[1]
                    .clone()
                    .ok_or(MarkdownLineComposeError::InvalidRequest)?;
                if seam.boundary(authority)?.1 != response.coverage.end
                    || seam.levels().0 != text.level
                {
                    return Err(MarkdownLineComposeError::SourceMismatch);
                }
                CertifiedShapingEndpoint::Bidi(seam)
            }
            _ => return Err(MarkdownLineComposeError::InvalidRequest),
        }
    };
    #[cfg(not(feature = "paged-shaping-frontier"))]
    match payload.breaks.grapheme_query(
        authority,
        response.coverage.end,
        MarkdownGraphemeQuery::IsBoundary,
    )? {
        MarkdownGraphemeAnswer::Known(Some(boundary))
            if boundary.utf16 == response.coverage.end => {}
        MarkdownGraphemeAnswer::Known(None)
            if response.coverage.end == request.range.end
                && request.shaping_seams[1].as_ref().is_some_and(|seam| {
                    seam.boundary(authority)
                        .is_ok_and(|(_, at)| at == response.coverage.end)
                        && seam.levels().0 == text.level
                }) => {}
        _ => return Err(MarkdownLineComposeError::InvalidRequest),
    }
    let direction = if text.level & 1 == 0 {
        MarkdownShapingDirection::LeftToRight
    } else {
        MarkdownShapingDirection::RightToLeft
    };
    if let Some(registration) = &response.registration {
        let registered = registration.lease()?;
        if !response.font.permits(&registered.font().authority()) {
            return Err(MarkdownLineComposeError::SourceMismatch);
        } // 只有同一个实际Font payload才能使用该注册；数字slot、PSName均不能代替此门。
    }
    #[cfg(feature = "paged-shaping-frontier")]
    {
        // 响应显式缩短coverage时，它沿原API确定一个真实样式项末端；不能再把
        // 这个已确定末端当作SCX暂存的支持域而吞掉最后GCB。只有回应到开放请求
        // 的临时上界，才必须保留后继见证并在原Script后续推进后重新请求。
        payload.open_script &= response.coverage.end == request.range.end;
    }
    // 平台只提供实际Font与有限样式；段落、脚本、方向和BOT/EOT由原分析事实封闭决定。
    let facts = MarkdownLineItemFacts {
        source_utf16: response.coverage.clone(),
        paragraph_utf16: text.paragraph.clone(),
        script: text.script,
        level: text.level,
        direction,
        points: response.style.points,
        beginning_of_text: response.coverage.start == text.paragraph.start,
        end_of_text: response.coverage.end == text.paragraph.end,
    };
    let font = response.font.authority();
    // 先显式核固定小预算，才复制重形所需样式；非法大Vec不能为错误响应再分配一份。
    if response.style.features.len() > crate::MARKDOWN_SHAPING_MAX_FEATURES
        || response
            .style
            .language
            .as_ref()
            .is_some_and(|value| value.len() > 64)
    {
        return Err(MarkdownLineComposeError::InvalidRequest);
    }
    let replay_style = response.style.clone();
    #[cfg(feature = "paged-shaping-frontier")]
    let natural_tail = !payload.open_script && response.coverage == request.range;
    #[cfg(feature = "paged-shaping-frontier")]
    let admission = if payload.frontier_enabled {
        // 只有完整原request能取得闭项票；缩短响应即使已经清除open_script，仍不会
        // 借此把预算切片升级为自然项。两张端点票原样移交，不仅复制其坐标。
        closed_item::ClosedItem::admit(payload, authority, &response, end_proof, cancellation)?
    } else {
        closed_item::Admission::Frontier(end_proof) // 原非前沿入口保持既有完整项合同。
    };
    // 输入owner只在完整响应验证后创建；构造失败会真实归还候选，不发布半初始化item。
    let options = MarkdownShapingOptions {
        direction,
        script: facts.script,
        language: response.style.language.take(),
        points: facts.points,
        beginning_of_text: facts.beginning_of_text,
        end_of_text: facts.end_of_text,
        features: std::mem::take(&mut response.style.features),
    };
    #[cfg(feature = "paged-shaping-frontier")]
    let end_proof = match admission {
        closed_item::Admission::Closed(closed_item) => {
            // 原小项已自然封闭；这里只转移票和原有限样式，不能为装配Job同步读取
            // 邻接上下文。后继ClosedInput才实际收费，完整shape仍不是可恢复时间量子。
            let fonts = closed_item::FontRuns::new(
                &closed_item,
                response.font.clone(),
                response.registration.clone(),
                replay_style.clone(),
                facts.clone(),
                response.preferred_registration.clone(),
            )?;
            let shaping = crate::markdown_shaping::PendingClosedShaping::new(
                payload.source.clone(),
                authority,
                response.font,
                &font,
                closed_item,
                options,
            )?;
            payload.closed_input = Some(closed_item::PendingItem {
                shaping,
                font,
                registration: response.registration,
                replay_style,
                facts,
                shaping_seams: shaping_seams.clone(),
            });
            payload.closed_fonts = Some(fonts);
            payload.request.take();
            payload.font_probe_scope.take(); // 响应消费即撤旧试次，不能把相同范围的旧probe重用。
            payload.scan.take();
            payload.stage = Stage::ClosedInput;
            return Ok(()); // 后继仍按调用方原bytes/scalars预算输入，再交真实完整ItemReady。
        }
        closed_item::Admission::Frontier(endpoint) => endpoint,
    }; // 未准入时归还原末端票，后继前沿仍消费原scope，不能另造公开坐标许可。
    #[cfg(feature = "paged-shaping-frontier")]
    if payload.frontier_enabled {
        // 原完整响应先建能力owner，尚未认证的tail不创建glyph输入或伪造部分item。
        let resource_registration = payload.resources.begin_frontier_audit();
        let scan = payload
            .scan
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        let start = scan.endpoint_proofs[0]
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        let mut owner = if payload.open_script {
            let prefix = match payload.scripts.stable_prefix_at(
                authority,
                MarkdownScriptUnit::Utf16,
                facts.source_utf16.start,
            )? {
                crate::markdown_scripts::MarkdownScriptPrefixAnswer::Known(Some(prefix)) => prefix,
                _ => return Err(MarkdownLineComposeError::InconsistentCoverage),
            };
            crate::markdown_shaping::frontier::MarkdownShapingFrontierJob::new_with_scope(
                payload.source.clone(),
                authority,
                response.font.clone(),
                &font,
                crate::markdown_shaping::frontier::MarkdownShapingFrontierScope::Open {
                    original: response.coverage.clone(),
                    endpoints: [start, &end_proof],
                    script: &prefix,
                },
                options,
                cancellation.shape.clone(),
            )
            .map_err(frontier::map_error)?
        } else {
            crate::markdown_shaping::frontier::MarkdownShapingFrontierJob::new_with_scope(
                        payload.source.clone(), authority, response.font.clone(), &font,
                        crate::markdown_shaping::frontier::MarkdownShapingFrontierScope::CertifiedCoordinates {
                            original: response.coverage.clone(), endpoints: [start, &end_proof],
                        }, options, cancellation.shape.clone(),
                    )
                    .map_err(frontier::map_error)?
        };
        owner
            .attach_composer_resources(resource_registration)
            .map_err(frontier::map_error)?;
        payload.frontier = Some(frontier::PendingFrontier::new(
            owner,
            response,
            replay_style,
            facts,
            payload.open_script,
            natural_tail,
        ));
        payload.request.take();
        payload.font_probe_scope.take(); // 原响应已消费，所有在途字体试次立即失去原请求权限。
        payload.scan.take();
        payload.stage = Stage::Frontier;
        return Ok(());
    }
    let job = if shaping_seams.iter().any(Option::is_some) {
        // 非前沿入口仍保持完整项合同；真正 UBA seam 只裁断邻接上下文，不签新的 GCB/BOT/EOT。
        let start = match &shaping_seams[0] {
            Some(seam) => seam.boundary(authority)?.0,
            None => payload
                .source
                .byte_offset_for_utf16(response.coverage.start)?,
        };
        let end = match &shaping_seams[1] {
            Some(seam) => seam.boundary(authority)?.0,
            None => payload
                .source
                .byte_offset_for_utf16(response.coverage.end)?,
        };
        let context = (if shaping_seams[0].is_some() { start } else { 0 })
            ..(if shaping_seams[1].is_some() {
                end
            } else {
                payload.source.byte_len()
            });
        MarkdownShapingJob::from_certified_item_input(
            payload.source.clone(),
            authority,
            response.font,
            &font,
            (response.coverage, start..end, Some(context)),
            options,
            cancellation.shape.clone(),
        )?
    } else {
        MarkdownShapingJob::new_with_cancellation(
            payload.source.clone(),
            authority,
            response.font,
            &font,
            response.coverage,
            options,
            cancellation.shape.clone(),
        )?
    };
    payload.item = Some(MarkdownLineItem {
        job: super::worker_resources::ShapeResourceOwner::new(job, &payload.resources),
        source: payload.source.clone(),
        authority: authority.clone(),
        font,
        registration: response.registration,
        replay_style,
        facts,
        shaping_seams,
    });
    payload.request.take();
    payload.font_probe_scope.take(); // 不保活已完成样式阶段的字体探测或相同坐标旧票。
    payload.scan.take();
    payload.stage = Stage::Input;
    Ok(())
}

/// 只查看当前真实unit及CR后可能的LF；不通过完整NSString/段落String识别控制。
fn initial_inline_control(
    source: &MarkdownLayoutTextSource,
    authority: &MarkdownLayoutTextAuthority,
    bidi: Option<&MarkdownBidiParagraph>,
    offset: usize,
) -> Result<Option<MarkdownLineInlineControl>> {
    #[cfg(feature = "paged-shaping-frontier")]
    if offset < source.utf16_len() {
        let bidi = bidi.ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        let end = offset
            .checked_add(1)
            .ok_or(MarkdownLineComposeError::Overflow)?;
        if bidi
            .uniform_control_free_span(authority, offset..end)?
            .is_some()
        {
            return Ok(None); // 同原完整证明排除控制，起点也不为一次分类重新读取冻结窗口。
        }
    }
    #[cfg(not(feature = "paged-shaping-frontier"))]
    let _ = (authority, bidi); // 旧功能配置继续使用原控制getter，不引入未启用的证书API。
    inline_control(source, offset)
}

/// 无完整控制证明的原路径；其有限UTF16窗口读取尚未纳入ItemScan的bytes配额。
fn inline_control(
    source: &MarkdownLayoutTextSource,
    offset: usize,
) -> Result<Option<MarkdownLineInlineControl>> {
    source.health()?;
    if offset >= source.utf16_len() {
        return Ok(None);
    }
    let snapshot = source.snapshot_for_bidi();
    let unit = snapshot.utf16_code_unit_at(offset)?;
    let kind = match unit {
        9 => MarkdownLineInlineControlKind::Tab,
        13 => MarkdownLineInlineControlKind::CarriageReturn,
        10 => MarkdownLineInlineControlKind::LineFeed,
        0x85 => MarkdownLineInlineControlKind::NextLine,
        0x2028 => MarkdownLineInlineControlKind::LineSeparator,
        0x2029 => MarkdownLineInlineControlKind::ParagraphSeparator,
        _ => return Ok(None), // 普通UTF16（含代理项）仍交原Script/UBA/GCB，不改变字符。
    };
    let mut end = offset
        .checked_add(1)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    if unit == 13 && end < source.utf16_len() && snapshot.utf16_code_unit_at(end)? == 10 {
        end = end
            .checked_add(1)
            .ok_or(MarkdownLineComposeError::Overflow)?;
    }
    source.health()?;
    Ok(Some(MarkdownLineInlineControl::new(
        source.authority(),
        offset..end,
        kind,
    )))
}

impl std::fmt::Debug for MarkdownLineInlineControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkdownLineInlineControl")
            .field("source_utf16", &self.source_utf16)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[cfg(all(test, feature = "paged-shaping-frontier"))]
#[path = "item/scan_tests.rs"]
mod scan_tests;

#[cfg(all(test, feature = "paged-shaping-frontier"))]
#[path = "item/open_prefix_tests.rs"]
mod open_prefix_tests;
