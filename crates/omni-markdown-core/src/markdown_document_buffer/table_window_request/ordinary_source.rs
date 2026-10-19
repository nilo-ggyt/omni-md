//! 同一真实结构请求的有限普通来源出口；来源完成不代表 ordinary Ready、样式或隐藏权限。

use super::*;
use pulldown_cmark::global_frontier::{
    InlineBlockKind, InlineOriginCursor, InlineOriginLimits, InlineOriginLine, InlineOriginStep,
    LeafBlockCloseReason, LeafBlockDetails, LeafBlockKind, VerifiedGlobalInlineBlock,
    VerifiedGlobalLeafBlock,
};
use std::rc::{Rc, Weak};

mod coordinates;
use coordinates::Coordinates;
mod required;
pub use required::MarkdownViewportSourceRequiredRowQuery;
mod semantic;
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

/// 物理行来源来自原 InlineOriginCursor；UTF16 坐标按同一真实源逐 byte 计量。
/// 此记录没有可用于隐藏 Markdown 标记、inline 配对或 Native 编辑的权限。
pub struct VerifiedMarkdownOrdinaryLine {
    block_kind: InlineBlockKind,
    block_lexical: Range<usize>,
    origin: InlineOriginLine,
    utf16: [usize; 4],
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
    pub fn origin(&self) -> &InlineOriginLine {
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
            Self::Line(line) => line.physical_utf16_range(),
            Self::Leaf(leaf) => leaf.physical_utf16.clone(),
        }
    }
}

struct SourcePublication {
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: MarkdownTextSelection,
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
        check_authority(
            &self.publication.source,
            self.publication.context_revision,
            self.publication.selection,
            document,
            authority,
        )
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
                SourceRecord::Line(line) => Some(line),
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
                SourceRecord::Leaf(leaf) => Some(leaf),
                SourceRecord::Line(_) => None,
            }))
    }
}

struct InlineJob {
    cursor: Box<InlineOriginCursor>,
    kind: InlineBlockKind,
    lexical: Range<usize>,
    pending_line: Option<InlineOriginLine>,
    semantic_block: Option<Rc<OrdinaryBlockRecord>>,
}

enum OrdinaryJob {
    Inline(InlineJob),
    Leaf(VerifiedGlobalLeafBlock),
}

