//! 同一真实结构请求的有限普通来源出口；来源完成不代表 ordinary Ready、样式或隐藏权限。

use super::*;
use pulldown_cmark::global_frontier::{
    BlockOriginCursor, BlockOriginLimits, BlockOriginLine, BlockOriginStep, InlineBlockKind,
    LeafBlockCloseReason, LeafBlockDetails, LeafBlockKind, VerifiedGlobalInlineBlock,
    VerifiedGlobalLeafBlock,
};
use std::cell::Cell;
use std::rc::{Rc, Weak};

mod coordinates;
use coordinates::Coordinates;
mod required;
mod table_row;
pub use required::{
    MarkdownViewportSourceRequiredRowQuery, MarkdownViewportSourceRequiredRowResult,
};
pub use table_row::VerifiedMarkdownTableRowSource;
mod cell_inline;
pub use cell_inline::{MarkdownTableCellInlineStep, VerifiedMarkdownTableCellInline};
#[cfg(feature = "paged-line-compose")]
mod selection_prefix;
pub(in crate::markdown_document_buffer) mod semantic;
#[cfg(feature = "paged-line-compose")]
pub use selection_prefix::MarkdownSelectionStyleInvariant;
pub use semantic::{
    MarkdownOrdinaryPendingReason, MarkdownOrdinarySemanticState, VerifiedMarkdownPlainViewport,
};
use semantic::{OrdinaryBlockRecord, PlainSemanticJob};
mod leaf_semantic;
pub use leaf_semantic::{MarkdownRootLeafPendingReason, VerifiedMarkdownRootLeafViewport};
use leaf_semantic::{RootLeafSemanticJob, RootLeafSemanticPublication};

// 一个来源页只保有限行/叶描述；总段落行数不变成正文缓存或历史目录。
const MAX_SOURCE_RECORDS: usize = 256;
const CONTROL_COST: usize = 64;

pub(super) enum CapturedOrdinary {
    Inline(VerifiedGlobalInlineBlock),
    Leaf(VerifiedGlobalLeafBlock),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 每次调用由真实 owner 提供的当前上下文；有序 range 不足以代替 anchor/head。
pub struct MarkdownViewportSourceAuthority {
    pub context_revision: u64,
    pub selection: MarkdownTextSelection,
    pub cancelled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownViewportSourceStep {
    Progress { work: SemanticWork },
    SourcesAvailable { work: SemanticWork },
}

/// 物理行来源来自原 BlockOriginCursor；UTF16 坐标按同一真实源逐 byte 计量。
/// 此记录没有可用于隐藏 Markdown 标记、inline 配对或 Native 编辑的权限。
pub struct VerifiedMarkdownOrdinaryLine {
    block_kind: InlineBlockKind,
    block_lexical: Range<usize>,
    origin: BlockOriginLine,
    utf16: [usize; 4],
    /// 原 terminal 行可另拥有 Setext 标记尾部；这个已计量端点只用于续批，公开 Origin 仍是原物理行。
    semantic_utf16_end: usize,
    semantic_block: Option<Rc<OrdinaryBlockRecord>>,
}

impl VerifiedMarkdownOrdinaryLine {
    pub fn block_kind(&self) -> InlineBlockKind {
        self.semantic_block
            .as_ref()
            .map_or(self.block_kind, |block| block.seed.kind())
    }
    pub fn block_lexical_range(&self) -> &Range<usize> {
        &self.block_lexical
    }
    pub fn origin(&self) -> &BlockOriginLine {
        &self.origin
    }
    pub fn physical_utf16_range(&self) -> Range<usize> {
        self.utf16[0]..self.utf16[3]
    }
    pub fn prefix_utf16_range(&self) -> Range<usize> {
        self.utf16[0]..self.utf16[1]
    }
    pub fn content_utf16_range(&self) -> Range<usize> {
        self.utf16[1]..self.utf16[2]
    }
    pub fn eol_utf16_range(&self) -> Range<usize> {
        self.utf16[2]..self.utf16[3]
    }
}

/// 保留原 opaque LeafBlock，不能把 enclosure 当作已经去掉容器 prefix 的正文。
/// consumed_end 与 item source_range.end 可能不同，二者按原 producer 语义分别保留。
pub struct VerifiedMarkdownOrdinaryLeaf {
    block: VerifiedGlobalLeafBlock,
    physical_utf16: Range<usize>,
    #[cfg(feature = "paged-line-compose")]
    root_code_body: Option<RootCodeBody>,
}

/// 只在原 Root Code Leaf 的四端点转换完成时建立，不接受外部范围或推测正文末端。
/// 关闭围栏整条物理行与后继块都在半开 body 外；两个选择的四端点均须留在正文内。
#[cfg(feature = "paged-line-compose")]
struct RootCodeBody {
    utf16: Range<usize>,
}

#[cfg(feature = "paged-line-compose")]
impl RootCodeBody {
    fn contains_selection(&self, selection: MarkdownTextSelection) -> bool {
        self.utf16.contains(&selection.anchor_utf16) && self.utf16.contains(&selection.head_utf16)
    }
}

/// 同一 opaque Leaf 的真实 Root scope 与种类共同决定正文，容器内同形代码不借此能力。
/// 这里只借已完成 producer 的元数据；Coordinates 仍负责原字节到 UTF16 的真实计量。
fn root_code_body(block: &VerifiedGlobalLeafBlock) -> Option<&Range<usize>> {
    if block.scope_depth() != 0 {
        return None;
    }
    match (block.kind(), block.details()) {
        (LeafBlockKind::FencedCode, LeafBlockDetails::FencedCode { body_enclosure, .. })
        | (LeafBlockKind::IndentedCode, LeafBlockDetails::IndentedCode { body_enclosure, .. }) => {
            Some(body_enclosure)
        }
        _ => None,
    }
}

impl VerifiedMarkdownOrdinaryLeaf {
    pub fn kind(&self) -> LeafBlockKind {
        self.block.kind()
    }
    pub fn source_range(&self) -> Range<usize> {
        self.block.source_range()
    }
    pub fn physical_start(&self) -> usize {
        self.block.physical_start()
    }
    pub fn consumed_end(&self) -> usize {
        self.block.consumed_end()
    }
    pub fn close_reason(&self) -> LeafBlockCloseReason {
        self.block.close_reason()
    }
    pub fn details(&self) -> &LeafBlockDetails {
        self.block.details()
    }
    /// 仅为真实 physical_start..consumed_end 的计量，不宣称覆盖 enclosure 的额外尾 prefix。
    pub fn physical_utf16_range(&self) -> &Range<usize> {
        &self.physical_utf16
    }
}

enum SourceRecord {
    Line(VerifiedMarkdownOrdinaryLine),
    Leaf(VerifiedMarkdownOrdinaryLeaf),
}

impl SourceRecord {
    fn utf16_range(&self) -> Range<usize> {
        match self {
            Self::Line(line) => line.utf16[0]..line.semantic_utf16_end,
            Self::Leaf(leaf) => leaf.physical_utf16.clone(),
        }
    }
}

struct SourcePublication {
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    // 只有原请求明确接受新的选择投影时才撤销本次发布。旧 Rc 即使仍被调用方保留，
    // 也不能在文档以后选回相同 anchor/head 时恢复旧样式权限；结构记录本身仍可共享。
    retired: Cell<bool>,
}

impl SourcePublication {
    /// 真实源、取消和完整选择的原错误优先；退休状态只撤销这份发布，不能掩盖 Source 首因。
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
        self.require_live()
    }

