//! 原publication签发的单行查询；表内回放与视觉窗口共用一批语义和配额。

use super::super::super::checkpoint::{ReplayOutcome, ReplayRequest};
use super::*;

/// 查询只含原发布的弱身份和真实origin成员；目标ordinal尚未经过源验证，不能据此编辑。
pub struct MarkdownTableViewportRequiredRowQuery {
    publication: Weak<Publication>,
    origin: QueryOrigin,
    header: usize,
    origin_row: usize,
    requested_row: usize,
}

/// 两条实际生产路径各保原弱票；结构导航不为复用旧窗口入口构造投影或替身 WindowRecord。
enum QueryOrigin {
    Window(Weak<WindowRecord>),
    Structure(Weak<MarkdownTableStructuralWindow>),
}

/// 在途请求只保已准入的真实 origin，取消后随原 RequiredState 一并释放共享 checkpoint 页。
enum RequiredOrigin {
    Window(Rc<WindowRecord>),
    Structure(Rc<MarkdownTableStructuralWindow>),
}

impl RequiredOrigin {
    fn header(&self) -> usize {
        match self {
            Self::Window(record) => record.window.presentation.header_byte_range.start,
            Self::Structure(ticket) => ticket.header.header_byte_start,
        }
    }

    fn replay(&self) -> Option<&ReplayAnchors> {
        match self {
            Self::Window(record) => record.window.replay.as_ref(),
            Self::Structure(ticket) => ticket.replay.as_ref(),
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::markdown_document_buffer::table_window_request) enum RequiredLocation {
    Window(usize),
    Structure(usize),
}

/// 结果只借同一Ready的窗口索引或真实End；不存在独立Window的所有权转换。
pub enum MarkdownTableViewportRequiredRowResult<'a> {
    Row {
        window_index: usize,
        row_ordinal: usize,
    },
    TableEnd {
        end: &'a VerifiedTableEnd,
        terminal_window_index: usize,
    },
}

pub(in crate::markdown_document_buffer::table_window_request) enum RequiredPublication {
    Row {
        location: RequiredLocation,
        row_ordinal: usize,
    },
    End {
        end: VerifiedTableEnd,
        terminal: RequiredLocation,
    },
}

pub(super) struct RequiredState {
    origin: RequiredOrigin,
    requested_row: usize,
    resolved: bool,
    end: Option<VerifiedTableEnd>,
    replay: Option<ReplayRequest>,
}
impl RequiredState {
    pub(super) fn keeps(&self, record: &Rc<WindowRecord>) -> bool {
        // origin保留草稿的实际源行；目标行或真实末行必须同样保留，不能prune成悬空索引。
        matches!(&self.origin, RequiredOrigin::Window(origin) if Rc::ptr_eq(record, origin))
            || (record.window.presentation.header_byte_range.start == self.origin.header()
                && (record.window.presentation.row(self.requested_row).is_some()
                    || self.end.as_ref().is_some_and(|end| {
                        record
                            .window
                            .presentation
                            .row(end.last_body_row_ordinal().unwrap_or(0))
                            .is_some()
                    })))
    }

    pub(super) fn keeps_structure(&self, ticket: &Rc<MarkdownTableStructuralWindow>) -> bool {
        // 目标和真实 terminal 即使离开视觉范围仍属于本次导航结果；不能在发布前按视口裁掉。
        matches!(&self.origin, RequiredOrigin::Structure(origin) if Rc::ptr_eq(ticket, origin))
            || (ticket.header.header_byte_start == self.origin.header()
                && (contains_structure_row(ticket, self.requested_row)
                    || self.end.as_ref().is_some_and(|end| {
                        contains_structure_row(ticket, end.last_body_row_ordinal().unwrap_or(0))
                    })))
    }
}

fn contains_structure_row(ticket: &MarkdownTableStructuralWindow, ordinal: usize) -> bool {
    (ordinal == 0 && ticket.include_header)
        || ticket.body.iter().any(|row| row.row_ordinal == ordinal)
}

impl VerifiedMarkdownTableViewport {
    /// 从真实origin row签查询；不接收另一个RequiredRow producer的Ready，也不凭同长range换票。
    pub fn required_row_query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        window_index: usize,
        origin_row: usize,
        requested_row: usize,
    ) -> Result<MarkdownTableViewportRequiredRowQuery, MarkdownTableRequestError> {
        self.check(document)?;
        let record = self
            .windows
            .get(window_index)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if record.window.presentation.row(origin_row).is_none() || record.window.replay.is_none() {
            // context-only H不是可编辑row；没有原实际成员或真实cp时不能制造origin许可。
            return Err(MarkdownTableRequestError::CoverageRequiresCheckpoint);
        }
        Ok(MarkdownTableViewportRequiredRowQuery {
            publication: Rc::downgrade(&self.publication),
            origin: QueryOrigin::Window(Rc::downgrade(record)),
            header: record.window.presentation.header_byte_range.start,
            origin_row,
            requested_row,
        })
    }

