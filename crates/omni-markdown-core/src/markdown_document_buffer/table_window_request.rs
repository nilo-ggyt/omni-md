//! 真实piece源上的有限table结构请求；当前只签发内部结构票据，不冒充完整语义Ready。

use super::table_source::MarkdownTableSourceIdentity;
use super::*;
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableRowMathContext, MarkdownTableWindowError, TABLE_WINDOW_MAX_CELLS,
    TABLE_WINDOW_MAX_COLUMNS, TABLE_WINDOW_MAX_ROWS,
};
mod cell_edit;
pub use cell_edit::{MarkdownTableCellEditRequest, MarkdownTableCellEditStep};
mod checkpoint;
use checkpoint::{Boundary, ReplayAnchors};
mod ordinary_source;
use ordinary_source::CapturedOrdinary;
pub use ordinary_source::{
    MarkdownOrdinaryPendingReason, MarkdownOrdinarySemanticState, MarkdownRootLeafPendingReason,
    MarkdownViewportSourceAuthority, MarkdownViewportSourceRequest,
    MarkdownViewportSourceRequiredRowQuery, MarkdownViewportSourceStep,
    VerifiedMarkdownOrdinaryLeaf, VerifiedMarkdownOrdinaryLine, VerifiedMarkdownPlainViewport,
    VerifiedMarkdownRootLeafViewport, VerifiedMarkdownViewportSources,
};

