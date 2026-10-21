//! Coordinate actual itemization inside one worker. Requests carry weak Source
//! tickets and finite facts, never copied text or native objects in responses.

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

/// Nonglyph controls need actual paragraph/tab policy; they cannot be treated
/// as missing glyphs and passed through font fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineInlineControlKind {
    Tab,
    CarriageReturn,
    LineFeed,
    NextLine,
    LineSeparator,
    ParagraphSeparator,
    EmptyParagraph,
    /// A complete original hidden-range control has no glyphs; its endpoints
    /// still belong to the same complete Source.
    FoldedSection,
    /// An original structural-row block; only its actual cell display sources
    /// own layout of the contained text.
    TableRow,
}
#[derive(Clone)]
/// Return this original request nonce with its weak source authority; public
/// range/kind facts cannot independently create an accepted response.
pub struct MarkdownLineInlineControl {
    pub source_utf16: std::ops::Range<usize>,
    pub kind: MarkdownLineInlineControlKind,
    pub(super) source: MarkdownLayoutTextAuthority,
    pub(super) nonce: Arc<()>,
}

/// Share this composition's original deep-cancellation state. Cloning retains
/// no text, font payload, or scheduling thread.
#[derive(Clone, Default)]
pub struct MarkdownLineComposeCancellation {
    bidi: MarkdownBidiCancellation,
    script: MarkdownScriptCancellation,
    shape: MarkdownShapingCancellation,
    measure: crate::MarkdownSoftLineMeasureCancellation,
}
impl MarkdownLineComposeCancellation {
    /// Create cancellation for the same bidi/script/shaping/measure pipeline.
    pub fn new() -> Self {
        Self::default()
    }
    /// Revoke every actual dependency, including work inside a long shaping call.
    pub fn cancel(&self) {
        self.bidi.cancel();
        self.script.cancel();
        // Long GSUB/GPOS processing observes this same state, not only outer step boundaries.
        self.shape.request();
        self.measure.cancel();
    }
    /// Observe the shared cancellation request without claiming child completion.
    pub fn is_cancelled(&self) -> bool {
        self.bidi.is_cancelled()
    }
    /// Lend the original shaping cancellation state to the actual child job.
    pub(super) fn shaping(&self) -> MarkdownShapingCancellation {
        self.shape.clone()
    }
    /// Lend the original measurement cancellation state without a new task lifetime.
    pub(super) fn measure(&self) -> crate::MarkdownSoftLineMeasureCancellation {
        self.measure.clone()
    }
}

/// Advance one dependency or input role per step; script and break work cannot
/// each spend the same byte allowance independently.
#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineComposeBudget {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub metadata_units: usize,
}
impl Default for MarkdownLineComposeBudget {
    /// Keep the established finite source and metadata quanta independent of item length.
    fn default() -> Self {
        Self {
            bytes: 4096,
            scalars: 256,
            records: 256,
            metadata_units: METADATA_UNITS,
        }
    }
}

/// Platform-provided style accompanies an actual font. Core's original facts
/// determine direction, script, and BOT/EOT rather than trusting response claims.
#[derive(Clone, Debug)]
pub struct MarkdownLineComposeStyle {
    /// Seal original semantic drawing roles with the same font response without
    /// changing glyphs, source coordinates, or line-break authority.
    pub presentation: crate::MarkdownPresentationStyles,
    pub points: f32,
    pub language: Option<String>,
    pub features: Vec<MarkdownShapingFeature>,
}

/// Bind this actual outstanding intersection through a private nonce. Another
/// request with equal coordinates cannot reuse its older response.
#[derive(Clone)]
pub struct MarkdownLineStyleRequest {
    nonce: Arc<()>,
    source: MarkdownLayoutTextAuthority,
    range: Range<usize>,
    scope: StyleScope,
    // Only a real natural bidi seam may differ from a grapheme boundary; short
    // coverage or public source coordinates cannot manufacture that authority.
    shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
    #[cfg(feature = "paged-shaping-frontier")]
    continuation_font: Option<MarkdownLayoutFontAuthority>,
    // Font handoff already owns original grapheme endpoint proof. Retain exact
    // bytes for bounded first-scalar input without another distant coordinate lookup.
    #[cfg(feature = "paged-shaping-frontier")]
    continuation_bytes: Option<Range<usize>>,
}