    pub fn required_row_result_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<MarkdownTableViewportRequiredRowResult<'_>>, MarkdownTableRequestError> {
        self.check(document)?;
        self.required
            .as_ref()
            .map(|result| match result {
                RequiredPublication::Row {
                    location: RequiredLocation::Window(window_index),
                    row_ordinal,
                } => Ok(MarkdownTableViewportRequiredRowResult::Row {
                    window_index: *window_index,
                    row_ordinal: *row_ordinal,
                }),
                RequiredPublication::End {
                    end,
                    terminal: RequiredLocation::Window(terminal_window_index),
                } => Ok(MarkdownTableViewportRequiredRowResult::TableEnd {
                    end,
                    terminal_window_index: *terminal_window_index,
                }),
                _ => Err(MarkdownTableRequestError::InvalidRequest), // 结构结果只能移交原 Source 行，不伪造完整窗口索引。
            })
            .transpose()
    }

    /// 正式来源只接受本批实际结构成员；目标整数仍是内部请求，必须等原 Replay 返回真实 Row/End。
    pub(in crate::markdown_document_buffer::table_window_request) fn required_structure_query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        origin: &Rc<MarkdownTableStructuralWindow>,
        origin_row: usize,
        requested_row: usize,
    ) -> Result<MarkdownTableViewportRequiredRowQuery, MarkdownTableRequestError> {
        self.check(document)?;
        if !self
            .structures
            .iter()
            .any(|ticket| Rc::ptr_eq(ticket, origin))
            || !contains_structure_row(origin, origin_row)
            || origin.replay.is_none()
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        Ok(MarkdownTableViewportRequiredRowQuery {
            publication: Rc::downgrade(&self.publication),
            origin: QueryOrigin::Structure(Rc::downgrade(origin)),
            header: origin.header.header_byte_start,
            origin_row,
            requested_row,
        })
    }

    /// Source 层凭同一不可变发布把真实结构位置封装成 opaque 行；不公开裸目录索引或 End。
    pub(in crate::markdown_document_buffer::table_window_request) fn required_publication_for(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<&RequiredPublication>, MarkdownTableRequestError> {
        self.check(document)?;
        Ok(self.required.as_ref())
    }

    /// End只关联本批真实末行；单个窗口没有End时也不能借另一表或旧发布的终点追加。
    pub(crate) fn required_end_for_window(
        &self,
        document: &MarkdownDocumentBuffer,
        window_index: usize,
    ) -> Result<Option<&VerifiedTableEnd>, MarkdownTableRequestError> {
        self.check(document)?;
        let window = self
            .windows
            .get(window_index)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if let Some(RequiredPublication::End {
            end,
            terminal: RequiredLocation::Window(index),
        }) = &self.required
            && *index == window_index
            && end.header_byte_start() == window.window.presentation.header_byte_range.start
        {
            return Ok(Some(end));
        }
        window.window.table_end_for(document)
    }
}