use pulldown_cmark::{
    block_frontier::{SourceFrame, TableCheckpoint, TableRowFact},
    global_frontier::{
        GlobalFact, GlobalFrontier, GlobalFrontierError, GlobalFrontierLimits,
        GlobalSourceAuthority, GlobalStep, GlobalTableFact, GlobalWork, StructuralSpineLimits,
        VerifiedTableEnd,
    },
    math_context::{
        MathContext, MathContextError, MathContextScan, MathContextStep, MathSourceAuthority,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// viewport查询使用真实UTF16；导航查询使用H物理身份与绝对row ordinal。
pub enum MarkdownTableWindowTarget {
    AtUtf16(usize),
    /// 寻找首个与viewport相交的真实表；顶部普通段落不能否定视口下方的表格。
    FirstTableInViewport,
    RequiredRow {
        header_byte_start: usize,
        row_ordinal: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownTableRequestError {
    /// 原源读取/归还真实失败；取消和结构坐标错误不得遮住它。
    Source(MarkdownDocumentBufferError),
    InvalidRequest,
    SourceSuperseded,
    /// 续体只接受本请求最后一次发布的 opaque 权限，不能借同代次的另一批或旧发布续读。
    PublicationSuperseded,
    /// 所需左侧或内部区间尚无真实覆盖；调用方需要可信 checkpoint，不能暗中从源起点重扫。
    CoverageRequiresCheckpoint,
    Cancelled,
    WorkLimit,
    Global(GlobalFrontierError),
    Math(MathContextError),
    Window(MarkdownTableWindowError),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// 真实重放读取和状态转换均累计；不把source推进距离当作实际CPU读取量。
pub(crate) struct MarkdownTableRequestWork {
    pub byte_reads: usize,
    pub transitions: usize,
    pub source_tree_steps: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkdownTableStructureStep {
    Progress { work: MarkdownTableRequestWork },
    StructureReady { work: MarkdownTableRequestWork },
    NotTable { work: MarkdownTableRequestWork },
    TableEndReached { work: MarkdownTableRequestWork },
}

#[derive(Debug)]
/// 只有本模块消费真实GlobalFact的路径能构造；不公开可伪造的字段或From转换。
/// 它仅证明成员/坐标/math，尚未证明ref/underline/继承样式，不可直接交Native隐藏正文。
pub(crate) struct MarkdownTableStructuralWindow {
    source: MarkdownTableSourceIdentity,
    header: GlobalTableFact,
    checkpoint: TableCheckpoint,
    header_utf16: Range<usize>,
    delimiter_utf16: Range<usize>,
    body: Vec<TableRowFact>,
    math_rows: Vec<MarkdownTableRowMathContext>,
    include_header: bool,
    next_required_row: Option<usize>,
    table_end: Option<VerifiedTableEnd>,
    replay: Option<ReplayAnchors>,
}

impl MarkdownTableStructuralWindow {
    /// 内部结构票据进入语义阶段时仍核原root/代次，不能由相同整数坐标替代其源权限。
    fn is_current(&self, document: &MarkdownDocumentBuffer) -> bool {
        self.source.is_current(document)
    }
}

#[cfg(test)]
impl MarkdownTableStructuralWindow {
    pub fn rows(&self) -> &[TableRowFact] {
        &self.body
    }
    pub fn math_rows(&self) -> &[MarkdownTableRowMathContext] {
        &self.math_rows
    }
    pub fn includes_header(&self) -> bool {
        self.include_header
    }
    pub fn next_required_row(&self) -> Option<usize> {
        self.next_required_row
    }
    pub fn table_end(&self) -> Option<&VerifiedTableEnd> {
        self.table_end.as_ref()
    }
}

struct ActiveTable {
    header: GlobalTableFact,
    checkpoint: TableCheckpoint,
    header_utf16: Range<usize>,
    delimiter_utf16: Range<usize>,
    context: MathContext,
    header_context: Option<MathContext>,
    body: Vec<TableRowFact>,
    math_rows: Vec<MarkdownTableRowMathContext>,
    found: bool,
    include_header: bool,
    first_overflow: Option<usize>,
    // 只保真实D边界、当前接受点与前驱；不为所有历史行建立checkpoint目录。
    base_boundary: Option<Boundary>,
    boundary: Option<Boundary>,
    halo_boundary: Option<Boundary>,
    chunk_boundary: Option<Boundary>,
    chunk_halo: Option<Boundary>,
    pending_row_boundary: Option<(TableCheckpoint, TableRowFact)>,
}

enum MathJob {
    HeaderScan(MathContextScan),
    HeaderActive(MathContextScan),
    Body(MathContextScan),
}
impl MathJob {
    fn scanner(&mut self) -> &mut MathContextScan {
        match self {
            Self::HeaderScan(scan) | Self::HeaderActive(scan) | Self::Body(scan) => scan,
        }
    }
}

/// 请求不持document、mapping或正文String；每次advance由当前owner借一片真实源。
/// Pending/结构完成/未发现分别记录，失败sticky，不在失败后继续扫到假Complete。
pub(crate) struct MarkdownTableWindowStructureRequest {
    source: MarkdownTableSourceIdentity,
    target: MarkdownTableWindowTarget,
    continuous: bool,
    continuous_body_limit: Option<usize>,
    viewport: Range<usize>,
    global: Option<GlobalFrontier>,
    pending: Option<GlobalFact>,
    // 仅 source-aware 包装入口启用；旧 table-only 调用仍直接忽略普通块事实。
    capture_ordinary: bool,
    ordinary_pending: Option<CapturedOrdinary>,
    coordinates: Option<HeaderCoordinates>,
    current: Option<ActiveTable>,
    math: Option<MathJob>,
    ready: Option<MarkdownTableStructuralWindow>,
    no_table: bool,
    required_row_end: Option<VerifiedTableEnd>,
    failed: Option<MarkdownTableRequestError>,
    total: MarkdownTableRequestWork,
    max_byte_work: usize,
    max_transitions: usize,
}

impl MarkdownDocumentBuffer {
    /// 每个请求绑定当前非空piece root；target/viewport都是已知真实坐标，不探测个人文件。
    pub(crate) fn table_window_structure_request(
        &self,
        target: MarkdownTableWindowTarget,
        viewport: Range<usize>,
    ) -> Result<MarkdownTableWindowStructureRequest, MarkdownTableRequestError> {
        let source = self
            .table_source_identity()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if viewport.start > viewport.end
            || viewport.end > self.utf16_len()
            || viewport.end - viewport.start > 64 * 1024
            || matches!(target, MarkdownTableWindowTarget::AtUtf16(at) if !viewport.contains(&at))
            || matches!(target, MarkdownTableWindowTarget::FirstTableInViewport)
                && viewport.is_empty()
        {
            // 不允许窗口上限被无界请求扩大，也不让EOF caret越界被误认作源成员。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let max_byte_work = self
            .byte_len()
            .checked_mul(256)
            .and_then(|n| n.checked_add(1024 * 1024))
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let max_transitions = self
            .byte_len()
            .checked_mul(2048)
            .and_then(|n| n.checked_add(1024 * 1024))
            .ok_or(MarkdownTableRequestError::WorkLimit)?;
        let options =
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true);
        let global = GlobalFrontier::new(
            source.generation(),
            source.byte_len(),
            options,
            GlobalFrontierLimits {
                spine: StructuralSpineLimits {
                    max_depth: 16_384,
                    max_live_page_bytes: 4 * 1024 * 1024,
                    max_page_allocations: 1_000_000,
                    max_role_work: max_transitions,
                },
                max_byte_work,
                max_transitions,
            },
        )
        .map_err(MarkdownTableRequestError::Global)?;
        Ok(MarkdownTableWindowStructureRequest {
            source,
            target,
            continuous: false,
            continuous_body_limit: None,
            viewport,
            global: Some(global),
            pending: None,
            capture_ordinary: false,
            ordinary_pending: None,
            coordinates: None,
            current: None,
            math: None,
            ready: None,
            no_table: false,
            required_row_end: None,
            failed: None,
            total: MarkdownTableRequestWork::default(),
            max_byte_work,
            max_transitions,
        })
    }
}

impl MarkdownTableWindowStructureRequest {
    #[cfg(test)]
    pub(crate) fn total_work(&self) -> MarkdownTableRequestWork {
        self.total
    }

    /// 每步总byte与转换均≤调用预算；真实结构Fact在下一step消费，避免原scanner用满预算后
    /// 又同步做坐标查询/分配。所有源借用都随本纯函数返回结束，不允许越过Native回调。
    pub(crate) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
        budget: usize,
    ) -> Result<MarkdownTableStructureStep, MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = self.advance_inner(document, cancelled, budget);
        match result {
            Ok(step) => {
                let work = match step {
                    MarkdownTableStructureStep::Progress { work }
                    | MarkdownTableStructureStep::StructureReady { work }
                    | MarkdownTableStructureStep::NotTable { work }
                    | MarkdownTableStructureStep::TableEndReached { work } => work,
                };
                let cumulative = self
                    .total
                    .byte_reads
                    .checked_add(work.byte_reads)
                    .zip(
                        self.total
                            .source_tree_steps
                            .checked_add(work.source_tree_steps),
                    )
                    .zip(self.total.transitions.checked_add(work.transitions));
                let Some(((bytes, tree), transitions)) = cumulative else {
                    self.fail(MarkdownTableRequestError::WorkLimit);
                    return Err(MarkdownTableRequestError::WorkLimit);
                };
                self.total = MarkdownTableRequestWork {
                    byte_reads: bytes,
                    source_tree_steps: tree,
                    transitions,
                };
                if self.total.byte_reads > self.max_byte_work
                    || self.total.transitions > self.max_transitions
                {
                    // 元数据、spine与math重放共享请求总fuel，不因切换子scanner重置累计量。
                    self.fail(MarkdownTableRequestError::WorkLimit);
                    return Err(MarkdownTableRequestError::WorkLimit);
                }
                Ok(step)
            }
            Err(error) => {
                self.fail(error);
                Err(error)
            }
        }
    }

    fn fail(&mut self, error: MarkdownTableRequestError) {
        // 首错撤权同时释放派生metadata/共享spine与全部容量；source只保Weak身份，
        // 不触用户canonical正文/history，也不让过期Ready继续通过getter。
        self.failed = Some(error);
        self.global = None;
        self.pending = None;
        self.ordinary_pending = None;
        self.coordinates = None;
        self.current = None;
        self.math = None;
        self.ready = None;
        self.required_row_end = None;
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        cancelled: bool,
        budget: usize,
    ) -> Result<MarkdownTableStructureStep, MarkdownTableRequestError> {
        if cancelled {
            return Err(MarkdownTableRequestError::Cancelled);
        }
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        let minimum = if self.continuous { 3072 } else { 4096 };
        if !(minimum..=64 * 1024).contains(&budget) {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if self.ready.is_some() {
            return Ok(MarkdownTableStructureStep::StructureReady {
                work: MarkdownTableRequestWork::default(),
            });
        }
        if self.required_row_end.is_some() {
            return Ok(MarkdownTableStructureStep::TableEndReached {
                work: MarkdownTableRequestWork::default(),
            });
        }
        if self.no_table {
            return Ok(MarkdownTableStructureStep::NotTable {
                work: MarkdownTableRequestWork::default(),
            });
        }
        if self.ordinary_pending.is_some() {
            // 原票据尚未移交时暂停同一 Global，不覆盖 pending 或暗中扫描下一个普通块。
            return Ok(MarkdownTableStructureStep::Progress {
                work: MarkdownTableRequestWork::default(),
            });
        }
        if self.coordinates.is_some() {
            return self.advance_coordinates(document, budget);
        }
        if self.math.is_some() {
            return self.advance_math(document, budget);
        }
        if let Some(fact) = self.pending.take() {
            // 一个Fact只包含有限metadata；可能产生下一段math job，但不在本step顺带扫描正文。
            return self.consume_fact(document, fact, budget);
        }
        let source_tree_steps = tree_height(document);
        let child_budget = child_budget(budget, source_tree_steps)?;
        let global = self
            .global
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let frame = document
            .table_source_frame(global.next_byte(), child_budget)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = global
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
                child_budget,
            )
            .map_err(MarkdownTableRequestError::Global);
        // 回收成功后才消费step，不能让原parser提前终止遮住真实映射错误。
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = step?;
        let work = match step {
            GlobalStep::Progress { work, .. } | GlobalStep::NeedInput { work, .. } => work,
            GlobalStep::Fact { fact, work } => {
                self.pending = Some(fact);
                work
            }
            GlobalStep::Complete { work } => {
                self.no_table = true;
                if !self.continuous {
                    self.global = None;
                }
                self.current = None;
                return Ok(MarkdownTableStructureStep::NotTable {
                    work: global_work(work, source_tree_steps),
                });
            }
        };
        Ok(MarkdownTableStructureStep::Progress {
            work: global_work(work, source_tree_steps),
        })
    }

    fn consume_fact(
        &mut self,
        document: &MarkdownDocumentBuffer,
        fact: GlobalFact,
        budget: usize,
    ) -> Result<MarkdownTableStructureStep, MarkdownTableRequestError> {
        let work = MarkdownTableRequestWork {
            byte_reads: 0,
            transitions: 64,
            source_tree_steps: 0,
        };
        match fact {
            GlobalFact::Table(header) => {
                // 坐标前缀只定位一个真实leaf；H/D正文的UTF16计量在后继有界step完成。
                // 不调用旧utf16_offset_for_byte重复解码最多64KiB编辑leaf。
                let coordinates = HeaderCoordinates::new(document, header, budget)?;
                let source_tree_steps = tree_height(document);
                self.coordinates = Some(coordinates);
                return Ok(MarkdownTableStructureStep::Progress {
                    work: MarkdownTableRequestWork {
                        byte_reads: 0,
                        transitions: source_tree_steps + 64,
                        source_tree_steps,
                    },
                });
            }
            GlobalFact::Row(row) => {
                let current = self
                    .current
                    .as_mut()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                if row.header_byte_start != current.header.header_byte_start {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                let target_here = match self.target {
                    MarkdownTableWindowTarget::AtUtf16(at) => {
                        (row.utf16_start..row.utf16_end).contains(&at)
                    }
                    MarkdownTableWindowTarget::FirstTableInViewport => {
                        row.utf16_start < self.viewport.end && self.viewport.start < row.utf16_end
                    }
                    MarkdownTableWindowTarget::RequiredRow {
                        header_byte_start,
                        row_ordinal,
                    } => {
                        header_byte_start == row.header_byte_start && row_ordinal == row.row_ordinal
                    }
                };
                if target_here {
                    current.found = true;
                    validate_columns(current.header.columns)?;
                }
                let selected = match self.target {
                    MarkdownTableWindowTarget::AtUtf16(_)
                    | MarkdownTableWindowTarget::FirstTableInViewport => {
                        row.utf16_start < self.viewport.end && self.viewport.start < row.utf16_end
                    }
                    MarkdownTableWindowTarget::RequiredRow {
                        header_byte_start,
                        row_ordinal,
                    } => {
                        header_byte_start == row.header_byte_start && row.row_ordinal == row_ordinal
                    }
                };
                if current.found && current.first_overflow.is_some() {
                    let next = current.first_overflow;
                    if self.continuous {
                        self.pending = Some(GlobalFact::Row(row));
                    }
                    self.finish(next, None)?;
                    return Ok(MarkdownTableStructureStep::StructureReady { work });
                }
                if current.found && !selected {
                    // 已越过所需窗口：不读取额外row正文，也不把本地最后row标作整表末尾。
                    if self.continuous {
                        self.pending = Some(GlobalFact::Row(row));
                    }
                    self.finish(None, None)?;
                    return Ok(MarkdownTableStructureStep::StructureReady { work });
                }
                if selected {
                    let maximum_body = (TABLE_WINDOW_MAX_CELLS / current.header.columns)
                        .saturating_sub(1)
                        .min(TABLE_WINDOW_MAX_ROWS - 1)
                        .min(self.continuous_body_limit.unwrap_or(usize::MAX));
                    if maximum_body == 0 && self.continuous {
                        return Err(MarkdownTableRequestError::Window(
                            MarkdownTableWindowError::RowLimit,
                        ));
                    }
                    if current.body.len() == maximum_body {
                        // target可能在viewport中间：首批先满仍须继续证明target属同表，不能漏前部可见row。
                        current.first_overflow.get_or_insert(row.row_ordinal);
                        if current.found {
                            if self.continuous {
                                self.pending = Some(GlobalFact::Row(row));
                            }
                            self.finish(Some(row.row_ordinal), None)?;
                            return Ok(MarkdownTableStructureStep::StructureReady { work });
                        }
                    }
                    if current.first_overflow.is_none() && current.math_rows.is_empty() {
                        current.math_rows.push(MarkdownTableRowMathContext {
                            row_ordinal: 0,
                            context: current
                                .header_context
                                .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                        });
                    }
                    if current.first_overflow.is_none() {
                        if current.body.is_empty() {
                            // 当前Row尚未扫描math；此处保留的仍是本行之前已接受的精确状态。
                            current.chunk_boundary = current.boundary.clone();
                            current.chunk_halo = current.halo_boundary.clone();
                        }
                        current.body.push(row);
                        current.math_rows.push(MarkdownTableRowMathContext {
                            row_ordinal: row.row_ordinal,
                            context: current.context,
                        });
                    }
                }
                // 被省略的真实row也必须更新math；不保正文、不跳过超列cell或按brace净计数猜状态。
                let checkpoint = self
                    .global
                    .as_ref()
                    .and_then(GlobalFrontier::table_checkpoint)
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                current.pending_row_boundary = Some((checkpoint, row));
                self.math = Some(MathJob::Body(
                    MathContextScan::new(
                        document.generation(),
                        document.byte_len(),
                        row.byte_start + row.container_prefix_bytes..row.byte_end,
                        current.context,
                    )
                    .map_err(MarkdownTableRequestError::Math)?,
                ));
            }
            GlobalFact::TableEnd(end) => {
                if self.current.as_ref().is_some_and(|table| table.found) {
                    self.finish(None, Some(end))?;
                    return Ok(MarkdownTableStructureStep::StructureReady { work });
                }
                if matches!(self.target, MarkdownTableWindowTarget::RequiredRow { header_byte_start, .. }
                    if header_byte_start == end.header_byte_start())
                {
                    // 缺少后继row与“不属于table”不同；保留真实Ended签发的opaque证明，
                    // 上层还须核当前格确为该末行/末列，不能仅凭请求坐标授权append。
                    self.required_row_end = Some(end);
                    self.current = None;
                    self.global = None;
                    return Ok(MarkdownTableStructureStep::TableEndReached { work });
                }
                // 目标不属于此table；drop旧checkpoint/metadata后继续真实全局frontier，不保历史表目录。
                self.current = None;
            }
            GlobalFact::Reference(_) => {} // 结构查找不存全文ref目录；有限查询库存形成后另从真实源顺序解析。
            GlobalFact::InlineBlock(block) => {
                if self.capture_ordinary {
                    // 保留同一真实前沿的 opaque capture；后继只能消费它，不能从 gap 重建 paragraph。
                    self.ordinary_pending = Some(CapturedOrdinary::Inline(block));
                }
            }
            GlobalFact::LeafBlock(block) => {
                if self.capture_ordinary {
                    // Leaf 尚无逐行 prefix 权限；移交原票据只证明真实闭合来源，不制造 ordinary Ready。
                    self.ordinary_pending = Some(CapturedOrdinary::Leaf(block));
                }
            }
        }
        Ok(MarkdownTableStructureStep::Progress { work })
    }

    fn install_header(
        &mut self,
        document: &MarkdownDocumentBuffer,
        header: GlobalTableFact,
        coordinates: [usize; 4],
    ) -> Result<(), MarkdownTableRequestError> {
        let checkpoint = self
            .global
            .as_ref()
            .and_then(GlobalFrontier::table_checkpoint)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let header_utf16 = coordinates[0]..coordinates[1];
        let delimiter_utf16 = coordinates[2]..coordinates[3];
        let found = match self.target {
            MarkdownTableWindowTarget::AtUtf16(at) => {
                (header_utf16.start..delimiter_utf16.end).contains(&at)
            }
            MarkdownTableWindowTarget::FirstTableInViewport => {
                header_utf16.start < self.viewport.end && self.viewport.start < delimiter_utf16.end
            }
            MarkdownTableWindowTarget::RequiredRow {
                header_byte_start,
                row_ordinal,
            } => header_byte_start == header.header_byte_start && row_ordinal == 0,
        };
        let include_header = header_utf16.start < self.viewport.end
            && self.viewport.start < delimiter_utf16.end
            || matches!(
                self.target,
                MarkdownTableWindowTarget::RequiredRow { row_ordinal: 0, .. }
            );
        if found {
            validate_columns(header.columns)?;
        }
        // 每个真实table独立reset；首次H Scan与后续Active H都使用真实prefix后的源。
        self.math = Some(MathJob::HeaderScan(
            MathContextScan::new(
                document.generation(),
                document.byte_len(),
                header.header_byte_start + header.header_prefix_bytes..header.header_byte_end,
                MathContext::default(),
            )
            .map_err(MarkdownTableRequestError::Math)?,
        ));
        self.current = Some(ActiveTable {
            header,
            checkpoint,
            header_utf16,
            delimiter_utf16,
            context: MathContext::default(),
            header_context: None,
            body: Vec::new(),
            math_rows: Vec::new(),
            found,
            include_header,
            first_overflow: None,
            base_boundary: None,
            boundary: None,
            halo_boundary: None,
            chunk_boundary: None,
            chunk_halo: None,
            pending_row_boundary: None,
        });
        Ok(())
    }

    fn advance_coordinates(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableStructureStep, MarkdownTableRequestError> {
        let coordinate = self
            .coordinates
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let (work, done) = coordinate.advance(document, budget)?;
        if matches!(self.target, MarkdownTableWindowTarget::FirstTableInViewport)
            && coordinate.boundary_index > 0
            && coordinate.values[0] >= self.viewport.end
        {
            // 首个已验证H已在窗口之后，真实源顺序排除了此前gap内的任何表。
            // 不为窗口外巨header继续解码H/D整行或创建下一个checkpoint。
            if !self.continuous {
                self.coordinates = None;
                self.global = None;
            }
            // 连续批保留已验证下表H及其尚未完成的坐标游标，后继viewport不重建源起点。
            self.current = None;
            self.no_table = true;
            return Ok(MarkdownTableStructureStep::NotTable { work });
        }
        if done {
            let coordinate = self
                .coordinates
                .take()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            self.install_header(document, coordinate.header, coordinate.values)?;
        }
        Ok(MarkdownTableStructureStep::Progress { work })
    }

    fn advance_math(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<MarkdownTableStructureStep, MarkdownTableRequestError> {
        let job = self
            .math
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let scanner = job.scanner();
        let source_tree_steps = tree_height(document);
        let child_budget = child_budget(budget, source_tree_steps)?;
        let frame = document
            .table_source_frame(scanner.next_byte(), child_budget)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = scanner
            .advance(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                MathSourceAuthority {
                    current_revision: document.generation(),
                    cancelled: false,
                },
                child_budget,
            )
            .map_err(MarkdownTableRequestError::Math);
        // 回收成功后才消费step，不能让原parser提前终止遮住真实映射错误。
        frame
            .finish()
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let step = step?;
        let (work, complete) = match step {
            MathContextStep::Progress { work, .. } | MathContextStep::NeedInput { work, .. } => {
                (work, None)
            }
            MathContextStep::Complete { context, work } => (work, Some(context)),
        };
        if let Some(context) = complete {
            let previous = self
                .math
                .take()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            let table = self
                .current
                .as_mut()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            match previous {
                MathJob::HeaderScan(_) => {
                    table.header_context = Some(context);
                    self.math = Some(MathJob::HeaderActive(
                        MathContextScan::new(
                            document.generation(),
                            document.byte_len(),
                            table.header.header_byte_start + table.header.header_prefix_bytes
                                ..table.header.header_byte_end,
                            context,
                        )
                        .map_err(MarkdownTableRequestError::Math)?,
                    ));
                }
                MathJob::HeaderActive(_) => {
                    table.context = context;
                    // D后的cp来自成功Table事实；只有两轮真实H math完成后才能配成body起点。
                    let boundary = Boundary::after_delimiter(
                        table.checkpoint.clone(),
                        &table.header,
                        table.delimiter_utf16.end,
                        context,
                    );
                    table.base_boundary = Some(boundary.clone());
                    table.boundary = Some(boundary);
                }
                MathJob::Body(_) => {
                    let (checkpoint, row) = table
                        .pending_row_boundary
                        .take()
                        .ok_or(MarkdownTableRequestError::InvalidRequest)?;
                    // Global的after-row cp先暂存；本行math成功后才推进联合接受边界。
                    table.halo_boundary = table.boundary.take();
                    table.boundary = Some(Boundary::after_row(checkpoint, row, context));
                    table.context = context;
                }
            }
        }
        Ok(MarkdownTableStructureStep::Progress {
            work: MarkdownTableRequestWork {
                byte_reads: work.byte_reads,
                transitions: work.transitions + source_tree_steps + 64,
                source_tree_steps,
            },
        })
    }

    fn finish(
        &mut self,
        next_required_row: Option<usize>,
        table_end: Option<VerifiedTableEnd>,
    ) -> Result<(), MarkdownTableRequestError> {
        let current = self
            .current
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if !current.found {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if current.math_rows.is_empty() {
            current.math_rows.push(MarkdownTableRowMathContext {
                row_ordinal: 0,
                context: current
                    .header_context
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
            });
        }
        // 连续消费者仅移交本窗metadata；同一frontier、H上下文和真实前一行math留在活动表。
        // first未消费Row已由调用者保留pending，下一窗先消费它，不能从越过该行的checkpoint跳过。
        self.ready = Some(MarkdownTableStructuralWindow {
            source: self.source.clone(),
            header: current.header,
            checkpoint: current.checkpoint.clone(),
            header_utf16: current.header_utf16.clone(),
            delimiter_utf16: current.delimiter_utf16.clone(),
            body: std::mem::take(&mut current.body),
            math_rows: std::mem::take(&mut current.math_rows),
            include_header: current.include_header,
            next_required_row: next_required_row.or(current.first_overflow),
            table_end,
            replay: Some(ReplayAnchors {
                header: current.header,
                header_utf16: current.header_utf16.clone(),
                delimiter_utf16: current.delimiter_utf16.clone(),
                header_math: current
                    .header_context
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                base: current
                    .base_boundary
                    .clone()
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                chunk: current
                    .chunk_boundary
                    .clone()
                    .or_else(|| current.base_boundary.clone())
                    .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                halo: current.chunk_halo.clone(),
            }),
        });
        if !self.continuous {
            self.global = None;
            self.current = None;
        } else if self
            .ready
            .as_ref()
            .is_some_and(|ticket| ticket.table_end.is_some())
        {
            // Global真实Ended已经回到原外层spine；保留Global继续下一块，不保不存在的活动表。
            self.current = None;
        } else {
            let current = self.current.as_mut().unwrap();
            current.include_header = false;
            current.first_overflow = None;
            current.found = false;
            current.chunk_boundary = None;
            current.chunk_halo = None;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn required_row_end_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&VerifiedTableEnd>, MarkdownTableRequestError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if let Some(error) = self.failed {
            return Err(error);
        }
        Ok(self.required_row_end.as_ref())
    }

    /// 只有相同source root/代次可借票据；返回值仍是结构阶段，尚非允许隐藏正文的语义window。
    #[cfg(test)]
    pub(crate) fn structural_window_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&MarkdownTableStructuralWindow>, MarkdownTableRequestError> {
        if !self.source.is_current(document) {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        if let Some(error) = self.failed {
            return Err(error);
        }
        Ok(self.ready.as_ref())
    }
}

fn validate_columns(columns: usize) -> Result<(), MarkdownTableRequestError> {
    if columns == 0 || columns > TABLE_WINDOW_MAX_COLUMNS {
        Err(MarkdownTableRequestError::Window(
            MarkdownTableWindowError::ColumnLimit,
        ))
    } else {
        Ok(())
    }
}
fn global_work(work: GlobalWork, source_tree_steps: usize) -> MarkdownTableRequestWork {
    MarkdownTableRequestWork {
        byte_reads: work.byte_reads,
        transitions: work.transitions + source_tree_steps + 64,
        source_tree_steps,
    }
}

// 原AVL路径上界加有限共享目录工作上界；实际正文借用仍只落在一个4KiB逻辑片。
fn tree_height(document: &MarkdownDocumentBuffer) -> usize {
    document.root.as_ref().map_or(0, |root| {
        // 单个普通Leaf没有共享目录，保持原步进切点；其余路径预留一个有限目录的最坏成本。
        let directory = if matches!(root.as_ref(), MarkdownPieceTreeNode::Leaf(_)) {
            0
        } else {
            ORIGINAL_SPAN_METADATA_STEPS
        };
        usize::from(root.height()) + directory
    })
}
fn child_budget(budget: usize, tree: usize) -> Result<usize, MarkdownTableRequestError> {
    let child = budget
        .checked_sub(tree + 64)
        .ok_or(MarkdownTableRequestError::InvalidRequest)?;
    if child < 2048 {
        return Err(MarkdownTableRequestError::InvalidRequest);
    }
    Ok(child)
}

struct HeaderCoordinates {
    header: GlobalTableFact,
    boundaries: [usize; 4],
    values: [usize; 4],
    boundary_index: usize,
    next: usize,
    utf16: usize,
}
impl HeaderCoordinates {
    fn new(
        document: &MarkdownDocumentBuffer,
        header: GlobalTableFact,
        budget: usize,
    ) -> Result<Self, MarkdownTableRequestError> {
        if tree_height(document).saturating_add(64) > budget {
            // 定位路径本身也必须先准入，不能借低byte消耗隐形走过任意高树。
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut node = document
            .root
            .as_deref()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let mut local = header.header_byte_start;
        let mut before_utf16 = 0;
        // 缓存AVL高度给出有限路径准入；只加完整左子树metrics，不解码整个leaf。
        while let MarkdownPieceTreeNode::Branch { left, right, .. } = node {
            if local < left.metrics().byte_len {
                // 查询点位于左子树，右侧完整metrics不进入前缀。
                node = left;
            } else {
                // 完整左子树直接复用缓存计量，只把剩余byte位置带入右侧。
                local -= left.metrics().byte_len;
                before_utf16 += left.metrics().utf16_len;
                node = right;
            }
        }
        if let MarkdownPieceTreeNode::OriginalSpan(span) = node {
            // 合组叶只压缩元数据；从命中4KiB小片起计量，不能把整个64KiB叶前缀借入正文。
            let hit = span
                .locate_byte(local, true)
                .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
            local = hit.local;
            before_utf16 += hit.prefix.utf16_len;
        }
        Ok(Self {
            header,
            boundaries: [
                header.header_byte_start,
                header.header_byte_end,
                header.separator_byte_start,
                header.separator_byte_end,
            ],
            values: [0; 4],
            boundary_index: 0,
            next: header.header_byte_start - local,
            utf16: before_utf16,
        })
    }

    fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(MarkdownTableRequestWork, bool), MarkdownTableRequestError> {
        let source_tree_steps = tree_height(document);
        let child = child_budget(budget, source_tree_steps)?;
        let frame = document
            .table_source_frame(self.next, child / 2)
            .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
        let mut work = MarkdownTableRequestWork {
            byte_reads: 0,
            transitions: source_tree_steps + 64,
            source_tree_steps,
        };
        loop {
            // 相邻H.end/D.start只记录同一真实边界两次；固定4项，不按源长扩大元数据。
            while self.boundary_index < 4 && self.boundaries[self.boundary_index] == self.next {
                self.values[self.boundary_index] = self.utf16;
                self.boundary_index += 1;
            }
            if self.boundary_index == 4 {
                frame
                    .finish()
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return Ok((work, true));
            }
            if work.transitions + 2 > budget {
                frame
                    .finish()
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return Ok((work, false));
            }
            let Some(byte) = frame.bytes.get(self.next - frame.byte_start) else {
                frame
                    .finish()
                    .map_err(|_| MarkdownTableRequestError::InvalidRequest)?;
                return Ok((work, false));
            };
            // piece root的构造/编辑已经验证UTF8，真实H/D范围在scalar边界；continuation byte
            // 不重复计量，4-byte scalar贡献两个UTF16 unit，即使frame切在scalar内部也保持一致。
            self.utf16 += if *byte < 0x80 || *byte >= 0xc0 {
                if *byte >= 0xf0 { 2 } else { 1 }
            } else {
                0
            };
            self.next += 1;
            work.byte_reads += 1;
            work.transitions += 2;
        }
    }
}

#[cfg(test)]
mod tests;

mod semantic;
pub use semantic::{
    MarkdownTableViewportBudget, MarkdownTableViewportCoverage, MarkdownTableViewportRequest,
    MarkdownTableViewportRequiredRowQuery, MarkdownTableViewportRequiredRowResult,
    MarkdownTableViewportStep, MarkdownTableWindowRequest, MarkdownTableWindowStep, SemanticWork,
    VerifiedMarkdownTableGap, VerifiedMarkdownTableViewport, VerifiedMarkdownTableWindow,
};
