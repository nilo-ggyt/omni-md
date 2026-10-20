//! 原来源 owner 内的 Root Plain/Setext 有限语义；复杂 inline/非 Root/空洞明确保持 Pending。

use super::*;
use crate::MarkdownSourceLink;
use crate::markdown_document_buffer::table_source::MarkdownTableUnderlineRequest;
use crate::markdown_viewport_presentation::parser::markdown_selection_intersects;
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota;
use crate::markdown_viewport_presentation::{
    MarkdownPresentationQuery, MarkdownPresentationRun, MarkdownPresentationStyles,
};
use pulldown_cmark::global_frontier::{
    InlinePlainCursor, InlinePlainLimits, InlinePlainStep, VerifiedInlinePlainProof,
    VerifiedInlineSemanticSeed,
};
mod bounded_inline;
use bounded_inline::BoundedInline;
mod code;
use code::CodeJob;
pub(super) use code::{PrefixDecoration, SpanProof};

const PLAIN_CONTROL: usize = 128;
const MAX_PLAIN_RUNS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownOrdinaryPendingReason {
    MissingClosedBlock,
    IncompleteCoverage,
    NeedsGeneralInline,
    NonRootScope,
}

/// 这里只保原 paired seed；各物理行与当前语义任务通过同一个 Rc 关联，不按行复制 scope。
pub(super) struct OrdinaryBlockRecord {
    pub(super) seed: VerifiedInlineSemanticSeed,
}

impl OrdinaryBlockRecord {
    /// 容器段落须持原 Origin 才可进入 inline；Root 与 Setext 保留各自完整块合同。
    pub(super) fn is_scoped_paragraph(&self) -> bool {
        self.seed.scope_depth() > 1 && self.seed.kind() == InlineBlockKind::Paragraph
    }
}

/// 来源发布与有限语义结果共享原 publication；SourcesOnly/Pending 均不是默认样式的完成证明。
pub enum MarkdownOrdinarySemanticState {
    SourcesOnly,
    Pending(MarkdownOrdinaryPendingReason),
    PlainReady(Box<VerifiedMarkdownPlainViewport>),
    LeafPending(MarkdownRootLeafPendingReason),
    /// 只证明结果列出的真实闭合块交集；原请求窗口中的空洞仍未知，不是默认样式。
    RootLeafReady(Box<VerifiedMarkdownRootLeafViewport>),
}

impl MarkdownOrdinarySemanticState {
    /// 旧结果仍与后继同活；两个真正 Ready 的全部 run 都必须进入原联合库存。
    pub(super) fn run_count(&self) -> usize {
        match self {
            Self::PlainReady(ready) => ready.run_count(),
            Self::RootLeafReady(ready) => ready.run_count(),
            Self::SourcesOnly | Self::Pending(_) | Self::LeafPending(_) => 0,
        }
    }

    /// 外带结果只登记实际保留的 Box 与页；Pending 没有借预留量虚构出语义载荷。
    pub(super) fn retained_metadata(&self) -> usize {
        match self {
            Self::PlainReady(ready) => {
                plain_publication_metadata()
                    + ready.rich_runs.capacity()
                        * std::mem::size_of::<Option<MarkdownPresentationRun>>()
                    + ready.links.capacity() * std::mem::size_of::<MarkdownSourceLink>()
                    + ready
                        .links
                        .iter()
                        .map(|link| link.destination.capacity())
                        .sum::<usize>()
            }
            Self::RootLeafReady(ready) => ready.retained_metadata(),
            Self::SourcesOnly | Self::Pending(_) | Self::LeafPending(_) => 0,
        }
    }
}

/// 每个 query 先核原 source、完整选择及原 publication；只读结果不为请求续批授权。
pub struct VerifiedMarkdownPlainViewport {
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    publication: Weak<SourcePublication>,
    requested: Range<usize>,
    runs: [Option<MarkdownPresentationRun>; MAX_PLAIN_RUNS],
    len: usize,
    block: Rc<OrdinaryBlockRecord>,
    rich_runs: Vec<Option<MarkdownPresentationRun>>,
    links: Vec<MarkdownSourceLink>,
}

/// Box 载荷在原来源发布预留时统一准入；枚举自身的句柄已包含在父发布结构大小中。
pub(super) fn plain_publication_metadata() -> usize {
    std::mem::size_of::<VerifiedMarkdownPlainViewport>()
}

impl VerifiedMarkdownPlainViewport {
    pub(super) fn run_count(&self) -> usize {
        self.len
    }
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
        // 原来源结果已释放或退休时，孤立语义 metadata 不能借相同 g 恢复发布身份。
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
        Ok(&self.requested)
    }

    pub fn block_source_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Range<usize>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(self.block.seed.source_range())
    }

    /// Some(default) 与覆盖外 None 有明确区别；这里不会为 gap 或 Pending 合成空 styles。
    pub fn query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<MarkdownPresentationQuery>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        if !self.requested.contains(&index) {
            return Ok(None);
        }
        // 固定最多16段，不遍历全文、所有块或 source 页；真实 default run 也显式返回。
        Ok(self.runs[..self.len.min(MAX_PLAIN_RUNS)]
            .iter()
            .chain(self.rich_runs.iter())
            .flatten()
            .find(|run| run.utf16_range.contains(&index))
            .map(|run| MarkdownPresentationQuery {
                utf16_range: run.utf16_range.clone(),
                styles: run.styles,
            }))
    }

    pub fn runs_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<impl Iterator<Item = &MarkdownPresentationRun>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(self.runs[..self.len.min(MAX_PLAIN_RUNS)]
            .iter()
            .chain(self.rich_runs.iter())
            .flatten())
    }

    /// 链接命中保留原 Source/完整选择/publication 核验；样式位本身不授予 URL 权限。
    pub fn link_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        index: usize,
    ) -> Result<Option<&MarkdownSourceLink>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        if !self.requested.contains(&index) {
            return Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::ContextPending,
            ));
        }
        Ok(self
            .links
            .iter()
            .find(|link| link.label_utf16_range.contains(&index)))
    }
}

#[derive(Clone, Copy)]
enum SemanticPhase {
    Bounds,
    AwaitSource,
    Proof,
    AtxBounded,
    ScopedBounded,
    BoundedInline,
    Code,
    Coordinates,
    Underline,
    Build,
    Done,
}

/// 状态属于原请求；前缀 cursor 在同一块 forward 时保留，既不建新 Global，也不重发累计 fuel。
pub(super) struct PlainSemanticJob {
    requested: Range<usize>,
    cell: Option<VerifiedMarkdownTableCellSource>,
    selection: MarkdownTextSelection,
    positions: [PositionLocator; 4],
    position_index: usize,
    byte_coverage: Option<Range<usize>>,
    selection_bytes: Range<usize>,
    block: Option<Rc<OrdinaryBlockRecord>>,
    // 原 Origin 只授予同块 scoped 入口；真实前缀与行内配对由唯一 Code owner 分段证明。
    line: Option<BlockOriginLine>,
    cursor: Option<InlinePlainCursor>,
    proof: Option<VerifiedInlinePlainProof>,
    projection: Option<Coordinates>,
    projected: Option<[usize; 4]>,
    underline: Option<MarkdownTableUnderlineRequest>,
    runs: [Option<MarkdownPresentationRun>; MAX_PLAIN_RUNS],
    len: usize,
    run_next: usize,
    underline_index: usize,
    pending: Option<MarkdownOrdinaryPendingReason>,
    saw_intersection: bool,
    phase: SemanticPhase,
    bounded: Option<Box<BoundedInline>>,
    code: Option<Box<CodeJob>>,
    quota: MarkdownTableWindowQuota,
}