impl MarkdownTableViewportRequest {
    /// 同一最后publication、原context/selection上的单行扩展；旧Ready仍可读，主Global不回退。
    /// 远距目标可以从原D后cp逐step重放，最坏CPU线性；没有cp则typed拒绝，不重新Global(0)。
    pub fn extend_from_ready_with_required_row(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownTableViewport,
        viewport: Range<usize>,
        query: MarkdownTableViewportRequiredRowQuery,
    ) -> Result<(), MarkdownTableRequestError> {
        self.check_extension(document, previous, &viewport)?;
        if !query
            .publication
            .ptr_eq(&Rc::downgrade(&previous.publication))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let origin = match &query.origin {
            QueryOrigin::Window(identity) => previous
                .windows
                .iter()
                .find(|record| {
                    identity.ptr_eq(&Rc::downgrade(record))
                        && record.window.presentation.header_byte_range.start == query.header
                        && record.window.presentation.row(query.origin_row).is_some()
                })
                .map(|record| RequiredOrigin::Window(Rc::clone(record))),
            QueryOrigin::Structure(identity) => previous
                .structures
                .iter()
                .find(|ticket| {
                    identity.ptr_eq(&Rc::downgrade(ticket))
                        && ticket.header.header_byte_start == query.header
                        && contains_structure_row(ticket, query.origin_row)
                })
                .map(|ticket| RequiredOrigin::Structure(Rc::clone(ticket))),
        }
        .ok_or(MarkdownTableRequestError::PublicationSuperseded)?;
        let anchors = origin
            .replay()
            .ok_or(MarkdownTableRequestError::CoverageRequiresCheckpoint)?;
        // 在改变请求前构造纯续体；来源封闭且只克隆cp标量/同一spine，不申请源或历史正文。
        let replay = ReplayRequest::new(self.source.clone(), anchors.clone(), query.requested_row)?;
        self.extend_from_ready(document, previous, viewport)?;
        // 原previous仍全额预扣；此处另外预留一个回放状态与固定cursor工作区，不按每行重置预算。
        let metadata = std::mem::size_of::<RequiredState>() + 4096;
        if let Err(error) = self.quota.admit_metadata(metadata) {
            // 已开始本次扩展后失败必须sticky并释放scratch；previous及canonical正文不受影响。
            self.failed = Some(MarkdownTableRequestError::Window(error));
            self.structure = None;
            self.windows = Vec::new();
            self.structures = Vec::new();
            self.gaps = Vec::new();
            self.queries = None;
            return Err(MarkdownTableRequestError::Window(error));
        }
        self.required = Some(RequiredState {
            origin,
            requested_row: query.requested_row,
            resolved: false,
            end: None,
            replay: Some(replay),
        });
        if !matches!(self.phase, BatchPhase::Sweep) {
            // 无视觉新后缀也仍需完成目标；不能沿原prune路径直接发布缺失目标的Ready。
            self.phase = BatchPhase::RequiredRow;
            self.queries = Some(MarkdownTableReferenceQueries::for_batch(Some(
                self.quota.clone(),
            )));
        }
        Ok(())
    }

    /// 同时检查已共享Ready与本轮Sweep已准入的ticket；只认实际H/源行成员，不读context-only H。
    /// 返回实际检查的有限metadata项数，调用者把它并入同一1024控制预留。
    fn contains_prepared_row(&self, header: usize, ordinal: usize) -> (bool, usize) {
        let mut work = 0;
        for record in &self.windows {
            work += 1;
            if record.window.presentation.header_byte_range.start == header {
                for row in &record.window.presentation.rows {
                    work += 1;
                    if row.row_ordinal == ordinal && (ordinal != 0 || row.is_header) {
                        return (true, work);
                    }
                }
            }
        }
        for ticket in &self.structures {
            work += 1 + ticket.body.len();
            if ticket.header.header_byte_start == header && contains_structure_row(ticket, ordinal)
            {
                return (true, work);
            }
        }
        for job in &self.jobs {
            work += 1;
            let Some(ticket) = job.as_ref().and_then(|job| job.ticket.as_ref()) else {
                continue; // 只有仍持真实结构票的待投影job能承接本次目标。
            };
            if ticket.header.header_byte_start != header {
                continue;
            }
            if ordinal == 0 {
                work += 1;
                if ticket.include_header {
                    return (true, work); // H必须由该job真实发布，保留的结构checkpoint不能代替可见语义。
                }
            } else {
                for row in &ticket.body {
                    work += 1;
                    if row.row_ordinal == ordinal {
                        return (true, work);
                    }
                }
            }
        }
        (false, work)
    }