#[derive(Clone, Copy)]
enum SourcePhase {
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
    viewport: Range<usize>,
    job: Option<OrdinaryJob>,
    coordinates: Option<Coordinates>,
    coordinate_active: bool,
    origin_floor: usize,
    records: Vec<Rc<SourceRecord>>,
    blocks: Vec<Rc<OrdinaryBlockRecord>>,
    semantic: Option<PlainSemanticJob>,
    // 独立 Plain 实验与联合 RootLeaf 模式互斥；发布后只保模式标量供原请求显式续批。
    leaf_mode: bool,
    leaf_semantic: Option<Box<RootLeafSemanticJob>>,
    leaf_offer_index: usize,
    ready: Option<VerifiedMarkdownViewportSources>,
    last_publication: Option<Weak<SourcePublication>>,
    phase: SourcePhase,
    failed: Option<MarkdownTableRequestError>,
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
        request.semantic = Some(PlainSemanticJob::new(viewport, selection));
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
                false,
                budget,
                limits,
                &mut self.semantic,
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
                    true,
                    budget,
                    limits,
                    &mut self.semantic,
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
        let work = SemanticWork {
            // Box Plain 载荷只有实际创建时另付固定控制费；配额已在原发布预留中包含载荷和句柄。
            structural_transitions: 2 * CONTROL_COST
                + self
                    .semantic
                    .as_ref()
                    .map_or(Ok(0), |job| job.publication_control(document))?
                + leaf_work.structural_transitions
                + usize::from(self.leaf_semantic.is_some())
                    * RootLeafSemanticJob::completed_owner_drop_control(),
            metadata_bytes: publication_metadata()
                + self
                    .semantic
                    .as_ref()
                    .map_or(0, PlainSemanticJob::publication_payload_metadata)
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
        });
        let semantic = if let Some(mut job) = self.leaf_semantic.take() {
            // 同一实际 SourcePublication 才能授权 Leaf Ready；先计费后移交，随后释放所有临时续体。
            let (published, measured) = job.publish(
                document,
                self.current_authority(),
                Rc::downgrade(&publication),
                budget,
            )?;
            if measured != leaf_work {
                // 纯 Core 发布间没有回调；费用不一致表示内部阶段失配，禁止移交部分成功。
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            drop(job); // 已计固定目录 Drop 费用；请求不保留旧 pair、record 或 run 页的第二份 owner。
            match published {
                RootLeafSemanticPublication::Pending(reason) => {
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
                let (max_bytes, max_transitions) = self.batch.source_limits()?;
                let total = self.batch.total_work();
                let limits = InlineOriginLimits {
                    max_byte_work: max_bytes.saturating_sub(total.source_bytes),
                    max_transitions: max_transitions.saturating_sub(total.structural_transitions),
                };
                let keep = self
                    .semantic
                    .as_ref()
                    .is_some_and(|job| job.considers(&block.physical_range()))
                    || self
                        .leaf_semantic
                        .as_ref()
                        .is_some_and(|job| job.considers(&block.physical_range()));
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
                    if let Some(semantic) = self.semantic.as_mut() {
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
                job.pending_line.is_some()
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
            InlineOriginStep::Progress { work, .. } => {
                // 原唯一游标仍在匹配容器/正文；真实工作入账，但没有完整物理行可发布。
                (work, false)
            }
            InlineOriginStep::NeedInput { work, .. } => {
                // frame 尾只要求从原位置借下一段；不能当作 source EOF 或完成来源覆盖。
                (work, false)
            }
            InlineOriginStep::Line { line, work } => {
                // 只保留这一条有限来源元数据；先转换真实绝对边界，再决定是否属于当前视口。
                job.pending_line = Some(line);
                (work, false)
            }
            InlineOriginStep::Complete { work } => {
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
                    let line = job.pending_line.as_ref().unwrap();
                    [
                        line.physical_range().start,
                        line.content_range().start,
                        line.content_range().end,
                        line.eol_range().end,
                    ]
                }
                OrdinaryJob::Leaf(block) => {
                    // source_range.end 可能含后继行 prefix；只按真实返回 cursor 推进共享坐标游标。
                    [
                        block.physical_start(),
                        block.source_range().start,
                        block.consumed_end(),
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
        self.origin_floor = values[3];
        self.batch.set_source_origin_floor(self.origin_floor);
        let (record, complete) = match self.job.as_mut().unwrap() {
            OrdinaryJob::Inline(job) => {
                // 原行与四个坐标成对移交；只有 terminal 行已转换时才能释放未完 origin owner。
                let origin = job.pending_line.take().unwrap();
                let complete = origin.is_terminal();
                (
                    SourceRecord::Line(VerifiedMarkdownOrdinaryLine {
                        block_kind: job.kind,
                        block_lexical: job.lexical.clone(),
                        origin,
                        utf16: values,
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
                (
                    SourceRecord::Leaf(VerifiedMarkdownOrdinaryLeaf {
                        block,
                        physical_utf16: values[0]..values[3],
                    }),
                    true,
                )
            }
        };
        let range = record.utf16_range();
        // 离屏行仍真实扫描和计量但不保留；只有相交记录进入有限目录，后继沿原 cursor 前进。
        if range.start < self.viewport.end && self.viewport.start < range.end {
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
                range.start < viewport.end && viewport.start < range.end
            })
            .cloned()
            .collect();
        self.blocks = previous.blocks.clone();
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
            .and_then(|count| count.checked_add(CONTROL_COST + 2 * previous.blocks.len()))
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        self.batch.account_source_work(SemanticWork {
            structural_transitions: control,
            ..SemanticWork::default()
        })?;
        if let Some(semantic) = self.semantic.as_mut() {
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

    /// 只由显式 Leaf 工厂与已接受的原 forward 路径调用，不会额外创建 source request。
    fn install_leaf_semantic(
        &mut self,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        if self.semantic.is_some() || self.leaf_semantic.is_some() {
            // 双语义 owner 会重复占有 underline 工作区，必须在任何新派生分配前拒绝。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let job = RootLeafSemanticJob::new(
            self.source.clone(),
            self.context_revision,
            self.selection,
            viewport,
            self.batch.source_semantic_quota()?,
        )?;
        self.batch
            .account_source_work(RootLeafSemanticJob::construction_work())?;
        self.leaf_semantic = Some(Box::new(job));
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
    std::mem::size_of::<MarkdownViewportSourceRequest>() + std::mem::size_of::<InlineOriginCursor>()
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
