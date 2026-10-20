//! 原来源请求内闭合块、ATX、代码块与分隔线有限语义；容器沿原 capture/Origin，不另建结构前沿。
//!
//! 只接父请求的真实 opaque Leaf/Inline seed、同一 quota 和 SourcePublication。未接完整的复杂 inline/角色
//! 保持未知；只有真实成功的交集才发布，绝不把 source 范围或已解析相邻块当作全窗口 Ready。

use super::semantic::PositionLocator;
use super::*;
use crate::markdown_document_buffer::table_source::MarkdownTableUnderlineRequest;
use crate::markdown_viewport_presentation::parser::{
    HeadingMarkerTail, markdown_selection_intersects,
};
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota;
use crate::markdown_viewport_presentation::{
    MarkdownPresentationQuery, MarkdownPresentationRun, MarkdownPresentationStyles,
    MarkdownSourceLink, markdown_physical_line_is_active,
};

pub(super) mod scoped;
use scoped::ScopedLeaf;

const CONTROL: usize = 128;
const PAGE_RUNS: usize = 64;
const PAGE_COUNT: usize = 128;
type RunPage = [Option<MarkdownPresentationRun>; PAGE_RUNS];
const LEAF_PAGE_ENTRIES: usize = 16;
const LEAF_PAGE_COUNT: usize = MAX_SOURCE_RECORDS.div_ceil(LEAF_PAGE_ENTRIES);

/// 候选来源和成功覆盖共用小页容器；只在原quota准入后逐页分配，避免一次初始化整组来源。
struct LeafPages<T> {
    pages: [Option<Box<[Option<T>; LEAF_PAGE_ENTRIES]>>; LEAF_PAGE_COUNT],
    len: usize,
}

impl<T> LeafPages<T> {
    fn new() -> Self {
        Self {
            pages: std::array::from_fn(|_| None),
            len: 0,
        }
    }
    fn page_count(&self) -> usize {
        self.len.div_ceil(LEAF_PAGE_ENTRIES)
    }
    fn retained_metadata(&self) -> usize {
        self.page_count() * std::mem::size_of::<[Option<T>; LEAF_PAGE_ENTRIES]>()
    }
    fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None; // 未发布的页槽不能冒充已完成的来源或覆盖。
        }
        self.pages[index / LEAF_PAGE_ENTRIES].as_ref()?[index % LEAF_PAGE_ENTRIES].as_ref()
    }
    fn iter(&self) -> impl Iterator<Item = &T> {
        self.pages[..self.page_count()]
            .iter()
            .flatten()
            .flat_map(|page| page.iter().flatten())
    }
    fn append(
        &mut self,
        value: T,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if self.len == MAX_SOURCE_RECORDS {
            return Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::SemanticLimit,
            )); // 与原来源目录同一上限，不能靠另一容器扩大发布库存。
        }
        let page_index = self.len / LEAF_PAGE_ENTRIES;
        let needs_page = self.pages[page_index].is_none();
        let bytes = usize::from(needs_page) * std::mem::size_of::<[Option<T>; LEAF_PAGE_ENTRIES]>();
        let work = SemanticWork {
            structural_transitions: usize::from(needs_page) * LEAF_PAGE_ENTRIES + 1,
            metadata_bytes: bytes + std::mem::size_of::<T>(),
            ..SemanticWork::default()
        };
        if work.structural_transitions > budget || work.metadata_bytes > budget {
            return Err(MarkdownTableRequestError::WorkLimit); // 准入和分配之前先核本次实际量子。
        }
        quota
            .admit_metadata(bytes)
            .map_err(MarkdownTableRequestError::Window)?;
        if needs_page {
            self.pages[page_index] = Some(Box::new(std::array::from_fn(|_| None)));
        } // 已收费的小页只初始化一次，其余槽仍不授予读取权限。
        self.pages[page_index].as_mut().unwrap()[self.len % LEAF_PAGE_ENTRIES] = Some(value);
        self.len += 1;
        Ok(work)
    }
}

/// 一段真实交集保留原 opaque 块身份；相邻段不合并，避免抹掉不同块的来源和未知空洞。
struct LeafCoverage {
    record: Rc<SourceRecord>,
    utf16: Range<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownRootLeafPendingReason {
    MissingClosedLeaf,
    IncompleteCoverage,
    NonRootScope,
    NeedsGeneralInline,
}

pub(super) enum RootLeafSemanticPublication {
    Pending(MarkdownRootLeafPendingReason),
    Ready(Box<VerifiedMarkdownRootLeafViewport>),
}

/// 每页只在原quota准入后分配；目录固定128槽，8192 run上限与现有presentation合同相同。
struct RunPages {
    pages: [Option<Box<RunPage>>; PAGE_COUNT],
    len: usize,
}

impl RunPages {
    fn new() -> Self {
        Self {
            pages: std::array::from_fn(|_| None),
            len: 0,
        }
    }
    fn page_count(&self) -> usize {
        self.len.div_ceil(PAGE_RUNS)
    }
    fn retained_metadata(&self) -> usize {
        self.page_count() * std::mem::size_of::<RunPage>()
    }
    fn last_mut(&mut self) -> Option<&mut MarkdownPresentationRun> {
        let index = self.len.checked_sub(1)?;
        self.pages[index / PAGE_RUNS]
            .as_mut()?
            .get_mut(index % PAGE_RUNS)?
            .as_mut()
    }
    fn append(
        &mut self,
        run: MarkdownPresentationRun,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if let Some(previous) = self.last_mut()
            && previous.styles == run.styles
            && previous.utf16_range.end == run.utf16_range.start
        {
            // 真正相邻的等样式段原位合并；不多占run配额或新建页。
            previous.utf16_range.end = run.utf16_range.end;
            return Ok(SemanticWork {
                metadata_bytes: std::mem::size_of::<MarkdownPresentationRun>(),
                ..SemanticWork::default()
            });
        }
        if self.len == PAGE_COUNT * PAGE_RUNS {
            return Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::SemanticLimit,
            ));
        }
        let page_index = self.len / PAGE_RUNS;
        let needs_page = self.pages[page_index].is_none();
        let page_bytes = usize::from(needs_page) * std::mem::size_of::<RunPage>();
        let metadata = page_bytes + std::mem::size_of::<MarkdownPresentationRun>();
        if metadata > budget || CONTROL + PAGE_RUNS > budget {
            // 单页初始化必须容纳在当前量子；不足不分配、不消耗原quota、不发布半段。
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        quota
            .admit_retained(1, page_bytes)
            .map_err(MarkdownTableRequestError::Window)?;
        if needs_page {
            // 联合run/页容量准入已经成功；只初始化这一页，不一次性建立8192条大数组。
            self.pages[page_index] = Some(Box::new(std::array::from_fn(|_| None)));
        }
        self.pages[page_index].as_mut().unwrap()[self.len % PAGE_RUNS] = Some(run);
        self.len += 1;
        Ok(SemanticWork {
            structural_transitions: usize::from(needs_page) * PAGE_RUNS,
            metadata_bytes: metadata,
            ..SemanticWork::default()
        })
    }
    fn query(&self, index: usize) -> Option<MarkdownPresentationQuery> {
        let count = self.page_count();
        let pages = &self.pages[..count];
        let page_index = pages.partition_point(|page| {
            let page = page.as_ref().unwrap();
            let last = page.iter().rev().find_map(Option::as_ref).unwrap();
            last.utf16_range.end <= index
        });
        let page = pages.get(page_index)?.as_ref()?;
        let used = (self.len - page_index * PAGE_RUNS).min(PAGE_RUNS);
        let next =
            page[..used].partition_point(|run| run.as_ref().unwrap().utf16_range.end <= index);
        let run = page.get(next)?.as_ref()?;
        run.utf16_range
            .contains(&index)
            .then(|| MarkdownPresentationQuery {
                utf16_range: run.utf16_range.clone(),
                styles: run.styles,
            })
    }
    fn iter(&self) -> impl Iterator<Item = &MarkdownPresentationRun> {
        self.pages[..self.page_count()]
            .iter()
            .flatten()
            .flat_map(|page| page.iter().flatten())
    }
}

/// 只保真实有限 run 页、逐闭合块交集与弱 publication；未证明的复杂 inline/容器范围仍未知。
/// 没有正文、AST、文档强owner或Native对象；Ready不代表整个requested都已完成语义。
pub struct VerifiedMarkdownRootLeafViewport {
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    publication: Weak<SourcePublication>,
    requested: Range<usize>,
    coverage: LeafPages<LeafCoverage>,
    runs: RunPages,
    links: Vec<MarkdownSourceLink>,
}