impl PlainSemanticJob {
    /// 一个真实 cell seed 只复制固定标量及原共享 scope；绝不复制格正文或把 raw 范围当成默认样式。
    pub(super) fn for_cell(
        cell: &VerifiedMarkdownTableCellSource,
        selection: MarkdownTextSelection,
        selection_bytes: Option<Range<usize>>,
        underline: Option<MarkdownTableUnderlineRequest>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<Self, MarkdownTableRequestError> {
        quota
            .admit_metadata(
                // Self 内嵌于唯一 CellInlineJob，已经随原固定槽准入；这里只保原 block 的独立 Rc。
                std::mem::size_of::<OrdinaryBlockRecord>() + 2 * std::mem::size_of::<usize>(),
            )
            .map_err(MarkdownTableRequestError::Window)?;
        let mut job = Self::new(cell.range.clone(), selection, quota);
        job.block = Some(Rc::new(OrdinaryBlockRecord {
            seed: cell
                .inline_seed
                .as_deref()
                .expect("非空真实格持原 Scanner 签发的 seed")
                .clone(),
        }));
        job.cell = Some(cell.clone());
        job.underline = underline;
        job.saw_intersection = true;
        if let Some(selection_bytes) = selection_bytes {
            // 原 Scanner 已同时签发本格 UTF16 范围和真实 trimmed byte seed；父请求又持有
            // 同完整选择的已定位端点，因此直接接续语义阶段，不为每一格重读四条全源前缀。
            job.byte_coverage = Some(
                job.block
                    .as_ref()
                    .expect("非空真实格已持原 seed")
                    .seed
                    .physical_range(),
            );
            job.selection_bytes = selection_bytes;
            job.phase = SemanticPhase::AwaitSource;
        }
        Ok(job)
    }
    pub(super) fn new(
        requested: Range<usize>,
        selection: MarkdownTextSelection,
        quota: MarkdownTableWindowQuota,
    ) -> Self {
        let ordered = ordered_selection(selection);
        Self {
            positions: [
                PositionLocator::new(requested.start, false),
                PositionLocator::new(requested.end, true),
                PositionLocator::new(ordered.start, false),
                PositionLocator::new(ordered.end, true),
            ],
            run_next: requested.start,
            requested,
            cell: None,
            selection,
            position_index: 0,
            byte_coverage: None,
            selection_bytes: 0..0,
            block: None,
            line: None,
            cursor: None,
            proof: None,
            projection: None,
            projected: None,
            underline: None,
            runs: std::array::from_fn(|_| None),
            len: 0,
            underline_index: 0,
            pending: None,
            saw_intersection: false,
            phase: SemanticPhase::Bounds,
            bounded: None,
            code: None,
            quota,
        }
    }

    /// 同一 RootLeaf 已在真实源上计量四端点和闭合块交集；这里只接原 seed 与唯一 underline。
    /// 不接受外部语法判断，也不重新定位远处选择或建立第二个全源配对请求。
    pub(super) fn for_closed_block(
        requested: Range<usize>,
        selection: MarkdownTextSelection,
        byte_coverage: Range<usize>,
        selection_bytes: Range<usize>,
        block: &Rc<OrdinaryBlockRecord>,
        underline: MarkdownTableUnderlineRequest,
        quota: MarkdownTableWindowQuota,
    ) -> Result<Self, MarkdownTableRequestError> {
        let physical = block.seed.physical_range();
        if requested.is_empty()
            || byte_coverage.is_empty()
            || physical.start > byte_coverage.start
            || byte_coverage.end > physical.end
        {
            // 原闭合 Fact 必须完整包含实际交集，不能借邻行或请求包络补签 Plain。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut job = Self::new(requested, selection, quota);
        job.byte_coverage = Some(byte_coverage);
        job.selection_bytes = selection_bytes;
        job.block = Some(Rc::clone(block));
        job.saw_intersection = true;
        job.underline = Some(underline);
        job.phase = SemanticPhase::AwaitSource;
        Ok(job)
    }

    /// 原 SourceRequest 已经用同一 Global/PrefixCursor 证明 physical、prefix、content 和 EOL。
    /// 这里只移交固定 Origin 身份；同一块只创建一个 Code owner，不按物理行重扫跨行强调。
    pub(super) fn for_closed_origin(
        requested: Range<usize>,
        selection: MarkdownTextSelection,
        byte_coverage: Range<usize>,
        selection_bytes: Range<usize>,
        line: &VerifiedMarkdownOrdinaryLine,
        underline: MarkdownTableUnderlineRequest,
        quota: MarkdownTableWindowQuota,
    ) -> Result<Self, MarkdownTableRequestError> {
        let mut job = Self::for_closed_block(
            requested,
            selection,
            byte_coverage,
            selection_bytes,
            line.semantic_block
                .as_ref()
                .expect("语义行必须持原配对 seed"),
            underline,
            quota,
        )?;
        job.line = Some(line.origin.clone());
        Ok(job)
    }

    /// 真闭合 ATX 只借原 Leaf 工厂取得有限解析权限；容器的原 capture/prefix 由父续体先完成。
    pub(super) fn supports_closed_atx(leaf: &VerifiedGlobalLeafBlock) -> bool {
        BoundedInline::supports_atx(leaf)
    }

    /// ATX 复用父请求唯一语义槽；本步只填固定字段，正文/Parser 库存在下一独立量子取得。
    pub(super) fn for_closed_atx(
        requested: Range<usize>,
        selection: MarkdownTextSelection,
        byte_coverage: Range<usize>,
        selection_bytes: Range<usize>,
        leaf: &VerifiedGlobalLeafBlock,
        underline: Option<MarkdownTableUnderlineRequest>,
        quota: MarkdownTableWindowQuota,
    ) -> Result<Self, MarkdownTableRequestError> {
        if !Self::supports_closed_atx(leaf)
            || requested.is_empty()
            || byte_coverage.is_empty()
            || leaf.physical_start() > byte_coverage.start
            || byte_coverage.end > leaf.source_range().end
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut job = Self::new(requested, selection, quota);
        job.byte_coverage = Some(byte_coverage);
        job.selection_bytes = selection_bytes;
        job.underline = underline;
        job.phase = SemanticPhase::AtxBounded;
        Ok(job)
    }

    /// 原 Leaf owner 每步仍提供同一真实票；初次只分配小块，后继沿既有 Parser/配对/Build 链推进。
    pub(super) fn advance_atx_after_sources(
        &mut self,
        document: &MarkdownDocumentBuffer,
        leaf: &VerifiedGlobalLeafBlock,
        budget: usize,
        limits: (usize, usize),
        global: Option<&mut GlobalFrontier>,
    ) -> Result<(SemanticWork, usize), MarkdownTableRequestError> {
        if matches!(self.phase, SemanticPhase::AtxBounded) {
            self.bounded = Some(BoundedInline::for_atx(leaf, &self.quota)?);
            self.phase = SemanticPhase::BoundedInline;
            return Ok((
                SemanticWork {
                    metadata_bytes: BoundedInline::atx_metadata(),
                    structural_transitions: PLAIN_CONTROL,
                    ..Default::default()
                },
                0,
            ));
        }
        self.advance_after_sources(document, budget, limits, global)
    }

    /// 父 SourceRequest 已预留整个固定槽；新块只填写活跃字段，不另分配内嵌的空 cursor 工作区。
    pub(super) fn closed_block_metadata() -> usize {
        std::mem::size_of::<Self>() - std::mem::size_of::<Option<InlinePlainCursor>>()
            + std::mem::size_of::<usize>()
    }

    /// 固定槽结束时计入所有有限指针/运行槽的清理；不把真正的 cursor Drop 藏进下一量子。
    pub(super) fn completed_drop_control() -> usize {
        std::mem::size_of::<Self>().div_ceil(std::mem::size_of::<usize>()) + PLAIN_CONTROL
    }

    /// 负证明仍是实际语法结果；父 RootLeaf 只能保留空洞，不能将其转换成默认样式。
    pub(super) fn pending_reason(&self) -> Option<MarkdownOrdinaryPendingReason> {
        self.pending
    }

    /// 只移交真正完成的固定 run 槽；父请求逐条联合准入，未完成或负证明没有输出权限。
    pub(super) fn take_proven_run(
        &mut self,
        index: usize,
    ) -> Result<Option<MarkdownPresentationRun>, MarkdownTableRequestError> {
        if !self.done()
            || self.pending.is_some()
            || self.run_next != self.requested.end
            || self.len == 0
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok(if let Some(code) = self.code.as_mut() {
            code.output.get_mut(index).and_then(Option::take)
        } else if let Some(bounded) = self.bounded.as_mut() {
            bounded.output.get_mut(index).and_then(Option::take)
        } else {
            self.runs.get_mut(index).and_then(Option::take)
        })
    }

    /// Native 链接目标按同一发布路径移交；只移动已验证 String 的唯一归属，不复制目标字节。
    pub(super) fn take_proven_link(
        &mut self,
    ) -> Result<Option<MarkdownSourceLink>, MarkdownTableRequestError> {
        if !self.done() || self.pending.is_some() {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok(self
            .bounded
            .as_mut()
            .and_then(|bounded| bounded.links.pop()))
    }

    /// 原全源配对请求在一个活动块完成后归还 RootLeaf，下一块复用同一已闭合上下文。
    pub(super) fn return_underline(&mut self) -> Option<MarkdownTableUnderlineRequest> {
        self.underline.take()
    }

    /// Leaf 已逐项移走输出后，只保原普通块的已证前缀；复杂 inline、容器和未完任务不能驻留。
    /// 这仍是父请求已经准入的唯一 semantic 槽，不持 publication、正文帧或历史 run 集合。
    pub(super) fn retains_plain_prefix(&self) -> bool {
        self.done()
            && self.pending.is_none()
            && self.cell.is_none()
            && self.line.is_none()
            && self.cursor.is_some()
            && self.proof.is_some()
            && self.projection.is_some()
            && self.projected.is_some()
            && self.bounded.is_none()
            && self.code.is_none()
            && self.underline.is_none()
            && self.runs.iter().all(Option::is_none)
    }

    /// 当前来源记录必须仍持同一真实 seed；相同文本或数值范围不授予另一个块继续扫描的权限。
    pub(super) fn retains_plain_block(&self, block: &Rc<OrdinaryBlockRecord>) -> bool {
        self.retains_plain_prefix()
            && self
                .block
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, block))
    }

    /// 父 Leaf 已计量新窗口与完整选择端点，且接回唯一 underline 后，才续同一普通块。
    /// 同选择只接受不重叠前进；新选择可重投影同 seed 已证前缀，实际坐标另行认证。
    /// 异块与复杂路径仍保持原冷准备，未知后缀不能继承旧 proof 的默认样式。
    pub(super) fn continue_closed_block(
        &mut self,
        requested: Range<usize>,
        selection: MarkdownTextSelection,
        byte_coverage: Range<usize>,
        selection_bytes: Range<usize>,
        underline: &mut Option<MarkdownTableUnderlineRequest>,
        quota: MarkdownTableWindowQuota,
    ) -> bool {
        let new_selection = self.selection != selection;
        if !self.retains_plain_prefix() || requested.is_empty() || byte_coverage.is_empty() {
            return false;
        }
        if !new_selection
            && (self.selection_bytes != selection_bytes
                || requested.start < self.requested.end
                || self
                    .byte_coverage
                    .as_ref()
                    .is_none_or(|previous| byte_coverage.start < previous.end))
        {
            return false;
        }
        // 唯一 Leaf 调用点已紧邻核对其真实 SourceRecord 的 Rc 身份；这里继续沿本槽
        // 原 seed 核 byte 包络，两个纯 Core 检查之间没有回调、读源或挂起。
        let physical = self.block.as_ref().unwrap().seed.physical_range();
        if physical.start > byte_coverage.start || byte_coverage.end > physical.end {
            return false;
        }
        // 新选择只能来自父 SourceRequest 已接受的真实重投影；Leaf 在进入本分支前，
        // 已用同 Source 重新定位四端点并核当前完整选择。旧 run 不参与这次投影。
        // cursor/proof 只证明同一个 opaque seed 的原字节前缀，不携带选择显隐或 publication 权限。
        if new_selection {
            self.selection = selection;
            self.selection_bytes = selection_bytes;
            self.projection = None;
            self.projected = None; // 逆向坐标必须重新从原 Source 认证，不回退旧前向游标。
        }
        let covered = self
            .proof
            .as_ref()
            .is_some_and(|proof| byte_coverage.end <= proof.proven_end());
        // 新 batch 的同一 quota 必须替换旧句柄；不给后继重新发放累计扫描 fuel 或新增库存。
        self.run_next = requested.start;
        self.requested = requested;
        self.byte_coverage = Some(byte_coverage);
        if !covered {
            self.proof = None;
        } // 超出原已扫描前缀仍须由同 cursor.extend_to 真实读源，不能借新选区扩大证明。
        // 同选择的前进继续使用唯一完成坐标游标；新选择已在上方结束旧坐标。
        // projected 只区分真实完成/待准备，不拿上一组数值作为新窗口结果。
        self.underline = underline.take();
        self.runs = std::array::from_fn(|_| None);
        self.len = 0;
        self.underline_index = 0;
        self.saw_intersection = true;
        self.quota = quota;
        self.phase = SemanticPhase::AwaitSource;
        true
    }

    /// 仅 scoped Code 的真实 prefix 需要原来源页；Root/cell 与已验证 owner 不进入此分支。
    pub(super) fn prefix_origin_record_index(&self) -> Option<usize> {
        self.code.as_ref()?.prefix_origin_record_index()
    }

    /// 父请求每量子只借一条现有记录；同 Rc block 和 exact prefix 都成立才交物理行事实。
    pub(super) fn advance_prefix_origin(
        &mut self,
        record: &Rc<SourceRecord>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let code = self
            .code
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let line = match record.as_ref() {
            SourceRecord::Line(line)
                if line.semantic_block.as_ref().is_some_and(|block| {
                    self.block
                        .as_ref()
                        .is_some_and(|current| Rc::ptr_eq(block, current))
                }) =>
            {
                Some(&line.origin)
            }
            _ => None,
        };
        code.advance_prefix_origin(line, &self.selection_bytes)
    }

    pub(super) fn preparing(&self) -> bool {
        matches!(self.phase, SemanticPhase::Bounds)
    }
    pub(super) fn done(&self) -> bool {
        matches!(self.phase, SemanticPhase::Done)
    }

    /// byte 覆盖只在真实定位结束或父 owner 移交已证端点后存在；零选区仍是有效事实。
    pub(super) fn verified_selection_bytes(&self) -> Option<Range<usize>> {
        self.byte_coverage.as_ref()?;
        Some(self.selection_bytes.clone())
    }
    pub(super) fn publication_control(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<usize, MarkdownTableRequestError> {
        // Build 已闭合的临时配对目录在本次发布内释放；其真实有限元素 Drop 不跨到下一 quota。
        let cleanup = match self.underline.as_ref() {
            Some(underline) => {
                PLAIN_CONTROL
                    + underline
                        .ready_pairs_for(document)
                        .map_err(MarkdownTableRequestError::Window)?
                        .ok_or(MarkdownTableRequestError::InvalidRequest)?
                        .len()
            }
            None => 0,
        };
        // 只有真正完成有限语义才创建 Box；Pending 没有这项分配/固定载荷移交费用。
        Ok(cleanup
            + if self.done() && self.pending.is_none() {
                PLAIN_CONTROL
            } else {
                0
            })
    }
    pub(super) fn publication_payload_metadata(&self) -> usize {
        // 预留允许覆盖最坏分支，实际工作与外带库存只登记真正存在的 Box 载荷。
        if self.done() && self.pending.is_none() {
            plain_publication_metadata()
        } else {
            0
        }
    }
    pub(super) fn needs_underline_workspace(&self) -> bool {
        matches!(self.phase, SemanticPhase::Underline) && self.underline.is_none()
    }

    pub(super) fn retarget(&mut self, requested: Range<usize>) -> SemanticWork {
        // 只重定位新请求的四个有限端点；原语义 cursor/负证明与原 seed 留待新 byte coverage 核对。
        let ordered = ordered_selection(self.selection);
        self.positions = [
            PositionLocator::new(requested.start, false),
            PositionLocator::new(requested.end, true),
            PositionLocator::new(ordered.start, false),
            PositionLocator::new(ordered.end, true),
        ];
        self.requested = requested;
        self.position_index = 0;
        self.byte_coverage = None;
        self.proof = None;
        self.projection = None;
        self.projected = None;
        self.underline = None;
        self.bounded = None;
        self.code = None;
        self.runs = std::array::from_fn(|_| None);
        self.len = 0;
        self.run_next = self.requested.start;
        self.underline_index = 0;
        self.saw_intersection = false;
        self.phase = SemanticPhase::Bounds;
        SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            metadata_bytes: std::mem::size_of::<[PositionLocator; 4]>()
                + std::mem::size_of::<[Option<MarkdownPresentationRun>; MAX_PLAIN_RUNS]>(),
            ..SemanticWork::default()
        }
    }

    pub(super) fn considers(&self, physical: &Range<usize>) -> bool {
        self.byte_coverage
            .as_ref()
            .is_some_and(|coverage| physical.start < coverage.end && coverage.start < physical.end)
    }

    pub(super) fn consider(&mut self, block: &Rc<OrdinaryBlockRecord>) {
        let Some(coverage) = &self.byte_coverage else {
            return;
        };
        let physical = block.seed.physical_range();
        self.saw_intersection |= physical.start < coverage.end && coverage.start < physical.end;
        if physical.start <= coverage.start
            && coverage.end <= physical.end
            && self
                .block
                .as_ref()
                .is_none_or(|current| !Rc::ptr_eq(current, block))
        {
            // 只有另一个真实 Fact 能更换块；先丢旧进度，绝不把旧范围当新块的种子。
            self.cursor = None;
            self.bounded = None;
            self.code = None;
            self.pending = None;
            self.block = Some(Rc::clone(block));
        }
    }

    pub(super) fn advance_bounds(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let work = self.positions[self.position_index].advance(document, budget)?;
        if self.positions[self.position_index].result.is_some() {
            self.position_index += 1;
            if self.position_index == self.positions.len() {
                // viewport 以完整 scalar 的 byte 边界求上下文，公开覆盖仍严格剪回实际请求 UTF16。
                let coverage = self.positions[0].result.unwrap()..self.positions[1].result.unwrap();
                self.selection_bytes =
                    self.positions[2].result.unwrap()..self.positions[3].result.unwrap();
                if coverage.is_empty() {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                if self.block.as_ref().is_some_and(|block| {
                    let range = block.seed.physical_range();
                    range.start > coverage.start || coverage.end > range.end
                }) {
                    // 新窗口已经离开旧块；旧 prefix 只可读，不能续作另一块的证明。
                    self.block = None;
                    self.cursor = None;
                    self.pending = None;
                }
                self.byte_coverage = Some(coverage);
                self.phase = SemanticPhase::AwaitSource;
            }
        }
        Ok(work)
    }

    pub(super) fn advance_after_sources(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
        limits: (usize, usize),
        global: Option<&mut GlobalFrontier>,
    ) -> Result<(SemanticWork, usize), MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..SemanticWork::default()
        };
        let mut new_runs = 0;
        match self.phase {
            SemanticPhase::AwaitSource => {
                if self.block.is_none() {
                    // 来源仍可发布，但没有完整覆盖所需区间的真实 InlineBlock 时不签默认样式。
                    self.pending = Some(if self.saw_intersection {
                        MarkdownOrdinaryPendingReason::IncompleteCoverage
                    } else {
                        MarkdownOrdinaryPendingReason::MissingClosedBlock
                    });
                    self.phase = SemanticPhase::Done;
                } else if self.pending.is_some() {
                    // 同一 seed 已有复杂/非 Root 负证明，前移窗口不能抹掉原前缀中的未支持状态。
                    self.phase = SemanticPhase::Done;
                } else {
                    // 来源与覆盖已经收齐，但仍须真实 Root/LUT 证明及全局上下文，不能直接 Ready。
                    self.phase = if self.proof.is_some() {
                        // 原请求保留的负语法证明已经覆盖当前 byte 末端；只重做实际坐标、
                        // underline 与选择显隐，不再借一片远端源帧来重复返回同一 Proven。
                        SemanticPhase::Coordinates
                    } else if self.block.as_ref().is_some_and(|block| {
                        matches!(block.seed.kind(), InlineBlockKind::SetextHeading(_))
                            && BoundedInline::supports_scoped(block)
                    }) {
                        SemanticPhase::ScopedBounded
                    } else if self.line.is_some() {
                        SemanticPhase::Code
                    } else {
                        SemanticPhase::Proof
                    };
                }
            }
            SemanticPhase::ScopedBounded => {
                self.bounded = Some(BoundedInline::for_scoped(
                    self.block.as_ref().unwrap(),
                    limits,
                    &self.quota,
                )?);
                self.phase = SemanticPhase::BoundedInline;
                work.metadata_bytes = BoundedInline::scoped_metadata();
            }
            SemanticPhase::Proof => {
                let end = self.byte_coverage.as_ref().unwrap().end;
                match self.cursor.as_mut() {
                    None => {
                        // 构造只保存原 capture；LUT、scope初始化留给下个有预算的 vendor 步骤。
                        let seed = &self.block.as_ref().unwrap().seed;
                        let limits = InlinePlainLimits {
                            max_byte_work: limits.0,
                            max_transitions: limits.1,
                        };
                        self.cursor = Some(
                            seed.plain_cursor(end, limits)
                                .map_err(MarkdownTableRequestError::Global)?,
                        );
                    }
                    Some(cursor) if cursor.requested_end() < end && self.proof.is_none() => {
                        // 原 prefix 已真实完成时只扩大 required_end；累计上限与原扫描位置均不重设。
                        let measured = cursor
                            .extend_to(end)
                            .map_err(MarkdownTableRequestError::Global)?;
                        work.structural_transitions += measured.transitions;
                    }
                    Some(cursor) => {
                        // 原同一 cursor 只扫描 required_end 前缀；frame/tree实际借用都归本请求总账。
                        let tree = tree_height(document);
                        let child = child_budget(budget - PLAIN_CONTROL, tree)?;
                        let frame = document
                            .table_source_frame(cursor.next_byte(), child)
                            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                        let step = cursor
                            .advance(
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
                            )
                            .map_err(MarkdownTableRequestError::Global);
                        // 回收成功后才消费step，不能让原parser提前终止遮住真实映射错误。
                        frame
                            .finish()
                            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                        let step = step?;
                        let measured = match step {
                            InlinePlainStep::Progress { work }
                            | InlinePlainStep::NeedInput { work, .. } => work, // frame结束只续读，不冒充完整块/EOF。
                            InlinePlainStep::NeedsGeneralInline { work } => {
                                if self
                                    .cursor
                                    .as_ref()
                                    .unwrap()
                                    .bounded_inline_range()
                                    .is_some()
                                {
                                    self.phase = SemanticPhase::BoundedInline;
                                } else {
                                    self.phase = SemanticPhase::Code;
                                }
                                work
                            }
                            InlinePlainStep::NonRootScope { work } => {
                                // 只有完整短块可以进入原 scoped context；长块仍需真实 Origin，不能假装 Root。
                                if self
                                    .block
                                    .as_ref()
                                    .is_some_and(|block| BoundedInline::supports_scoped(block))
                                {
                                    self.phase = SemanticPhase::ScopedBounded;
                                } else {
                                    self.pending =
                                        Some(MarkdownOrdinaryPendingReason::NonRootScope);
                                    self.phase = SemanticPhase::Done;
                                }
                                work
                            }
                            InlinePlainStep::Proven { proof, work } => {
                                // 这里只证明Root普通前缀；UTF16边界和跨省略区<u>仍须各自完成。
                                self.proof = Some(proof);
                                self.phase = SemanticPhase::Coordinates;
                                work
                            }
                        };
                        work.source_bytes = measured.byte_reads;
                        work.source_tree_steps = tree;
                        work.structural_transitions += measured.transitions + tree;
                    }
                }
            }
            SemanticPhase::BoundedInline => {
                if let Some(bounded) = self.bounded.as_mut() {
                    if bounded.references_attached()
                        && self.underline.as_ref().is_some_and(|underline| {
                            underline
                                .ready_pairs_for(document)
                                .is_ok_and(|pairs| pairs.is_none())
                        })
                    {
                        let measured = self.underline.as_mut().unwrap().advance(
                            document,
                            global.ok_or(MarkdownTableRequestError::InvalidRequest)?,
                            budget - PLAIN_CONTROL,
                        )?;
                        work = underline_work(measured);
                        work.structural_transitions += PLAIN_CONTROL;
                    } else {
                        let (measured, complete) = bounded.advance(
                            document,
                            &mut self.underline,
                            &self.quota,
                            self.selection,
                            &self.selection_bytes,
                            budget - PLAIN_CONTROL,
                        )?;
                        work = measured;
                        work.structural_transitions += PLAIN_CONTROL;
                        if complete {
                            if bounded.unsupported_scope() {
                                self.pending = Some(MarkdownOrdinaryPendingReason::NonRootScope);
                                self.phase = SemanticPhase::Done;
                            } else {
                                self.phase = SemanticPhase::Build;
                            }
                        }
                    }
                } else {
                    self.bounded = BoundedInline::new(
                        self.cursor.as_ref().unwrap(),
                        self.block.as_ref().unwrap(),
                        &self.quota,
                    )?;
                    work.metadata_bytes = std::mem::size_of::<BoundedInline>();
                }
            }
            SemanticPhase::Code => {
                if let Some(code) = self.code.as_mut() {
                    let (measured, complete) = code.advance(
                        document,
                        &self.selection_bytes,
                        &self.quota,
                        budget - PLAIN_CONTROL,
                    )?;
                    work = measured;
                    work.structural_transitions += PLAIN_CONTROL;
                    if complete {
                        if code.general {
                            if self
                                .block
                                .as_ref()
                                .is_some_and(|block| BoundedInline::supports_scoped(block))
                            {
                                // 原 streaming Plain/Code/Emphasis 成功时继续零复制；只把真实 general 负证明
                                // 转交完整短块 Parser。旧试次先真正归还，不能保两套活动 inline/配对 owner。
                                code.release_scratch();
                                self.code = None;
                                self.cursor = None;
                                self.phase = SemanticPhase::ScopedBounded;
                            } else {
                                self.pending =
                                    Some(MarkdownOrdinaryPendingReason::NeedsGeneralInline);
                                self.phase = SemanticPhase::Done;
                            }
                        } else {
                            self.phase = SemanticPhase::Underline;
                        }
                    }
                } else {
                    self.code = CodeJob::new(
                        self.block.as_ref().unwrap(),
                        self.line.as_ref(),
                        self.byte_coverage.as_ref().unwrap().clone(),
                        self.requested.clone(),
                        limits,
                        &self.quota,
                    )?;
                    if self.code.is_none() {
                        self.pending = Some(MarkdownOrdinaryPendingReason::NeedsGeneralInline);
                        self.phase = SemanticPhase::Done;
                    }
                    work.metadata_bytes = std::mem::size_of::<CodeJob>();
                }
            }
            SemanticPhase::Coordinates => {
                if self.projected.is_none()
                    && let Some(projection) = self.projection.as_mut()
                {
                    // 当前组尚未完成时直接推进原坐标游标；分支借用同时证明它真实存在。
                    let (measured, values) =
                        projection.advance(document, budget - PLAIN_CONTROL)?;
                    work = measured;
                    work.structural_transitions += PLAIN_CONTROL;
                    if let Some(values) = values {
                        // 所有可见边界已在原源上真实解码，才能把样式分段转为绝对UTF16坐标。
                        self.projected = Some(values);
                        // 原完成游标只持四个边界/计量，不持正文；Leaf 同块续批继续使用这一实例。
                        self.phase = SemanticPhase::Underline;
                    }
                } else {
                    let coverage = self.byte_coverage.as_ref().unwrap();
                    let proof = self.proof.as_ref().unwrap();
                    let owner = proof
                        .source_range()
                        .start
                        .clamp(coverage.start, coverage.end);
                    let marker = proof.marker_range().map_or(coverage.end, |range| {
                        range.start.clamp(coverage.start, coverage.end)
                    });
                    // 只转换实际可见边界；不为判断远处完整owner反复解码整个 giant paragraph。
                    let boundaries = [coverage.start, owner, marker, coverage.end];
                    let measured = if let Some(projection) = self.projection.as_mut() {
                        // 同块前进只从原真实末端接续；间隙仍由后继 advance 实际读取，
                        // 不能每个六单元样式窗口又回到 source 小片起点解码前缀。
                        projection.continue_at(boundaries)?
                    } else {
                        let (projection, measured) =
                            Coordinates::new(document, boundaries, budget - PLAIN_CONTROL)?;
                        self.projection = Some(projection);
                        measured
                    };
                    self.projected = None;
                    work = measured;
                    work.structural_transitions += PLAIN_CONTROL;
                }
            }
            SemanticPhase::Underline => {
                match self.underline.as_mut() {
                    None => {
                        // 父owner在进入本阶段前已联合准入唯一underline工作区；只接实际有限byte覆盖。
                        self.underline = Some(if let Some(cell) = &self.cell {
                            document.table_cell_underline_request(cell, self.quota.clone())?
                        } else {
                            document
                                .table_underline_request(
                                    std::slice::from_ref(self.byte_coverage.as_ref().unwrap()),
                                    self.quota.clone(),
                                )
                                .map_err(MarkdownTableRequestError::Window)?
                        });
                    }
                    Some(underline)
                        if underline
                            .ready_pairs_for(document)
                            .map_err(MarkdownTableRequestError::Window)?
                            .is_some() =>
                    {
                        // 原全局上下文已经闭合并移交配对后才允许生成可查询的有限样式。
                        self.phase = SemanticPhase::Build;
                    }
                    Some(underline) => {
                        // 每源 byte 同时可能做UTF8前缀验证；保守缩分后所有实际读取/树路径都进入原总账。
                        let measured = underline.advance(
                            document,
                            global.ok_or(MarkdownTableRequestError::InvalidRequest)?,
                            budget - PLAIN_CONTROL,
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
                }
            }
            SemanticPhase::Build => {
                if self.run_next == self.requested.end {
                    // 每个可查询位置都有真实样式段后才结束；覆盖外仍是None而非默认样式。
                    self.phase = SemanticPhase::Done;
                    if let Some(bounded) = self.bounded.as_mut() {
                        bounded.release_scratch();
                    }
                    if let Some(code) = self.code.as_mut() {
                        code.release_scratch();
                    }
                } else {
                    let pairs = self
                        .underline
                        .as_ref()
                        .unwrap()
                        .ready_pairs_for(document)
                        .map_err(MarkdownTableRequestError::Window)?
                        .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                    if pairs
                        .get(self.underline_index)
                        .is_some_and(|pair| pair.close.end <= self.run_next)
                    {
                        // 已闭合且完全位于当前位置前的配对不再影响剩余有限覆盖。
                        self.underline_index += 1;
                    } else {
                        let mut end = self.requested.end;
                        let mut styles = MarkdownPresentationStyles::default();
                        if let Some(code) = self.code.as_mut() {
                            let (measured, query) =
                                code.advance_query(self.run_next, budget - PLAIN_CONTROL);
                            work.structural_transitions += measured.structural_transitions;
                            let Some(query) = query else {
                                return Ok((work, 0)); // 样式交集仍未完整扫完时不提交半个run，后继继续同一查询。
                            };
                            end = end.min(query.utf16_range.end);
                            styles = query.styles;
                        } else if let Some(bounded) = self.bounded.as_ref() {
                            let query = bounded.query(self.run_next, self.requested.end);
                            end = end.min(query.utf16_range.end);
                            styles = query.styles;
                        } else {
                            let values = self.projected.unwrap();
                            let proof = self.proof.as_ref().unwrap();
                            let heading = match proof.kind() {
                                InlineBlockKind::Paragraph | InlineBlockKind::TableCell => {
                                    MarkdownPresentationStyles::default()
                                } // 容器样式由其原 scope 投影提供，不从格内容猜测。
                                InlineBlockKind::SetextHeading(
                                    pulldown_cmark::HeadingLevel::H1,
                                ) => MarkdownPresentationStyles::HEADING_1,
                                InlineBlockKind::SetextHeading(_) => {
                                    MarkdownPresentationStyles::HEADING_2
                                }
                            };
                            if self.run_next >= values[1] {
                                // heading只从原Parser owner开始，原物理缩进前缀仍保持默认样式。
                                styles = styles.union(heading);
                            } else {
                                end = end.min(values[1]);
                            }
                            if proof.marker_range().is_some() {
                                // underline标记仅使用原Setext证明的实际区间，显隐按完整owner与完整选择判定。
                                if self.run_next >= values[2] {
                                    styles = styles.union(MarkdownPresentationStyles::MARKER);
                                    if !markdown_selection_intersects(
                                        &self.selection_bytes,
                                        &proof.source_range(),
                                    ) {
                                        styles =
                                            styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
                                    }
                                } else {
                                    end = end.min(values[2]);
                                }
                            }
                        }
                        if let Some(pair) = pairs.get(self.underline_index) {
                            // 全源scanner给出真实配对；这里只剪其边界，不把省略段重解析为局部标签。
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
                            if pair.content.contains(&self.run_next)
                                && !styles.contains(MarkdownPresentationStyles::INLINE_CODE)
                                && !styles.contains(MarkdownPresentationStyles::FENCED_CODE)
                            {
                                // 原完整投影的外部配对跨过代码，但线条不进入已证代码本体或其 delimiter。
                                styles = styles.union(MarkdownPresentationStyles::UNDERLINE);
                            }
                            if pair.open.contains(&self.run_next)
                                || pair.close.contains(&self.run_next)
                            {
                                styles = styles.union(MarkdownPresentationStyles::MARKER);
                                if !markdown_selection_intersects(
                                    &ordered_selection(self.selection),
                                    &(pair.open.start..pair.close.end),
                                ) {
                                    styles =
                                        styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
                                }
                            }
                        }
                        if end <= self.run_next {
                            // 坐标失配不能靠跳过字符恢复；拒绝半份覆盖避免query出现隐式空洞。
                            return Err(MarkdownTableRequestError::InvalidRequest);
                        }
                        if let Some(code) = self.code.as_mut() {
                            if let Some(previous) = code.output.last_mut().and_then(Option::as_mut)
                                && previous.styles == styles
                            {
                                previous.utf16_range.end = end;
                            } else {
                                code.push_output(
                                    MarkdownPresentationRun {
                                        utf16_range: self.run_next..end,
                                        styles,
                                    },
                                    &self.quota,
                                )?;
                                self.len += 1;
                                new_runs = 1;
                            }
                        } else if let Some(bounded) = self.bounded.as_mut() {
                            if let Some(previous) =
                                bounded.output.last_mut().and_then(Option::as_mut)
                                && previous.styles == styles
                            {
                                previous.utf16_range.end = end;
                            } else {
                                bounded.push_output(
                                    MarkdownPresentationRun {
                                        utf16_range: self.run_next..end,
                                        styles,
                                    },
                                    &self.quota,
                                )?;
                                self.len += 1;
                                new_runs = 1;
                            }
                        } else if self.len > 0
                            && self.runs[self.len - 1].as_ref().unwrap().styles == styles
                        {
                            // 相邻等样式段原地合并；不建立boundary Vec、排序或随段落大小增长的输出队列。
                            self.runs[self.len - 1].as_mut().unwrap().utf16_range.end = end;
                        } else {
                            if self.len == MAX_PLAIN_RUNS {
                                // 有限目录满时整项失败；不得保留部分Ready或退回未证明的默认样式。
                                return Err(MarkdownTableRequestError::Window(
                                    MarkdownTableWindowError::SemanticLimit,
                                ));
                            }
                            self.runs[self.len] = Some(MarkdownPresentationRun {
                                utf16_range: self.run_next..end,
                                styles,
                            });
                            self.len += 1;
                            new_runs = 1;
                        }
                        self.run_next = end;
                        work.metadata_bytes = std::mem::size_of::<MarkdownPresentationRun>();
                    }
                }
            }
            SemanticPhase::Bounds => return Err(MarkdownTableRequestError::InvalidRequest), // 端点阶段必须由父owner优先推进。
            SemanticPhase::AtxBounded => return Err(MarkdownTableRequestError::InvalidRequest), // ATX 必须借父 owner 当前真 Leaf 安装，不能从空 block 槽构造权限。
            SemanticPhase::Done => (), // 父owner只执行一次显式发布，不在此重复分配结果。
        }
        Ok((work, new_runs))
    }

    pub(super) fn finish(
        &mut self,
        source: MarkdownTableSourceIdentity,
        context_revision: u64,
        publication: Weak<SourcePublication>,
    ) -> Result<MarkdownOrdinarySemanticState, MarkdownTableRequestError> {
        if !self.done() {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if let Some(reason) = self.pending {
            return Ok(MarkdownOrdinarySemanticState::Pending(reason));
        }
        if self.run_next != self.requested.end || self.len == 0 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        // 原配额仍有效且发布已先付实际清理费；后继只保留原 seed/cursor/fuel，不带临时 pairs Vec。
        self.underline = None;
        self.projection = None;
        self.projected = None;
        let (rich_runs, links) = if let Some(code) = self.code.as_mut() {
            (std::mem::take(&mut code.output), Vec::new())
        } else {
            self.bounded.as_mut().map_or_else(
                || (Vec::new(), Vec::new()),
                |bounded| {
                    (
                        std::mem::take(&mut bounded.output),
                        std::mem::take(&mut bounded.links),
                    )
                },
            )
        };
        // 父发布已先计入 Box 载荷、句柄与显式发布步骤；这里只移交固定数组，不复制正文。
        Ok(MarkdownOrdinarySemanticState::PlainReady(Box::new(
            VerifiedMarkdownPlainViewport {
                source,
                context_revision,
                selection: self.selection,
                publication,
                requested: self.requested.clone(),
                runs: std::mem::replace(&mut self.runs, std::array::from_fn(|_| None)),
                len: self.len,
                block: Rc::clone(self.block.as_ref().unwrap()),
                rich_runs,
                links,
            },
        )))
    }
}

/// 复用唯一 Global 前探的完整费用；引用的有限复制/Parser 不可藏在 underline 的 byte 位移里。
fn underline_work(
    measured: crate::markdown_document_buffer::table_source::MarkdownTableUnderlineRequestStep,
) -> SemanticWork {
    SemanticWork {
        source_bytes: measured.source_bytes + measured.global_source_bytes,
        utf8_validation_bytes: measured.utf8_validation_bytes,
        source_tree_steps: measured.source_tree_steps + measured.global_source_tree_steps,
        structural_transitions: measured.structural_transitions,
        copied_source_bytes: measured.copied_source_bytes,
        metadata_bytes: measured.metadata_bytes,
        normalizer_input_bytes: measured.normalizer_input_bytes,
        parser_input_bytes: measured.parser_input_bytes,
        allocated_source_capacity: measured.allocated_source_capacity,
    }
}

/// 一个UTF16端点只在目标leaf前缀分步解码；选区跨度再大也不读取两端之间的正文。
pub(super) struct PositionLocator {
    target: usize,
    ceil: bool,
    located: bool,
    next: usize,
    utf16: usize,
    scalar_start: usize,
    scalar_units: usize,
    remaining: u8,
    result: Option<usize>,
    leaf: Option<PositionLeaf>,
}

#[derive(Eq, PartialEq)]
/// 原树定位取得的实际小片身份；只保标量，不强持Doc、Reader、树节点或原映射。
struct PositionLeaf {
    node: usize,
    ordinal: Option<u8>,
    bytes: Range<usize>,
    utf16: Range<usize>,
    source: MarkdownPieceSource,
    source_byte_start: usize,
}

impl PositionLocator {
    pub(super) const PREFIX_REUSE_CONTROL: usize = 32;
    pub(super) const PREFIX_REUSE_METADATA: usize = 4 * std::mem::size_of::<usize>();

    pub(super) fn new(target: usize, ceil: bool) -> Self {
        Self {
            target,
            ceil,
            located: false,
            next: 0,
            utf16: 0,
            scalar_start: 0,
            scalar_units: 0,
            remaining: 0,
            result: None,
            leaf: None,
        }
    }

    /// 唯一行查询还在定位时不能换另一个点；只比较同一定位器已经拥有的目标，不授来源权限。
    pub(super) fn targets(&self, position: usize) -> bool {
        self.target == position
    }

    /// 原父请求已核同Source/完整选择；只续同一小片中真正完成的scalar前沿。
    /// floor落在代理对内部时result不等于next，不能把已越过的后半scalar当成新起点。
    pub(super) fn retarget_forward(&mut self, target: usize, ceil: bool) -> SemanticWork {
        let reusable = self.located
            && self.remaining == 0
            && self.result == Some(self.next)
            && target >= self.utf16
            && self
                .leaf
                .as_ref()
                .is_some_and(|leaf| target <= leaf.utf16.end && self.next <= leaf.bytes.end);
        let metadata_bytes = if reusable {
            // 真实叶身份、解码位置和完整scalar均留在原槽；只撤旧答案，不能沿用旧byte结果。
            self.target = target;
            self.ceil = ceil;
            self.result = None;
            std::mem::size_of::<usize>()
                + std::mem::size_of::<bool>()
                + std::mem::size_of::<Option<usize>>()
        } else {
            // 跨片、逆向、未完成或半代理对均走原冷定位；没有另一份定位器或前缀缓存。
            *self = Self::new(target, ceil);
            std::mem::size_of::<Self>()
        };
        SemanticWork {
            structural_transitions: Self::PREFIX_REUSE_CONTROL,
            metadata_bytes,
            ..SemanticWork::default()
        }
    }

    /// 行首查询只需要完整首标量，不冷填Doc的4096-unit NSString缓存或读取后方整窗。
    /// 原定位步骤先独立归账；确定byte起点后最多借四字节，允许真实piece边界把标量分成小片。
    pub(super) fn advance_row_source_viewport(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> (
        SemanticWork,
        Result<Option<Range<usize>>, MarkdownTableRequestError>,
    ) {
        if self.result.is_none() {
            // 同一定位实现写入本次实际费用；发生Source/解码失败时也不能丢掉已经借过的前缀。
            let mut work = SemanticWork::default();
            let result = self
                .advance_position(document, budget, &mut work)
                .map(|()| None);
            return (work, result);
        }
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..SemanticWork::default()
        };
        let result = (|| {
            // 普通viewport端点允许floor/ceil；真实行首必须恰好是scalar边界，代理对中部不能冒充行首。
            if self.utf16 != self.target || self.remaining != 0 {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let start = self.result.expect("本分支已经完成原定位");
            let tree = tree_height(document);
            // 最坏四个真实piece借用与四字节解码全部预留；这不是一轮完整行或一个新Parser的费用。
            let reservation = PLAIN_CONTROL + 4 * (tree + PLAIN_CONTROL + 4);
            if reservation > budget {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            let mut bytes = [0_u8; 4];
            let mut read = 0;
            let mut width = 1;
            while read < width {
                let frame = document
                    .table_source_frame(start + read, width - read)
                    .map_err(MarkdownTableRequestError::Source)?;
                work.source_tree_steps += tree;
                work.structural_transitions += tree + PLAIN_CONTROL;
                let count = frame.bytes.len();
                bytes[read..read + count].copy_from_slice(&frame.bytes);
                read += count;
                work.source_bytes += count;
                work.utf8_validation_bytes += count;
                work.structural_transitions += count * 2;
                frame.finish().map_err(MarkdownTableRequestError::Source)?;
                if count == 0 {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                if read == 1 {
                    width = match bytes[0] {
                        0x00..=0x7f => 1,
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => return Err(MarkdownTableRequestError::InvalidRequest),
                    };
                }
            }
            let scalar = std::str::from_utf8(&bytes[..read])
                .map_err(|_| MarkdownTableRequestError::InvalidRequest)?
                .chars()
                .next()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            Ok(Some(self.target..self.target + scalar.len_utf16()))
        })();
        (work, result) // Source归还或UTF8验证失败也交回真实借字节，不把失败误记为零工作。
    }

    /// 同源 Leaf job 复用原端点续体，不能另建整段 UTF16 或正文副本。
    pub(super) fn result(&self) -> Option<usize> {
        self.result
    }

    /// 只在目标刚完成自己的树/ordinal定位后尝试同片续用，不能从未定位目标猜测源片。
    pub(super) fn needs_location(&self) -> bool {
        !self.located
    }

    /// 前一端点的完整scalar进度只能在同一真实小片内向前交付；目标与floor/ceil始终归自身。
    pub(super) fn reuse_completed_prefix(&mut self, previous: &Self) -> bool {
        let Some(leaf) = self.leaf.as_ref() else {
            return false; // 未定位或真实叶超过4KiB时仍沿原独立解码，不串扫整段。
        };
        if previous.leaf.as_ref() != Some(leaf)
            || !self.located
            || self.result.is_some()
            || self.remaining != 0
            || self.next != leaf.bytes.start
            || self.utf16 != leaf.utf16.start
            || previous.remaining != 0
            || previous.result != Some(previous.next)
            || previous.utf16 > self.target
            || previous.next <= self.next
        {
            // 身份不同、目标已开始读取、半scalar/floor结果或反向目标均不授予续读权限。
            return false;
        }
        self.next = previous.next;
        self.utf16 = previous.utf16;
        self.scalar_start = previous.scalar_start;
        self.scalar_units = previous.scalar_units;
        true // 只移动四个实际坐标标量；不复制result、候选权限、读窗或Source owner。
    }

    /// 父Bounds合并步骤前先为本状态保留实际最小费用；额度不足只留待原下一量子。
    pub(super) fn minimum_budget(&self, document: &MarkdownDocumentBuffer) -> usize {
        if !self.located {
            // AVL定位还会填写固定locator字段，metadata同样从父量子扣除。
            PLAIN_CONTROL + tree_height(document) + std::mem::size_of::<Self>()
        } else if self.remaining == 0 && self.utf16 == self.target {
            PLAIN_CONTROL // 已在完整scalar边界，下一步只发布真实byte位置，不借读窗。
        } else {
            // 原advance按剩余额/4借帧，至少一个byte才有真实解码进展；树路径已包含在控制费用中。
            PLAIN_CONTROL + tree_height(document) + 4
        }
    }

    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork::default();
        self.advance_position(document, budget, &mut work)?;
        Ok(work)
    }

    /// 普通Bounds与行首恢复共用实际定位步骤；后者在失败时仍向原请求交回已发生的读取。
    fn advance_position(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
        work: &mut SemanticWork,
    ) -> Result<(), MarkdownTableRequestError> {
        work.structural_transitions = PLAIN_CONTROL;
        if !self.located {
            let tree = tree_height(document);
            if tree + PLAIN_CONTROL > budget {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let mut node = document
                .root
                .as_deref()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            let mut local = self.target;
            let mut bytes = 0;
            let mut utf16 = 0;
            // 完整子树只读已验证缓存计量；真实叶片前缀留给下一步，不做同步64KiB扫描。
            while let MarkdownPieceTreeNode::Branch { left, right, .. } = node {
                let metrics = left.metrics();
                if local < metrics.utf16_len {
                    node = left;
                } else {
                    local -= metrics.utf16_len;
                    bytes += metrics.byte_len;
                    utf16 += metrics.utf16_len;
                    node = right;
                }
            }
            if local > node.metrics().utf16_len {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let (piece, ordinal) = match node {
                MarkdownPieceTreeNode::OriginalSpan(span) => {
                    // 每个目标独立取得原真实ordinal及逻辑前缀；不能因数值接近而借另一片的进度。
                    let hit = span
                        .locate_utf16(local, true)
                        .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                    bytes += hit.prefix.byte_len;
                    utf16 += hit.prefix.utf16_len;
                    (hit.piece, Some(hit.ordinal))
                }
                MarkdownPieceTreeNode::Leaf(piece) => (piece.clone(), None),
                MarkdownPieceTreeNode::Branch { .. } => unreachable!("原AVL定位已经到达实际叶"),
            };
            self.leaf =
                (piece.byte_len <= crate::original_backing::ORIGINAL_READ_BYTES).then(|| {
                    PositionLeaf {
                        node: std::ptr::from_ref(node).addr(),
                        ordinal,
                        bytes: bytes..bytes + piece.byte_len,
                        utf16: utf16..utf16 + piece.metrics.utf16_len,
                        source: piece.source,
                        source_byte_start: piece.byte_start,
                    }
                });
            self.next = bytes;
            self.utf16 = utf16;
            self.located = true;
            work.source_tree_steps = tree;
            work.structural_transitions += tree;
            work.metadata_bytes = std::mem::size_of::<Self>();
            return Ok(()); // 已完成的AVL定位即刻归账，取消也不会遗漏同步工作。
        }
        if self.remaining == 0 && self.utf16 == self.target {
            self.result = Some(self.next);
            return Ok(());
        }
        let tree = tree_height(document);
        let wanted = budget
            .checked_sub(PLAIN_CONTROL + tree)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?
            / 4;
        if wanted == 0 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let frame = document
            .table_source_frame(self.next, wanted)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        work.source_tree_steps = tree;
        work.structural_transitions += tree;
        for &byte in frame.bytes.iter() {
            work.source_bytes += 1;
            work.utf8_validation_bytes += 1;
            work.structural_transitions += 2;
            if self.remaining == 0 {
                self.scalar_start = self.next;
                let (width, units) = match byte {
                    0..=0x7f => (1, 1),
                    0xc2..=0xdf => (2, 1),
                    0xe0..=0xef => (3, 1),
                    0xf0..=0xf4 => (4, 2),
                    _ => return Err(MarkdownTableRequestError::InvalidRequest),
                };
                self.remaining = width - 1;
                self.scalar_units = units;
            } else {
                if byte & 0xc0 != 0x80 {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                self.remaining -= 1;
            }
            self.next += 1;
            if self.remaining == 0 {
                let end = self
                    .utf16
                    .checked_add(self.scalar_units)
                    .ok_or(MarkdownTableRequestError::WorkLimit)?;
                if self.target <= end {
                    // surrogate内部的viewport端点扩到完整scalar；公开query仍剪回原UTF16请求。
                    self.result = Some(if self.ceil || self.target == end {
                        self.next
                    } else {
                        self.scalar_start
                    });
                    self.utf16 = end;
                    break;
                }
                self.utf16 = end;
            }
        }
        if frame.bytes.is_empty() && self.result.is_none() {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