    pub(super) fn advance_required_row(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        let (header, target) = {
            let required = self
                .required
                .as_ref()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?;
            (required.origin.header(), required.requested_row)
        };
        let (prepared, lookup_work) = self.contains_prepared_row(header, target);
        let mut control = SWEEP_CONTROL_BASE + lookup_work;
        if control > SWEEP_CONTROL_RESERVE {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        if prepared {
            // Sweep可能刚准备同一目标。仅标已排入本批，不在投影前假签窗口索引或再回放一份。
            let required = self.required.as_mut().unwrap();
            required.resolved = true;
            required.replay = None;
            self.phase = BatchPhase::Prepare;
            self.index = 0;
            return Ok(SemanticWork {
                structural_transitions: control,
                ..SemanticWork::default()
            });
        }
        // 先结束原required借用，再调用全批查询/准入；不把跨phase的可变借用藏进helper。
        let (mut work, outcome) = {
            let required = self.required.as_mut().unwrap();
            let result = required
                .replay
                .as_mut()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?
                .advance(document, budget - SWEEP_CONTROL_RESERVE)?;
            if result.1.is_some() {
                required.resolved = true;
                required.replay = None;
            }
            result
        };
        if let Some(outcome) = outcome {
            match outcome {
                ReplayOutcome::Window(ticket) => {
                    let ticket = *ticket;
                    control += ticket.body.len() * SWEEP_ROW_ADMISSION_COST;
                    // 与视觉job同一query库存、同一累计额度；不让命令额外取得一份行/cell/run上限。
                    let cost = self.admit_ticket(&ticket)?;
                    if self.source_structures {
                        control += self.insert_required_structure(ticket);
                    } else {
                        self.job_costs.push(cost);
                        self.jobs.push(Some(self.semantic_job(ticket)));
                    }
                }
                ReplayOutcome::End { end, terminal } => {
                    let terminal_row = end.last_body_row_ordinal().unwrap_or(0);
                    let (exists, checked) =
                        self.contains_prepared_row(end.header_byte_start(), terminal_row);
                    control += checked;
                    if !exists {
                        // 真实末行可能也在Sweep的待投影job中。两处共用判据，避免End重复准入同一行。
                        let terminal = *terminal;
                        control += terminal.body.len() * SWEEP_ROW_ADMISSION_COST;
                        let cost = self.admit_ticket(&terminal)?;
                        if self.source_structures {
                            control += self.insert_required_structure(terminal);
                        } else {
                            self.job_costs.push(cost);
                            self.jobs.push(Some(self.semantic_job(terminal)));
                        }
                    }
                    // 真 End 由本次 RequiredPublication 保留并封入 terminal 行票；不替换已有可见多行票。
                    self.required.as_mut().unwrap().end = Some(end);
                }
            }
            self.phase = BatchPhase::Prepare;
            self.index = 0;
        }
        // 全批最多256行槽，record/job数量也包含在该上限内。两次最多各检查一份目录及实际行：
        // 每次<=384项，另128固定控制和至多3次行移交转换，合计<=899，保持在原1024预留内。
        // 未用reserve不记消耗；source/math子阶段仍只取得budget-1024，原总fuel不会重授。
        if control > SWEEP_CONTROL_RESERVE {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        work.structural_transitions += control;
        Ok(work)
    }

    pub(super) fn finish_required_publication(
        &mut self,
    ) -> Result<Option<RequiredPublication>, MarkdownTableRequestError> {
        let Some(required) = self.required.take() else {
            return Ok(None);
        };
        if !required.resolved {
            return Err(MarkdownTableRequestError::Window(Error::ContextPending));
        }
        let header = required.origin.header();
        if let Some(end) = required.end {
            let row = end.last_body_row_ordinal().unwrap_or(0);
            let terminal = self.required_location(header, row)?;
            return Ok(Some(RequiredPublication::End { end, terminal }));
        }
        let location = self.required_location(header, required.requested_row)?;
        Ok(Some(RequiredPublication::Row {
            location,
            row_ordinal: required.requested_row,
        }))
    }

    /// 只查已准入的真实成员；结构模式不为目录位置补一个完整窗口，也不按 ordinal 猜测来源。
    fn required_location(
        &self,
        header: usize,
        ordinal: usize,
    ) -> Result<RequiredLocation, MarkdownTableRequestError> {
        if self.source_structures {
            self.structures
                .iter()
                .position(|ticket| {
                    ticket.header.header_byte_start == header
                        && contains_structure_row(ticket, ordinal)
                })
                .map(RequiredLocation::Structure)
        } else {
            self.windows
                .iter()
                .position(|record| {
                    record.window.presentation.header_byte_range.start == header
                        && record.window.presentation.row(ordinal).is_some()
                })
                .map(RequiredLocation::Window)
        }
        .ok_or(MarkdownTableRequestError::Window(Error::ContextPending))
    }

    /// Replay 可以逆向返回较早行；仍按实际物理起点插入同一目录，供覆盖证明和后续 flow 线性消费。
    /// 只申请已由 admit_ticket 预扣的一个句柄槽；比较与 Rc 移动计入本次原 control，不另造索引。
    fn insert_required_structure(&mut self, ticket: MarkdownTableStructuralWindow) -> usize {
        let start = structure_range(&ticket, 0)
            .expect("原 Replay 只移交实际行或 H/D")
            .start;
        let mut inspected = 0;
        let index = self
            .structures
            .iter()
            .position(|existing| {
                inspected += 1;
                structure_range(existing, 0)
                    .expect("原结构目录只保真实成员")
                    .start
                    > start
            })
            .unwrap_or(self.structures.len());
        let moved = self.structures.len() - index;
        self.structures.reserve_exact(1);
        self.structures.insert(index, Rc::new(ticket));
        inspected + moved + 1
    }
}