impl VerifiedMarkdownRootLeafViewport {
    fn check(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        check_authority(
            &self.source,
            self.context_revision,
            self.selection,
            document,
            authority,
        )?;
        self.publication
            .upgrade()
            .ok_or(MarkdownTableRequestError::PublicationSuperseded)?
            .require_live()
    }
    pub fn coverage_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        if self.coverage.len != 1 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        } // 旧单段getter不能把多块及中间未知区域合成凸包；多段调用方须逐段消费。
        Ok(&self.coverage.get(0).unwrap().utf16)
    }
    /// 原窗口只表达请求范围，不能据此为其中未覆盖的普通段落或容器授予语义。
    pub fn requested_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(&self.requested)
    }
    /// 按原来源顺序借出实际交集；每个非空段都独立证明，不能用首尾包络填空洞。
    pub fn covered_ranges_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<impl Iterator<Item = &'a Range<usize>> + use<'a>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(self.coverage.iter().map(|coverage| &coverage.utf16))
    }
    pub fn block_kind_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<LeafBlockKind, MarkdownTableRequestError> {
        self.check(document, authority)?;
        if self.coverage.len != 1 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        } // 一个窗口可同时含代码和分隔线，不能用首块种类代表后续块。
        let SourceRecord::Leaf(leaf) = self.coverage.get(0).unwrap().record.as_ref() else {
            // 普通块已有真实样式证明，但不是 LeafBlockKind，不能伪造另一类语法身份。
            return Err(MarkdownTableRequestError::InvalidRequest);
        };
        Ok(leaf.kind())
    }
    /// 只有真实交集内的index才有块种类；未知间隙不能退回首个块或默认Plain。
    pub fn block_kind_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<LeafBlockKind>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        let Some(coverage) = self
            .coverage
            .iter()
            .find(|part| part.utf16.contains(&index))
        else {
            return Ok(None); // 至多256个原来源交集，有限目录查询不读正文。
        };
        let SourceRecord::Leaf(leaf) = coverage.record.as_ref() else {
            return Ok(None); // 普通闭合块的查询仍由真实 run 授权，不能编造 Leaf 种类。
        };
        Ok(Some(leaf.kind()))
    }
    pub fn query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<MarkdownPresentationQuery>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        let Some(coverage) = self
            .coverage
            .iter()
            .find(|part| part.utf16.contains(&index))
        else {
            return Ok(None); // 请求窗口中的空洞同样未知，没有默认样式、隐藏或绘制权限。
        };
        Ok(self.runs.query(index).map(|mut query| {
            // 相邻同样式run可共享页记录，但一次查询仍只授予这个真实Leaf的交集。
            query.utf16_range.start = query.utf16_range.start.max(coverage.utf16.start);
            query.utf16_range.end = query.utf16_range.end.min(coverage.utf16.end);
            query
        }))
    }
    pub fn runs_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<
        impl Iterator<Item = &'a MarkdownPresentationRun> + use<'a>,
        MarkdownTableRequestError,
    > {
        self.check(document, authority)?;
        Ok(self.runs.iter())
    }
    pub(super) fn retained_metadata(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.coverage.retained_metadata()
            + self.runs.retained_metadata()
            + self.links.capacity() * std::mem::size_of::<MarkdownSourceLink>()
            + self
                .links
                .iter()
                .map(|link| link.destination.capacity())
                .sum::<usize>()
    }
    /// 原有限Parser已证明的完整label可独立返回正链接；它不授予窗口外样式或负证明。
    /// 没有正事实时，只有原发布覆盖内的None才证明无链接，未知空洞仍是ContextPending。
    pub fn link_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<&MarkdownSourceLink>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        if let Some(link) = self
            .links
            .iter()
            .find(|link| link.label_utf16_range.contains(&index))
        {
            // label与decoded目标已由同源闭合块全量有限事件证明并付配额；不能被当前小窗裁掉。
            return Ok(Some(link));
        }
        if self.query_for(document, authority, index)?.is_none() {
            return Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::ContextPending,
            ));
        }
        Ok(None)
    }
    pub(super) fn run_count(&self) -> usize {
        self.runs.len
    }
}

#[derive(Clone, Copy)]
enum Phase {
    Bounds,
    ResumeUnderline,
    AwaitSource,
    NextLeaf,
    Scope,
    HeadingMarkers,
    Heading,
    Coverage,
    Boundary,
    Coordinates,
    Underline,
    Build,
    AtxInline,
    Ordinary,
    OrdinaryRuns,
    DiscardOrdinary,
    FinishLeaf,
    Done,
    Published,
}

/// 一次来源发布的叶语义续体。所有实际工作仍由父请求account_source_work写回原累计fuel；
/// quota必须来自该父batch的同一Rc，不能构造新额度或另开Global。
pub(super) struct RootLeafSemanticJob {
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    requested: Range<usize>,
    positions: [PositionLocator; 4],
    position_index: usize,
    byte_coverage: Option<Range<usize>>,
    // 只接受父同源请求已经完成的两个选择端点；None才需要在本冷窗口实际定位。
    selection_bytes: Option<Range<usize>>,
    candidates: LeafPages<Rc<SourceRecord>>,
    candidate_index: usize,
    coverage: LeafPages<LeafCoverage>,
    record: Option<Rc<SourceRecord>>,
    active_byte_coverage: Option<Range<usize>>,
    active_coverage: Option<Range<usize>>,
    saw_intersection: bool,
    boundary: Option<FenceBoundaryProof>,
    scoped: Option<Box<ScopedLeaf>>,
    ordinary_run: Option<MarkdownPresentationRun>,
    heading_next: Option<usize>,
    heading_tail: Option<AtxMarkerTail>,
    raw_markers: [Option<Range<usize>>; 2],
    projection: Option<Coordinates>,
    markers: Option<[Range<usize>; 2]>,
    underline: Option<MarkdownTableUnderlineRequest>,
    underline_index: usize,
    // 普通块只借父 SourceRequest 已准入的唯一 semantic 槽；本续体只记逐 run 迁移位置。
    ordinary_run_index: usize,
    run_next: usize,
    runs: RunPages,
    links: Vec<MarkdownSourceLink>,
    quota: MarkdownTableWindowQuota,
    pending: Option<MarkdownRootLeafPendingReason>,
    failed: Option<MarkdownTableRequestError>,
    phase: Phase,
}

impl RootLeafSemanticJob {
    /// Pending发布或逆向重定位真正释放固定续体时归账；Ready移交不虚报一次Drop。
    pub(super) fn completed_owner_drop_control() -> usize {
        // publish已计量清候选并移走输出；这里只遍历空run目录和两组小页目录，不再虚付256个候选。
        CONTROL + PAGE_COUNT + 2 * LEAF_PAGE_COUNT
    }