    /// 弱语义票先保原 Source/取消优先顺序，再沿其真实发布核同一撤权事实。
    fn require_live(&self) -> Result<(), MarkdownTableRequestError> {
        if self.retired.get() {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        Ok(())
    }
}

/// 同一真实请求生成的表格结果与有限普通来源；整个类型仍然不是 ordinary 语义 Ready。
/// 只保不可变来源 metadata、原共享 scope 页及 source Weak，不持正文 owner。
pub struct VerifiedMarkdownViewportSources {
    table: VerifiedMarkdownTableViewport,
    records: Vec<Rc<SourceRecord>>,
    requested: Range<usize>,
    publication: Rc<SourcePublication>,
    retained_metadata: usize,
    blocks: Vec<Rc<OrdinaryBlockRecord>>,
    semantic: MarkdownOrdinarySemanticState,
}

impl VerifiedMarkdownViewportSources {
    /// 派生的冻结 Source 样式边界已经自行核源健康；仍须沿原出版物拒绝显式退休。
    /// 这里只观察该 Rc 自身的生命周期，不替代有活 Doc 时的完整 Source/选择/context 校验。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn require_live_publication(&self) -> Result<(), MarkdownTableRequestError> {
        self.publication.require_live()
    }

    /// SourcesOnly/Pending 不代表默认样式已验证；两个 Ready variant 各自只授权真实有限覆盖。
    pub fn ordinary_semantics_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<&MarkdownOrdinarySemanticState, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(&self.semantic)
    }

    fn check(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        self.publication.check(document, authority)
    }

    pub fn requested_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(&self.requested)
    }

    pub fn table_sources_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<&VerifiedMarkdownTableViewport, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(&self.table)
    }

    pub fn ordinary_lines_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<
        impl Iterator<Item = &'a VerifiedMarkdownOrdinaryLine> + use<'a>,
        MarkdownTableRequestError,
    > {
        self.check(document, authority)?;
        Ok(self
            .records
            .iter()
            .filter_map(|record| match record.as_ref() {
                SourceRecord::Line(line)
                    if line.physical_utf16_range().start < self.requested.end
                        && self.requested.start < line.physical_utf16_range().end =>
                {
                    Some(line)
                }
                SourceRecord::Line(_) => None, // 原下一行只为同一续体保留；当前窗口不能从 getter 提前借它的来源。
                SourceRecord::Leaf(_) => None,
            }))
    }

    pub fn ordinary_leaves_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<
        impl Iterator<Item = &'a VerifiedMarkdownOrdinaryLeaf> + use<'a>,
        MarkdownTableRequestError,
    > {
        self.check(document, authority)?;
        Ok(self
            .records
            .iter()
            .filter_map(|record| match record.as_ref() {
                SourceRecord::Leaf(leaf)
                    if leaf.physical_utf16.start < self.requested.end
                        && self.requested.start < leaf.physical_utf16.end =>
                {
                    Some(leaf)
                }
                SourceRecord::Leaf(_) => None, // 窗后原 Fact 只等待后继真实交集，不扩展本次发布包络。
                SourceRecord::Line(_) => None,
            }))
    }

    /// 原 closing fence 的 LF/CRLF 由 producer 消费，却不属于 Parser 的 CodeBlock 样式。
    /// 只给完整控制范围借出同一 Root 叶的实际 separator；它不扩充 inline coverage 或隐藏权限。
    pub fn root_fence_separator_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        range: &Range<usize>,
    ) -> Result<Option<Range<usize>>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        if range.is_empty() || range.start < self.requested.start || range.end > self.requested.end
        {
            return Ok(None); // 请求包络外的实际行也不能借当前 publication 取得控制权限。
        }
        for record in &self.records {
            let SourceRecord::Leaf(leaf) = record.as_ref() else {
                continue;
            };
            if leaf.block.scope_depth() != 0
                || leaf.kind() != LeafBlockKind::FencedCode
                || leaf.close_reason() != LeafBlockCloseReason::ExplicitMarker
            {
                continue; // 容器、EOF 与其他叶的 consumed cursor 不代表这份 Root closing-EOL 合同。
            }
            let length = leaf.consumed_end() - leaf.source_range().end;
            if length == 0 {
                continue; // 真实 EOF closer 没有 separator，不能凭期望合成一条空控制。
            }
            // firstpass 的 closing scanner 停在 CR/LF 前，scan_blank_line 只再消费同一 LF/CRLF。
            // 这些 ASCII 字节与 UTF16 单位一一对应；使用已计量 physical 端点，不再读取或复制正文。
            let end = leaf.physical_utf16.end;
            let separator = end - length..end;
            if *range == separator {
                return Ok(Some(separator)); // CRLF 是同一个真实控制；只请求半个不能获得另一半的票。
            }
        }
        Ok(None)
    }
}

struct InlineJob {
    cursor: Box<BlockOriginCursor>,
    kind: InlineBlockKind,
    lexical: Range<usize>,
    semantic_end: usize,
    // Setext终行不属于正文origin；仍须真实计量其原闭合尾部，才能释放唯一来源游标。
    tail_pending: bool,
    terminal_coordinates: Option<[usize; 4]>,
    pending_line: Option<BlockOriginLine>,
    semantic_block: Option<Rc<OrdinaryBlockRecord>>,
}

enum OrdinaryJob {
    Inline(InlineJob),
    Leaf(VerifiedGlobalLeafBlock),
}

#[derive(Clone, Copy)]
enum SourcePhase {
    RestartLeaf,
    Produce,
    Available,
    Consumed,
}

/// opt-in owner 包住一份原 table batch；没有第二个结构 Global 或从 gap 重启的 Parser。
/// 分类水位属于 table batch，origin_floor 属于原唯一来源 cursor，二者从不相互替代。
pub struct MarkdownViewportSourceRequest {
    batch: MarkdownTableViewportRequest,
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    // 完整选择在本请求中不可变；只保原定位器已证明的两个 byte 端点，不持正文或另一份目录。
    selection_bytes: Option<Range<usize>>,
    // 恢复真实行来源只定位当前一个查询点；原定位器与Source共用同一累计账，不保行范围副本。
    row_source_position: Option<semantic::PositionLocator>,
    viewport: Range<usize>,
    job: Option<OrdinaryJob>,
    coordinates: Option<Coordinates>,
    coordinate_active: bool,
    origin_floor: usize,
    records: Vec<Rc<SourceRecord>>,
    blocks: Vec<Rc<OrdinaryBlockRecord>>,
    semantic: Option<PlainSemanticJob>,
    // 独立Plain与联合RootLeaf互斥；Ready后只保原固定job坐标，样式页和publication均已移交。
    leaf_mode: bool,
    leaf_semantic: Option<Box<RootLeafSemanticJob>>,
    leaf_offer_index: usize,
    ready: Option<VerifiedMarkdownViewportSources>,
    last_publication: Option<Weak<SourcePublication>>,
    phase: SourcePhase,
    failed: Option<MarkdownTableRequestError>,
    cell_inline: Option<Box<cell_inline::CellInlineJob>>,
    // Leaf/格语义发布只取走样式；原 HTML 扫描前沿在同 Source 请求内串行移交，不从零重扫。
    source_underline:
        Option<crate::markdown_document_buffer::table_source::MarkdownTableUnderlineRequest>,
}

impl MarkdownDocumentBuffer {
    /// 同一真实来源请求内准备 Root 普通闭合块、标题、代码块与分隔线的逐段真实交集。
    ///
    /// Ready 只授权 `covered_ranges_for` 列出的范围；混合窗口里的未知语法仍是空洞，
    /// 没有任何已证交集时才返回 LeafPending，不能根据原请求包络推断全部语义完成。
    pub fn viewport_leaf_source_request(
        &self,
        viewport: Range<usize>,
        context_revision: u64,
        selection: MarkdownTextSelection,
    ) -> Result<MarkdownViewportSourceRequest, MarkdownTableRequestError> {
        let mut request =
            self.viewport_source_request(viewport.clone(), context_revision, selection)?;
        request.leaf_mode = true;
        request.batch.enable_structural_tables();
        request.install_leaf_semantic(viewport)?;
        Ok(request)
    }

