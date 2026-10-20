//! 同一真实结构前沿上的多表批次；gap仅有无表权限，不能被当作普通inline语义Ready。

use super::*;
use crate::markdown_viewport_presentation::table_window::{
    TABLE_WINDOW_MAX_BYTES, TABLE_WINDOW_MAX_UTF16, collect_table_window_reference_queries,
};
use std::rc::{Rc, Weak};

mod required;
use required::RequiredState;
pub use required::{MarkdownTableViewportRequiredRowQuery, MarkdownTableViewportRequiredRowResult};
pub(in crate::markdown_document_buffer::table_window_request) use required::{
    RequiredLocation, RequiredPublication,
};

const MAX_COLUMN_SLOTS: usize = 1024;
// 本层先保留最坏收尾额度，未使用的额度不能再伪记为已消耗转换。
const SWEEP_CONTROL_RESERVE: usize = 1024;
// 支付固定目标/结束事实检查、gap边界判断、至多一次票据/作业移交和phase更新。
// 这是具名状态机fuel，不是CPU指令或allocator内部搬移字节的测量。
const SWEEP_CONTROL_BASE: usize = 128;
// 每个真实body成员支付bytes与UTF16两次fold，以及一次行metadata移交准入。
const SWEEP_ROW_ADMISSION_COST: usize = 3;
// 最后一个窗口句柄且持有 End 时，释放至多原 4MiB spine 页库存；不把此费用加到普通 Rc 减计数。
const PRUNE_LAST_SPINE_COST: usize = 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// 本次构建包含旧 publication 同活载荷的联合准入量；不等于 allocator 容量或整产品峰值。
/// Ready 内部另存修剪后真实保留量，供下一次扩展预扣，不能以修剪退款绕过本次峰值上限。
/// 配额只涵盖传入 previous 与本次新记录，不涵盖调用方任意保留的全部历史 Ready。
pub struct MarkdownTableViewportBudget {
    pub row_slots: usize,
    pub cells: usize,
    pub column_slots: usize,
    pub source_bytes: usize,
    pub source_utf16: usize,
    pub runs: usize,
    pub metadata_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 每个区间都来自真实Table成员或分类水位；不能用单一凸包掩盖未证明的空洞。
pub enum MarkdownTableViewportCoverage {
    Table {
        window_index: usize,
        utf16_range: Range<usize>,
    },
    Gap {
        gap_index: usize,
        utf16_range: Range<usize>,
    },
    /// 只证明真实物理行与 H/D 上下文；单格 inline 必须沿同请求另行完成。
    Structure {
        table_index: usize,
        utf16_range: Range<usize>,
    },
}
impl MarkdownTableViewportCoverage {
    pub fn utf16_range(&self) -> &Range<usize> {
        match self {
            Self::Table { utf16_range, .. }
            | Self::Gap { utf16_range, .. }
            | Self::Structure { utf16_range, .. } => utf16_range,
        }
    }
}

/// 仅由源序producer构造。此票证明range无真实table，不证明可从该处重置paragraph/容器。
pub struct VerifiedMarkdownTableGap {
    source: MarkdownTableSourceIdentity,
    bytes: Range<usize>,
    utf16: Range<usize>,
}

// 每份记录只保不可变 metadata；后继发布仅 clone Rc，不复制 row/cell/run 或保父批次链。
// source 额度保留原结构票实际复制的 H/D/物理行开销，不能用已 trim 的投影范围低估它。
struct WindowRecord {
    window: VerifiedMarkdownTableWindow,
    retained: MarkdownTableViewportBudget,
}

// 此类型没有公开构造路径。地址身份只证明发布来自同一请求，源与上下文仍须分别核验。
struct Publication {
    source: MarkdownTableSourceIdentity,
    context_revision: u64,
    selection: Range<usize>,
}
impl VerifiedMarkdownTableGap {
    pub fn byte_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.utf16_range_for(document)?;
        Ok(&self.bytes)
    }
    pub fn utf16_range_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(&self.utf16)
    }
}