/// Actual text Unicode facts and certified structural rows are mutually
/// exclusive; a table handshake owns no invented script, level, or paragraph.
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
    /// Borrow this original request's weak source identity without recapturing text.
    pub fn source(&self) -> &MarkdownLayoutTextAuthority {
        &self.source
    }
    /// Return finite requested geometry without granting new source-reading authority.
    pub fn source_range(&self) -> Range<usize> {
        self.range.clone()
    }
    /// Only text scope owns an original Unicode paragraph. Structural rows must
    /// return their actual row ticket; misuse violates this accessor's contract.
    pub fn paragraph_range(&self) -> Range<usize> {
        self.text_scope()
            .expect("结构表格请求没有文字段落权限")
            .paragraph
            .clone()
    }
    /// Return only actual script analysis. Structural table handshakes have no
    /// text script value, so calling this accessor for them violates its contract.
    pub fn script(&self) -> [u8; 4] {
        self.text_scope().expect("结构表格请求没有脚本权限").script
    }
    /// Return the original bidi level. Structural rows do not pass text bidi
    /// analysis and cannot supply this accessor's required text fact.
    pub fn level(&self) -> u8 {
        self.text_scope()
            .expect("结构表格请求没有 UBA 级别权限")
            .level
    }
    /// Font/shaping entry requires actual text scope. A structural request can
    /// return only its original row proof, never degrade into fabricated text.
    fn text_scope(&self) -> Result<&TextStyleScope> {
        match &self.scope {
            // The original analyzed text scope owns all required Unicode facts.
            StyleScope::Text(text) => Ok(text),
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            // A structural row grants no script/bidi/text-shaping authority.
            StyleScope::Table(_) => Err(MarkdownLineComposeError::TableRange),
        }
    }
    /// Preserve the original item's font context during handoff; ordinary style
    /// requests have no continuation identity. This is not platform preference:
    /// each complete grapheme still tries its actual semantic base so an old
    /// fallback cannot absorb characters that can return to that preferred font.
    #[cfg(feature = "paged-shaping-frontier")]
    pub fn continuation_font(&self) -> Option<&MarkdownLayoutFontAuthority> {
        self.continuation_font.as_ref()
    }
    /// Cover one actual style interval beginning at this request's start. Its
    /// recipient must still revalidate grapheme proof and the original pending nonce.
    pub fn respond(
        self,
        coverage: Range<usize>,
        font: MarkdownLayoutFont,
        style: MarkdownLineComposeStyle,
    ) -> Result<MarkdownLineStyleResponse> {
        // A structural request has no Unicode/font authority to produce a text response.
        self.text_scope()?;
        if coverage.start != self.range.start
            || coverage.end <= coverage.start
            || coverage.end > self.range.end
        {
            // Neither a gap at the start nor an oversized/empty response grants
            // authority over a different part of this original item request.
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

    /// Use the registration still owned by the actual palette. A weak ticket
    /// cannot claim drawable glyph authority after that font has been released.
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

/// Private fields require consuming an actual request to respond; a caller
/// cannot construct a new item solely from public source coordinates.
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
    /// Attach only adjacent semantic proof issued for this same actual request;
    /// short coverage alone cannot close an item. Later work still obtains the
    /// complete right grapheme within budget and probes its actual font. A color
    /// change cannot arbitrarily split a ligature.
    ///
    /// # Errors
    /// Reject another source/request even at equal coordinates. The proof's
    /// exact left style must agree with this response.
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
            // A new request cannot reuse an older adjacent-style publication's
            // nonce or Source authority merely because the ranges coincide.
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if self.coverage.end != boundary.left().utf16_range.end
            || self.coverage.start < boundary.left().utf16_range.start
            || self.style.presentation != boundary.left().styles
        {
            // Preserve the actual left style interval and drawing roles, not
            // just a numeric endpoint that could hide another semantic segment.
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        self.root_leaf_boundary = Some(boundary);
        Ok(self)
    }

    /// Seal the platform's original semantic base font with this nonce. It is
    /// real font preference, not range/closure authority. After selecting fallback,
    /// each later whole grapheme still tries that base; fallback covering spaces
    /// does not let it absorb content belonging to the preferred font.
    ///
    /// # Errors
    /// Reject expired registration or a bound point size differing from this
    /// style, without substituting a newly resolved equal-name font.
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
            // A font bound to another size cannot acquire this response's exact
            // shaping authority through its otherwise valid registration.
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        self.preferred_registration = Some(registration);
        Ok(self)
    }

    /// Seal paragraph inset with the original response nonce; literal callers
    /// that omit it retain their existing zero-inset behavior.
    pub fn with_paragraph_inset(mut self, inset_26_6: i32) -> Result<Self> {
        if inset_26_6 < 0 {
            // Reject negative available-width input before it can be clipped
            // during drawing; the actual measure still validates final line width.
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        self.paragraph_inset_26_6 = inset_26_6;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq)]
/// Finite actual item facts; they do not independently certify a soft line,
/// glyph coverage, caret boundary, or a still-current font/Source.
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

/// Own one actual worker-local job. Item readiness promises neither completed
/// soft-line geometry nor native presentation or whole-item font fallback.
pub struct MarkdownLineItem {
    pub(super) job: super::worker_resources::ShapeResourceOwner,
    pub(super) source: MarkdownLayoutTextSource,
    pub(super) authority: MarkdownLayoutTextAuthority,
    pub(super) font: MarkdownLayoutFontAuthority,
    pub(super) registration: Option<MarkdownLineFontRegistration>,
    // Retain only the validated finite style. Replaying a genuinely unsafe
    // shaping boundary must use these original options without retaining text.
    pub(super) replay_style: MarkdownLineComposeStyle,
    facts: MarkdownLineItemFacts,
    shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
}
impl MarkdownLineItem {
    /// Lend only original item-issued scalar shaping seams to Store; absent
    /// seams still require actual grapheme proof. These endpoints grant no
    /// LineEnd/caret authority: a visual part edge is not automatically editable.
    pub(in crate::markdown_line_composer) fn shaping_seams(
        &self,
    ) -> &[Option<ResolvedBidiShapingSeam>; 2] {
        &self.shaping_seams
    }
    /// Borrow original finite facts without cloning glyph data or the job owner.
    pub fn facts(&self) -> &MarkdownLineItemFacts {
        &self.facts
    }
    /// Borrow the original weak font registration, if this item was registered.
    pub fn font_registration(&self) -> Option<&MarkdownLineFontRegistration> {
        self.registration.as_ref()
    }
    /// Reuse the original job's validated style during unsafe-boundary reshaping,
    /// rather than querying newer active-document presentation.
    pub fn shaping_style(&self) -> &MarkdownLineComposeStyle {
        &self.replay_style
    }
    /// Query the actual original job only while Source and font authority remain valid.
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
    /// Report actual frontier-child adjacent-context input separately from its
    /// central output and ordinary input quantum.
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
    // Only the actually consumed item's right seam authorizes the next matching
    // start; a bare cursor cannot reconstruct its original proof.
    next_shaping_seam: Option<ResolvedBidiShapingSeam>,
    scan: Option<Scan>,
    request: Option<MarkdownLineStyleRequest>,
    // Retain one shortened internal response while waiting for its actual
    // metadata quantum, without a native borrow or another text value.
    #[cfg(feature = "paged-shaping-frontier")]
    pending_style: Option<MarkdownLineStyleResponse>,
    // Only the coordinator awaiting this original style response keeps the
    // probe scope alive. Escaped request clones cannot extend font-trial authority.
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

/// Retain one pending item rather than a long-document Vec of jobs/runs or a
/// complete copied body.
pub struct MarkdownLineItemCoordinator {
    payload: Option<Payload>,
    authority: MarkdownLayoutTextAuthority,
    cancellation: MarkdownLineComposeCancellation,
    backing: Option<crate::MarkdownOriginalBackingObserver>,
    resources: super::worker_resources::ComposerResourceScope,
}
impl MarkdownLineItemCoordinator {
    /// Bind one original source/cancellation scope and construct its actual
    /// script/break dependencies before publishing the worker-local coordinator.
    pub fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        cancellation: MarkdownLineComposeCancellation,
    ) -> Result<Self> {
        source.health()?;
        if !source.permits(authority) {
            // Equal text or generation cannot replace the captured source allocation.
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if cancellation.is_cancelled() {
            // Do not construct dependencies for an already-cancelled composition.
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

    /// Share only this worker's original resource observation scope with actual child owners.
    pub(super) fn resource_scope(&self) -> super::worker_resources::ComposerResourceScope {
        self.resources.clone()
    }

    /// Share finite original folding proof with word movement/deletion. Adjacent
    /// visible endpoints cannot authorize reading hidden content as one original grapheme.
    pub(in crate::markdown_line_composer) fn fold_plan(
        &self,
    ) -> Option<crate::markdown_layout_source::folds::FoldPlan> {
        self.payload
            .as_ref()
            .and_then(|payload| payload.folds.shared())
    }

    /// Bind folding as construction-time display intent; once Source consumption
    /// begins, do not rewrite the still-active original control intervals.
    pub(super) fn install_folded_ranges(&mut self, ranges: &[Range<usize>]) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !matches!(payload.stage, Stage::Bidi) || payload.cursor != 0 {
                // A later cursor or phase already depends on the original visible sequence.
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

    /// Changing marker visibility on the same frozen Source resets styled items
    /// while retaining already-paid bidi/script/grapheme/line-break indexes.
    /// Native code first cancels and returns old font trials; revoked nonces cannot accept them later.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn reset_styles(&mut self) -> Result<()> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            // Retire actual shaping owners for the old style first. Source and
            // language indexes remain the same allocation without another mmap scan.
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
            // Reuse an existing bidi value; unfinished preparation resumes the
            // same original continuation under its next actual work quantum.
            payload.stage = Stage::Bidi;

            // A new glyph writer has independent release observations. Reuse
            // actual dependency observers without overwriting the old writer's
            // still-outstanding resource facts with apparent completion.
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
            }
            // If bidi was never created, invent no observation; its actual later
            // preparation registers the original owner when construction begins.
            payload.resources = resources.clone();
            self.resources = resources;
            Ok(())
        })();
        self.finish(result)
    }

    /// Retain sealed glyphs only when every still-active style scope is inside
    /// actual plain-text proof for this original Source. Change no cursor/nonce/
    /// scope; later NeedStyle still uses the new selection's real semantics.
    /// Unpublished input and old RootLeaf boundaries lie outside this capability;
    /// default styles or equal coordinates cannot establish that missing proof.
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(super) fn retains_selection_styles(
        &self,
        captured_end: usize,
        proven_end: usize,
    ) -> Result<bool> {
        self.guard()?;
        let result = (|| {
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if payload.cursor != captured_end
                || payload.cursor > proven_end
                || payload.request.is_some()
                || payload.pending_style.is_some()
                || payload.font_probe_scope.is_some()
                || payload.item.is_some()
                || payload.closed_input.is_some()
                || payload.scan.is_some()
                || payload.control.is_some()
                || payload.open_probe.is_some()
                || !matches!(
                    payload.stage,
                    Stage::ItemStart | Stage::ClosedFonts | Stage::Frontier | Stage::End
                )
            {
                // Any active request/input/scan outside the proven retained
                // prefix still depends on old selection semantics and must reset.
                return Ok(false);
            }
            // ClosedFonts clears AwaitChild one step after Store actually returns
            // its child. Keep that genuinely empty continuation without spending
            // an extra quantum to clear a slot or treating a live child as returned.
            if let Some(fonts) = &payload.closed_fonts
                && !fonts.retains_selection_styles(&self.authority, proven_end)?
            {
                return Ok(false);
            }
            if let Some(frontier) = &payload.frontier
                && !frontier.retains_selection_styles(proven_end)?
            {
                return Ok(false);
            }
            Ok(true)
        })();
        // A failed font-registration query still yields the original Source
        // health failure first if that same source became unavailable meanwhile.
        self.guard()?;
        result
    }

    /// Keep original backing failure ahead of payload retirement, source
    /// identity, and cancellation; metadata cannot revive an obsolete owner.
    fn guard(&self) -> Result<()> {
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            // A previously recorded backing failure remains authoritative even
            // after payload destruction or a later cancellation request.
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?;
        payload.source.health()?;
        if !payload.source.permits(&self.authority) {
            // A replacement Source cannot use this worker's original authority.
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if self.cancellation.is_cancelled() {
            // Cancellation revokes further work, not proof of actual resource completion.
            return Err(MarkdownLineComposeError::Cancelled);
        }
        Ok(())
    }

    /// Revalidate the original owner after work, retire failed payloads, then
    /// check cleanup-time Source failure before exposing a result.
    fn finish<T>(&mut self, answer: Result<T>) -> Result<T> {
        let answer = match self.guard() {
            Err(error) => {
                // Return an unexposed actual item before checking I/O facts
                // produced by its destruction after post-work revocation.
                drop(answer);
                Err(error)
            }
            // Still-current work retains its actual result, including a dependency failure.
            Ok(()) => answer,
        };
        if answer.is_err() {
            // Return Source, indexes, and actual jobs; an escaped request shell
            // cannot restore this retired worker's authority.
            drop(self.payload.take());
        }
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|observer| observer.first_errno())
        {
            // A real source failure during teardown keeps its original priority.
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        answer
    }

    /// Whole-source bidi preparation is cancellable worker work, not a promise
    /// that one call has bounded main-thread execution time.
    pub fn step(&mut self, budget: MarkdownLineComposeBudget) -> Result<MarkdownLineItemStep> {
        let result = self.advance(budget);
        self.finish(result)
    }

    /// Word movement advances only original bidi preparation. Whole-source and
    /// visible-paragraph proof remain distinct, with no uncharged grapheme work.
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
            }
            // An already published original paragraph lends its existing facts;
            // each key press does not rescan text or create another analyzer.
            let proved = self.word_move_scope()?;
            Ok((step, proved))
        })();
        // This answer grants a capability, not style/position. Real Source,
        // cancellation, or I/O failure follows the same original retirement path.
        self.finish(result)
    }

    /// The prepared representation selects its real later consumer. Visible
    /// scope permits per-grapheme queries of an original complete zero-level bidi paragraph.
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
            // No completed original bidi representation grants word-order capability yet.
            return Ok(None);
        };
        let scope = if bidi.whole_source_uniform_zero(&self.authority)? {
            // Complete source proof can authorize the whole uniform zero-level path.
            super::MarkdownWordMoveScope::WholeUniformZero
        } else if bidi.uses_visible_paragraph_scopes(&self.authority)? {
            // Otherwise only prepared visible paragraphs carry the required local proof.
            super::MarkdownWordMoveScope::VisibleParagraphScopes
        } else {
            // A different resolved representation does not imply supported word movement.
            super::MarkdownWordMoveScope::Unsupported
        };
        Ok(Some(scope))
    }

    /// Prepare the actual grapheme's complete bidi paragraph in this same Item
    /// owner; source/metadata work ends this step before another classification role can run.
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
                // Newly completed preparation still consumed its original quantum;
                // obtain its word-order proof only in a later step.
                return Ok((step, None));
            }
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

    /// Observe the paused original item cursor. Word work keeps only this finite
    /// position, without another Source or bidi-paragraph owner.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_line_composer) fn word_layout_position(&self) -> Result<usize> {
        self.guard()?;
        Ok(self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .cursor)
    }

    /// Word work may replace a retained bidi slot. Restore the original layout
    /// paragraph in the same owner before publishing its endpoint, so Style/Scan
    /// cannot consume an evicted paragraph's stale facts.
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

    /// Advance one actual dependency/input stage. Each returned step exposes
    /// only work and readiness justified by the original Source and its proofs.
    fn advance(&mut self, budget: MarkdownLineComposeBudget) -> Result<MarkdownLineItemStep> {
        self.guard()?;
        if budget.bytes == 0
            || budget.scalars == 0
            || budget.records == 0
            || budget.metadata_units == 0
        {
            // A zero budget cannot make actual progress or authorize a partial stage transition.
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
            // Direct coordinator callers also advance only in the next quantum.
            // Engine already consumes this same demand when scheduling, so the
            // two driving paths cannot charge one pending request twice.
            return self.advance_pending_line_breaks(budget);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if self
            .payload
            .as_ref()
            .ok_or(MarkdownLineComposeError::Poisoned)?
            .script_source_demand
        {
            // Script demand resumes that same original script job exclusively.
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
            // Grapheme demand does not implicitly advance line-break or item input work.
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
            // This actual table-directory access owns the quantum. Obtain its
            // structural semantics before allowing the row to enter body bidi.
            return Ok(step);
        }
        match payload.stage {
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            Stage::TableDirectory | Stage::TableRow => {
                // Table handoff uses its explicit row API, not ordinary text stepping.
                return Err(MarkdownLineComposeError::InvalidRequest);
            }

            Stage::Bidi => {
                #[cfg(feature = "paged-shaping-frontier")]
                if payload.bidi.is_some() {
                    // Reuse the real prepared bidi owner and prepare its current
                    // paragraph before script/grapheme dependencies may continue.
                    if paragraph::prepare(payload, &self.authority, budget, &mut step)? {
                        payload.stage = Stage::Dependencies;
                    }
                    // Paragraph preparation alone consumes this quantum.
                    return Ok(step);
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
                // ASCII uses its exact dense/script path. Ordinary LTR Unicode
                // queries stable original prefixes only with complete uniform
                // control-free proof; otherwise alternate the two full dependencies.
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
                    // Keep a complete script batch for sources fitting the
                    // existing source-read domain. Only longer sources prepare
                    // an open current paragraph; size grants no script/font/shape/line boundary.
                    payload.source.byte_len() > crate::MARKDOWN_SCRIPT_SOURCE_BYTES
                } else {
                    // Nonparagraph preparation uses only its existing original prefix proof.
                    prefix
                };
                #[cfg(feature = "paged-shaping-frontier")]
                if payload.frontier_enabled
                    && start_from_prefix
                    && !payload.breaks.ascii_prefix_ready(&self.authority)?
                {
                    payload.stage = Stage::ItemStart;
                    // Later ItemStart still checks actual controls for a long
                    // multi-paragraph source; prefix mode does not manufacture control-free text.
                    return Ok(step);
                }
                #[cfg(not(feature = "paged-shaping-frontier"))]
                let prefix = false;
                let scripts = !payload.scripts_ready
                    && (prefix || payload.script_first || payload.breaks_ready);
                if scripts {
                    // Advance script alone, charging its actual source/scalar/record work.
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
                    // The alternating break phase receives this quantum only
                    // when the script phase did not consume it.
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
                    // A prefix-capable break representation is distinct from
                    // complete dependencies; later queries still expose genuine Pending.
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
                    // Real cross-paragraph endpoint work must end this step;
                    // newly Ready does not allow spending its quantum again on itemization.
                    return Ok(step);
                }
                if let Some(range) = payload.folds.at(payload.cursor) {
                    // A hidden interval returns its original control nonce,
                    // never Script/font/shaping input. Original indexes still advance in real steps.
                    let control = MarkdownLineInlineControl::new(
                        self.authority.clone(),
                        range,
                        MarkdownLineInlineControlKind::FoldedSection,
                    );
                    payload.control = Some(control.clone());
                    payload.stage = Stage::Control;
                    step.answer = MarkdownLineItemAnswer::NeedsControl(control);
                } else if payload.cursor == payload.source.utf16_len() {
                    // Report only actual item-input EOF. The later composer
                    // owns any empty final visual line caused by a real trailing separator.
                    payload.stage = Stage::End;
                    step.answer = MarkdownLineItemAnswer::EndOfSource;
                } else if let Some(control) = initial_inline_control(
                    &payload.source,
                    &self.authority,
                    payload.bidi.as_ref(),
                    payload.cursor,
                )? {
                    // CRLF stays one actual two-unit control; do not split its
                    // grapheme or introduce a synthetic paragraph boundary.
                    payload.control = Some(control.clone());
                    payload.stage = Stage::Control;
                    step.answer = MarkdownLineItemAnswer::NeedsControl(control);
                } else {
                    // Script run, actual bidi paragraph, and level all belong
                    // to this same Source. Queries cannot recapture a newer document to fill gaps.
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
                            // Complete original proof already binds this range
                            // and level zero. Consume those facts without another
                            // scalar lookup or treating the one-unit probe as a paragraph.
                            // Script intersection and later grapheme admission still bound the item.
                            (span.paragraph_range(), span.level())
                        } else {
                            // Without complete control-free proof, keep actual
                            // paragraph/scalar queries rather than guessing extent from level zero.
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
                    // Starting from original paragraph/level proof or an actual
                    // query both consume real metadata work.
                    step.metadata_units = 1;
                    payload.stage = Stage::ItemScan;
                }
            }
            Stage::ItemScan => {
                // Original UTF-16 records determine the actual same-level end.
                // Low surrogates repeat their original level; later admission
                // needs complete grapheme proof or an actual bidi seam, never a budget cutoff.
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
                    // Complete bidi analysis already proved uniform level zero
                    // and absence of the six controls. Reuse this same Source's
                    // real script/paragraph intersection without per-unit CJK/emoji reads.
                    // The span itself proves no endpoints; admission below still checks them.
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
                    // Leaf creation/editing proved the entire frozen root is
                    // U+0020..U+007E. Its resolved Auto bidi level is zero with no
                    // inline controls, so consume the original script/paragraph
                    // bound without rereading a long item for its first viewport.
                    // This exact aggregate is used only within the current Core
                    // borrow and outer health checks, never inferred from a sample,
                    // generation, or the already displayed prefix.
                    if scan.level != 0 || scan.limit > payload.source.utf16_len() {
                        return Err(MarkdownLineComposeError::InconsistentCoverage);
                    }
                    scan.next = scan.limit;
                    complete = true;
                    // The constant-size aggregate query still consumes metadata
                    // work, with no hidden per-unit level or text probe.
                    step.metadata_units = 1;
                }
                for _ in 0..budget.metadata_units.min(METADATA_UNITS) {
                    // Resume original records only within this step's remaining
                    // metadata allowance; incomplete coverage keeps the same Scan.
                    if complete {
                        break;
                    }
                    if inline_control(&payload.source, scan.next)?.is_some() {
                        // The actual boundary before a control closes glyph
                        // input; the control remains for the next original stage.
                        complete = true;
                        break;
                    }
                    let unit = bidi.shaping_unit_at_utf16(&self.authority, scan.next)?;
                    let level = match &unit {
                        Some(unit) => unit.level,
                        None => bidi.level_at_utf16(scan.next)?,
                    };
                    if level != scan.level {
                        // A real changed-level adjacent scalar pair, rather
                        // than a direction parity guess, can issue a shaping seam.
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
                        // Charge the actual differing-level record too; ending
                        // the item cannot conceal that metadata access.
                        step.metadata_units += 1;
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
                    // Completion needs original endpoint proof before any
                    // style request can expose this interval to the platform.
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
                        }
                        // Only a changed-level seam actually read by this
                        // scan is valid; script/style/budget bounds cannot create it.
                        #[cfg(feature = "paged-shaping-frontier")]
                        {
                            if payload.frontier_enabled {
                                if ordinal < scan.verified_endpoints {
                                    // Earlier steps already retained this exact endpoint proof.
                                    continue;
                                }
                                if step.metadata_units == budget.metadata_units {
                                    // Do not query another endpoint after the quantum is spent.
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
                                // Keep the actual GCB certificate together with
                                // any coincident original bidi context barrier.
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
                                // A proven bidi seam may bound shaping inside
                                // a grapheme, without changing the editing index.
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
                    }
                    // Nongrapheme seams authorize only local shaping partitions;
                    // grapheme records, carets, and soft breaks keep their original facts.
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
                // Waiting returns only a clone of the original nonce; it creates
                // no font/file/Source and no independent request authority.
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
                // Charge before the actual query. Pending cannot retroactively
                // attribute this work to a later quantum.
                step.metadata_units = 1;
                let (answer, endpoint) = payload
                    .breaks
                    .certified_grapheme_endpoint(&self.authority, end)?;
                match answer {
                    // Engine consumes the original GCB demand in a separate later quantum.
                    MarkdownGraphemeAnswer::Pending => {}
                    MarkdownGraphemeAnswer::Known(Some(boundary)) if boundary.utf16 == end => {
                        // Transfer this exact response only after obtaining its
                        // real shortened endpoint; reuse the original request nonce.
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
                // Queuing transferred only original proof. Read actual adjacent
                // context here under this quantum's byte/scalar allowance;
                // context_bytes is a subset of bytes, with no simultaneous central input.
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
                    // Context preparation yields one real job owner only when
                    // complete; transfer its original facts and registered font together.
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
                }
                // Central source input begins only in a later quantum, after
                // this actual fixed context preparation has finished.
            }
            Stage::Input => {
                // The original job consumes this same frozen Source in finite
                // batches. Source requests stay at most 4 KiB regardless of item length.
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
                    // Input completion authorizes the actual later shape call,
                    // not a precomputed glyph result or final visual line.
                    payload.stage = Stage::Shape;
                }
            }
            Stage::Shape => {
                // Actual shaping may perform long-run algorithmic work and must
                // run on the worker, with its original deep-cancellation state.
                let item = payload
                    .item
                    .as_mut()
                    .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
                item.job.shape(&self.authority, &item.font)?;
                step.shapes = 1;
                #[cfg(feature = "paged-shaping-frontier")]
                if closed_item::inspect_shaped(payload)? {
                    // A successful first-grapheme font trial does not prove the
                    // rest has no missing glyphs. Inspect actual output before Store admission.
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
                // The sole unconsumed item repeats only finite original facts,
                // without copying glyphs or substituting a newer task's job.
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
                // A still-outstanding original control stays pending under
                // the same nonce until its actual platform response is accepted.
                step.answer = MarkdownLineItemAnswer::NeedsControl(
                    payload
                        .control
                        .as_ref()
                        .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
                        .clone(),
                );
            }
            Stage::End => {
                // Repeated EOF queries still pass original Source/cancellation guards.
                step.answer = MarkdownLineItemAnswer::EndOfSource;
            }
        }
        Ok(step)
    }

    /// Preserve immediate GCB validation for the synchronous public entry;
    /// its actual queries cannot be charged to a later step's budget.
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

    /// Consume only the current original control nonce. Actual platform font
    /// and paragraph facts do not require a substitute text character.
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
                // A stale or foreign response cannot consume this pending control.
                return Err(MarkdownLineComposeError::SourceMismatch);
            }
            for offset in [request.source_utf16.start, request.source_utf16.end] {
                // Both actual control endpoints need original GCB facts; do not
                // split CRLF or an independently larger grapheme for convenience.
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
            // Only after all fallible validation succeeds may the sole cursor
            // advance and relinquish the original outstanding control.
            payload.cursor = range.end;
            payload.control = None;
            payload.stage = Stage::ItemStart;
            Ok(super::controls::ValidatedControl::from_response(
                response, facts,
            ))
        })();
        self.finish(result)
    }

    /// Lend only the composition's original certified dependencies. In open
    /// mode, each actual query still returns Known or genuine Pending independently.
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
            // Neither incomplete full dependencies nor absent prefix authority
            // can authorize downstream composition queries.
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        // Actual paragraph-control starts may also query Pending LB/GCB. This
        // borrow grants no independent control-free or script-range proof.
        Ok((
            payload
                .bidi
                .as_mut()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
            &mut payload.breaks,
        ))
    }

    /// Logical word deletion borrows this worker's original grapheme dependency
    /// without requiring completed fonts, scripts, or visual lines.
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

    /// Consume only demand from an actual line-break query. An unfinished
    /// grapheme/cluster or spent candidate budget does not itself request a source scan.
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

    /// Advance the same original line-break job after the consumer's quantum
    /// ends, preserving original Item/style/measure identity and cursors.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn advance_pending_line_breaks(
        &mut self,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownLineItemStep> {
        self.advance_pending_break_dependency(budget, false)
    }

    /// Transfer the sole Ready item to later composition in this same worker;
    /// an item's end does not certify a soft line end.
    pub fn take_item(&mut self) -> Result<MarkdownLineItem> {
        let result = (|| {
            self.guard()?;
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLineComposeError::Poisoned)?;
            if !matches!(payload.stage, Stage::Ready) {
                // No other stage owns an actual fully shaped item ready for transfer.
                return Err(MarkdownLineComposeError::InvalidRequest);
            }
            let mut item = payload
                .item
                .take()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
            let profile = item.job.profile()?;
            // Query the original job's terminal ordinal to certify complete
            // glyph coverage and health before handing over its actual owner.
            item.job
                .glyph(&self.authority, &item.font, profile.glyph_count)?;
            if item.facts.source_utf16.start != payload.cursor {
                // The item cannot skip or replay text relative to this original cursor.
                return Err(MarkdownLineComposeError::InconsistentCoverage);
            }
            #[cfg(feature = "paged-shaping-frontier")]
            frontier::lend_font_item(payload, &self.authority, &mut item)?;
            #[cfg(feature = "paged-shaping-frontier")]
            closed_item::lend_font_item(payload, &item)?;
            // Register this one actual Ready child with font continuation.
            // The original frontier cannot advance past it before actual return.
            payload.cursor = item.facts.source_utf16.end;
            payload.next_shaping_seam = item.shaping_seams[1].clone();
            #[cfg(feature = "paged-shaping-frontier")]
            if payload.closed_fonts.is_some() {
                // Continue the original closed item's font coverage after this child returns.
                payload.stage = Stage::ClosedFonts;
            } else if payload.frontier.is_some() {
                // Otherwise resume its existing frontier rather than constructing another item.
                payload.stage = Stage::Frontier;
            } else {
                // A fully consumed standalone item can begin the next original input interval.
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

/// Only an internally obtained, already-charged endpoint may avoid another
/// synchronous query; absent proof retains public entry's exact validation.
#[derive(Default)]
struct StyleCoordinates {
    #[cfg(feature = "paged-shaping-frontier")]
    endpoint: Option<CertifiedShapingEndpoint>,
}

/// Validate one original style response before creating any real shaping owner.
/// Preserve its actual endpoint certificates, context barriers, and resource scope.
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
        // Equal bytes/coordinates or replayed requests cannot substitute for
        // this actual still-outstanding unique nonce.
        return Err(MarkdownLineComposeError::SourceMismatch);
    }
    // Actual text-style installation requires analyzed text; structural rows
    // cannot acquire manufactured Unicode facts through this entry.
    let text = request.text_scope()?;
    let mut shaping_seams = request.shaping_seams.clone();
    if response.coverage.end != request.range.end {
        // A shortened interval has not reached the original right bidi seam
        // and cannot inherit its context barrier or endpoint authority.
        shaping_seams[1] = None;
    }
    #[cfg(feature = "paged-shaping-frontier")]
    if request.continuation_font.is_some() {
        #[cfg(feature = "s2-table-window-prototype")]
        if frontier::has_style_boundary_probe(payload) {
            return frontier::provide_boundary_style(payload, authority, cancellation, response);
        }
        // Both continuation paths already own their original grapheme endpoints.
        // A closed-item response cannot be consumed by a different frontier owner.
        if payload.closed_fonts.is_some() {
            return closed_item::provide_font_style(payload, authority, response);
        }
        return frontier::provide_terminal_style(payload, authority, response);
    }
    #[cfg(feature = "paged-shaping-frontier")]
    let end_proof = if let Some(endpoint) = coordinates.endpoint {
        // Reuse only the actual endpoint previously charged for this exact response extent.
        if endpoint.boundary(authority)?.utf16 != response.coverage.end {
            return Err(MarkdownLineComposeError::InvalidRequest);
        }
        endpoint
    } else {
        // Synchronous callers still obtain the original endpoint now; this
        // query is not retroactively assigned to another step's quantum.
        let (answer, endpoint) = payload
            .breaks
            .certified_grapheme_endpoint(authority, response.coverage.end)?;
        match answer {
            MarkdownGraphemeAnswer::Known(Some(boundary))
                if boundary.utf16 == response.coverage.end =>
            {
                // The real GCB certificate keeps any coincident original bidi barrier.
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
                // Only this full original request may use its actual bidi seam
                // as a nongrapheme shaping endpoint; short coverage has no such proof.
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
        }
        // Only the same actual font payload may use this registration. A numeric
        // slot or equal PostScript name cannot replace its authority.
    }
    #[cfg(feature = "paged-shaping-frontier")]
    {
        // Explicit shortened coverage establishes a real style end; it cannot
        // later be treated as temporary SCX support and lose its last grapheme.
        // Only a response reaching the open request's provisional bound retains
        // a following witness and requests more after the original script progresses.
        payload.open_script &= response.coverage.end == request.range.end;
    }
    // The platform supplies actual font and finite style only. Original
    // paragraph/script/bidi facts determine direction and BOT/EOT boundaries.
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
    // Check finite style budgets before cloning replay options; an invalid
    // oversized feature vector cannot force another retained allocation.
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
        // Only the complete original request can obtain closed-item authority.
        // Clearing open_script on shortened coverage cannot upgrade a budget
        // fragment into natural closure. Transfer actual endpoint tickets, not just numbers.
        closed_item::ClosedItem::admit(payload, authority, &response, end_proof, cancellation)?
    } else {
        // Nonfrontier callers preserve their established complete-item contract.
        closed_item::Admission::Frontier(end_proof)
    };
    // Create an input owner only after complete response validation. Constructor
    // failure returns real candidate resources without publishing a partial item.
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
            // This small item closed naturally. Transfer only proof and finite
            // original style; do not synchronously read context while assembling
            // the job. ClosedInput later pays real input work, while a full shape
            // call still is not a resumable wall-clock quantum.
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
            // Consuming the response revokes its old font trial immediately;
            // equal coordinates cannot make that escaped probe current again.
            payload.font_probe_scope.take();
            payload.scan.take();
            payload.stage = Stage::ClosedInput;
            // Later steps consume original byte/scalar quanta before actual complete ItemReady.
            return Ok(());
        }
        // Retain the original endpoint for frontier work when natural closure was not admitted.
        closed_item::Admission::Frontier(endpoint) => endpoint,
    };
    #[cfg(feature = "paged-shaping-frontier")]
    if payload.frontier_enabled {
        // Establish the original frontier owner first. An uncertified tail
        // creates neither glyph input nor a fabricated partial Ready item.
        let resource_registration = payload.resources.begin_frontier_audit();
        let scan = payload
            .scan
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        let start = scan.endpoint_proofs[0]
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        let mut owner = if payload.open_script {
            // An open item keeps the actual script-prefix scope and its original
            // endpoint witnesses rather than treating provisional coverage as closed.
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
            // Closed script coordinates still carry original endpoint proof;
            // numeric ranges alone cannot construct certified shaping input.
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
        // The original response was consumed, so every outstanding font trial
        // immediately loses that request's authority.
        payload.font_probe_scope.take();
        payload.scan.take();
        payload.stage = Stage::Frontier;
        return Ok(());
    }
    let job = if shaping_seams.iter().any(Option::is_some) {
        // Nonfrontier input retains its complete-item contract. An actual bidi
        // seam narrows adjacent context only; it grants no new GCB/BOT/EOT fact.
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
        // With no original bidi barriers, preserve the standard job's actual
        // bounded input and adjacent-context construction.
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
    // Finished style work keeps neither font-trial authority nor old equal-coordinate tickets alive.
    payload.font_probe_scope.take();
    payload.scan.take();
    payload.stage = Stage::Input;
    Ok(())
}

/// Inspect only the current actual unit and a possible LF after CR, without
/// constructing an NSString or paragraph String to classify controls.
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
            // The same complete original proof excludes controls; no extra
            // frozen-window read is needed just to classify this starting unit.
            return Ok(None);
        }
    }
    #[cfg(not(feature = "paged-shaping-frontier"))]
    // Configurations without frontier proof retain their existing control-unit
    // query and do not reference an unavailable certificate API.
    let _ = (authority, bidi);
    inline_control(source, offset)
}

/// Without complete control-free proof, use the original finite UTF-16 cache.
/// This path's unit/cache work is not included in ItemScan's reported byte quantum.
fn inline_control(
    source: &MarkdownLayoutTextSource,
    offset: usize,
) -> Result<Option<MarkdownLineInlineControl>> {
    source.health()?;
    if offset >= source.utf16_len() {
        // Actual EOF contains no control unit and grants no out-of-range read.
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
        // Ordinary UTF-16, including surrogate units, remains original text for
        // script/bidi/grapheme analysis without character substitution.
        _ => return Ok(None),
    };
    let mut end = offset
        .checked_add(1)
        .ok_or(MarkdownLineComposeError::Overflow)?;
    if unit == 13 && end < source.utf16_len() && snapshot.utf16_code_unit_at(end)? == 10 {
        // Preserve the actual CRLF pair as one control range, including its LF
        // instead of manufacturing two independent paragraph transitions.
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
    /// Format only finite public control geometry, without exposing Source internals
    /// or retaining/copying content for diagnostics.
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