    /// 仅显式实验入口准备 Root Plain/Setext；复杂语法和非 Root 仍保留明确 Pending。
    pub fn viewport_plain_source_request(
        &self,
        viewport: Range<usize>,
        context_revision: u64,
        selection: MarkdownTextSelection,
    ) -> Result<MarkdownViewportSourceRequest, MarkdownTableRequestError> {
        let mut request =
            self.viewport_source_request(viewport.clone(), context_revision, selection)?;
        request.semantic = Some(PlainSemanticJob::new(
            viewport,
            selection,
            request.batch.source_semantic_quota()?,
        ));
        Ok(request)
    }

    /// 显式 opt-in 才截留普通来源；完整选择必须与当前 Core 事实相同，不能传有序主范围代替。
    pub fn viewport_source_request(
        &self,
        viewport: Range<usize>,
        context_revision: u64,
        selection: MarkdownTextSelection,
    ) -> Result<MarkdownViewportSourceRequest, MarkdownTableRequestError> {
        if selection != self.selection() {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let mut batch = self.table_viewport_request(
            viewport.clone(),
            context_revision,
            ordered_selection(selection),
        )?;
        batch.enable_source_capture();
        // 工作区只预留一次，页/allocator 实际内存仍由整产品验证，不能以本逻辑量代替。
        batch.admit_source_metadata(
            source_workspace_metadata() + publication_reservation_metadata(),
        )?;
        let source = batch.source_identity();
        Ok(MarkdownViewportSourceRequest {
            batch,
            source,
            context_revision,
            selection,
            selection_bytes: None,
            row_source_position: None,
            viewport,
            job: None,
            coordinates: None,
            coordinate_active: false,
            origin_floor: 0,
            records: Vec::new(),
            blocks: Vec::new(),
            semantic: None,
            leaf_mode: false,
            leaf_semantic: None,
            leaf_offer_index: 0,
            ready: None,
            last_publication: None,
            phase: SourcePhase::Produce,
            failed: None,
            cell_inline: None,
            source_underline: None,
        })
    }
}

impl MarkdownViewportSourceRequest {
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
        // 只撤销 owner 的后续动作；外带 immutable metadata 仍按原 source/context 检查可读。
        self.failed = Some(error);
        self.cell_inline = None;
        self.source_underline = None;
        self.row_source_position = None;
        self.job = None;
        self.coordinates = None;
        self.coordinate_active = false;
        self.records = Vec::new();
        self.blocks = Vec::new();
        self.semantic = None;
        if error == MarkdownTableRequestError::Cancelled
            && let Some(job) = self.leaf_semantic.as_mut()
        {
            // 显式取消也撤销叶续体自身的首因；其派生页随后与请求一起立即释放。
            job.cancel();
        }
        self.leaf_semantic = None;
        self.leaf_offer_index = 0;
        self.ready = None;
        self.last_publication = None;
        self.batch.abort_source_request(error);
    }

    pub fn cancel(&mut self) {
        if self.failed.is_none() {
            // 已失败时保留首因；显式取消释放全部新派生容量，不给旧 generation 复活入口。
            self.fail(MarkdownTableRequestError::Cancelled);
        }
    }

    pub fn total_work(&self) -> SemanticWork {
        self.batch.total_work()
    }