/// 窗口已经完成其真实inline上下文；gap仍是有独立权限的无表区间，不能因此隐藏普通正文。
/// 整批一次move给Native Rc后，cell只需批次handle+窗口下标，无需clone完整表或拆分权限。
pub struct VerifiedMarkdownTableViewport {
    source: MarkdownTableSourceIdentity,
    requested: Range<usize>,
    windows: Vec<Rc<WindowRecord>>,
    structures: Vec<Rc<MarkdownTableStructuralWindow>>,
    gaps: Vec<Rc<VerifiedMarkdownTableGap>>,
    coverage: Vec<MarkdownTableViewportCoverage>,
    budget: MarkdownTableViewportBudget,
    publication: Rc<Publication>,
    // 只保最终 payload 与本发布自身目录/句柄，不把已释放的 Parser/ref/underline scratch 带到续批。
    retained: MarkdownTableViewportBudget,
    required: Option<RequiredPublication>,
}
impl VerifiedMarkdownTableViewport {
    /// 结构出口只借原票；没有投影 String、Parser 图或默认 inline 样式。
    pub(in crate::markdown_document_buffer::table_window_request) fn structural_tables(
        &self,
    ) -> &[Rc<MarkdownTableStructuralWindow>] {
        &self.structures
    }
    fn check(&self, document: &MarkdownDocumentBuffer) -> Result<(), MarkdownTableRequestError> {
        self.source
            .is_current(document)
            .then_some(())
            .ok_or(MarkdownTableRequestError::SourceSuperseded)
    }
    pub fn windows_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<
        impl ExactSizeIterator<Item = &'a VerifiedMarkdownTableWindow> + DoubleEndedIterator + use<'a>,
        MarkdownTableRequestError,
    > {
        self.check(document)?;
        Ok(self.windows.iter().map(|record| &record.window))
    }
    pub fn window_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        index: usize,
    ) -> Result<Option<&VerifiedMarkdownTableWindow>, MarkdownTableRequestError> {
        self.check(document)?;
        Ok(self.windows.get(index).map(|record| &record.window))
    }
    pub fn gaps_for<'a>(
        &'a self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<
        impl ExactSizeIterator<Item = &'a VerifiedMarkdownTableGap> + DoubleEndedIterator + use<'a>,
        MarkdownTableRequestError,
    > {
        self.check(document)?;
        Ok(self.gaps.iter().map(Rc::as_ref))
    }
    pub fn gap_at_for(
        &self,
        document: &MarkdownDocumentBuffer,
        index: usize,
    ) -> Result<Option<&VerifiedMarkdownTableGap>, MarkdownTableRequestError> {
        self.check(document)?;
        Ok(self.gaps.get(index).map(Rc::as_ref))
    }
    pub fn coverage_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&[MarkdownTableViewportCoverage], MarkdownTableRequestError> {
        self.check(document)?;
        Ok(&self.coverage)
    }
    pub fn requested_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<&Range<usize>, MarkdownTableRequestError> {
        self.check(document)?;
        Ok(&self.requested)
    }
    pub fn budget(&self) -> MarkdownTableViewportBudget {
        self.budget
    }

    fn proves(&self, required: &Range<usize>) -> bool {
        if required.is_empty() {
            return true;
        }
        let mut tables = self
            .windows
            .iter()
            .flat_map(|record| record.window.coverage.iter().cloned())
            .peekable();
        let mut structures = self
            .structures
            .iter()
            .flat_map(|ticket| (0..2).filter_map(move |index| structure_range(ticket, index)))
            .peekable();
        let mut gaps = self.gaps.iter().map(|gap| gap.utf16.clone()).peekable();
        let mut covered = required.start;
        // 三个原目录按真实物理范围归并；结构票同样可证明覆盖，但省略 H context 或行间洞仍不能补齐。
        // 这里只复制固定 Range 标量，不分配目录、不读取正文，也不把 clipped requested 当整行来源。
        while let Some((_, kind)) = [
            tables.peek().map(|range| (range.start, 0)),
            structures.peek().map(|range| (range.start, 1)),
            gaps.peek().map(|range| (range.start, 2)),
        ]
        .into_iter()
        .flatten()
        .min()
        {
            let range = match kind {
                0 => tables.next().unwrap(),
                1 => structures.next().unwrap(),
                _ => gaps.next().unwrap(),
            };
            if range.end <= covered {
                continue;
            }
            if range.start > covered {
                return false;
            }
            covered = range.end;
            if covered >= required.end {
                return true;
            }
        }
        false
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownTableViewportStep {
    Progress { work: SemanticWork },
    Ready { work: SemanticWork },
}
#[derive(Clone, Copy)]
enum BatchPhase {
    Sweep,
    RequiredRow,
    Prepare,
    References,
    Project,
    Insert,
    PruneWindows,
    PruneStructures,
    PruneGaps,
    Coverage,
    Ready,
    Consumed,
}

/// 只有一个活结构Global；语义库存收齐本批query后才另做一次真实源序refs pass。
/// 后者仍可能到EOF，本请求不宣称零全源扫描，也不把有限Parser输入当逐token工作证明。
/// 两个Global各自最多4MiB活spine页，总预留上界8MiB；这不是一个共享4MiB库存。
/// 额外1MiB逻辑metadata准入包含有限工作区预留，不是allocator或整产品峰值测量。
pub struct MarkdownTableViewportRequest {
    source: MarkdownTableSourceIdentity,
    structure: Option<MarkdownTableWindowStructureRequest>,
    viewport: Range<usize>,
    context_revision: u64,
    selection: Range<usize>,
    phase: BatchPhase,
    jobs: Vec<Option<MarkdownTableWindowRequest>>,
    job_costs: Vec<MarkdownTableViewportBudget>,
    index: usize,
    queries: Option<MarkdownTableReferenceQueries>,
    windows: Vec<Rc<WindowRecord>>,
    source_structures: bool,
    structures: Vec<Rc<MarkdownTableStructuralWindow>>,
    gaps: Vec<Rc<VerifiedMarkdownTableGap>>,
    coverage: Vec<MarkdownTableViewportCoverage>,
    coverage_window: usize,
    coverage_range: usize,
    coverage_structure: usize,
    coverage_structure_range: usize,
    coverage_gap: usize,
    covered_until: usize,
    prune_index: usize,
    prune_keep: usize,
    between_start: (usize, usize),
    active_header: Option<usize>,
    // 同一前沿的真实物理行/分类边界；byte 与 UTF16 同源保存，绝不从 UTF16 猜 byte。
    resume_boundary: (usize, usize),
    budget: MarkdownTableViewportBudget,
    records: MarkdownTableViewportBudget,
    quota: super::prefixes::BatchQuota,
    total: SemanticWork,
    ready: Option<VerifiedMarkdownTableViewport>,
    required: Option<RequiredState>,
    pending_record: Option<Rc<WindowRecord>>,
    last_publication: Option<Weak<Publication>>,
    // 只有 source-aware owner 更新；结构 classified 水位不能替代实际 ordinary 来源消费。
    source_origin_floor: usize,
    failed: Option<MarkdownTableRequestError>,
}

impl MarkdownDocumentBuffer {
    pub fn table_viewport_request(
        &self,
        viewport: Range<usize>,
        context_revision: u64,
        active_selection: Range<usize>,
    ) -> Result<MarkdownTableViewportRequest, MarkdownTableRequestError> {
        if active_selection.start > active_selection.end || active_selection.end > self.utf16_len()
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut structure = self.table_window_structure_request(
            MarkdownTableWindowTarget::FirstTableInViewport,
            viewport.clone(),
        )?;
        structure.continuous = true;
        let quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        quota
            .admit_metadata(publication_metadata())
            .map_err(MarkdownTableRequestError::Window)?;
        Ok(MarkdownTableViewportRequest {
            source: structure.source.clone(),
            structure: Some(structure),
            viewport: viewport.clone(),
            context_revision,
            selection: active_selection,
            phase: BatchPhase::Sweep,
            jobs: Vec::new(),
            job_costs: Vec::new(),
            index: 0,
            queries: Some(MarkdownTableReferenceQueries::for_batch(Some(
                quota.clone(),
            ))),
            windows: Vec::new(),
            source_structures: false,
            structures: Vec::new(),
            gaps: Vec::new(),
            coverage: Vec::new(),
            coverage_window: 0,
            coverage_range: 0,
            coverage_structure: 0,
            coverage_structure_range: 0,
            coverage_gap: 0,
            covered_until: viewport.start,
            prune_index: 0,
            prune_keep: 0,
            between_start: (0, 0),
            active_header: None,
            resume_boundary: (0, 0),
            budget: MarkdownTableViewportBudget::default(),
            records: MarkdownTableViewportBudget::default(),
            quota,
            total: SemanticWork::default(),
            ready: None,
            required: None,
            pending_record: None,
            last_publication: None,
            source_origin_floor: 0,
            failed: None,
        })
    }
}

impl MarkdownTableViewportRequest {
    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
        budget: usize,
    ) -> Result<MarkdownTableViewportStep, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = if cancelled {
            Err(MarkdownTableRequestError::Cancelled)
        } else if !self.source.is_current(document) {
            Err(MarkdownTableRequestError::SourceSuperseded)
        } else if !(4096..=65536).contains(&budget) {
            Err(MarkdownTableRequestError::InvalidRequest)
        } else {
            self.advance_quantum(document, budget)
        };
        if let Err(error) = result {
            // 首因sticky，取消/失败同时撤销所有窗口、scratch及两个Global；不能继续发布部分overlay。
            self.failed = Some(error);
            self.structure = None;
            self.jobs = Vec::new();
            self.job_costs = Vec::new();
            self.queries = None;
            self.windows = Vec::new();
            self.structures = Vec::new();
            self.gaps = Vec::new();
            self.coverage = Vec::new();
            self.ready = None;
            self.required = None;
            self.pending_record = None;
            self.last_publication = None;
        }
        result
    }

    fn advance_quantum(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableViewportStep, MarkdownTableRequestError> {
        let mut work = SemanticWork::default();
        loop {
            // 单次quantum可以推进多个零source控制阶段；每阶段仍收费，不把async队列当公平性证明。
            if matches!(self.phase, BatchPhase::Ready) {
                return Ok(MarkdownTableViewportStep::Ready { work });
            }
            if matches!(self.phase, BatchPhase::Consumed) {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            if self
                .structure
                .as_ref()
                .is_some_and(|structure| structure.ordinary_pending.is_some())
            {
                // opt-in owner 必须先接收原普通块；本 quantum 不循环读取同一 pending 或推进 Global。
                return Ok(MarkdownTableViewportStep::Progress { work });
            }
            let used = work
                .source_bytes
                .max(work.structural_transitions)
                .max(work.copied_source_bytes)
                .max(work.utf8_validation_bytes);
            let remaining = budget.saturating_sub(used);
            // Gap 仅持 Range 与原 root Weak；一次裁剪/归还只付原固定64控制费，没有源借用、
            // spine 析构或外部回调。只让它消费本量子的真实余额，不为每个 Gap 重领 budget。
            // 进入 Coverage 或其它阶段立即恢复4096准入；Native原4096量子不会顺带推进它们。
            let minimum = if matches!(self.phase, BatchPhase::PruneGaps) {
                64
            } else {
                4096
            };
            if remaining < minimum {
                return Ok(MarkdownTableViewportStep::Progress { work });
            }
            let mut measured = self.advance_one(document, remaining)?;
            measured.structural_transitions = measured.structural_transitions.max(64);
            add_work(&mut work, measured)?;
            add_work(&mut self.total, measured)?;
            let structure = self
                .structure
                .as_ref()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            if self.total.source_bytes > structure.max_byte_work
                || self.total.structural_transitions > structure.max_transitions
            {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
        }
    }

    fn advance_one(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let mut work = SemanticWork {
            structural_transitions: 64,
            ..SemanticWork::default()
        };
        match self.phase {
            BatchPhase::Sweep => return self.advance_sweep(document, budget),
            BatchPhase::RequiredRow => return self.advance_required_row(document, budget),
            BatchPhase::Prepare => {
                if self.index == self.jobs.len() {
                    self.queries.as_mut().unwrap().seal_batch();
                    self.index = 0;
                    if self.queries.as_ref().unwrap().is_ready() {
                        self.phase = BatchPhase::Project;
                    } else {
                        // 所有有限query已封口；唯一refs Global从真实0起点按first-wins扫描，不能后来加标签。
                        let job = self.jobs[0].as_mut().unwrap();
                        job.queries = self.queries.take();
                        // 原resolver的唯一活动raw/prefix/Unicode-normalizer工作区预先保留上界。
                        // 这是可复用的一份工作区，不按每个query/每张表累乘，也不等于heap精确容量。
                        self.quota
                            .admit_metadata(
                                16 * 1024 * (2 * std::mem::size_of::<usize>() + 2)
                                    + 12 * (16 * 1024 + 1000),
                            )
                            .map_err(MarkdownTableRequestError::Window)?;
                        job.references = Some(new_global(document)?);
                        job.phase = Phase::References;
                        self.phase = BatchPhase::References;
                    }
                } else {
                    let job = self.jobs[self.index].as_mut().unwrap();
                    if matches!(job.phase, Phase::Queries) {
                        // 直接在同一库存分配前准入；不能先建第二张Map再合并拒绝。
                        let queries = self.queries.as_mut().unwrap();
                        work.parser_input_bytes = job.with_input(document, false, |input| {
                            collect_table_window_reference_queries(input, queries)
                        })?;
                        job.phase = Phase::AwaitReferences;
                        self.index += 1;
                    } else {
                        work = step_work(job.advance(document, false, budget)?);
                    }
                }
            }
            BatchPhase::References => {
                let job = self.jobs[0].as_mut().unwrap();
                work = step_work(job.advance(document, false, budget)?);
                if matches!(job.phase, Phase::Underline) {
                    self.queries = job.queries.take();
                    self.phase = BatchPhase::Project;
                    self.index = 0;
                }
            }
            BatchPhase::Project => {
                if self.index == self.jobs.len() {
                    self.jobs = Vec::new();
                    self.job_costs = Vec::new();
                    self.queries = None;
                    self.begin_prune();
                } else {
                    let job = self.jobs[self.index].as_mut().unwrap();
                    if matches!(job.phase, Phase::AwaitReferences) {
                        job.phase = Phase::Underline;
                    }
                    work = step_work(
                        job.advance_with_references(
                            document,
                            false,
                            budget,
                            self.queries.as_ref(),
                            self.structure
                                .as_mut()
                                .and_then(|structure| structure.global.as_mut()),
                        )?,
                    );
                    if matches!(job.phase, Phase::Ready) {
                        let window = job
                            .take_ready_for(document)?
                            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                        self.budget.runs = self
                            .budget
                            .runs
                            .checked_add(
                                window.presentation.runs.len()
                                    + window.presentation.header_context.runs.len(),
                            )
                            .ok_or(MarkdownTableRequestError::WorkLimit)?;
                        if self.budget.runs > MAX_RUNS {
                            return Err(MarkdownTableRequestError::Window(Error::SemanticLimit));
                        }
                        // 原输入额度来自 admit_ticket；最终保留量只计不可变数组，不携带已释放 scratch。
                        let retained = retained_window_budget(&window, self.job_costs[self.index])?;
                        self.records = add_budget(self.records, retained)?;
                        self.pending_record = Some(Rc::new(WindowRecord { window, retained }));
                        self.jobs[self.index] = None;
                        self.phase = BatchPhase::Insert;
                    }
                }
            }
            BatchPhase::Insert => {
                // 回放可能产生旧窗口之前的行。只移动有界Rc目录，绝不复制行/cell/run载荷。
                // 移交独立于Parser调用，避免在子阶段耗尽fuel后追加未计量的目录移动。
                let record = self
                    .pending_record
                    .take()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                let start = record
                    .window
                    .coverage
                    .first()
                    .map_or(usize::MAX, |range| range.start);
                let at = self.windows.partition_point(|existing| {
                    existing
                        .window
                        .coverage
                        .first()
                        .is_some_and(|range| range.start <= start)
                });
                work.structural_transitions += self.windows.len() * 2 + 1;
                self.windows.insert(at, record);
                self.index += 1;
                self.phase = BatchPhase::Project;
            }
            BatchPhase::PruneWindows => {
                if self.prune_index < self.windows.len() {
                    let record = &self.windows[self.prune_index];
                    // 单窗 coverage 只有 H/D 和真实 body 两段，不能按包含离屏 gap 的凸包保留。
                    if record
                        .window
                        .coverage
                        .iter()
                        .any(|range| intersects(range, &self.viewport))
                        || self
                            .required
                            .as_ref()
                            .is_some_and(|required| required.keeps(record))
                    {
                        self.records = add_budget(self.records, record.retained)?;
                        self.windows.swap(self.prune_keep, self.prune_index);
                        self.prune_keep += 1;
                    }
                    self.prune_index += 1;
                } else if self.windows.len() > self.prune_keep {
                    // 每步只释放一份未被本 publication 使用的记录；原 previous 句柄仍可继续读。
                    // 只有实际销毁带 End 的最后一个记录才支付有限 spine 清理费用；普通共享句柄只走固定控制费。
                    let last = self.windows.last().unwrap();
                    if Rc::strong_count(last) == 1 && last.window.table_end.is_some() {
                        work.structural_transitions = PRUNE_LAST_SPINE_COST;
                    }
                    drop(self.windows.pop());
                } else {
                    self.prune_index = 0;
                    self.prune_keep = 0;
                    self.phase = BatchPhase::PruneStructures;
                }
            }
            BatchPhase::PruneStructures => {
                if self.prune_index < self.structures.len() {
                    let ticket = &self.structures[self.prune_index];
                    if (0..2)
                        .filter_map(|index| structure_range(ticket, index))
                        .any(|range| intersects(&range, &self.viewport))
                        || self
                            .required
                            .as_ref()
                            .is_some_and(|required| required.keeps_structure(ticket))
                    {
                        self.records = add_budget(self.records, structural_budget(ticket))?;
                        self.structures.swap(self.prune_keep, self.prune_index);
                        self.prune_keep += 1;
                    }
                    self.prune_index += 1;
                } else if self.structures.len() > self.prune_keep {
                    work.structural_transitions = PRUNE_LAST_SPINE_COST;
                    drop(self.structures.pop());
                } else {
                    self.prune_index = 0;
                    self.prune_keep = 0;
                    self.phase = BatchPhase::PruneGaps;
                }
            }
            BatchPhase::PruneGaps => {
                if self.prune_index < self.gaps.len() {
                    if intersects(&self.gaps[self.prune_index].utf16, &self.viewport) {
                        self.records.metadata_bytes = self
                            .records
                            .metadata_bytes
                            .checked_add(gap_metadata())
                            .ok_or(MarkdownTableRequestError::WorkLimit)?;
                        self.gaps.swap(self.prune_keep, self.prune_index);
                        self.prune_keep += 1;
                    }
                    self.prune_index += 1;
                } else if self.gaps.len() > self.prune_keep {
                    drop(self.gaps.pop());
                } else {
                    self.phase = BatchPhase::Coverage;
                }
            }
            BatchPhase::Coverage => {
                // required结果只扫描当前有限句柄目录；把索引解析的上界单列进本层控制fuel。
                work.structural_transitions += (self.windows.len() + self.structures.len()) * 2;
                self.advance_coverage(document)?;
            }
            BatchPhase::Ready | BatchPhase::Consumed => {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
        }
        self.budget.metadata_bytes = self.quota.metadata_used();
        Ok(work)
    }

    fn advance_sweep(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let classified = self.structure.as_ref().and_then(|structure| {
            (structure.capture_ordinary
                && structure.ordinary_pending.is_none()
                && structure.pending.is_none()
                && structure.coordinates.is_none()
                && self.active_header.is_none())
            .then(|| {
                structure
                    .global
                    .as_ref()
                    .map(GlobalFrontier::table_classified_position)
            })
            .flatten()
        });
        if let Some(boundary) = classified.filter(|boundary| {
            boundary.1 >= self.viewport.end && self.source_origin_floor >= self.viewport.end
        }) {
            // 来源 owner 已消费本视口所需 ordinary 行后，结构分类水位可以独立封 gap。
            // 该水位不代表 origin cursor 已到段尾；未完游标仍由 opt-in owner 独占保留。
            self.push_gap(document, boundary)?;
            self.resume_boundary = boundary;
            self.phase = if self.required.is_some() {
                BatchPhase::RequiredRow
            } else {
                BatchPhase::Prepare
            };
            self.index = 0;
            return Ok(SemanticWork {
                structural_transitions: SWEEP_CONTROL_BASE,
                ..SemanticWork::default()
            });
        }
        let structure = self.structure.as_mut().unwrap();
        // 已封窗口仍占全批预算，下一窗不能重新获得256行/1024格的额度。
        let available_rows = TABLE_WINDOW_MAX_ROWS.saturating_sub(self.budget.row_slots + 2);
        let available_cells_as_rows = structure.current.as_ref().map_or(usize::MAX, |table| {
            TABLE_WINDOW_MAX_CELLS
                .saturating_sub(self.budget.cells)
                .checked_div(table.header.columns)
                .unwrap_or(0)
                .saturating_sub(1)
        });
        structure.continuous_body_limit = Some(available_rows.min(available_cells_as_rows));
        if structure.capture_ordinary
            && available_rows != 0
            && available_cells_as_rows == 0
            && matches!(structure.pending.as_ref(), Some(GlobalFact::Row(row))
                if row.utf16_start < self.viewport.end && self.viewport.start < row.utf16_end)
        {
            // source-aware 联合库存确实是 cells 耗尽，而不是 rows 槽耗尽；
            // 在原子层把 cell 派生的零上限统一标成 RowLimit 之前给出准确首因，原 table-only 行为不变。
            return Err(MarkdownTableRequestError::Window(Error::CellLimit));
        }
        let ending = match structure.pending.as_ref() {
            Some(GlobalFact::TableEnd(end)) => {
                Some((end.header_byte_start(), end.byte_end(), end.utf16_end()))
            }
            _ => None,
        };
        // 固定预留覆盖最坏收尾；真正消费量在下方按实际执行的控制和body成员计算。
        // 不能把每次NeedInput或零source步骤都当作已经耗尽这份预留。
        let step = structure.advance(document, false, budget - SWEEP_CONTROL_RESERVE)?;
        let mut control_work = SWEEP_CONTROL_BASE;
        let measured = match step {
            MarkdownTableStructureStep::Progress { work }
            | MarkdownTableStructureStep::StructureReady { work }
            | MarkdownTableStructureStep::NotTable { work }
            | MarkdownTableStructureStep::TableEndReached { work } => work,
        };
        if structure.ordinary_pending.is_some() {
            // 原 Fact 先移交 source-aware owner；不能在同一尾部用分类水位跳过其实际来源处理。
            return Ok(SemanticWork {
                source_bytes: measured.byte_reads,
                structural_transitions: measured.transitions + control_work,
                source_tree_steps: measured.source_tree_steps,
                ..SemanticWork::default()
            });
        }
        if let Some((header, byte, utf16)) = ending {
            if self.active_header != Some(header) {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            self.between_start = (byte, utf16);
            self.active_header = None;
        }
        let header = structure
            .current
            .as_ref()
            .map(|table| (table.header.header_byte_start, table.header_utf16.start));
        if let Some((byte, utf16)) = header
            && self.active_header != Some(byte)
        {
            self.push_gap(document, (byte, utf16))?;
            self.active_header = Some(byte);
        }
        let structure = self.structure.as_mut().unwrap();
        // 目标已经完整落在已证实成员中即可封窗；不为寻找一个无关的下条巨行先扫描整行。
        let reached = structure.ready.is_none()
            && structure.current.as_ref().is_some_and(|table| {
                table.found
                    && (table.include_header && table.delimiter_utf16.end >= self.viewport.end
                        || table
                            .body
                            .last()
                            .is_some_and(|row| row.utf16_end >= self.viewport.end))
            });
        if reached {
            structure.finish(None, None)?;
        }
        let ticket = structure.ready.take();
        let mut done = reached;
        if let Some(ticket) = ticket {
            control_work += ticket.body.len() * SWEEP_ROW_ADMISSION_COST;
            // 原单窗至多255个body，128+3*255=893≤1024；真实batch还把D计入总行槽。
            // 在两次输入相关fold及移交之前验证，不能超支后靠缩小报数继续。
            if control_work > SWEEP_CONTROL_RESERVE {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            self.resume_boundary = ticket.body.last().map_or(
                (ticket.header.separator_byte_end, ticket.delimiter_utf16.end),
                |row| (row.byte_end, row.utf16_end),
            );
            // 未消费行必须先由同一前沿继续；仅在本窗真覆盖目标末端时才能停止整个sweep。
            if matches!(structure.pending.as_ref(), Some(GlobalFact::Row(row)) if row.utf16_start >= self.viewport.end)
            {
                done = true;
            }
            let cost = self.admit_ticket(&ticket)?;
            if self.source_structures {
                self.structures.reserve_exact(1);
                self.structures.push(Rc::new(ticket)); // 原行票直接移交，巨行不进入 LineCopy 或完整 table Parser。
            } else {
                self.job_costs.push(cost);
                self.jobs.push(Some(self.semantic_job(ticket)));
            }
        }
        let structure = self.structure.as_ref().unwrap();
        let outside_header = structure
            .coordinates
            .as_ref()
            .filter(|coordinates| coordinates.boundary_index > 0)
            .map(|coordinates| (coordinates.header.header_byte_start, coordinates.values[0]));
        if let Some(boundary) = outside_header.filter(|(_, units)| *units >= self.viewport.end) {
            self.push_gap(document, boundary)?;
            self.resume_boundary = boundary;
            done = true;
        } else if self.active_header.is_none()
            && structure.pending.is_none()
            && structure.coordinates.is_none()
        {
            let boundary = structure
                .global
                .as_ref()
                .unwrap()
                .table_classified_position();
            if boundary.1 >= self.viewport.end
                && (!structure.capture_ordinary
                    || self.source_origin_floor >= self.viewport.end
                    || structure.no_table)
            {
                self.push_gap(document, boundary)?;
                self.resume_boundary = boundary;
                done = true;
            }
        }
        if done {
            self.phase = if self.required.is_some() {
                BatchPhase::RequiredRow
            } else {
                BatchPhase::Prepare
            };
            self.index = 0;
        }
        Ok(SemanticWork {
            source_bytes: measured.byte_reads,
            structural_transitions: measured.transitions + control_work,
            source_tree_steps: measured.source_tree_steps,
            ..SemanticWork::default()
        })
    }

    fn push_gap(
        &mut self,
        document: &MarkdownDocumentBuffer,
        end: (usize, usize),
    ) -> Result<(), MarkdownTableRequestError> {
        let start = self.between_start;
        if end.0 < start.0 || end.1 < start.1 {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if start.1 < end.1 && start.1 < self.viewport.end && self.viewport.start < end.1 {
            let size = gap_metadata();
            self.quota
                .admit_metadata(size + std::mem::size_of::<Rc<VerifiedMarkdownTableGap>>())
                .map_err(MarkdownTableRequestError::Window)?;
            self.records.metadata_bytes = self
                .records
                .metadata_bytes
                .checked_add(size)
                .ok_or(MarkdownTableRequestError::WorkLimit)?;
            self.gaps.push(Rc::new(VerifiedMarkdownTableGap {
                source: self.source.clone(),
                bytes: start.0..end.0,
                utf16: start.1..end.1,
            }));
        }
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(())
    }

    fn admit_ticket(
        &mut self,
        ticket: &MarkdownTableStructuralWindow,
    ) -> Result<MarkdownTableViewportBudget, MarkdownTableRequestError> {
        let columns = ticket.header.columns;
        let rows = ticket
            .body
            .len()
            .checked_add(2)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let cells = (ticket.body.len() + 1)
            .checked_mul(columns)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let bytes = if self.source_structures {
            0
        } else {
            ticket
                .body
                .iter()
                .try_fold(
                    ticket.header.separator_byte_end - ticket.header.header_byte_start,
                    |n, row| n.checked_add(row.byte_end - row.byte_start),
                )
                .ok_or(MarkdownTableRequestError::WorkLimit)?
        };
        let units = if self.source_structures {
            0
        } else {
            ticket
                .body
                .iter()
                .try_fold(
                    ticket.delimiter_utf16.end - ticket.header_utf16.start,
                    |n, row| n.checked_add(row.utf16_end - row.utf16_start),
                )
                .ok_or(MarkdownTableRequestError::WorkLimit)?
        };
        let cost = MarkdownTableViewportBudget {
            row_slots: rows,
            cells,
            column_slots: columns,
            source_bytes: bytes,
            source_utf16: units,
            ..MarkdownTableViewportBudget::default()
        };
        let next = add_budget(self.budget, cost)?;
        if next.row_slots > TABLE_WINDOW_MAX_ROWS {
            return Err(MarkdownTableRequestError::Window(Error::RowLimit));
        }
        if next.cells > TABLE_WINDOW_MAX_CELLS {
            return Err(MarkdownTableRequestError::Window(Error::CellLimit));
        }
        if columns > TABLE_WINDOW_MAX_COLUMNS || next.column_slots > MAX_COLUMN_SLOTS {
            return Err(MarkdownTableRequestError::Window(Error::ColumnLimit));
        }
        if next.source_bytes > TABLE_WINDOW_MAX_BYTES || next.source_utf16 > TABLE_WINDOW_MAX_UTF16
        {
            return Err(MarkdownTableRequestError::Window(Error::SourceLimit));
        }
        // 逻辑结构与最终cell/row/origin的保留容量先准入；source Strings随后由LineCopy逐行创建。
        let metadata = if self.source_structures {
            structural_budget(ticket).metadata_bytes
                + std::mem::size_of::<Rc<MarkdownTableStructuralWindow>>()
        } else {
            rows
            * (std::mem::size_of::<TableRowFact>()
                + std::mem::size_of::<VerifiedTableInlineRow>()
                + std::mem::size_of::<
                    crate::markdown_viewport_presentation::table_window::MarkdownTableWindowRow,
                >()
                + std::mem::size_of::<
                    crate::markdown_viewport_presentation::table_window::MarkdownTableOrigin,
                >())
            + cells
                * std::mem::size_of::<
                    crate::markdown_viewport_presentation::table_window::MarkdownTableWindowCell,
                >()
            + std::mem::size_of::<MarkdownTableStructuralWindow>()
            + std::mem::size_of::<WindowRecord>()
            + 2 * std::mem::size_of::<usize>()
            + std::mem::size_of::<Rc<WindowRecord>>()
            + std::mem::size_of::<MarkdownTableViewportBudget>()
        };
        self.quota
            .admit_metadata(metadata)
            .map_err(MarkdownTableRequestError::Window)?;
        self.budget = next;
        Ok(cost)
    }

    fn semantic_job(&self, ticket: MarkdownTableStructuralWindow) -> MarkdownTableWindowRequest {
        MarkdownTableWindowRequest {
            source: self.source.clone(),
            batch: true,
            quota: self.quota.clone(),
            structure: None,
            ticket: Some(ticket),
            scope_cursor: None,
            scope: ScopeProjection::with_quota(self.quota.clone()),
            copier: None,
            lines: Vec::new(),
            prefixes: None,
            prefix_runs: Vec::new(),
            queries: None,
            references: None,
            reference_fact: None,
            pending_reference: None,
            underline: None,
            ready: None,
            required_end: None,
            context_revision: self.context_revision,
            selection: self.selection.clone(),
            phase: Phase::Scope,
            failed: None,
        }
    }

    fn advance_coverage(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<(), MarkdownTableRequestError> {
        // 两个已按源序建立的有限流逐项归并，不排序/凸包化，也不在一个低fuel step遍历整批。
        while self.coverage_window < self.windows.len()
            && self.coverage_range == self.windows[self.coverage_window].window.coverage.len()
        {
            self.coverage_window += 1;
            self.coverage_range = 0;
        }
        while self.coverage_structure < self.structures.len()
            && structure_range(
                &self.structures[self.coverage_structure],
                self.coverage_structure_range,
            )
            .is_none()
        {
            self.coverage_structure += 1;
            self.coverage_structure_range = 0;
        }
        let table = if self.source_structures {
            self.structures
                .get(self.coverage_structure)
                .and_then(|ticket| structure_range(ticket, self.coverage_structure_range))
        } else {
            self.windows
                .get(self.coverage_window)
                .and_then(|record| record.window.coverage.get(self.coverage_range))
                .cloned()
        };
        let gap = self
            .gaps
            .get(self.coverage_gap)
            .map(|gap| gap.utf16.clone());
        let take_table = match (&table, &gap) {
            (Some(a), Some(b)) => a.start <= b.start,
            (Some(_), None) => true,
            _ => false,
        };
        let (range, part) = if take_table {
            let range = table.unwrap();
            if self.source_structures {
                self.coverage_structure_range += 1;
            } else {
                self.coverage_range += 1;
            }
            (
                range.clone(),
                if self.source_structures {
                    MarkdownTableViewportCoverage::Structure {
                        table_index: self.coverage_structure,
                        utf16_range: range,
                    }
                } else {
                    MarkdownTableViewportCoverage::Table {
                        window_index: self.coverage_window,
                        utf16_range: range,
                    }
                },
            )
        } else if let Some(range) = gap {
            self.coverage_gap += 1;
            (
                range.clone(),
                MarkdownTableViewportCoverage::Gap {
                    gap_index: self.coverage_gap - 1,
                    utf16_range: range,
                },
            )
        } else {
            if self.covered_until != self.viewport.end {
                return Err(MarkdownTableRequestError::Window(Error::ContextPending));
            }
            self.budget.metadata_bytes = self.quota.metadata_used();
            // 先计算同一不可变发布真实保留的逻辑量，再 mint 不可伪造的本次 publication。
            // 不保父发布 Rc；旧 Native 句柄可继续读，后继只有平坦记录 Rc 与自己的覆盖目录。
            let retained = add_budget(
                self.records,
                MarkdownTableViewportBudget {
                    metadata_bytes: publication_metadata()
                        + self.coverage.len()
                            * std::mem::size_of::<MarkdownTableViewportCoverage>()
                        + self.windows.len() * std::mem::size_of::<Rc<WindowRecord>>()
                        + self.structures.len()
                            * std::mem::size_of::<Rc<MarkdownTableStructuralWindow>>()
                        + self.gaps.len() * std::mem::size_of::<Rc<VerifiedMarkdownTableGap>>(),
                    ..MarkdownTableViewportBudget::default()
                },
            )?;
            let publication = Rc::new(Publication {
                source: self.source.clone(),
                context_revision: self.context_revision,
                selection: self.selection.clone(),
            });
            self.last_publication = Some(Rc::downgrade(&publication));
            let required = self.finish_required_publication()?;
            self.ready = Some(VerifiedMarkdownTableViewport {
                source: self.source.clone(),
                requested: self.viewport.clone(),
                windows: std::mem::take(&mut self.windows),
                structures: std::mem::take(&mut self.structures),
                gaps: std::mem::take(&mut self.gaps),
                coverage: std::mem::take(&mut self.coverage),
                budget: self.budget,
                publication,
                retained,
                required,
            });
            self.phase = BatchPhase::Ready;
            return Ok(());
        };
        let clipped = range.start.max(self.viewport.start)..range.end.min(self.viewport.end);
        if clipped.is_empty() {
            return Ok(());
        }
        if clipped.start != self.covered_until {
            return Err(MarkdownTableRequestError::Window(Error::ProjectionBoundary));
        }
        self.quota
            .admit_metadata(std::mem::size_of::<MarkdownTableViewportCoverage>())
            .map_err(MarkdownTableRequestError::Window)?;
        self.covered_until = clipped.end;
        self.coverage.push(match part {
            MarkdownTableViewportCoverage::Table { window_index, .. } => {
                MarkdownTableViewportCoverage::Table {
                    window_index,
                    utf16_range: clipped,
                }
            }
            MarkdownTableViewportCoverage::Gap { gap_index, .. } => {
                MarkdownTableViewportCoverage::Gap {
                    gap_index,
                    utf16_range: clipped,
                }
            }
            MarkdownTableViewportCoverage::Structure { table_index, .. } => {
                MarkdownTableViewportCoverage::Structure {
                    table_index,
                    utf16_range: clipped,
                }
            }
        });
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(())
    }

    pub fn ready_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&VerifiedMarkdownTableViewport>, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        Ok(self.ready.as_ref())
    }
    pub fn take_ready_for(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<VerifiedMarkdownTableViewport>, MarkdownTableRequestError> {
        self.ready_for(document)?;
        let ready = self.ready.take();
        if ready.is_some() {
            self.phase = BatchPhase::Consumed;
        }
        Ok(ready)
    }
    pub fn forward_resume_floor(&self) -> usize {
        self.resume_boundary.1
    }
    pub fn total_work(&self) -> SemanticWork {
        self.total
    }

    /// 新选择只重投影原来已经闭合的无表区间；不倒退唯一 Global，也不恢复历史 Fact。
    /// 调用者先核同一 SourceRequest 的完整选择及最新发布；这里继续核本批自己的发布身份。
    pub(in crate::markdown_document_buffer::table_window_request) fn reproject_source_selection(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: Range<usize>,
        context_revision: u64,
        selection: Range<usize>,
    ) -> Result<bool, MarkdownTableRequestError> {
        previous.check(document)?;
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if !matches!(self.phase, BatchPhase::Consumed)
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
            || self.context_revision != previous.publication.context_revision
            || self.selection != previous.publication.selection
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        // 这里只重用无表结构权限。旧 table window 中的 marker/inline 选择投影必须另做，
        // 不能把整个 classified 凸包当作可重放库存，也不为旧表开第二个语义 Global。
        if !previous.windows.is_empty() || !previous.structures.is_empty() {
            return Ok(false);
        }
        let Some((index, gap)) =
            previous.gaps.iter().enumerate().find(|(_, gap)| {
                gap.utf16.start <= viewport.start && viewport.end <= gap.utf16.end
            })
        else {
            return Ok(false);
        };
        if self
            .structure
            .as_ref()
            .and_then(|structure| structure.global.as_ref())
            .is_none_or(GlobalFrontier::semantic_boundary_retained)
        {
            return Ok(false); // 未归还语义前探义务不能因选择变化而丢掉。
        }
        // 先联合准入仍活的旧发布和本次新目录，再改变唯一 owner。共享 Gap 只复制 Rc，
        // 其 Source/byte/UTF16 事实保持原值；新选择不能扩张它的真实无表覆盖。
        let quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        quota
            .admit_retained(previous.retained.runs, previous.retained.metadata_bytes)
            .and_then(|()| {
                quota.admit_metadata(
                    publication_metadata() + std::mem::size_of::<Rc<VerifiedMarkdownTableGap>>(),
                )
            })
            .map_err(MarkdownTableRequestError::Window)?;
        let gap = Rc::clone(gap);
        self.viewport = viewport.clone();
        self.context_revision = context_revision;
        self.selection = selection;
        self.gaps = vec![gap];
        self.coverage = Vec::new();
        self.coverage_window = 0;
        self.coverage_range = 0;
        self.coverage_structure = 0;
        self.coverage_structure_range = 0;
        self.coverage_gap = 0;
        self.covered_until = viewport.start;
        self.records = MarkdownTableViewportBudget {
            metadata_bytes: gap_metadata(),
            ..Default::default()
        };
        self.budget = MarkdownTableViewportBudget {
            metadata_bytes: quota.metadata_used(),
            ..Default::default()
        };
        self.quota = quota;
        self.required = None;
        self.last_publication = None;
        // 结构与 refs 不需要重新扫描：本次只含原无表 Gap。Coverage 仍逐步裁剪，
        // 只有它正常完成后才能生成本批新 publication；原累计 fuel 与 Global 前沿不变。
        self.phase = BatchPhase::Coverage;
        self.account_source_work(SemanticWork {
            structural_transitions: SWEEP_CONTROL_BASE + index + 1,
            ..Default::default()
        })?;
        Ok(true)
    }

    /// 原来源发布被新目标接管后重放唯一Global；旧窗口只预扣同活库存，不复制内容或重发扫描fuel。
    pub(in crate::markdown_document_buffer::table_window_request) fn reposition_source(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        previous.check(document)?;
        if !matches!(self.phase, BatchPhase::Consumed)
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        quota
            .admit_retained(previous.retained.runs, previous.retained.metadata_bytes)
            .and_then(|()| quota.admit_metadata(publication_metadata()))
            .map_err(MarkdownTableRequestError::Window)?;
        let structure = self.structure.as_mut().expect("来源发布保留唯一结构续体");
        // 未作任何新源借用，仍先保真实mmap首错；相同revision不能替代原DocRoot健康。
        document
            .byte_offset_for_utf16(0)
            .map_err(MarkdownTableRequestError::Source)?;
        let work = structure
            .global
            .as_mut()
            .expect("continuous来源保留原Global")
            .restart_from_root(
                GlobalSourceAuthority {
                    current_revision: document.generation(),
                    cancelled: false,
                },
                4096,
            )
            .map_err(MarkdownTableRequestError::Global)?;
        structure.total.transitions = structure
            .total
            .transitions
            .checked_add(work.transitions)
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        // 这些未发布派生值只属于旧目标；根重放会重新给出真实表头与ordinary Fact。
        structure.viewport = viewport.clone();
        structure.target = MarkdownTableWindowTarget::FirstTableInViewport;
        structure.continuous_body_limit = None;
        structure.pending = None;
        structure.ordinary_pending = None;
        structure.coordinates = None;
        structure.current = None;
        structure.ready = None;
        structure.no_table = false;
        structure.required_row_end = None;
        self.jobs = Vec::new();
        self.job_costs = Vec::new();
        self.windows = Vec::new();
        self.structures = Vec::new();
        self.gaps = Vec::new();
        self.coverage = Vec::new();
        self.index = 0;
        self.coverage_window = 0;
        self.coverage_range = 0;
        self.coverage_structure = 0;
        self.coverage_structure_range = 0;
        self.coverage_gap = 0;
        self.covered_until = viewport.start;
        self.prune_index = 0;
        self.prune_keep = 0;
        self.between_start = (0, 0);
        self.active_header = None;
        self.resume_boundary = (0, 0);
        self.source_origin_floor = 0;
        self.ready = None;
        self.required = None;
        self.pending_record = None;
        self.last_publication = None;
        self.records = MarkdownTableViewportBudget::default();
        self.budget = MarkdownTableViewportBudget {
            metadata_bytes: quota.metadata_used(),
            ..Default::default()
        };
        self.queries = Some(MarkdownTableReferenceQueries::for_batch(Some(
            quota.clone(),
        )));
        self.quota = quota;
        self.viewport = viewport;
        self.phase = BatchPhase::Sweep;
        self.account_source_work(SemanticWork {
            structural_transitions: work.transitions + SWEEP_CONTROL_RESERVE,
            ..Default::default()
        })
    }

    /// 只有同模块的 opt-in 包装入口开启截留；原 table-only factory 与 advance 语义不变。
    pub(in crate::markdown_document_buffer::table_window_request) fn enable_source_capture(
        &mut self,
    ) {
        self.structure.as_mut().unwrap().capture_ordinary = true;
    }

    /// 原生分页只需要结构行；单格语义另沿同 Global 完成，不复制整行后才交出第一帧来源。
    pub(in crate::markdown_document_buffer::table_window_request) fn enable_structural_tables(
        &mut self,
    ) {
        self.source_structures = true;
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn set_source_origin_floor(
        &mut self,
        floor: usize,
    ) {
        self.source_origin_floor = floor;
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn take_ordinary_source(
        &mut self,
    ) -> Option<CapturedOrdinary> {
        self.structure.as_mut()?.ordinary_pending.take()
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn source_identity(
        &self,
    ) -> MarkdownTableSourceIdentity {
        self.source.clone()
    }

    #[cfg(test)]
    pub(in crate::markdown_document_buffer::table_window_request) fn source_probe_state(
        &self,
    ) -> (usize, usize, usize) {
        let global = self
            .structure
            .as_ref()
            .and_then(|structure| structure.global.as_ref());
        (
            global.map_or(0, |global| std::ptr::from_ref(global).addr()),
            global.map_or(0, GlobalFrontier::next_byte),
            self.quota.metadata_used(),
        )
    }

    /// 只短借原结构Global；语义前探在返回Ready前恢复它，不产生另一个源或初始化图。
    pub(in crate::markdown_document_buffer::table_window_request) fn source_global_mut(
        &mut self,
    ) -> Result<&mut GlobalFrontier, MarkdownTableRequestError> {
        self.structure
            .as_mut()
            .and_then(|structure| structure.global.as_mut())
            .ok_or(MarkdownTableRequestError::InvalidRequest)
    }

    /// 来源内的串行语义 job 取得原批次同一份额度；不重建 quota、不退款或重发库存。
    pub(in crate::markdown_document_buffer::table_window_request) fn source_semantic_quota(
        &self,
    ) -> Result<MarkdownTableWindowQuota, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            // 失败后的测试同样保留真实首因，不能复制额度后绕过原 owner 的撤权。
            return Err(error);
        }
        self.source_limits()?; // 原结构续体必须仍在；失去它的请求不能借测试入口重建库存。
        Ok(self.quota.clone())
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn source_limits(
        &self,
    ) -> Result<(usize, usize), MarkdownTableRequestError> {
        let structure = self
            .structure
            .as_ref()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        Ok((structure.max_byte_work, structure.max_transitions))
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn admit_source_metadata(
        &mut self,
        bytes: usize,
    ) -> Result<(), MarkdownTableRequestError> {
        self.quota
            .admit_metadata(bytes)
            .map_err(MarkdownTableRequestError::Window)?;
        self.budget.metadata_bytes = self.quota.metadata_used();
        Ok(())
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn admit_source_runs(
        &mut self,
        runs: usize,
    ) -> Result<(), MarkdownTableRequestError> {
        // Plain与table使用同一实际run库存；固定Plain scratch已由source工作区预扣，不能重发run额度。
        self.quota
            .admit_retained(runs, 0)
            .map_err(MarkdownTableRequestError::Window)
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn reserve_source_underline_workspace(
        &mut self,
    ) -> Result<(), MarkdownTableRequestError> {
        // 复用原唯一underline容量预留；table工作区已经释放后Plain只串行接管这一份库存。
        self.quota
            .reserve_underline_workspace()
            .map_err(MarkdownTableRequestError::Window)?;
        self.budget.metadata_bytes = self.quota.metadata_used();
        Ok(())
    }

    /// ordinary 重读与坐标转换进入同一个 batch 总账；子 cursor 自己的限额不能重发 owner fuel。
    pub(in crate::markdown_document_buffer::table_window_request) fn account_source_work(
        &mut self,
        work: SemanticWork,
    ) -> Result<(), MarkdownTableRequestError> {
        add_work(&mut self.total, work)?;
        let (bytes, transitions) = self.source_limits()?;
        if self.total.source_bytes > bytes || self.total.structural_transitions > transitions {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        Ok(())
    }

    pub(in crate::markdown_document_buffer::table_window_request) fn abort_source_request(
        &mut self,
        error: MarkdownTableRequestError,
    ) {
        // 只释放本请求新派生容量；外带不可变 publication 记录仍可按原源身份读取。
        self.failed = Some(error);
        self.structure = None;
        self.jobs = Vec::new();
        self.job_costs = Vec::new();
        self.queries = None;
        self.windows = Vec::new();
        self.structures = Vec::new();
        self.gaps = Vec::new();
        self.coverage = Vec::new();
        self.ready = None;
        self.required = None;
        self.pending_record = None;
        self.last_publication = None;
    }

    /// source-aware 跳向结构水位之后时仍与真实 previous 同活联合准入；
    /// 原 table-only resume_forward 的独立批次语义保持不变。
    pub(in crate::markdown_document_buffer::table_window_request) fn resume_source_from_ready(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        previous.check(document)?;
        if !self.source.is_current(document)
            || !matches!(self.phase, BatchPhase::Consumed)
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
            || self.context_revision != previous.publication.context_revision
            || self.selection != previous.publication.selection
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let mut records = previous.windows.iter().try_fold(
            MarkdownTableViewportBudget {
                metadata_bytes: previous.gaps.len() * gap_metadata(),
                ..MarkdownTableViewportBudget::default()
            },
            |sum, record| add_budget(sum, record.retained),
        )?;
        for ticket in &previous.structures {
            records = add_budget(records, structural_budget(ticket))?;
        }
        let quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        quota
            .admit_retained(previous.retained.runs, previous.retained.metadata_bytes)
            .map_err(MarkdownTableRequestError::Window)?;
        quota
            .admit_metadata(
                publication_metadata()
                    + previous.windows.len() * std::mem::size_of::<Rc<WindowRecord>>()
                    + previous.structures.len()
                        * std::mem::size_of::<Rc<MarkdownTableStructuralWindow>>()
                    + previous.gaps.len() * std::mem::size_of::<Rc<VerifiedMarkdownTableGap>>(),
            )
            .map_err(MarkdownTableRequestError::Window)?;
        // 同一真实 Global 继续；只有完成上述来源/发布和库存 preflight 后才改变原请求。
        self.resume_forward(
            document,
            viewport,
            self.context_revision,
            self.selection.clone(),
        )?;
        self.windows = previous.windows.clone();
        self.structures = previous.structures.clone();
        self.gaps = previous.gaps.clone();
        self.budget = MarkdownTableViewportBudget {
            metadata_bytes: quota.metadata_used(),
            ..records
        };
        self.records = records;
        self.queries = Some(MarkdownTableReferenceQueries::for_batch(Some(
            quota.clone(),
        )));
        self.quota = quota;
        Ok(())
    }

    fn begin_prune(&mut self) {
        // 不退本次 quota/budget；仅重算即将发布的保留目录。下次扩展才使用这份真实 retained 额度。
        self.records = MarkdownTableViewportBudget::default();
        self.prune_index = 0;
        self.prune_keep = 0;
        // 空目录没有句柄、spine 或行载荷可归还；按这次真实库存选择第一个有工作的阶段，
        // 避免普通 Gap 小窗为两个空表目录各自交还一次量子。非空目录仍逐项裁剪、计费，
        // Gap 及 Coverage 始终走原流程，不从目录为空推导任何正文样式或新的无表覆盖。
        self.phase = if !self.windows.is_empty() {
            BatchPhase::PruneWindows
        } else if !self.structures.is_empty() {
            BatchPhase::PruneStructures
        } else {
            BatchPhase::PruneGaps
        };
    }

    /// 在本请求最后一份 Ready 上共享已验证窗口，只从仍活的原 Global/Math 续体读取右侧后缀。
    /// 前置权限、覆盖或额度拒绝均不改变旧发布或请求；开始推进之后失败/取消仍为 sticky。
    /// 旧发布可以继续读取，但不能用其凸包补未知洞，也不能更新 selection/context 后复用旧样式。
    /// 调用方须保持唯一发布槽，旧同步 Rc 借用结束后再推进后继批；外部历史句柄不归此配额追踪。
    /// selection/context 是本请求的原始身份，不会由 SourceIdentity 自动感知同代次的外部选择改变。
    /// Native 仍须守原完整选择、输入与 context 票，不能只凭 source 仍 current 就发布旧样式。
    pub fn extend_from_ready(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        self.check_extension(document, previous, &viewport)?;
        // 元数据只按仍同活的不可变载荷预扣，不恢复已经释放的旧 refs/underline 工作区。
        // 后缀收齐自身新 query 后仍需从源 0 做 first-wins 扫描；这里没有全局 refs 索引。
        let mut records = previous.windows.iter().try_fold(
            MarkdownTableViewportBudget {
                metadata_bytes: previous.gaps.len() * gap_metadata(),
                ..MarkdownTableViewportBudget::default()
            },
            |sum, record| add_budget(sum, record.retained),
        )?;
        for ticket in &previous.structures {
            records = add_budget(records, structural_budget(ticket))?;
        }
        let quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        quota
            .admit_retained(previous.retained.runs, previous.retained.metadata_bytes)
            .map_err(MarkdownTableRequestError::Window)?;
        quota
            .admit_metadata(
                publication_metadata()
                    + previous.windows.len() * std::mem::size_of::<Rc<WindowRecord>>()
                    + previous.structures.len()
                        * std::mem::size_of::<Rc<MarkdownTableStructuralWindow>>()
                    + previous.gaps.len() * std::mem::size_of::<Rc<VerifiedMarkdownTableGap>>(),
            )
            .map_err(MarkdownTableRequestError::Window)?;
        // 所有同步 preflight 完成后才复制有限 Rc 句柄。没有窗口内容 clone，也不保父 publication。
        let windows = previous.windows.clone();
        let gaps = previous.gaps.clone();
        let suffix = viewport.end > self.resume_boundary.1;
        let structure = self.structure.as_mut().unwrap();
        if suffix {
            structure.viewport = self.resume_boundary.1..viewport.end;
            structure.no_table = false;
            if let Some(table) = structure.current.as_mut() {
                // 当前 H/math/pending Row 均仍属同一真实表，只把新 body 范围交给后续语义作业。
                table.found = false;
                table.include_header = false;
            }
            if self.active_header.is_none() {
                // 普通 gap 旧票已经证明至该 byte/UTF16 同源边界；新票不能重复覆盖旧 gap。
                self.between_start = self.resume_boundary;
            }
        }
        self.required = None;
        self.pending_record = None;
        self.viewport = viewport.clone();
        self.phase = BatchPhase::Sweep;
        self.index = 0;
        self.windows = windows;
        self.structures = previous.structures.clone();
        self.gaps = gaps;
        self.coverage = Vec::new();
        self.coverage_window = 0;
        self.coverage_range = 0;
        self.coverage_structure = 0;
        self.coverage_structure_range = 0;
        self.coverage_gap = 0;
        self.covered_until = viewport.start;
        self.budget = MarkdownTableViewportBudget {
            metadata_bytes: quota.metadata_used(),
            ..records
        };
        self.records = records;
        self.queries =
            suffix.then(|| MarkdownTableReferenceQueries::for_batch(Some(quota.clone())));
        self.quota = quota;
        if !suffix {
            self.begin_prune();
        }
        Ok(())
    }

    fn check_extension(
        &self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: &Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        previous.check(document)?;
        if !self.source.is_current(document) || !previous.publication.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if !matches!(self.phase, BatchPhase::Consumed)
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
            || self.context_revision != previous.publication.context_revision
            || self.selection != previous.publication.selection
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        if viewport.start >= viewport.end
            || viewport.end > document.utf16_len()
            || viewport.len() > TABLE_WINDOW_MAX_UTF16
        {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if self.structure.is_none() {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        // 只复用完整物理覆盖，不把旧 requested 的截断端点误当唯一可信边界。
        // 后缀必须紧接真实续读点；其左侧每个字节对应的 UTF16 区间都必须已有独立票据。
        let required = viewport.start..viewport.end.min(self.resume_boundary.1);
        if viewport.start > self.resume_boundary.1 || !previous.proves(&required) {
            return Err(MarkdownTableRequestError::CoverageRequiresCheckpoint);
        }
        Ok(())
    }

    pub fn resume_forward(
        &mut self,
        document: &MarkdownDocumentBuffer,
        viewport: Range<usize>,
        context_revision: u64,
        selection: Range<usize>,
    ) -> Result<(), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if !matches!(self.phase, BatchPhase::Consumed)
            || viewport.is_empty()
            || viewport.start < self.resume_boundary.1
            || viewport.end > document.utf16_len()
            || viewport.len() > TABLE_WINDOW_MAX_UTF16
            || selection.start > selection.end
            || selection.end > document.utf16_len()
        {
            return Err(MarkdownTableRequestError::InvalidRequest); // 逆向必须另取可信checkpoint，绝不暗中重建Global。
        }
        let structure = self
            .structure
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        structure.viewport = viewport.clone();
        structure.no_table = false;
        if let Some(table) = structure.current.as_mut() {
            table.found = false;
            table.include_header = false;
        }
        self.required = None;
        self.pending_record = None;
        self.viewport = viewport.clone();
        self.context_revision = context_revision;
        self.selection = selection;
        self.phase = BatchPhase::Sweep;
        self.index = 0;
        self.covered_until = viewport.start;
        self.coverage_window = 0;
        self.coverage_range = 0;
        self.coverage_structure = 0;
        self.coverage_structure_range = 0;
        self.coverage_gap = 0;
        self.prune_index = 0;
        self.prune_keep = 0;
        self.budget = MarkdownTableViewportBudget::default();
        self.records = MarkdownTableViewportBudget::default();
        self.quota = super::prefixes::BatchQuota::new(MAX_RUNS, MAX_METADATA_BYTES);
        self.quota
            .admit_metadata(publication_metadata())
            .map_err(MarkdownTableRequestError::Window)?;
        self.queries = Some(MarkdownTableReferenceQueries::for_batch(Some(
            self.quota.clone(),
        )));
        // 显式不重叠 resume 仍是独立批次；旧发布只能读，不能再借给这份续体作 overlap 扩展。
        self.last_publication = None;
        Ok(())
    }
}

fn publication_metadata() -> usize {
    // Rc 控制头按两个 usize 计逻辑开销；不声称覆盖 allocator 对齐或实际峰值。
    std::mem::size_of::<VerifiedMarkdownTableViewport>()
        + std::mem::size_of::<Publication>()
        + 2 * std::mem::size_of::<usize>()
}

fn intersects(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn gap_metadata() -> usize {
    std::mem::size_of::<VerifiedMarkdownTableGap>() + 2 * std::mem::size_of::<usize>()
}

/// 结构票至多 H/D 与连续真实 body 两段；省略的 header context 不能贡献可见 flow 权限。
fn structure_range(ticket: &MarkdownTableStructuralWindow, index: usize) -> Option<Range<usize>> {
    if ticket.include_header && index == 0 {
        return Some(ticket.header_utf16.start..ticket.delimiter_utf16.end);
    }
    if index != usize::from(ticket.include_header) {
        return None;
    }
    Some(ticket.body.first()?.utf16_start..ticket.body.last()?.utf16_end)
}

/// 只计真正保留的原行与scope句柄；没有复制源，因此不挪用完整Parser的源输入额度。
fn structural_budget(ticket: &MarkdownTableStructuralWindow) -> MarkdownTableViewportBudget {
    MarkdownTableViewportBudget {
        row_slots: ticket.body.len() + 2,
        cells: (ticket.body.len() + 1) * ticket.header.columns,
        column_slots: ticket.header.columns,
        metadata_bytes: std::mem::size_of::<MarkdownTableStructuralWindow>()
            + 2 * std::mem::size_of::<usize>()
            + ticket.body.capacity() * std::mem::size_of::<TableRowFact>()
            + ticket.inline_rows.capacity() * std::mem::size_of::<VerifiedTableInlineRow>(),
        ..MarkdownTableViewportBudget::default()
    }
}

fn retained_window_budget(
    window: &VerifiedMarkdownTableWindow,
    mut source: MarkdownTableViewportBudget,
) -> Result<MarkdownTableViewportBudget, MarkdownTableRequestError> {
    use crate::markdown_viewport_presentation::table_window::{
        MarkdownTableOrigin, MarkdownTableWindowCell, MarkdownTableWindowRow,
    };
    let presentation = &window.presentation;
    source.runs = presentation.runs.len() + presentation.header_context.runs.len();
    // 结构票的原物理输入额度继续保留；metadata 只记录最终发布的数组与固定对象。
    // 临时 scope/prefix/ref/underline 目录已释放，不能把旧工作区重复当作 retained payload。
    let arrays = [
        (
            presentation.rows.len(),
            std::mem::size_of::<MarkdownTableWindowRow>(),
        ),
        (
            presentation.cells.len(),
            std::mem::size_of::<MarkdownTableWindowCell>(),
        ),
        (
            presentation.header_context.cells.len(),
            std::mem::size_of::<MarkdownTableWindowCell>(),
        ),
        (source.runs, std::mem::size_of::<MarkdownPresentationRun>()),
        (
            presentation.origins.len(),
            std::mem::size_of::<MarkdownTableOrigin>(),
        ),
        (window.coverage.len(), std::mem::size_of::<Range<usize>>()),
    ];
    source.metadata_bytes = arrays
        .into_iter()
        .try_fold(
            std::mem::size_of::<WindowRecord>() + 2 * std::mem::size_of::<usize>(),
            |total, (len, size)| {
                len.checked_mul(size)
                    .and_then(|bytes| total.checked_add(bytes))
            },
        )
        .ok_or(MarkdownTableRequestError::WorkLimit)?;
    Ok(source)
}

fn add_budget(
    mut total: MarkdownTableViewportBudget,
    cost: MarkdownTableViewportBudget,
) -> Result<MarkdownTableViewportBudget, MarkdownTableRequestError> {
    macro_rules! add { ($($field:ident),*) => { $(total.$field = total.$field.checked_add(cost.$field).ok_or(MarkdownTableRequestError::WorkLimit)?;)* }; }
    add!(
        row_slots,
        cells,
        column_slots,
        source_bytes,
        source_utf16,
        runs,
        metadata_bytes
    );
    Ok(total)
}

fn step_work(step: MarkdownTableWindowStep) -> SemanticWork {
    match step {
        MarkdownTableWindowStep::Progress { work }
        | MarkdownTableWindowStep::Ready { work }
        | MarkdownTableWindowStep::NotTable { work }
        | MarkdownTableWindowStep::TableEndReached { work } => work,
    }
}
fn add_work(total: &mut SemanticWork, work: SemanticWork) -> Result<(), MarkdownTableRequestError> {
    macro_rules! add { ($($field:ident),*) => { $(total.$field = total.$field.checked_add(work.$field).ok_or(MarkdownTableRequestError::WorkLimit)?;)* }; }
    add!(
        source_bytes,
        copied_source_bytes,
        metadata_bytes,
        utf8_validation_bytes,
        source_tree_steps,
        structural_transitions,
        allocated_source_capacity,
        normalizer_input_bytes,
        parser_input_bytes
    );
    Ok(())
}

#[cfg(test)]
mod tests;