    pub(super) fn construction_work() -> SemanticWork {
        SemanticWork {
            structural_transitions: CONTROL + PAGE_COUNT + 2 * LEAF_PAGE_COUNT,
            metadata_bytes: std::mem::size_of::<Self>(),
            ..SemanticWork::default()
        }
    }
    pub(super) fn new(
        source: MarkdownTableSourceIdentity,
        context_revision: u64,
        selection: MarkdownTextSelection,
        requested: Range<usize>,
        selection_bytes: Option<Range<usize>>,
        underline: Option<MarkdownTableUnderlineRequest>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<Self, MarkdownTableRequestError> {
        if requested.start >= requested.end || requested.len() > 64 * 1_024 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        // 固定job和未来Box头先联合准入；正文页、run页与underline容量均未在此分配。
        quota
            .admit_metadata(
                std::mem::size_of::<Self>()
                    + std::mem::size_of::<VerifiedMarkdownRootLeafViewport>(),
            )
            .map_err(MarkdownTableRequestError::Window)?;
        let ordered = ordered_selection(selection);
        Ok(Self {
            source,
            context_revision,
            selection,
            positions: [
                PositionLocator::new(requested.start, false),
                PositionLocator::new(requested.end, true),
                PositionLocator::new(ordered.start, false),
                PositionLocator::new(ordered.end, true),
            ],
            position_index: 0,
            run_next: requested.start,
            requested,
            byte_coverage: None,
            selection_bytes,
            candidates: LeafPages::new(),
            candidate_index: 0,
            coverage: LeafPages::new(),
            record: None,
            active_byte_coverage: None,
            active_coverage: None,
            saw_intersection: false,
            boundary: None,
            scoped: None,
            ordinary_run: None,
            heading_next: None,
            heading_tail: None,
            raw_markers: [None, None],
            projection: None,
            markers: None,
            underline,
            underline_index: 0,
            ordinary_run_index: 0,
            runs: RunPages::new(),
            links: Vec::new(),
            quota,
            pending: None,
            failed: None,
            phase: Phase::Bounds,
        })
    }

    /// Ready已移走全部输出后复用父原唯一槽；新窗口仍从Bounds领取自己的真实byte覆盖。
    /// 这里只保两个小片定位前沿和不可变Source/完整选择，绝不保旧publication或未来语义。
    pub(super) fn continue_forward(
        &mut self,
        requested: Range<usize>,
        underline: Option<MarkdownTableUnderlineRequest>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if !matches!(self.phase, Phase::Published)
            || requested.start < self.requested.start
            || requested.end < self.requested.end
            || requested.is_empty()
            || requested.len() > 64 * 1_024
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        // 原Ready仍占联合库存；固定job也必须进入最新同batch额度，不能沿用上一批准入。
        quota
            .admit_metadata(
                std::mem::size_of::<Self>()
                    + std::mem::size_of::<VerifiedMarkdownRootLeafViewport>(),
            )
            .map_err(MarkdownTableRequestError::Window)?;
        let first = self.positions[0].retarget_forward(requested.start, false);
        let last = self.positions[1].retarget_forward(requested.end, true);
        self.run_next = requested.start;
        self.requested = requested;
        self.position_index = 0;
        self.byte_coverage = None;
        self.candidate_index = 0;
        self.saw_intersection = false;
        self.underline = underline;
        self.underline_index = 0;
        self.ordinary_run_index = 0;
        self.quota = quota;
        self.pending = None;
        self.phase = Phase::Bounds;
        // 页目录已在publish真正移走/清空，本步没有重建页数组或Box；只计实际固定重定位。
        // 选择端点仍属原完整选择，新窗口的两个locator必须再次完成，旧样式不授任何覆盖。
        Ok(SemanticWork {
            structural_transitions: CONTROL
                + first.structural_transitions
                + last.structural_transitions,
            metadata_bytes: first.metadata_bytes + last.metadata_bytes,
            ..SemanticWork::default()
        })
    }
    fn check(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        check_authority(
            &self.source,
            self.context_revision,
            self.selection,
            document,
            authority,
        )
    }
    fn fail(&mut self, error: MarkdownTableRequestError) {
        self.failed = Some(error);
        self.candidates = LeafPages::new();
        self.coverage = LeafPages::new();
        self.record = None;
        self.active_byte_coverage = None;
        self.active_coverage = None;
        self.boundary = None;
        self.scoped = None;
        self.ordinary_run = None;
        self.heading_next = None;
        self.heading_tail = None;
        self.projection = None;
        self.underline = None;
        self.runs = RunPages::new(); // 只释放有限派生页；旧外带publication不在job中，也不被改写。
        self.links = Vec::new(); // 取消时同步归还本发布尚未移交的 URL 字节与目录。
    }
    pub(super) fn cancel(&mut self) {
        if self.failed.is_none() {
            self.fail(MarkdownTableRequestError::Cancelled);
        }
    }
    pub(super) fn preparing(&self) -> bool {
        matches!(self.phase, Phase::Bounds | Phase::ResumeUnderline)
    }
    pub(super) fn done(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }

    /// 仅归还原四端点定位实际完成后的选择边界；父请求继续拥有相同 Source 与完整选择。
    pub(super) fn verified_selection_bytes(&self) -> Option<Range<usize>> {
        self.selection_bytes.clone()
    }

    /// 发布只移交样式库存；原请求接回唯一scanner，后继窗口仍消费其真实UTF16/标签前沿。
    pub(super) fn return_underline(&mut self) -> Option<MarkdownTableUnderlineRequest> {
        self.underline.take()
    }

    /// 原来源可能为跨过空白而先消费下一行；保它的同一 seed，后继才能沿原 Global 解析该正文。
    /// 前方无交集的旧块仍立即释放；此票不授予当前窗口任何额外语义覆盖。
    pub(super) fn retains_inline_seed(&self, physical: &Range<usize>) -> bool {
        self.byte_coverage
            .as_ref()
            .is_some_and(|coverage| coverage.start < physical.end)
    }

    /// 父owner只移交自己的真实来源Rc；与该页不相交的记录不进入本job库存。
    pub(super) fn offer_record(
        &mut self,
        record: &Rc<SourceRecord>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if !matches!(self.phase, Phase::AwaitSource) {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let physical = record.utf16_range();
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        if physical.start < self.requested.end && self.requested.start < physical.end {
            self.saw_intersection = true;
            let source_start = match record.as_ref() {
                SourceRecord::Leaf(leaf) => leaf.source_range().start,
                SourceRecord::Line(line) => {
                    let Some(block) = line.semantic_block.as_ref() else {
                        return Ok(work); // SourcesOnly 行只有来源计量，没有真实 Inline seed。
                    };
                    block.seed.source_range().start
                }
            };
            if let Some(previous) = self.candidates.get(self.candidates.len.saturating_sub(1)) {
                let previous_start = match previous.as_ref() {
                    SourceRecord::Leaf(leaf) => {
                        if Rc::ptr_eq(previous, record) {
                            return Ok(work); // 续批重遇同一 opaque Leaf 不再占候选页。
                        }
                        leaf.source_range().start
                    }
                    SourceRecord::Line(line) => {
                        let block = line.semantic_block.as_ref().unwrap();
                        if matches!(record.as_ref(), SourceRecord::Line(current)
                            if current.semantic_block.as_ref().is_some_and(|current| Rc::ptr_eq(current, block)))
                        {
                            return Ok(work); // 同一多行闭合块只证明一次，不能按物理行重复覆盖。
                        }
                        if Rc::ptr_eq(previous, record) {
                            return Ok(work); // 同一容器行续批只交付一次，不重复占候选或语义库存。
                        }
                        block.seed.source_range().start
                    }
                };
                if previous_start >= source_start {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                } // 只接受同一 Global 的真实源序，不能排序或用相同数值冒充同一块。
            }
            let measured =
                self.candidates
                    .append(Rc::clone(record), &self.quota, 4096 - CONTROL)?;
            work.structural_transitions += measured.structural_transitions;
            work.metadata_bytes = measured.metadata_bytes;
        }
        Ok(work)
    }

    /// 首四端点与后继Leaf阶段共用此入口；父owner只在原来源批次Ready后推进非Bounds阶段。
    /// source/选择/取消逐次核验，真实失权固定首因；不再用重复布尔值描述父调用位置。
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        budget: usize,
        limits: (usize, usize),
        ordinary: &mut Option<PlainSemanticJob>,
        global: Option<&mut GlobalFrontier>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let result = self.check(document, authority).and_then(|()| {
            if !(4096..=65536).contains(&budget) || matches!(self.phase, Phase::Published) {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            self.advance_inner(document, authority, budget, limits, ordinary, global)
        });
        if let Err(error) = result {
            *ordinary = None; // 父槽属于同一失败请求，错误不能留下旧 cursor 或未移交的 underline。
            self.fail(error);
        }
        result
    }
    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        budget: usize,
        limits: (usize, usize),
        ordinary: &mut Option<PlainSemanticJob>,
        global: Option<&mut GlobalFrontier>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        match self.phase {
            Phase::Bounds => work = self.advance_bounds(document, authority, budget)?,
            Phase::ResumeUnderline => {
                // 四端点或父已证选择已经齐备；只在独立原量子内转换这一个scanner的覆盖。
                // 新范围若不能续读，旧owner立即归还，后继Underline阶段沿原冷工厂从根认证。
                let mut underline = self.underline.take().expect("续批持原唯一scanner");
                if underline.continue_source(
                    document,
                    self.byte_coverage.as_ref().unwrap().clone(),
                    self.requested.clone(),
                    self.quota.clone(),
                )? {
                    self.underline = Some(underline);
                }
                work.metadata_bytes = 2 * std::mem::size_of::<Range<usize>>();
                self.phase = Phase::AwaitSource;
            }
            Phase::AwaitSource => {
                self.phase = Phase::NextLeaf; // 完整来源只结束收集；下一量子才开始第一份真实Leaf证明。
            }
            Phase::NextLeaf => {
                work = self.begin_next_leaf(document, limits, ordinary)?;
            }
            Phase::Scope => {
                let scoped = self.scoped.as_mut().unwrap();
                let (measured, done) = scoped.advance(document, &self.quota, budget - CONTROL)?;
                work = measured;
                work.structural_transitions += CONTROL;
                if done {
                    if scoped.unsupported() {
                        self.pending
                            .get_or_insert(MarkdownRootLeafPendingReason::NonRootScope);
                        self.clear_active_leaf();
                        self.phase = Phase::NextLeaf;
                    } else {
                        let SourceRecord::Leaf(leaf) = self.record.as_ref().unwrap().as_ref()
                        else {
                            unreachable!("Scope 只消费真实 Leaf");
                        };
                        self.phase = match leaf.kind() {
                            LeafBlockKind::AtxHeading(_) if self.heading_tail.is_some() => {
                                Phase::HeadingMarkers
                            }
                            LeafBlockKind::AtxHeading(_) => Phase::Coverage,
                            LeafBlockKind::FencedCode => Phase::Boundary,
                            LeafBlockKind::IndentedCode
                            | LeafBlockKind::Html
                            | LeafBlockKind::Metadata(_) => Phase::Coverage,
                            _ => return Err(MarkdownTableRequestError::InvalidRequest),
                        };
                    }
                } // 原 capture 与本窗口逐行 prefix 全部完成之后，才允许标题/代码语义继续。
            }
            Phase::HeadingMarkers => {
                let tail = self.heading_tail.as_mut().unwrap();
                work = tail.advance(document, budget - CONTROL)?;
                work.structural_transitions += CONTROL;
                if tail.done() {
                    self.raw_markers[1] = tail.marker();
                    self.heading_tail = None;
                    self.phase = Phase::Heading;
                } // 仅旧显示helper的真实尾段完成后续Plain正文；不改firstpass的closing事实。
            }
            Phase::Heading => work = self.advance_heading(document, budget)?,
            Phase::Coverage => {
                let bytes = self.active_byte_coverage.as_ref().unwrap();
                if let Some(projection) = self.projection.as_mut() {
                    let (measured, values) = projection.advance(document, budget - CONTROL)?;
                    work = measured;
                    work.structural_transitions += CONTROL;
                    if let Some(values) = values {
                        let start = values[0].max(self.requested.start);
                        let end = values[1].min(self.requested.end);
                        if start >= end {
                            return Err(MarkdownTableRequestError::InvalidRequest);
                        } // 非空byte交集必须对应非空scalar对齐UTF16，不能制造零宽已证段。
                        self.active_coverage = Some(start..end);
                        self.run_next = start;
                        self.projection = None;
                        self.phase = match self.record.as_ref().unwrap().as_ref() {
                            SourceRecord::Leaf(leaf)
                                if matches!(leaf.kind(), LeafBlockKind::AtxHeading(_))
                                    && self.heading_next.is_some() =>
                            {
                                Phase::AtxInline
                            }
                            SourceRecord::Leaf(_) => Phase::Coordinates,
                            SourceRecord::Line(_) => Phase::Underline,
                        }; // 普通块复用同一已计量交集；其样式仍须原 Plain cursor 真正证明。
                    }
                } else {
                    // 真实Leaf的source边界可能不同于physical/consumed范围；不能把邻行prefix纳入覆盖。
                    let (projection, measured) = Coordinates::new(
                        document,
                        [bytes.start, bytes.end, bytes.end, bytes.end],
                        budget - CONTROL,
                    )?;
                    self.projection = Some(projection);
                    work = measured;
                    work.structural_transitions += CONTROL;
                }
            }
            Phase::Boundary => {
                let boundary = self.boundary.as_mut().unwrap();
                work = boundary.advance(document)?;
                work.structural_transitions += CONTROL;
                if boundary.done {
                    self.raw_markers = [Some(boundary.opening.clone()), boundary.closing.clone()];
                    self.phase = Phase::Coverage;
                }
            }
            Phase::Coordinates => {
                if self.projection.is_none() {
                    let coverage = self.active_byte_coverage.as_ref().unwrap();
                    let empty_start = match self.record.as_ref().unwrap().as_ref() {
                        SourceRecord::Leaf(leaf) if leaf.kind() == LeafBlockKind::IndentedCode => {
                            leaf.source_range()
                                .start
                                .clamp(coverage.start, coverage.end)
                        }
                        _ => coverage.start,
                    }; // 缩进代码没有围栏；空marker仍保原正文起点，给已证首行物理缩进一个准确样式切点。
                    let first =
                        self.raw_markers[0]
                            .as_ref()
                            .map_or(empty_start..empty_start, |range| {
                                range.start.clamp(coverage.start, coverage.end)
                                    ..range.end.clamp(coverage.start, coverage.end)
                            });
                    let last =
                        self.raw_markers[1]
                            .as_ref()
                            .map_or(coverage.end..coverage.end, |range| {
                                range.start.clamp(coverage.start, coverage.end)
                                    ..range.end.clamp(coverage.start, coverage.end)
                            });
                    let (projection, measured) = Coordinates::new(
                        document,
                        [first.start, first.end, last.start, last.end],
                        budget - CONTROL,
                    )?;
                    self.projection = Some(projection);
                    work = measured;
                    work.structural_transitions += CONTROL;
                } else {
                    let (measured, values) = self
                        .projection
                        .as_mut()
                        .unwrap()
                        .advance(document, budget - CONTROL)?;
                    work = measured;
                    work.structural_transitions += CONTROL;
                    if let Some(values) = values {
                        self.markers = Some([values[0]..values[1], values[2]..values[3]]);
                        self.projection = None;
                        self.phase = match self.record.as_ref().unwrap().as_ref() {
                            SourceRecord::Leaf(leaf)
                                if matches!(
                                    leaf.kind(),
                                    LeafBlockKind::FencedCode | LeafBlockKind::IndentedCode
                                ) =>
                            {
                                Phase::Build
                            }
                            _ => Phase::Underline, // 非代码块继续等待同一全源配对，不能默认已知不存在标签。
                        }; // 真实代码本体始终字面；不为它创建或推进全源HTML配对，旧共享owner留给后继块。
                    }
                }
            }
            Phase::Underline => {
                if let Some(underline) = self.underline.as_mut() {
                    if matches!(
                        self.record.as_ref().unwrap().as_ref(),
                        SourceRecord::Line(_)
                    ) {
                        // 当前闭合块先取得原配对 owner，再收集自己的有限引用；
                        // 否则 code 前的引用会在尚未有查询权限时阻止配对，而配对又阻止查询准备。
                        work = self.begin_ordinary(ordinary, budget)?;
                    } else if underline
                        .ready_pairs_for(document)
                        .map_err(MarkdownTableRequestError::Window)?
                        .is_some()
                    {
                        // 非代码块才消费全源配对；代码本体不解释HTML，也不改变本共享游标。
                        self.phase = Phase::Build;
                    } else {
                        // 未完成时只推进原有请求的有限 frame；不重建上下文，也不循环等到 Ready。
                        let measured = underline.advance(
                            document,
                            global.ok_or(MarkdownTableRequestError::InvalidRequest)?,
                            budget - CONTROL,
                        )?;
                        work.source_bytes = measured.source_bytes + measured.global_source_bytes;
                        work.utf8_validation_bytes = measured.utf8_validation_bytes;
                        work.source_tree_steps =
                            measured.source_tree_steps + measured.global_source_tree_steps;
                        work.structural_transitions += measured.structural_transitions;
                        work.copied_source_bytes = measured.copied_source_bytes;
                        work.metadata_bytes = measured.metadata_bytes;
                        work.normalizer_input_bytes = measured.normalizer_input_bytes;
                        work.parser_input_bytes = measured.parser_input_bytes;
                        work.allocated_source_capacity = measured.allocated_source_capacity;
                    }
                } else {
                    // 与table/Plain共享原唯一underline工作区；必须先准入再创建context/pair库存。
                    self.quota
                        .reserve_underline_workspace()
                        .map_err(MarkdownTableRequestError::Window)?;
                    self.underline = Some(
                        document
                            .table_source_underline_request(
                                self.byte_coverage.as_ref().unwrap().clone(),
                                self.quota.clone(),
                            )
                            .map_err(MarkdownTableRequestError::Window)?,
                    );
                }
            }
            Phase::Build => work = self.build_one(document, budget)?,
            Phase::AtxInline => work = self.begin_atx_inline(ordinary, budget)?,
            Phase::Ordinary => {
                let job = ordinary.as_mut().unwrap();
                let (measured, _) = match self.record.as_ref().unwrap().as_ref() {
                    SourceRecord::Leaf(leaf) => job.advance_atx_after_sources(
                        document,
                        &leaf.block,
                        budget - CONTROL,
                        limits,
                        global,
                    )?,
                    SourceRecord::Line(_) => {
                        job.advance_after_sources(document, budget - CONTROL, limits, global)?
                    }
                };
                work = measured;
                work.structural_transitions += CONTROL;
                if job.done() {
                    if let Some(reason) = job.pending_reason() {
                        self.pending.get_or_insert(match reason {
                            MarkdownOrdinaryPendingReason::MissingClosedBlock => {
                                MarkdownRootLeafPendingReason::MissingClosedLeaf
                            }
                            MarkdownOrdinaryPendingReason::IncompleteCoverage => {
                                MarkdownRootLeafPendingReason::IncompleteCoverage
                            }
                            MarkdownOrdinaryPendingReason::NeedsGeneralInline => {
                                MarkdownRootLeafPendingReason::NeedsGeneralInline
                            }
                            MarkdownOrdinaryPendingReason::NonRootScope => {
                                MarkdownRootLeafPendingReason::NonRootScope
                            }
                        });
                        self.phase = Phase::DiscardOrdinary; // 下一量子真实归还旧 owner，不挤占引用 scanner 的最小工作。
                    } else {
                        self.phase = Phase::OrdinaryRuns;
                    } // 固定 scratch 中的 run 尚未取得输出库存，下一量子才逐条联合准入。
                }
            }
            Phase::OrdinaryRuns => {
                let job = ordinary.as_mut().unwrap();
                if self.ordinary_run.is_none() {
                    self.ordinary_run = job.take_proven_run(self.ordinary_run_index)?;
                }
                if let Some(run) = self.ordinary_run.as_mut() {
                    let query = self
                        .scoped
                        .as_ref()
                        .map(|scoped| scoped.query(run.utf16_range.start, run.utf16_range.end));
                    let end = query
                        .as_ref()
                        .map_or(run.utf16_range.end, |query| query.utf16_range.end);
                    let styles = query.map_or(run.styles, |query| run.styles.union(query.styles));
                    let measured = self.runs.append(
                        MarkdownPresentationRun {
                            utf16_range: run.utf16_range.start..end,
                            styles,
                        },
                        &self.quota,
                        budget - CONTROL,
                    )?;
                    run.utf16_range.start = end;
                    if run.utf16_range.is_empty() {
                        self.ordinary_run = None;
                        self.ordinary_run_index += 1;
                    }
                    work.structural_transitions += measured.structural_transitions;
                    work.metadata_bytes = measured.metadata_bytes;
                } else if let Some(link) = job.take_proven_link()? {
                    // URL 字节只移动归属；新目录容量与旧 job 共活，仍须独立联合准入。
                    self.quota
                        .admit_metadata(std::mem::size_of::<MarkdownSourceLink>())
                        .map_err(MarkdownTableRequestError::Window)?;
                    self.links.reserve_exact(1);
                    self.links.push(link);
                    work.metadata_bytes = std::mem::size_of::<MarkdownSourceLink>();
                } else {
                    self.underline = job.return_underline();
                    // 真正完成的普通前缀留在父原唯一槽；下一块仍须核同一个 seed 才能继续。
                    // 复杂语义不保留历史工作区，原配对 owner 已归还后在本量子真正析构。
                    if !job.retains_plain_prefix() {
                        *ordinary = None;
                        work.structural_transitions += PlainSemanticJob::completed_drop_control();
                    }
                    self.phase = Phase::FinishLeaf;
                }
            }
            Phase::DiscardOrdinary => {
                self.underline = ordinary.as_mut().unwrap().return_underline();
                *ordinary = None;
                self.clear_active_leaf();
                self.phase = Phase::NextLeaf;
                work.structural_transitions += PlainSemanticJob::completed_drop_control();
            }
            Phase::FinishLeaf => {
                let record = Rc::clone(self.record.as_ref().unwrap());
                let utf16 = self.active_coverage.as_ref().unwrap().clone();
                if self
                    .coverage
                    .get(self.coverage.len.saturating_sub(1))
                    .is_some_and(|previous| previous.utf16.end > utf16.start)
                {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                } // 成功交集必须真实无重叠；不裁切前一块、排序或取凸包来修补来源矛盾。
                let measured = self.coverage.append(
                    LeafCoverage { record, utf16 },
                    &self.quota,
                    budget - CONTROL,
                )?;
                work.structural_transitions += measured.structural_transitions;
                work.metadata_bytes = measured.metadata_bytes;
                self.clear_active_leaf();
                self.phase = Phase::NextLeaf; // 全部 run 成功后才发布这一段，未知语法的空洞仍没有 run。
            }
            Phase::Done => {} // 一次显式publish移走结果；重复advance不分配第二份Ready。
            Phase::Published => return Err(MarkdownTableRequestError::InvalidRequest),
        }
        Ok(work)
    }

    /// 冷窗口的四个端点共用原4096量子，不把每次树定位、短scalar或发布坐标拆成一轮主队列。
    /// 这里仅合并同一Bounds已有步骤，不能推进Global、重建locator或预支下一量子的预算。
    fn advance_bounds(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork::default();
        let mut remaining = budget;
        // 父请求的Source/context/完整选择从创建到续批均不变；上一真实发布已认证的选择
        // 字节端点可以原样交接，不能为每个小样式窗口重新解码远处选择所在的源片。
        // 冷路径仍跑完整四端点；热路径只跑当前窗口两端，始终使用原定位器和原预算。
        let position_limit = if self.selection_bytes.is_some() { 2 } else { 4 };
        while self.position_index < position_limit {
            let locator = &mut self.positions[self.position_index];
            let may_reuse = self.position_index != 0 && locator.needs_location();
            let reuse_reservation = if may_reuse {
                PositionLocator::PREFIX_REUSE_CONTROL + PositionLocator::PREFIX_REUSE_METADATA
            } else {
                0
            };
            if remaining < CONTROL + locator.minimum_budget(document) + reuse_reservation {
                break; // 原下一量子续同一locator；低余额不能变成非法子预算或凭空完成端点。
            }
            self.check(document, authority)?;
            let mut measured = self.positions[self.position_index]
                .advance(document, remaining - CONTROL - reuse_reservation)?;
            self.check(document, authority)?; // 每份源帧真正归还后仍守原Source/取消/完整选择。
            if may_reuse {
                // 目标已独立定位同一真实片/ordinal后才核前一端点，不把远选区中间正文接成线性扫描。
                let (previous, following) = self.positions.split_at_mut(self.position_index);
                let reused =
                    following[0].reuse_completed_prefix(&previous[self.position_index - 1]);
                measured.structural_transitions += PositionLocator::PREFIX_REUSE_CONTROL;
                if reused {
                    measured.metadata_bytes += PositionLocator::PREFIX_REUSE_METADATA;
                } // 未发生标量移交不虚计metadata；预留未使用额度仍属于当前原量子。
            }
            // 与原Coordinates同口径：树路径已包含在transitions，UTF8验证亦已由解码步骤计量。
            let cost = measured.source_bytes
                + measured.structural_transitions
                + measured.metadata_bytes
                + CONTROL;
            remaining -= cost;
            work.source_bytes += measured.source_bytes;
            work.utf8_validation_bytes += measured.utf8_validation_bytes;
            work.source_tree_steps += measured.source_tree_steps;
            work.structural_transitions += measured.structural_transitions + CONTROL;
            work.metadata_bytes += measured.metadata_bytes;
            if self.positions[self.position_index].result().is_some() {
                self.position_index += 1; // 只有原locator实际给出byte边界才移交下一个端点。
            }
            // 每步至少支付原PLAIN_CONTROL，且必然完成定位/读入byte/给出结果之一；余额严格减少，不空转。
        }
        if self.position_index == position_limit {
            self.byte_coverage =
                Some(self.positions[0].result().unwrap()..self.positions[1].result().unwrap());
            if self.selection_bytes.is_none() {
                self.selection_bytes =
                    Some(self.positions[2].result().unwrap()..self.positions[3].result().unwrap());
            }
            self.phase = if self.underline.is_some() {
                Phase::ResumeUnderline
            } else {
                Phase::AwaitSource
            }; // 本窗口和选择端点全齐；原scanner覆盖接续仍占下一独立量子。
        }
        Ok(work)
    }

    /// 一个量子只接一份同源候选；不能循环跳过所有未知块，也不能从可见正文重判语法。
    fn begin_next_leaf(
        &mut self,
        document: &MarkdownDocumentBuffer,
        limits: (usize, usize),
        ordinary: &mut Option<PlainSemanticJob>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        let Some(record) = self.candidates.get(self.candidate_index).map(Rc::clone) else {
            if self.coverage.len == 0 && self.pending.is_none() {
                self.pending = Some(if self.saw_intersection {
                    MarkdownRootLeafPendingReason::IncompleteCoverage
                } else {
                    MarkdownRootLeafPendingReason::MissingClosedLeaf
                });
            } // 只有实际成功段可签Ready；空目录依旧明确Pending。
            self.phase = Phase::Done;
            return Ok(work);
        };
        if ordinary.as_ref().is_some_and(|job| match record.as_ref() {
            SourceRecord::Line(line) => line
                .semantic_block
                .as_ref()
                .is_none_or(|block| !job.retains_plain_block(block)),
            SourceRecord::Leaf(_) => true,
        }) {
            // 新真实块不能继承上一普通块的前缀；只付本次旧 owner 的实际释放，
            // candidate 尚未前移，下一原量子再处理这条来源，不把析构和新构造挤在一起。
            *ordinary = None;
            work.structural_transitions += PlainSemanticJob::completed_drop_control();
            return Ok(work);
        }
        self.candidate_index += 1;
        let SourceRecord::Leaf(leaf) = record.as_ref() else {
            let SourceRecord::Line(line) = record.as_ref() else {
                unreachable!("来源只有 Leaf 或 Line");
            };
            let block = line.semantic_block.as_ref().unwrap();
            let physical = block.seed.physical_range();
            let requested = self.byte_coverage.as_ref().unwrap();
            let intersection = physical.start.max(requested.start)..physical.end.min(requested.end);
            if !intersection.is_empty() {
                let complete_window = intersection == *requested;
                self.active_byte_coverage = Some(intersection);
                self.record = Some(record);
                if complete_window {
                    // Bounds 已在原 Source 上逐量子证明请求两端的 floor/ceil 字节位置。
                    // 普通块完整包含这个外包时，原 Coverage 解码后再 max/min requested
                    // 必然得到同一 requested；包括非BMP中间UTF16端点，也不能把scalar外包扩大发布。
                    self.active_coverage = Some(self.requested.clone());
                    self.run_next = self.requested.start;
                    self.phase = Phase::Underline;
                    work.metadata_bytes += std::mem::size_of::<Range<usize>>()
                        + std::mem::size_of::<Option<Range<usize>>>()
                        + std::mem::size_of::<usize>();
                } else {
                    // 只有一部分请求属于这个真实块时，仍须独立解码交集的UTF16端点。
                    self.phase = Phase::Coverage;
                }
            } // 原闭合 Inline 的物理范围仍由 seed 提供，不能把行外邻段合成一个块。
            return Ok(work);
        };
        if leaf.block.source_revision() != document.generation()
            || leaf.block.source_end() != document.byte_len()
        {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if leaf.block.options()
            != crate::markdown_viewport_presentation::parser::markdown_presentation_options(true)
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        } // 其他解析选项的opaque事实不能为当前产品语义补签。
        let scoped = leaf.block.scope_depth() != 0;
        if scoped
            && !matches!(
                leaf.kind(),
                LeafBlockKind::AtxHeading(_)
                    | LeafBlockKind::FencedCode
                    | LeafBlockKind::IndentedCode
                    | LeafBlockKind::Html
                    | LeafBlockKind::Metadata(_)
            )
        {
            self.pending
                .get_or_insert(MarkdownRootLeafPendingReason::NonRootScope);
            return Ok(work); // 未接容器语法仍保留空洞，不从相似 marker 补造权限。
        }
        let source = leaf.source_range();
        let requested = self.byte_coverage.as_ref().unwrap();
        let start = if scoped
            || matches!(
                leaf.kind(),
                LeafBlockKind::FencedCode
                    | LeafBlockKind::IndentedCode
                    | LeafBlockKind::Html
                    | LeafBlockKind::Metadata(_)
            ) {
            leaf.physical_start()
        } else {
            source.start
        }; // 真实代码/字面叶保留首行物理空白；不扩大到 consumed_end 的邻行前缀或未证明容器。
        let intersection = start.max(requested.start)..source.end.min(requested.end);
        if intersection.is_empty() {
            return Ok(work); // physical范围可能碰到后继prefix，该prefix没有本Leaf的语义权限。
        }
        match leaf.kind() {
            LeafBlockKind::AtxHeading(_) => {
                // 标题种类、正文及真实#范围只消费原Leaf事实；特殊inline仍由原Plain内核识别。
                let LeafBlockDetails::AtxHeading {
                    opening_marker,
                    lexical_content,
                    attributes,
                    ..
                } = leaf.details()
                else {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                };
                let mut opening = opening_marker.clone();
                if opening.end < source.end {
                    let (byte, measured) = peek(document, opening.end)?;
                    work.source_bytes += measured.source_bytes;
                    work.source_tree_steps += measured.source_tree_steps;
                    work.structural_transitions += measured.structural_transitions;
                    if byte == b' ' {
                        opening.end += 1; // 与旧helper一致：只收一个ASCII space，Tab与余下空白保持字面。
                    }
                }
                self.raw_markers = [Some(opening), None];
                self.heading_next = Some(lexical_content.start);
                if attributes.is_some() {
                    if PlainSemanticJob::supports_closed_atx(&leaf.block) {
                        self.phase = Phase::Coverage;
                    } else {
                        self.pending
                            .get_or_insert(MarkdownRootLeafPendingReason::NeedsGeneralInline);
                        return Ok(work);
                    }
                } else {
                    self.heading_tail = Some(AtxMarkerTail::new(source));
                    self.phase = Phase::HeadingMarkers;
                } // 属性语法必须走原完整短标题入口，不能在 plain 探针中忽略或返回装配错误。
            }
            LeafBlockKind::FencedCode => {
                self.boundary = Some(FenceBoundaryProof::new(leaf)?);
                self.phase = Phase::Boundary;
            }
            LeafBlockKind::IndentedCode => self.phase = Phase::Coverage,
            LeafBlockKind::Html | LeafBlockKind::Metadata(_) => {
                // 原完整 Parser 对这两类真实叶保持 canonical 字面：没有 Markdown inline 样式、
                // HTML 执行、metadata marker 隐藏或 anchor。唯一 <u> allowlist 仍经原共享 Underline，
                // 因而这里仅选择 Coverage，绝不能略过后继配对就发布默认样式。
                self.phase = Phase::Coverage;
            }
            LeafBlockKind::ThematicBreak => {
                self.raw_markers[0] = Some(source);
                self.phase = Phase::Coverage;
            }
        }
        if scoped {
            self.scoped = Some(ScopedLeaf::new(
                &leaf.block,
                intersection.clone(),
                self.selection_bytes
                    .clone()
                    .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                limits,
                &self.quota,
            )?);
            work.metadata_bytes += std::mem::size_of::<ScopedLeaf>();
            self.phase = Phase::Scope;
        } // Leaf 的原 capture、opening state 与逐行 Origin 必须实际消费，不能仅删除 NonRootScope。
        self.active_byte_coverage = Some(intersection);
        self.record = Some(record);
        Ok(work)
    }

    /// 原 byte/UTF16 交集已完成；将唯一配对 owner 移给当前真实闭合块，查询准备先于其后继前探。
    fn begin_ordinary(
        &mut self,
        ordinary: &mut Option<PlainSemanticJob>,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let bytes = PlainSemanticJob::closed_block_metadata();
        if bytes > budget {
            return Err(MarkdownTableRequestError::WorkLimit);
        } // 原 SourceRequest 的固定槽已经联合预留；本量子只填写实际活跃字段。
        let SourceRecord::Line(line) = self.record.as_ref().unwrap().as_ref() else {
            return Err(MarkdownTableRequestError::InvalidRequest);
        };
        let requested = self.active_coverage.as_ref().unwrap().clone();
        let byte_coverage = self.active_byte_coverage.as_ref().unwrap().clone();
        if let Some(job) = ordinary.as_mut() {
            if job.retains_plain_block(line.semantic_block.as_ref().unwrap())
                && job.continue_closed_block(
                    requested.clone(),
                    self.selection,
                    byte_coverage.clone(),
                    self.selection_bytes
                        .clone()
                        .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                    &mut self.underline,
                    self.quota.clone(),
                )
            {
                // 原固定 scratch 只换本批端点、输出与 quota；沿原构造的固定元数据上界收费，
                // 真正前缀读取仍由后继 cursor.extend_to/advance 逐量子归账，没有预取新正文。
                self.ordinary_run_index = 0;
                self.phase = Phase::Ordinary;
                return Ok(SemanticWork {
                    structural_transitions: CONTROL,
                    metadata_bytes: bytes,
                    ..Default::default()
                });
            }
            // 同 seed 但范围重叠/回退仍走冷路径；先归还旧完成前缀，再由下一量子构造原 job。
            *ordinary = None;
            return Ok(SemanticWork {
                structural_transitions: CONTROL + PlainSemanticJob::completed_drop_control(),
                ..Default::default()
            });
        }
        let underline = self.underline.take().unwrap();
        let job = if line.semantic_block.as_ref().unwrap().is_scoped_paragraph() {
            PlainSemanticJob::for_closed_origin(
                requested,
                self.selection,
                byte_coverage,
                self.selection_bytes
                    .clone()
                    .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                line,
                underline,
                self.quota.clone(),
            )?
        } else {
            PlainSemanticJob::for_closed_block(
                requested,
                self.selection,
                byte_coverage,
                self.selection_bytes
                    .clone()
                    .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                line.semantic_block.as_ref().unwrap(),
                underline,
                self.quota.clone(),
            )?
        };
        *ordinary = Some(job); // 同一父槽的内联载荷，不申请 Box，也不重复初始化空的4KiB游标页。
        self.ordinary_run_index = 0;
        self.phase = Phase::Ordinary;
        Ok(SemanticWork {
            structural_transitions: CONTROL,
            metadata_bytes: bytes,
            ..SemanticWork::default()
        })
    }

    /// 只接当前已计量交集的真 ATX，借用父语义槽；scope 仍由本原 Leaf owner 负责。
    fn begin_atx_inline(
        &mut self,
        ordinary: &mut Option<PlainSemanticJob>,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let bytes = PlainSemanticJob::closed_block_metadata();
        if bytes > budget {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        let SourceRecord::Leaf(leaf) = self.record.as_ref().unwrap().as_ref() else {
            return Err(MarkdownTableRequestError::InvalidRequest);
        };
        *ordinary = Some(PlainSemanticJob::for_closed_atx(
            self.active_coverage.as_ref().unwrap().clone(),
            self.selection,
            self.active_byte_coverage.as_ref().unwrap().clone(),
            self.selection_bytes
                .clone()
                .expect("Leaf Bounds已完成同一完整选择的字节定位"),
            &leaf.block,
            self.underline.take(),
            self.quota.clone(),
        )?);
        self.ordinary_run_index = 0;
        self.phase = Phase::Ordinary;
        Ok(SemanticWork {
            structural_transitions: CONTROL,
            metadata_bytes: bytes,
            ..Default::default()
        })
    }

    /// 只撤本Leaf临时坐标；已成功段及全请求唯一underline工作区继续保留原quota和源身份。
    fn clear_active_leaf(&mut self) {
        self.record = None;
        self.active_byte_coverage = None;
        self.active_coverage = None;
        self.boundary = None;
        self.scoped = None;
        self.ordinary_run = None;
        self.heading_next = None;
        self.heading_tail = None;
        self.projection = None;
        self.raw_markers = [None, None];
        self.markers = None;
    }

    /// 原ATX正文逐帧调用同一Plain规则；只有整个真实正文无候选时才签本窗口交集。
    fn advance_heading(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        let SourceRecord::Leaf(leaf) = self.record.as_ref().unwrap().as_ref() else {
            unreachable!("活动ATX只保原Leaf");
        };
        let LeafBlockDetails::AtxHeading { trim_target, .. } = leaf.details() else {
            return Err(MarkdownTableRequestError::InvalidRequest);
        };
        let next = self.heading_next.unwrap();
        if next == *trim_target {
            self.heading_next = None;
            self.phase = Phase::Coverage;
            return Ok(work); // 空标题无需读取邻块；非空标题也必须此前逐帧真实到达原正文末端。
        }
        let tree = tree_height(document);
        let child = budget - CONTROL - tree;
        let frame = document
            .table_source_frame(next, child.min(trim_target - next))
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let result = leaf.block.plain_heading_content_step(
            SourceFrame {
                source_revision: frame.generation,
                byte_start: frame.byte_start,
                bytes: &frame.bytes,
                real_eof: frame.real_eof,
            },
            GlobalSourceAuthority {
                current_revision: document.generation(),
                cancelled: false,
            },
            child,
        );
        // 映射归还失败优先于候选判定；不能将真实回收错误改成NeedsGeneralInline继续旧功能。
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let (general, measured) = result.map_err(MarkdownTableRequestError::Global)?;
        work.source_bytes = measured.byte_reads;
        work.source_tree_steps = tree;
        work.structural_transitions += measured.transitions + tree;
        if general {
            if PlainSemanticJob::supports_closed_atx(&leaf.block) {
                self.phase = Phase::Coverage; // 小块仍须完整原 Parser；heading_next 保留本次已见候选的阶段事实。
            } else {
                self.pending
                    .get_or_insert(MarkdownRootLeafPendingReason::NeedsGeneralInline);
                self.clear_active_leaf();
                self.phase = Phase::NextLeaf; // 未支持的巨大标题不发布run；独立Leaf仍可完成真实覆盖。
            }
        } else {
            self.heading_next = Some(next + measured.byte_reads);
        }
        Ok(work)
    }
    fn build_one(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        let coverage = self.active_coverage.as_ref().unwrap();
        if self.run_next == coverage.end {
            self.phase = Phase::FinishLeaf;
            return Ok(work);
        }
        let SourceRecord::Leaf(leaf) = self.record.as_ref().unwrap().as_ref() else {
            unreachable!("原Leaf不可变");
        };
        let pairs = if matches!(
            leaf.kind(),
            LeafBlockKind::FencedCode | LeafBlockKind::IndentedCode
        ) {
            &[][..] // 已证代码没有HTML语义，不能借外部pair隐藏原<u>字符或给代码画下划线。
        } else {
            self.underline
                .as_ref()
                .unwrap()
                .ready_pairs_for(document)
                .map_err(MarkdownTableRequestError::Window)?
                .ok_or(MarkdownTableRequestError::InvalidRequest)?
        };
        if pairs
            .get(self.underline_index)
            .is_some_and(|pair| pair.close.end <= self.run_next)
        {
            self.underline_index += 1;
            return Ok(work); // 每次只跳过一个已过去的真实pair，不隐藏一次无预算大循环。
        }
        let mut styles = match leaf.kind() {
            // 原Leaf的真实level只映射既有六个样式位，不从标题文字或#数量重新识别。
            LeafBlockKind::AtxHeading(level) => match level {
                pulldown_cmark::HeadingLevel::H1 => MarkdownPresentationStyles::HEADING_1,
                pulldown_cmark::HeadingLevel::H2 => MarkdownPresentationStyles::HEADING_2,
                pulldown_cmark::HeadingLevel::H3 => MarkdownPresentationStyles::HEADING_3,
                pulldown_cmark::HeadingLevel::H4 => MarkdownPresentationStyles::HEADING_4,
                pulldown_cmark::HeadingLevel::H5 => MarkdownPresentationStyles::HEADING_5,
                pulldown_cmark::HeadingLevel::H6 => MarkdownPresentationStyles::HEADING_6,
            },
            LeafBlockKind::ThematicBreak => MarkdownPresentationStyles::HORIZONTAL_RULE,
            LeafBlockKind::FencedCode | LeafBlockKind::IndentedCode => {
                MarkdownPresentationStyles::FENCED_CODE
            }
            // 原源中的 ** / backtick / link spelling 在真实 HTML/metadata 内仍是字面。
            // 这是实际 Leaf kind 的既定显示语义，不是为未知或失败候选提供默认 fallback。
            LeafBlockKind::Html | LeafBlockKind::Metadata(_) => {
                MarkdownPresentationStyles::default()
            }
        };
        if (self.scoped.is_some()
            || matches!(
                leaf.kind(),
                LeafBlockKind::FencedCode | LeafBlockKind::IndentedCode
            ))
            && self.run_next < self.markers.as_ref().unwrap()[0].start
        {
            styles = MarkdownPresentationStyles::default();
        } // 原Parser代码source从首行缩进之后开始；前缀仍画canonical空白，不能隐藏或强加代码语义。
        let mut end = coverage.end;
        if let Some(scoped) = self.scoped.as_ref() {
            let query = scoped.query(self.run_next, end);
            end = query.utf16_range.end;
            styles = styles.union(query.styles);
        }
        for (index, marker) in self.markers.as_ref().unwrap().iter().enumerate() {
            for boundary in [marker.start, marker.end] {
                if boundary > self.run_next {
                    end = end.min(boundary);
                }
            }
            if marker.contains(&self.run_next) {
                styles = styles.union(MarkdownPresentationStyles::MARKER);
                let active = if leaf.kind() == LeafBlockKind::FencedCode {
                    let boundary = self.boundary.as_ref().unwrap();
                    // 活动归属使用原完整物理行，不能把视口裁剪后的marker端点当成可编辑边界。
                    // closing scanner明确停在EOL之前；opening是否包含EOL已由同Boundary实际读过。
                    let raw = self.raw_markers[index].as_ref().unwrap();
                    let owner = if index == 0 {
                        leaf.physical_start()..raw.end
                    } else {
                        raw.clone()
                    }; // opening行首缩进同属原端点行，但这些canonical空白不因此变成隐藏marker。
                    markdown_physical_line_is_active(
                        self.selection_bytes
                            .as_ref()
                            .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                        &owner,
                        index != 0 || boundary.opening_includes_end,
                    )
                } else {
                    markdown_selection_intersects(
                        self.selection_bytes
                            .as_ref()
                            .expect("Leaf Bounds已完成同一完整选择的字节定位"),
                        &leaf.source_range(),
                    )
                };
                if !active {
                    styles = styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
                }
            }
        }
        if let Some(pair) = pairs.get(self.underline_index) {
            for boundary in [
                pair.open.start,
                pair.open.end,
                pair.content.start,
                pair.content.end,
                pair.close.start,
                pair.close.end,
            ] {
                if boundary > self.run_next {
                    end = end.min(boundary);
                }
            }
            if pair.content.contains(&self.run_next) {
                styles = styles.union(MarkdownPresentationStyles::UNDERLINE);
            }
            if pair.open.contains(&self.run_next) || pair.close.contains(&self.run_next) {
                styles = styles.union(MarkdownPresentationStyles::MARKER);
                if !markdown_selection_intersects(
                    &ordered_selection(self.selection),
                    &(pair.open.start..pair.close.end),
                ) {
                    styles = styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
                }
            }
        }
        if end <= self.run_next {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let measured = self.runs.append(
            MarkdownPresentationRun {
                utf16_range: self.run_next..end,
                styles,
            },
            &self.quota,
            budget - CONTROL,
        )?;
        work.structural_transitions += measured.structural_transitions;
        work.metadata_bytes = measured.metadata_bytes;
        self.run_next = end;
        Ok(work)
    }

    /// 父 owner 在任何结果移交前取得确切清理与发布费用；这里只读有限元数据。
    pub(super) fn publication_work(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if !self.done() {
            // 未走完真实阶段时没有可计费的发布终点，更不能由父来源完成状态替代。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if let Some(underline) = &self.underline {
            // 保原来源检查；pair/scanner不在此析构，它们由父请求接回后串行移交。
            let _ = underline
                .ready_pairs_for(document)
                .map_err(MarkdownTableRequestError::Window)?;
        }
        Ok(SemanticWork {
            structural_transitions: CONTROL
                + 2 * PAGE_COUNT
                + 2 * LEAF_PAGE_COUNT
                + self.candidates.len
                // 空覆盖没有可续Ready；父发布Pending后实际归还固定owner，先在此预付其Drop。
                + usize::from(self.coverage.len == 0) * Self::completed_owner_drop_control(),
            metadata_bytes: if self.coverage.len != 0 {
                std::mem::size_of::<VerifiedMarkdownRootLeafViewport>()
            } else {
                0
            },
            ..SemanticWork::default()
        })
    }

    /// 父发布先取得实际SourcePublication；本方法核同源/fullselection并移走原有限载荷。
    pub(super) fn publish(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        publication: Weak<SourcePublication>,
        budget: usize,
    ) -> Result<(RootLeafSemanticPublication, SemanticWork), MarkdownTableRequestError> {
        if let Err(error) = self.check(document, authority) {
            self.fail(error); // 真实source/取消/fullselection失权是首因，恢复旧参数不能复活publish。
            return Err(error);
        }
        let original = publication
            .upgrade()
            .ok_or(MarkdownTableRequestError::PublicationSuperseded)?;
        if !original.source.is_current(document)
            || original.retired.get()
            || original.context_revision != self.context_revision
            || original.selection != self.selection
            || !self.done()
        {
            // 原发布及原完整选择共同授予移交权限；同坐标的其他上下文不能补签结果。
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let work = self.publication_work(document)?;
        if work.structural_transitions > budget || work.metadata_bytes > budget {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        // Ready不携带context或pair Vec；父请求在本次发布后接回同一个scanner，不建立第二图。
        self.boundary = None;
        self.scoped = None;
        self.ordinary_run = None;
        self.projection = None;
        self.candidates = LeafPages::new(); // 原候选目录的Rc与小页清理已在本发布步收费，不留第二份来源库存。
        let result = if self.coverage.len == 0 {
            RootLeafSemanticPublication::Pending(
                self.pending
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
            ) // 任何空结果都保持原明确原因，不用空run目录伪作Ready。
        } else {
            if self.runs.len == 0 || self.record.is_some() || self.active_coverage.is_some() {
                return Err(MarkdownTableRequestError::InvalidRequest);
            } // 只有所有已证段都完成且没有半份活动Leaf时，才移交唯一部分覆盖。
            RootLeafSemanticPublication::Ready(Box::new(VerifiedMarkdownRootLeafViewport {
                source: self.source.clone(),
                context_revision: self.context_revision,
                selection: self.selection,
                publication,
                requested: self.requested.clone(),
                coverage: std::mem::replace(&mut self.coverage, LeafPages::new()),
                runs: std::mem::replace(&mut self.runs, RunPages::new()),
                links: std::mem::take(&mut self.links),
            }))
        };
        self.phase = Phase::Published;
        Ok((result, work))
    }
}

/// 只负责原Leaf的有限反向读取；显示判断与旧Parser消费同一个HeadingMarkerTail。
struct AtxMarkerTail {
    source: Range<usize>,
    tail: Option<HeadingMarkerTail>,
}
impl AtxMarkerTail {
    fn new(source: Range<usize>) -> Self {
        Self { source, tail: None }
    }

    fn done(&self) -> bool {
        self.tail.as_ref().is_some_and(HeadingMarkerTail::done)
    }

    fn marker(&self) -> Option<Range<usize>> {
        self.tail.as_ref().and_then(HeadingMarkerTail::marker)
    }

    /// 一次只借一段真实尾片，Source读取与树定位均计入原父请求；不会复制超长空白或#串。
    fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL,
            ..SemanticWork::default()
        };
        let Some(tail) = self.tail.as_mut() else {
            let (byte, measured) = peek(document, self.source.end - 1)?;
            let end = self.source.end - usize::from(byte == b'\n');
            // 旧first_line只去掉首个raw-LF；原ATX Leaf已保证其真实单行边界，CR仍交共享规则。
            self.tail = Some(HeadingMarkerTail::new(self.source.start, end));
            return Ok(measured);
        };
        if tail.done() {
            return Ok(work); // 完成后只移交固定范围，不再次借源或重启末尾扫描。
        }
        let tree = tree_height(document);
        let wanted = child_budget(budget - CONTROL, tree)? / 4;
        let (_start, bytes) = backward_frame(document, tail.next_end(), self.source.start, wanted)?;
        work.source_tree_steps = tree;
        work.structural_transitions += tree;
        for &byte in bytes.iter().rev() {
            tail.observe_reverse(byte);
            work.source_bytes += 1;
            work.structural_transitions += 2;
            if tail.done() {
                break; // 仅共享规则决定显示终点；frame结束时保留同一个真实next，不能冒充完成。
            }
        }
        bytes
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        Ok(work)
    }
}

/// 原 producer 已证明 opening、body 与真实 closer；这里只读取 opening 的实际 EOL 归属。
/// 不重扫最后正文、不把 EOF 字面 fence 当作 closing，也不保留旧显示 helper 的错误分支。
struct FenceBoundaryProof {
    opening: Range<usize>,
    opening_includes_end: bool,
    closing: Option<Range<usize>>,
    done: bool,
}

impl FenceBoundaryProof {
    fn new(leaf: &VerifiedMarkdownOrdinaryLeaf) -> Result<Self, MarkdownTableRequestError> {
        let LeafBlockDetails::FencedCode {
            opening_line,
            body_enclosure,
            closing_marker,
            closing_scanner_end,
            ..
        } = leaf.details()
        else {
            return Err(MarkdownTableRequestError::InvalidRequest);
        };
        let source = leaf.source_range();
        if opening_line.start != source.start
            || opening_line.start >= opening_line.end
            || opening_line.end > source.end
            || body_enclosure.start != opening_line.end
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let closing = match (closing_marker, closing_scanner_end) {
            (Some(marker), Some(end))
                if body_enclosure.end <= marker.start
                    && marker.start < marker.end
                    && marker.end <= *end
                    && *end == source.end
                    && leaf.close_reason() == LeafBlockCloseReason::ExplicitMarker =>
            {
                Some(body_enclosure.end..*end)
            }
            (None, None)
                if matches!(
                    leaf.close_reason(),
                    LeafBlockCloseReason::TrueEof | LeafBlockCloseReason::ContainerExit
                ) =>
            {
                None
            }
            _ => return Err(MarkdownTableRequestError::InvalidRequest),
        };
        Ok(Self {
            opening: opening_line.clone(),
            opening_includes_end: false,
            closing,
            done: false,
        })
    }

    fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let (byte, work) = peek(document, self.opening.end - 1)?;
        self.opening_includes_end = !matches!(byte, b'\r' | b'\n');
        self.done = true;
        Ok(work)
    }
}

fn peek(
    document: &MarkdownDocumentBuffer,
    at: usize,
) -> Result<(u8, SemanticWork), MarkdownTableRequestError> {
    let tree = tree_height(document);
    let frame = document
        .table_source_frame(at, 1)
        .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
    let byte = *frame
        .bytes
        .first()
        .ok_or(MarkdownTableRequestError::InvalidRequest)?;
    frame
        .finish()
        .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
    Ok((
        byte,
        SemanticWork {
            source_bytes: 1,
            source_tree_steps: tree,
            structural_transitions: CONTROL + tree + 2,
            ..SemanticWork::default()
        },
    ))
}

/// 从同一piece root借一段真实反向末片；不复制、不读source.start前，也不把任意frame尾当EOF。
fn backward_frame(
    document: &MarkdownDocumentBuffer,
    end: usize,
    floor: usize,
    maximum: usize,
) -> Result<(usize, PieceBytes<'_>), MarkdownTableRequestError> {
    if floor >= end || end > document.byte_len() || maximum == 0 || maximum > 64 * 1_024 {
        return Err(MarkdownTableRequestError::InvalidRequest);
    }
    let mut local = end - 1;
    let mut node = document
        .root
        .as_deref()
        .ok_or(MarkdownTableRequestError::InvalidRequest)?;
    loop {
        match node {
            MarkdownPieceTreeNode::Branch { left, right, .. } => {
                let length = left.metrics().byte_len;
                if local < length {
                    node = left;
                } else {
                    local -= length;
                    node = right;
                }
            }
            MarkdownPieceTreeNode::OriginalSpan(span) => {
                // 反向读取也只借命中小片，不能把前置共享目录误当作一段64KiB正文。
                let hit = span
                    .locate_byte(local, false)
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                let local_end = hit.local + 1;
                let count = maximum.min(local_end).min(end - floor);
                let bytes = document
                    .piece_range_bytes(&hit.piece, local_end - count..local_end)
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return Ok((end - count, bytes));
            }
            MarkdownPieceTreeNode::Leaf(piece) => {
                let local_end = local + 1;
                let count = maximum.min(local_end).min(end - floor);
                let bytes = document
                    .piece_range_bytes(piece, local_end - count..local_end)
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return Ok((end - count, bytes));
            }
        }
    }
}

#[cfg(test)]
mod tests;