    /// 为原行来源恢复取得完整首标量的查询窗口；窗口本身不证明这里存在表格或任何格。
    /// 定位沿当前请求的原piece树分量子推进，最多四字节的标量读取同样计入原总账。
    /// 完成后调用方仍须用本请求最新publication执行continue/reposition，再取得真实行票。
    pub fn table_row_source_viewport(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        position: usize,
        budget: usize,
    ) -> Result<Option<Range<usize>>, MarkdownTableRequestError> {
        let result = (|| {
            document
                .original_backing_health()
                .map_err(MarkdownTableRequestError::Source)?;
            self.check(document, authority)?;
            if !self.leaf_mode
                || !matches!(self.phase, SourcePhase::Consumed)
                || self.cell_inline.is_some()
                || position >= document.utf16_len()
                || !(4096..=65536).contains(&budget)
                || self
                    .row_source_position
                    .as_ref()
                    .is_some_and(|locator| !locator.targets(position))
            {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let (bytes, transitions) = self.remaining_source_limits()?;
            let quantum = budget.min(bytes).min(transitions);
            if quantum < 4096 {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            let locator = self
                .row_source_position
                .get_or_insert_with(|| semantic::PositionLocator::new(position, false));
            let (work, result) = locator.advance_row_source_viewport(document, quantum);
            let charged = self.batch.account_source_work(work);
            // 帧归还后的真实Source首错优先；即使解码/归还失败，已借出的标量字节也先进入同一账。
            document
                .original_backing_health()
                .map_err(MarkdownTableRequestError::Source)?;
            let viewport = result?;
            charged?;
            if viewport.is_some() {
                self.row_source_position = None;
            }
            Ok(viewport)
        })();
        if let Err(error) = result {
            self.fail(error); // 定位失败与普通来源失败同属一个owner，不能重发fuel或留下半份查询续体。
        }
        result
    }

    /// 只有真实 ordinary 行/leaf 已完成计量才能推进；不是 Global 的分类水位。
    pub fn origin_forward_floor(&self) -> usize {
        self.origin_floor
    }

    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        budget: usize,
    ) -> Result<MarkdownViewportSourceStep, MarkdownTableRequestError> {
        let result = self.check(document, authority).and_then(|()| {
            if !(4096..=65536).contains(&budget) {
                // 公开 quantum 必须为内部真实控制/坐标步骤留出最小额度；非法调用不能取得输出权限。
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            if matches!(self.phase, SourcePhase::Available) {
                // 已发布的同一份来源可再次观察，不重复分配、计量或授予普通样式 Ready。
                return Ok(MarkdownViewportSourceStep::SourcesAvailable {
                    work: SemanticWork::default(),
                });
            }
            if matches!(self.phase, SourcePhase::Consumed) {
                // 结果已移交后必须用原 publication 显式续批；普通 advance 不能绕过续权检查。
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let (max_bytes, max_transitions) = self.batch.source_limits()?;
            let total = self.batch.total_work();
            let quantum = budget
                .min(max_bytes.saturating_sub(total.source_bytes))
                .min(max_transitions.saturating_sub(total.structural_transitions));
            if quantum < 4096 {
                // 余下累计 fuel 不足一个合法公开步骤时明确终止，不按下一批或下一行重新发放总额。
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            self.advance_inner(document, quantum)
        });
        if let Err(error) = result {
            // 可变控制入口固定首个失败并释放新派生资源，恢复取消值/旧代号不能复活该 owner。
            self.fail(error);
        }
        result
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownViewportSourceStep, MarkdownTableRequestError> {
        if matches!(self.phase, SourcePhase::RestartLeaf) {
            // 重定位的Global控制与新Leaf库存分开付原4096量子，不能把两次固定构造挤在一起。
            self.install_leaf_semantic(self.viewport.clone())?;
            self.phase = SourcePhase::Produce;
            return Ok(MarkdownViewportSourceStep::Progress {
                work: RootLeafSemanticJob::construction_work(),
            });
        }
        if self.leaf_mode
            && let Some(index) = self
                .semantic
                .as_ref()
                .filter(|job| !job.done())
                .and_then(PlainSemanticJob::prefix_origin_record_index)
        {
            // 只借原页当前一条来源；没有复制 Origin 目录或第二个 parser，未匹配则下量子继续前进。
            let record = self
                .records
                .get(index)
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            let work = self
                .semantic
                .as_mut()
                .unwrap()
                .advance_prefix_origin(record)?;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        if self
            .leaf_semantic
            .as_ref()
            .is_some_and(|job| job.preparing())
        {
            // 四端点仍由原 piece root 分步计量；未定位前不能猜测哪个 opaque Leaf 拥有覆盖。
            let authority = self.current_authority();
            let limits = self.remaining_source_limits()?;
            let work = self.leaf_semantic.as_mut().unwrap().advance(
                document,
                authority,
                budget,
                limits,
                &mut self.semantic,
                Some(self.batch.source_global_mut()?),
            )?;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        if let Some(job) = self.leaf_semantic.as_mut()
            && self.leaf_offer_index < self.records.len()
        {
            // 续批保留的目录与新来源都每次只移交一条，不能在一个量子内遍历全部256条记录。
            let work = job.offer_record(&self.records[self.leaf_offer_index])?;
            self.leaf_offer_index += 1;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        if !self.leaf_mode
            && self
                .semantic
                .as_ref()
                .is_some_and(PlainSemanticJob::preparing)
        {
            // 先分步定位实际viewport/选择端点；后继只为有限byte覆盖保留原Fact种子与全局u上下文。
            let mut work = self
                .semantic
                .as_mut()
                .unwrap()
                .advance_bounds(document, budget - CONTROL_COST - 2 * MAX_SOURCE_RECORDS)?;
            if !self.semantic.as_ref().unwrap().preparing() {
                let semantic = self.semantic.as_mut().unwrap();
                work.structural_transitions += 2 * self.blocks.len();
                self.blocks
                    .retain(|block| semantic.considers(&block.seed.physical_range()));
                for block in &self.blocks {
                    semantic.consider(block);
                }
            }
            work.structural_transitions += CONTROL_COST;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        if self.batch.ready_for(document)?.is_some() {
            if self.leaf_semantic.as_ref().is_some_and(|job| !job.done()) {
                // 原 table/source 已闭合才准备 Leaf 的语义；一次只推进一个有界阶段，费用回原总账。
                let authority = self.current_authority();
                let limits = self.remaining_source_limits()?;
                let work = self.leaf_semantic.as_mut().unwrap().advance(
                    document,
                    authority,
                    budget,
                    limits,
                    &mut self.semantic,
                    Some(self.batch.source_global_mut()?),
                )?;
                self.batch.account_source_work(work)?;
                return Ok(MarkdownViewportSourceStep::Progress { work });
            }
            if !self.leaf_mode
                && self
                    .semantic
                    .as_ref()
                    .is_some_and(|semantic| !semantic.done())
            {
                // 表格与来源已完成后再付独立语义步骤；不把来源水位或空styles偷换成PlainReady。
                if self.semantic.as_ref().unwrap().needs_underline_workspace() {
                    self.batch.reserve_source_underline_workspace()?;
                }
                let limits = self.batch.source_limits()?;
                let total = self.batch.total_work();
                let (work, runs) = self.semantic.as_mut().unwrap().advance_after_sources(
                    document,
                    budget,
                    (
                        limits.0.saturating_sub(total.source_bytes),
                        limits.1.saturating_sub(total.structural_transitions),
                    ),
                    Some(self.batch.source_global_mut()?),
                )?;
                // 新run仍只在已预留固定scratch里；通过同quota准入前不会移交给任何消费者。
                self.batch.admit_source_runs(runs)?;
                self.batch.account_source_work(work)?;
                return Ok(MarkdownViewportSourceStep::Progress { work });
            }
            // 表格子阶段可能刚好用满上一 quantum；来源 publication 必须另付本步控制/元数据费用。
            return self.publish_sources(document, budget);
        }
        if self.job.is_some() && self.origin_floor < self.viewport.end {
            // 先推进原唯一来源 cursor；Global 的无表分类水位即使已到 EOF，也不能跳过这些真实行。
            let work = self.advance_ordinary(document, budget)?;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        if let Some(captured) = self.batch.take_ordinary_source() {
            // 只接管同一实际 Global 发出的 opaque Fact，不从 gap 范围另造 paragraph/leaf 种子。
            if self.job.is_some() {
                // 原未完来源已覆盖本视口时应由分类水位封批，不能把另一个 Fact 塞进第二个活动游标。
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            let work = self.install_job(captured)?;
            self.batch.account_source_work(work)?;
            return Ok(MarkdownViewportSourceStep::Progress { work });
        }
        // 没有待消费来源才推进原 table batch；其中已有 Global/refs/math 的工作由它自己累计。
        let step = self.batch.advance(document, false, budget)?;
        let work = match step {
            MarkdownTableViewportStep::Progress { work }
            | MarkdownTableViewportStep::Ready { work } => work,
        };
        Ok(MarkdownViewportSourceStep::Progress { work })
    }

    fn publish_sources(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownViewportSourceStep, MarkdownTableRequestError> {
        let leaf_work = self
            .leaf_semantic
            .as_ref()
            .map_or(Ok(SemanticWork::default()), |job| {
                job.publication_work(document)
            })?;
        // Leaf 的 semantic 槽只保已移交输出的普通前缀，不另发布 Plain Box 或重复清理配对。
        let plain = (!self.leaf_mode)
            .then_some(self.semantic.as_ref())
            .flatten();
        let work = SemanticWork {
            // Box Plain 载荷只有实际创建时另付固定控制费；配额已在原发布预留中包含载荷和句柄。
            structural_transitions: 2 * CONTROL_COST
                + plain.map_or(Ok(0), |job| job.publication_control(document))?
                + leaf_work.structural_transitions,
            metadata_bytes: publication_metadata()
                + plain.map_or(0, PlainSemanticJob::publication_payload_metadata)
                + leaf_work.metadata_bytes,
            ..SemanticWork::default()
        };
        if work.structural_transitions > budget || work.metadata_bytes > budget {
            // 本步必须先容纳真实 publication 迁移和元数据登记；不足时不创建 Rc 或移走表格结果。
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        self.batch.account_source_work(work)?;
        // 表格结果与来源目录在同一个实际 batch 成功后移交；整体名称仍是 Sources，不授 ordinary Ready。
        let table = self
            .batch
            .take_ready_for(document)?
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let publication = Rc::new(SourcePublication {
            source: self.source.clone(),
            context_revision: self.context_revision,
            selection: self.selection,
            retired: Cell::new(false),
        });
        // 移交语义 owner 前接回同源、同完整选择的真实端点。后续每格仍须核当前发布，
        // 但无需为远处 Find 选区反复解码从根到选区的 UTF8；这些标量不授予额外语义覆盖。
        if self.selection_bytes.is_none() {
            self.selection_bytes = self
                .leaf_semantic
                .as_ref()
                .and_then(|job| job.verified_selection_bytes())
                .or_else(|| {
                    self.semantic
                        .as_ref()
                        .and_then(PlainSemanticJob::verified_selection_bytes)
                });
        }
        let authority = self.current_authority();
        let semantic = if let Some(job) = self.leaf_semantic.as_mut() {
            // 同一SourcePublication才授权Ready；输出移交后只留固定坐标owner，不持旧发布链。
            let (published, measured) =
                job.publish(document, authority, Rc::downgrade(&publication), budget)?;
            if measured != leaf_work {
                // 纯 Core 发布间没有回调；费用不一致表示内部阶段失配，禁止移交部分成功。
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            self.source_underline = job.return_underline();
            match published {
                RootLeafSemanticPublication::Pending(reason) => {
                    // 未知语义不保停车owner；publication_work已包含本次真实固定析构费用。
                    self.leaf_semantic = None;
                    MarkdownOrdinarySemanticState::LeafPending(reason)
                }
                RootLeafSemanticPublication::Ready(ready) => {
                    MarkdownOrdinarySemanticState::RootLeafReady(ready)
                }
            }
        } else {
            match self.semantic.as_mut() {
                Some(job) => job.finish(
                    self.source.clone(),
                    self.context_revision,
                    Rc::downgrade(&publication),
                )?,
                None => MarkdownOrdinarySemanticState::SourcesOnly,
            }
        };
        let retained_metadata = self
            .records
            .len()
            .checked_mul(record_metadata())
            .and_then(|bytes| bytes.checked_add(publication_metadata()))
            .and_then(|bytes| bytes.checked_add(semantic.retained_metadata()))
            .and_then(|bytes| {
                bytes.checked_add(
                    self.blocks.len() * block_metadata()
                        + self.blocks.capacity() * std::mem::size_of::<Rc<OrdinaryBlockRecord>>(),
                )
            })
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        self.last_publication = Some(Rc::downgrade(&publication));
        self.ready = Some(VerifiedMarkdownViewportSources {
            table,
            records: std::mem::take(&mut self.records),
            requested: self.viewport.clone(),
            publication,
            retained_metadata,
            blocks: std::mem::take(&mut self.blocks),
            semantic,
        });
        self.phase = SourcePhase::Available;
        Ok(MarkdownViewportSourceStep::SourcesAvailable { work })
    }

    fn install_job(
        &mut self,
        captured: CapturedOrdinary,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: CONTROL_COST,
            ..SemanticWork::default()
        };
        self.job = Some(match captured {
            CapturedOrdinary::Inline(block) => {
                // 仅真实已闭合票据可构造 origin；子游标上限取同一请求剩余额，最终费用仍回写父总账。
                let kind = block.kind();
                let lexical = block.lexical_range();
                let semantic_end = block.source_range().end;
                let (max_bytes, max_transitions) = self.batch.source_limits()?;
                let total = self.batch.total_work();
                let limits = BlockOriginLimits {
                    max_byte_work: max_bytes.saturating_sub(total.source_bytes),
                    max_transitions: max_transitions.saturating_sub(total.structural_transitions),
                };
                let keep = (!self.leaf_mode
                    && self
                        .semantic
                        .as_ref()
                        .is_some_and(|job| job.considers(&block.physical_range())))
                    || self
                        .leaf_semantic
                        .as_ref()
                        .is_some_and(|job| job.retains_inline_seed(&block.physical_range()));
                let (cursor, semantic_block) = if keep {
                    if self.blocks.len() == MAX_SOURCE_RECORDS {
                        return Err(MarkdownTableRequestError::Window(
                            MarkdownTableWindowError::SemanticLimit,
                        ));
                    }
                    // 在任何新Rc/目录容量前联合准入；完整正文、角色集合与source强owner均不进入seed。
                    self.batch.admit_source_metadata(
                        block_metadata() + std::mem::size_of::<Rc<OrdinaryBlockRecord>>(),
                    )?;
                    work.metadata_bytes = block_metadata()
                        + self.blocks.len() * std::mem::size_of::<Rc<OrdinaryBlockRecord>>();
                    work.structural_transitions += self.blocks.len() + CONTROL_COST;
                    let (cursor, seed) = block
                        .into_origins_and_semantic_seed(limits)
                        .map_err(MarkdownTableRequestError::Global)?;
                    let record = Rc::new(OrdinaryBlockRecord { seed });
                    if !self.leaf_mode
                        && let Some(semantic) = self.semantic.as_mut()
                    {
                        semantic.consider(&record);
                    } // 联合模式由原物理行交付同一 seed，不另建第二份 Plain 请求。

                    self.blocks.reserve_exact(1);
                    self.blocks.push(Rc::clone(&record));
                    (cursor, Some(record))
                } else {
                    // 原SourcesOnly与窗口外来源保原factory，不为无关块建立语义库存。
                    (
                        block
                            .into_line_origins(limits)
                            .map_err(MarkdownTableRequestError::Global)?,
                        None,
                    )
                };
                OrdinaryJob::Inline(InlineJob {
                    cursor: Box::new(cursor),
                    kind,
                    lexical,
                    semantic_end,
                    tail_pending: false,
                    terminal_coordinates: None,
                    pending_line: None,
                    semantic_block,
                })
            }
            CapturedOrdinary::Leaf(block) => {
                // Leaf 不重分类或重扫正文；保留原 opaque 块直到真实源坐标转换完成再移交。
                OrdinaryJob::Leaf(block)
            }
        });
        Ok(work)
    }

    fn advance_ordinary(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let needs_coordinates = match self.job.as_ref().unwrap() {
            OrdinaryJob::Inline(job) => {
                // 已拿到一条真实物理行时先完成该行坐标，避免新行覆盖唯一待移交记录。
                job.pending_line.is_some() || job.tail_pending
            }
            OrdinaryJob::Leaf(_) => true, // Leaf 原票据已完整闭合，只缺同源 UTF16 坐标。
        };
        if needs_coordinates {
            // 同一连续坐标游标优先消费已有边界，不能逐行重建后反复扫描同 leaf 前缀。
            return self.advance_coordinates(document, budget);
        }
        let tree = tree_height(document);
        let child = child_budget(budget - CONTROL_COST, tree)?;
        let OrdinaryJob::Inline(job) = self.job.as_mut().unwrap() else {
            unreachable!()
        };
        let frame = document
            .table_source_frame(job.cursor.next_byte(), child)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = job
            .cursor
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
        let (measured, complete) = match step {
            BlockOriginStep::Progress { work, .. } => {
                // 原唯一游标仍在匹配容器/正文；真实工作入账，但没有完整物理行可发布。
                (work, false)
            }
            BlockOriginStep::NeedInput { work, .. } => {
                // frame 尾只要求从原位置借下一段；不能当作 source EOF 或完成来源覆盖。
                (work, false)
            }
            BlockOriginStep::Line { line, work } => {
                // 只保留这一条有限来源元数据；先转换真实绝对边界，再决定是否属于当前视口。
                job.pending_line = Some(line);
                (work, false)
            }
            BlockOriginStep::Complete { work } => {
                // 真实闭合块来源耗尽，不授予普通 inline 样式或编辑 Ready。
                (work, true)
            }
        };
        if complete {
            // 原 capture 已完整消费才释放；后继块仍从同一个 Global 的实际位置继续。
            self.job = None;
        }
        Ok(SemanticWork {
            source_bytes: measured.byte_reads,
            source_tree_steps: tree,
            structural_transitions: measured.transitions + tree + 2 * CONTROL_COST,
            ..SemanticWork::default()
        })
    }

    fn advance_coordinates(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        if !self.coordinate_active {
            let boundaries = match self.job.as_ref().unwrap() {
                OrdinaryJob::Inline(job) => {
                    // prefix/content/EOL 四个原字节边界按单调顺序转换，不归一化 CRLF 或 Unicode。
                    if job.tail_pending {
                        // 原Setext Fact已证明尾部范围；只测量闭合尾端，不制造一条正文物理行。
                        [job.semantic_end; 4]
                    } else {
                        let line = job.pending_line.as_ref().unwrap();
                        [
                            line.physical_range().start,
                            line.content_range().start,
                            line.content_range().end,
                            line.eol_range().end,
                        ]
                    }
                }
                OrdinaryJob::Leaf(block) => {
                    // source_range.end 可能含后继行 prefix；只按真实返回 cursor 推进共享坐标游标。
                    // Root Code 复用原四端点中间的两个位置计量正文，不再从可见 marker
                    // 裁剪范围猜整叶边界，也不为选择等效检查另外建立 locator 或读取正文。
                    let body = root_code_body(block);
                    [
                        block.physical_start(),
                        body.map_or_else(|| block.source_range().start, |body| body.start),
                        body.map_or_else(|| block.consumed_end(), |body| body.end),
                        block.consumed_end(),
                    ]
                }
            };
            let mut work = if let Some(coordinates) = self.coordinates.as_mut() {
                // 后继行/块沿同一 byte→UTF16 位置继续，仅移交新的四边界而不重做 AVL 首定位。
                coordinates.continue_at(boundaries)?
            } else {
                // 首次真实 AVL 定位已同步发生；本步接回其费用，取消或 Drop 也不能漏记。
                let (coordinates, work) =
                    Coordinates::new(document, boundaries, budget - CONTROL_COST)?;
                self.coordinates = Some(coordinates);
                work
            };
            self.coordinate_active = true;
            work.structural_transitions = work
                .structural_transitions
                .checked_add(CONTROL_COST)
                .ok_or(MarkdownTableRequestError::WorkLimit)?;
            return Ok(work); // 同步定位已经发生，必须当步归账，不能等下次 advance 才补报。
        }
        let (mut work, values) = self
            .coordinates
            .as_mut()
            .unwrap()
            .advance(document, budget - CONTROL_COST)?;
        let Some(values) = values else {
            // 有限 frame 尚未覆盖全部四边界，只返回真实读取费用，不推进来源覆盖水位。
            return Ok(work);
        };
        // 发布行/Rc 仍在父 quantum 内：子转换器预留了这份真实移交控制费用。
        work.structural_transitions = work
            .structural_transitions
            .checked_add(CONTROL_COST)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        self.coordinate_active = false;
        if let Some(OrdinaryJob::Inline(job)) = self.job.as_mut()
            && !job.tail_pending
            && job.pending_line.as_ref().unwrap().is_terminal()
            && job.pending_line.as_ref().unwrap().eol_range().end < job.semantic_end
        {
            // 原正文 terminal 行仍持自己的四端点；下一量子只沿同坐标游标计量其已证 Setext 尾部。
            // 完整 owner 尚未移交，origin_floor 也不能先封窗，否则下一批会丢掉 underline 的唯一 seed。
            job.terminal_coordinates = Some(values);
            job.tail_pending = true;
            return Ok(work);
        }
        self.origin_floor = values[3];
        self.batch.set_source_origin_floor(self.origin_floor);
        let (record, complete) = match self.job.as_mut().unwrap() {
            OrdinaryJob::Inline(job) => {
                // 原行与四个坐标成对移交；只有 terminal 行已转换时才能释放未完 origin owner。
                let origin = job.pending_line.take().unwrap();
                let complete = origin.is_terminal();
                let physical = if job.tail_pending {
                    job.terminal_coordinates
                        .take()
                        .expect("原 Setext 尾部与自己的 terminal 行四端点一起移交")
                } else {
                    values // 普通行的物理末端与本次已计量末端相同，没有额外语法尾部。
                };
                (
                    SourceRecord::Line(VerifiedMarkdownOrdinaryLine {
                        block_kind: job.kind,
                        block_lexical: job.lexical.clone(),
                        origin,
                        utf16: physical,
                        semantic_utf16_end: values[3],
                        semantic_block: job.semantic_block.as_ref().map(Rc::clone),
                    }),
                    complete,
                )
            }
            OrdinaryJob::Leaf(_) => {
                // 移走原 opaque Leaf 而不是重建票据，原 source/consumed 范围保持独立且可追溯。
                let Some(OrdinaryJob::Leaf(block)) = self.job.take() else {
                    unreachable!()
                };
                #[cfg(feature = "paged-line-compose")]
                let root_code_body = root_code_body(&block).map(|_| RootCodeBody {
                    utf16: values[1]..values[2],
                });
                (
                    SourceRecord::Leaf(VerifiedMarkdownOrdinaryLeaf {
                        block,
                        physical_utf16: values[0]..values[3],
                        #[cfg(feature = "paged-line-compose")]
                        root_code_body,
                    }),
                    true,
                )
            }
        };
        let range = record.utf16_range();
        // 前方旧行仍随原扫描释放；跨空白封窗时，下一份已消费原 Fact 不能跟着丢掉。
        // 原 origin_floor 已越过它，若在这里释放，后继同 Global 无法重新交付其来源。
        // 只保首份越过窗口的原行/叶，仍付原256目录配额；后继保留过滤与语义覆盖只消费真实交集。
        if self.viewport.start < range.end {
            work.metadata_bytes = work
                .metadata_bytes
                .checked_add(record_metadata())
                .ok_or(MarkdownTableRequestError::WorkLimit)?;
            self.admit_record(record)?;
        }
        if complete {
            // 本块已到真实终行/Leaf 末端；释放游标后才允许同一 Global 交付后继普通事实。
            self.job = None;
        }
        Ok(work)
    }

    fn admit_record(&mut self, record: SourceRecord) -> Result<(), MarkdownTableRequestError> {
        if self.records.len() == MAX_SOURCE_RECORDS {
            // 单次来源目录到达256条即明确拒绝，不能把整篇段落的所有物理行积存在请求内。
            return Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::SemanticLimit,
            ));
        }
        // 原 previous 载荷已在续批预扣；每条新记录先联合准入后 Rc 分配，不为大段落保行集合。
        self.batch.admit_source_metadata(record_metadata())?;
        self.records.push(Rc::new(record));
        Ok(())
    }

    pub fn sources_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<&VerifiedMarkdownViewportSources>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        Ok(self.ready.as_ref())
    }

    pub fn take_sources_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<VerifiedMarkdownViewportSources>, MarkdownTableRequestError> {
        if let Err(error) = self.check(document, authority) {
            self.fail(error);
            return Err(error);
        }
        let sources = self.ready.take();
        if sources.is_some() {
            self.phase = SourcePhase::Consumed;
        }
        Ok(sources)
    }

    /// 同一文档只有选择改变时，重投影原有限来源页保留的闭合普通行或真实 Root Code 叶。
    /// Ok(false) 只表示当前库存不能覆盖目标，完全不改变原 owner、发布或证明；
    /// 成功仍须逐量子 advance 认证新选择并取得新发布，不能把旧 Ready/Reader 改名使用。
    ///
    /// # Errors
    /// Source 健康、取消、当前完整选择和最新发布分别核验；准入后的真实失败固定首因并结束原请求。
    pub fn reproject_selection(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<bool, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        // 根端点只触原健康门。旧选择此时已经不是 Doc 当前选择，不能调用旧 check
        // 后把 PublicationSuperseded 吞成能力缺失，更不能绕过真实映射首错与取消。
        let current = document
            .byte_offset_for_utf16(0)
            .map_err(MarkdownTableRequestError::Source)
            .and_then(|_| {
                check_authority(
                    &self.source,
                    self.context_revision,
                    authority.selection,
                    document,
                    authority,
                )
            });
        if let Err(error) = current {
            self.fail(error);
            return Err(error);
        }
        if !matches!(self.phase, SourcePhase::Consumed)
            || previous.publication.retired.get()
            || self.context_revision != previous.publication.context_revision
            || self.selection != previous.publication.selection
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        if viewport.is_empty() || viewport.end > document.utf16_len() || viewport.len() > 64 * 1024
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if !self.leaf_mode
            || self.job.is_some()
            || self.coordinate_active
            || self.cell_inline.is_some()
            || self.row_source_position.is_some()
            || !matches!(
                &previous.semantic,
                MarkdownOrdinarySemanticState::RootLeafReady(_)
            )
        {
            return Ok(false); // 活跃来源/格作业有自己的归还义务，不能借选择变更截掉它们。
        }
        let Some((index, record)) = previous.records.iter().enumerate().find(|(_, record)| {
            let range = record.utf16_range();
            range.start <= viewport.start
                && viewport.end <= range.end
                && match record.as_ref() {
                    SourceRecord::Line(line) => line.semantic_block.is_some(),
                    SourceRecord::Leaf(leaf) => root_code_body(&leaf.block).is_some(),
                }
        }) else {
            return Ok(false); // 旧 prefix 已释放或跨了未知块时仍走原冷路径，不补造历史行目录。
        };
        let block = match record.as_ref() {
            SourceRecord::Line(line) => line.semantic_block.as_ref(),
            // Code 不冒充 OrdinaryBlock seed。保同一个 opaque Leaf Rc，后继仍由
            // RestartLeaf 原坐标/语义流程签新选择；marker 显隐也须重新得到正常发布。
            SourceRecord::Leaf(_) => None,
        };
        let result = (|| {
            if !self.batch.reproject_source_selection(
                document,
                &previous.table,
                viewport.clone(),
                authority.context_revision,
                ordered_selection(authority.selection),
            )? {
                return Ok(false);
            }
            // 原 previous 仍活到调用返回；共享行/seed 只增加有限 Rc 目录，新 Leaf 固定槽
            // 和新发布按原 workspace/reservation 联合准入，既不复制正文也不重发累计 fuel。
            self.batch.admit_source_metadata(
                previous.retained_metadata
                    + std::mem::size_of::<Rc<SourceRecord>>()
                    + usize::from(block.is_some()) * std::mem::size_of::<Rc<OrdinaryBlockRecord>>()
                    + source_workspace_metadata()
                    + publication_reservation_metadata(),
            )?;
            self.batch
                .admit_source_runs(previous.semantic.run_count())?;
            let keep_plain = block.is_some_and(|block| {
                self.semantic
                    .as_ref()
                    .is_some_and(|job| job.retains_plain_block(block))
            });
            self.batch.account_source_work(SemanticWork {
                structural_transitions: CONTROL_COST
                    + index
                    + 1
                    + usize::from(self.leaf_semantic.is_some())
                        * RootLeafSemanticJob::completed_owner_drop_control()
                    + usize::from(self.semantic.is_some() && !keep_plain)
                        * PlainSemanticJob::completed_drop_control(),
                ..Default::default()
            })?;
            // 所有可失败的准入已完成，紧接着退休原发布。外带旧 Rc/弱语义票以后即使
            // 遇到相同选择值也不能复活；新 SourcePublication 只由正常发布步骤生成。
            previous.publication.retired.set(true);
            if !keep_plain {
                self.semantic = None;
            }
            self.leaf_semantic = None;
            self.records = vec![Rc::clone(record)];
            self.blocks = block.map_or_else(Vec::new, |block| vec![Rc::clone(block)]);
            self.selection = authority.selection;
            self.context_revision = authority.context_revision;
            self.selection_bytes = None; // 新选择必须重新通过原四端点定位，旧 byte pair 不授后继权限。
            self.viewport = viewport;
            self.ready = None;
            self.last_publication = None;
            self.leaf_offer_index = 0;
            self.phase = SourcePhase::RestartLeaf;
            // 唯一 underline 继续按原 ResumeUnderline 协议处理：反向覆盖必拒绝续读，
            // 在其独立量子归还旧 scanner 后冷准备；绝不把已有空 pair 当全文负证明。
            Ok(true)
        })();
        if let Err(error) = result {
            self.fail(error);
        }
        result
    }

    /// 只复用原最后 publication 与唯一未完 origin cursor；逆向且无 checkpoint 明确拒绝。
    pub fn continue_forward(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        self.check_forward_sources(document, previous, &viewport, authority)?;
        let result = self.begin_forward(document, previous, viewport);
        if let Err(error) = result {
            self.fail(error); // 已开始联合准入/状态迁移后失败不能留半份续体。
        }
        result
    }

    /// 为旧缓存视口恢复真实RootLeaf语义；同一Global从根重放，后继advance仍按原量子推进。
    ///
    /// # Errors
    /// 只接受本请求最近移交的来源及同Source/完整选择；非法外来票不改原请求，真实失权或重放失败固定首因。
    pub fn reposition(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error); // 已结束请求保留原首因，不让后来的源健康变化改写它。
        }
        // 真实源IO首错先于旧数值权限；根端点检查不读取正文，也不能把EIO洗成普通SourceSuperseded。
        if let Err(cause) = document.byte_offset_for_utf16(0) {
            let error = MarkdownTableRequestError::Source(cause);
            self.fail(error);
            return Err(error);
        }
        if let Err(error) = self.check(document, authority) {
            self.fail(error);
            return Err(error);
        }
        previous.check(document, authority)?;
        if !self.leaf_mode
            || viewport.is_empty()
            || viewport.end > document.utf16_len()
            || viewport.len() > 64 * 1024
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if !matches!(self.phase, SourcePhase::Consumed)
            || self.cell_inline.is_some()
            || self.row_source_position.is_some()
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let result = (|| {
            self.batch
                .reposition_source(document, &previous.table, viewport.clone())?;
            self.batch.admit_source_metadata(
                previous.retained_metadata
                    + source_workspace_metadata()
                    + publication_reservation_metadata(),
            )?;
            self.batch
                .admit_source_runs(previous.semantic.run_count())?;
            if self.semantic.is_some() {
                // 反向重定位真正释放父槽的已完成前缀；batch 的结构重放费用不代付这一资源析构。
                self.batch.account_source_work(SemanticWork {
                    structural_transitions: PlainSemanticJob::completed_drop_control(),
                    ..Default::default()
                })?;
            }
            if self.leaf_semantic.is_some() {
                // 逆向不能续原小片前沿；释放已移走输出的唯一固定job同样属于本次实际控制工作。
                self.batch.account_source_work(SemanticWork {
                    structural_transitions: RootLeafSemanticJob::completed_owner_drop_control(),
                    ..Default::default()
                })?;
            }
            // 外带旧Ready仍可按原Source读取；请求不保旧正文origin、语义工作区或第二个结构前沿。
            self.job = None;
            self.coordinates = None;
            self.coordinate_active = false;
            self.origin_floor = 0;
            self.records = Vec::new();
            self.blocks = Vec::new();
            self.semantic = None;
            self.leaf_semantic = None;
            self.leaf_offer_index = 0;
            self.ready = None;
            self.last_publication = None;
            self.viewport = viewport;
            self.phase = SourcePhase::RestartLeaf;
            Ok(())
        })();
        if let Err(error) = result {
            self.fail(error);
        }
        result
    }

    /// source 与 required-row 共用前置门禁；真实 owner 失权 sticky，外来 previous 或请求范围不改原状态。
    fn check_forward_sources(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: &Range<usize>,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        // 所有参数/身份 preflight 在改状态前完成；错误 publication 不损坏可重试的原 owner。
        if let Err(error) = self.check(document, authority) {
            // 当前 owner 的真实 source/context 已失效；恢复旧数值不能给该续体续权。
            self.fail(error);
            return Err(error);
        }
        previous.check(document, authority)?;
        if !matches!(self.phase, SourcePhase::Consumed)
            || self.cell_inline.is_some()
            || self.row_source_position.is_some()
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        if viewport.start < self.viewport.start || viewport.end < self.viewport.end {
            return Err(MarkdownTableRequestError::CoverageRequiresCheckpoint);
        }
        if viewport.start >= viewport.end
            || viewport.end > document.utf16_len()
            || viewport.len() > 64 * 1024
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok(())
    }

    fn begin_forward(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        if viewport.start > self.batch.forward_resume_floor() {
            // 显式跳向后方仍保原 Global；中间普通块由原前沿真实消费，不重建 Global(0)。
            self.batch
                .resume_source_from_ready(document, &previous.table, viewport.clone())?;
        } else {
            self.batch
                .extend_from_ready(document, &previous.table, viewport.clone())?;
        }
        self.retain_forward_sources(document, previous, viewport)
    }

    /// 原 batch 已接受唯一后继后，联合准入同活普通来源和固定工作区；不重建未完 origin。
    fn retain_forward_sources(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        let table_handles = previous
            .table
            .windows_for(document)?
            .len()
            .checked_add(previous.table.gaps_for(document)?.len())
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let handles = previous
            .records
            .len()
            .checked_mul(std::mem::size_of::<Rc<SourceRecord>>())
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        self.batch.admit_source_metadata(
            previous
                .retained_metadata
                .checked_add(handles)
                .and_then(|bytes| {
                    bytes.checked_add(
                        previous.blocks.len() * std::mem::size_of::<Rc<OrdinaryBlockRecord>>(),
                    )
                })
                .and_then(|bytes| {
                    bytes.checked_add(
                        source_workspace_metadata() + publication_reservation_metadata(),
                    )
                })
                .ok_or(MarkdownTableRequestError::WorkLimit)?,
        )?;
        self.batch
            .admit_source_runs(previous.semantic.run_count())?;
        // previous 仍占联合配额；这里只 clone 有界 Rc 目录，不复制原记录或保父 publication 链。
        self.records = previous
            .records
            .iter()
            .filter(|record| {
                let range = record.utf16_range();
                // Leaf 可能为跨空白封窗先消费唯一下一行。它的原 origin 已越过 Global，
                // 因此只淘汰前方记录；未来记录仍待真实交集，公开 getter 不扩充当前请求范围。
                viewport.start < range.end && (self.leaf_mode || range.start < viewport.end)
            })
            .cloned()
            .collect();
        let block_control = if self.leaf_mode {
            let (blocks, control) = self.retained_leaf_blocks(previous);
            self.blocks = blocks;
            control
        } else {
            self.blocks = previous.blocks.clone();
            2 * previous.blocks.len() // Plain 的既有 Bounds 阶段另按实际 byte 覆盖淘汰块。
        };
        // 旧 table 目录的库存 fold/句柄复制同样收费；仅 ordinary 目录收费会遗漏 source-aware resume 工作。
        let control = previous
            .records
            .len()
            .checked_mul(2)
            .and_then(|count| {
                table_handles
                    .checked_mul(3)
                    .and_then(|tables| count.checked_add(tables))
            })
            .and_then(|count| count.checked_add(CONTROL_COST + block_control))
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        self.batch.account_source_work(SemanticWork {
            structural_transitions: control,
            ..SemanticWork::default()
        })?;
        if !self.leaf_mode
            && let Some(semantic) = self.semantic.as_mut()
        {
            let work = semantic.retarget(viewport.clone());
            self.batch.account_source_work(work)?;
        }
        if self.leaf_mode {
            // 原 batch 已联合预扣旧 Ready；新 job 只借当前同一 quota，不复用上一批失效的额度句柄。
            self.install_leaf_semantic(viewport.clone())?;
        }
        self.viewport = viewport;
        self.phase = SourcePhase::Produce;
        self.last_publication = None;
        Ok(())
    }

    /// Leaf 续批只保实际记录、在途 origin 或原 Plain 前缀仍持有的 opaque seed。
    /// previous 保持不可变，旧公开句柄/分叉仍有原权限；这里只克隆后继真正需要的 Rc。
    fn retained_leaf_blocks(
        &self,
        previous: &VerifiedMarkdownViewportSources,
    ) -> (Vec<Rc<OrdinaryBlockRecord>>, usize) {
        let mut blocks = Vec::with_capacity(previous.blocks.len());
        let mut records = self
            .records
            .iter()
            .filter_map(|record| match record.as_ref() {
                SourceRecord::Line(line) => line.semantic_block.as_ref(),
                SourceRecord::Leaf(_) => None, // 真 Leaf 不持 Inline seed，不能凭相邻范围补一个。
            });
        let mut next_record = records.next();
        let mut control = 0;
        // 两个目录都沿同一 Global 的真实来源顺序产生；双游标只跳过已到前方的记录，
        // 坐标仅用于归并，不授予身份。最终保留必须命中原 Rc 或实际在途拥有者。
        for block in &previous.blocks {
            let start = block.seed.physical_range().start;
            while let Some(record) = next_record {
                control += 1;
                if record.seed.physical_range().start >= start {
                    break;
                }
                next_record = records.next();
            }
            let retained_record = next_record.is_some_and(|record| Rc::ptr_eq(record, block));
            let pending_origin = matches!(
                &self.job,
                Some(OrdinaryJob::Inline(job))
                    if job.semantic_block.as_ref().is_some_and(|current| Rc::ptr_eq(current, block))
            );
            let plain_prefix = self
                .semantic
                .as_ref()
                .is_some_and(|job| job.retains_plain_block(block));
            control += 3; // 三条实际拥有关系各核一次，没有全历史目录的嵌套扫描。
            if retained_record || pending_origin || plain_prefix {
                blocks.push(Rc::clone(block));
                control += 1;
            } // 已无后继拥有者的前方 seed 随旧 previous 归还，不占下一批256条来源库存。
        }
        (blocks, control)
    }

    /// 只由显式 Leaf 工厂与已接受的原 forward 路径调用，不会额外创建 source request。
    fn install_leaf_semantic(
        &mut self,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        if self
            .semantic
            .as_ref()
            .is_some_and(|job| !job.retains_plain_prefix())
        {
            // 只有已移走输出与 underline 的普通前缀可以驻留父原槽；活动语义不能并行，
            // 新 Leaf 仍先核来源与 Bounds，直到同 seed 匹配后才重新使用这个唯一槽。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let quota = self.batch.source_semantic_quota()?;
        let work = if let Some(job) = self.leaf_semantic.as_mut() {
            // 父forward已核原Source/完整选择与最近publication；只能重用这个已发布的唯一槽。
            job.continue_forward(viewport, self.source_underline.take(), quota)?
        } else {
            let job = RootLeafSemanticJob::new(
                self.source.clone(),
                self.context_revision,
                self.selection,
                viewport,
                self.selection_bytes.clone(),
                self.source_underline.take(),
                quota,
            )?;
            self.leaf_semantic = Some(Box::new(job));
            RootLeafSemanticJob::construction_work()
        };
        self.batch.account_source_work(work)?;
        self.leaf_offer_index = 0;
        Ok(())
    }

    /// 子语义续体使用原 batch 尚未消耗的累计额度；新块不能重新获得整份扫描预算。
    fn remaining_source_limits(&self) -> Result<(usize, usize), MarkdownTableRequestError> {
        let limits = self.batch.source_limits()?;
        let total = self.batch.total_work();
        Ok((
            limits.0.saturating_sub(total.source_bytes),
            limits.1.saturating_sub(total.structural_transitions),
        ))
    }

    /// 外层 advance 已核调用者与当前 Core 选择；内层仍逐阶段核同一完整 anchor/head。
    fn current_authority(&self) -> MarkdownViewportSourceAuthority {
        MarkdownViewportSourceAuthority {
            context_revision: self.context_revision,
            selection: self.selection,
            cancelled: false,
        }
    }
}

fn check_authority(
    source: &MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
    document: &MarkdownDocumentBuffer,
    authority: MarkdownViewportSourceAuthority,
) -> Result<(), MarkdownTableRequestError> {
    if authority.cancelled {
        // 实际取消先于其它数值检查；外层可变入口把该首因固定并立即释放派生状态。
        return Err(MarkdownTableRequestError::Cancelled);
    }
    if !source.is_current(document) {
        // Weak allocation 身份、generation 与长度必须同时相同；同字节另一个文档和 Undo 新代均拒绝。
        return Err(MarkdownTableRequestError::SourceSuperseded);
    }
    if context_revision != authority.context_revision
        || selection != authority.selection
        || selection != document.selection()
    {
        // 同有序 range 的反向选择也不同；当前 Core 事实与调用方上下文不能给旧来源补签。
        return Err(MarkdownTableRequestError::PublicationSuperseded);
    }
    Ok(())
}

fn ordered_selection(selection: MarkdownTextSelection) -> Range<usize> {
    selection.anchor_utf16.min(selection.head_utf16)
        ..selection.anchor_utf16.max(selection.head_utf16)
}

fn publication_metadata() -> usize {
    std::mem::size_of::<VerifiedMarkdownViewportSources>()
        + std::mem::size_of::<SourcePublication>()
        + 2 * std::mem::size_of::<usize>()
}

fn publication_reservation_metadata() -> usize {
    // 准入提前覆盖最坏 Plain 分支；实际工作/旧库存仍按 publication 的真实 variant 单独计算。
    publication_metadata() + semantic::plain_publication_metadata()
}

fn source_workspace_metadata() -> usize {
    std::mem::size_of::<MarkdownViewportSourceRequest>() + std::mem::size_of::<BlockOriginCursor>()
}

fn record_metadata() -> usize {
    std::mem::size_of::<SourceRecord>()
        + std::mem::size_of::<Rc<SourceRecord>>()
        + 2 * std::mem::size_of::<usize>()
}

fn block_metadata() -> usize {
    std::mem::size_of::<OrdinaryBlockRecord>() + 2 * std::mem::size_of::<usize>()
}

#[cfg(test)]
mod tests;
