//! 原来源 owner 内的 Root Plain/Setext 有限语义；复杂 inline/非 Root/空洞明确保持 Pending。

use super::*;
use crate::markdown_document_buffer::table_source::MarkdownTableUnderlineRequest;
use crate::markdown_viewport_presentation::parser::markdown_selection_intersects;
use crate::markdown_viewport_presentation::{
    MarkdownPresentationQuery, MarkdownPresentationRun, MarkdownPresentationStyles,
};
use pulldown_cmark::global_frontier::{
    InlinePlainCursor, InlinePlainLimits, InlinePlainStep, VerifiedInlinePlainProof,
    VerifiedInlineSemanticSeed,
};

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
            Self::PlainReady(_) => plain_publication_metadata(),
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
        if self.publication.upgrade().is_none() {
            // 原来源结果已释放时，孤立的语义 metadata 不能借相同 g 恢复发布身份。
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        Ok(())
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
        Ok(self.runs[..self.len]
            .iter()
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
        Ok(self.runs[..self.len].iter().flatten())
    }
}

#[derive(Clone, Copy)]
enum SemanticPhase {
    Bounds,
    AwaitSource,
    Proof,
    Coordinates,
    Underline,
    Build,
    Done,
}

/// 状态属于原请求；前缀 cursor 在同一块 forward 时保留，既不建新 Global，也不重发累计 fuel。
pub(super) struct PlainSemanticJob {
    requested: Range<usize>,
    selection: MarkdownTextSelection,
    positions: [PositionLocator; 4],
    position_index: usize,
    byte_coverage: Option<Range<usize>>,
    selection_bytes: Range<usize>,
    block: Option<Rc<OrdinaryBlockRecord>>,
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
}

impl PlainSemanticJob {
    pub(super) fn new(requested: Range<usize>, selection: MarkdownTextSelection) -> Self {
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
            selection,
            position_index: 0,
            byte_coverage: None,
            selection_bytes: 0..0,
            block: None,
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
        let mut job = Self::new(requested, selection);
        job.byte_coverage = Some(byte_coverage);
        job.selection_bytes = selection_bytes;
        job.block = Some(Rc::clone(block));
        job.saw_intersection = true;
        job.underline = Some(underline);
        job.phase = SemanticPhase::AwaitSource;
        Ok(job)
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
        Ok(self.runs.get_mut(index).and_then(Option::take))
    }

    /// 原全源配对请求在一个活动块完成后归还 RootLeaf，下一块复用同一已闭合上下文。
    pub(super) fn return_underline(&mut self) -> Option<MarkdownTableUnderlineRequest> {
        self.underline.take()
    }

    pub(super) fn preparing(&self) -> bool {
        matches!(self.phase, SemanticPhase::Bounds)
    }
    pub(super) fn done(&self) -> bool {
        matches!(self.phase, SemanticPhase::Done)
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
                    self.phase = SemanticPhase::Proof;
                }
            }
            SemanticPhase::Proof => {
                let end = self.byte_coverage.as_ref().unwrap().end;
                match self.cursor.as_mut() {
                    None => {
                        // 构造只保存原 capture；LUT、scope初始化留给下个有预算的 vendor 步骤。
                        self.cursor = Some(
                            self.block
                                .as_ref()
                                .unwrap()
                                .seed
                                .plain_cursor(
                                    end,
                                    InlinePlainLimits {
                                        max_byte_work: limits.0,
                                        max_transitions: limits.1,
                                    },
                                )
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
                                // 已遇 enabled special，不能猜其未来配对、引用或HTML效果，也不发布默认样式。
                                self.pending =
                                    Some(MarkdownOrdinaryPendingReason::NeedsGeneralInline);
                                self.phase = SemanticPhase::Done;
                                work
                            }
                            InlinePlainStep::NonRootScope { work } => {
                                // 原scope含祖先时留给后续真实容器投影，不能从前缀文本猜Root身份。
                                self.pending = Some(MarkdownOrdinaryPendingReason::NonRootScope);
                                self.phase = SemanticPhase::Done;
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
            SemanticPhase::Coordinates => {
                if self.projection.is_none() {
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
                    let (projection, measured) = Coordinates::new(
                        document,
                        [coverage.start, owner, marker, coverage.end],
                        budget - PLAIN_CONTROL,
                    )?;
                    self.projection = Some(projection);
                    work = measured;
                    work.structural_transitions += PLAIN_CONTROL;
                } else {
                    let (measured, values) = self
                        .projection
                        .as_mut()
                        .unwrap()
                        .advance(document, budget - PLAIN_CONTROL)?;
                    work = measured;
                    work.structural_transitions += PLAIN_CONTROL;
                    if let Some(values) = values {
                        // 所有可见边界已在原源上真实解码，才能把样式分段转为绝对UTF16坐标。
                        self.projected = Some(values);
                        self.projection = None;
                        self.phase = SemanticPhase::Underline;
                    }
                }
            }
            SemanticPhase::Underline => {
                match self.underline.as_mut() {
                    None => {
                        // 父owner在进入本阶段前已联合准入唯一underline工作区；只接实际有限byte覆盖。
                        self.underline = Some(
                            document
                                .table_underline_request(std::slice::from_ref(
                                    self.byte_coverage.as_ref().unwrap(),
                                ))
                                .map_err(MarkdownTableRequestError::Window)?,
                        );
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
                        let measured = underline
                            .advance(document, (budget - PLAIN_CONTROL) / 16)
                            .map_err(MarkdownTableRequestError::Window)?;
                        work.source_bytes = measured.source_bytes;
                        work.utf8_validation_bytes = measured.utf8_validation_bytes;
                        work.source_tree_steps = measured.source_tree_steps;
                        work.structural_transitions += measured.source_bytes
                            + measured.utf8_validation_bytes
                            + measured.source_tree_steps;
                    }
                }
            }
            SemanticPhase::Build => {
                if self.run_next == self.requested.end {
                    // 每个可查询位置都有真实样式段后才结束；覆盖外仍是None而非默认样式。
                    self.phase = SemanticPhase::Done;
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
                        let values = self.projected.unwrap();
                        let proof = self.proof.as_ref().unwrap();
                        let mut end = self.requested.end;
                        let mut styles = MarkdownPresentationStyles::default();
                        let heading = match proof.kind() {
                            InlineBlockKind::Paragraph => MarkdownPresentationStyles::default(), // 真Root paragraph没有隐式容器样式。
                            InlineBlockKind::SetextHeading(pulldown_cmark::HeadingLevel::H1) => {
                                MarkdownPresentationStyles::HEADING_1
                            }
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
                            if pair.content.contains(&self.run_next) {
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
                        if self.len > 0
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
            },
        )))
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
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..SemanticWork::default()
        };
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
            return Ok(work); // 已完成的AVL定位即刻归账，取消也不会遗漏同步工作。
        }
        if self.remaining == 0 && self.utf16 == self.target {
            self.result = Some(self.next);
            return Ok(work);
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
        Ok(work)
    }
}

#[cfg(test)]
mod tests;
