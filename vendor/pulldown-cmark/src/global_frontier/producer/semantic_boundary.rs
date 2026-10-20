// Copyright 2015 Google Inc. All rights reserved.
// 本地恢复能力只保真实闭合边界，沿原pulldown-cmark规则重放；MIT条款见LICENSE。

//! 同一个结构前沿的有限语义前探；主来源的恢复点不回退实际工作量或共享页预算。

use super::*;
use crate::block_frontier::TableCheckpoint;

/// 仅真实块/表行提交后可创建；完整位置保留CR与UTF8续态，spine只共享原预算和页。
pub(super) struct SemanticBoundary {
    position: Position,
    classified: (usize, usize),
    spine: StructuralSpineSnapshot,
    mode: Mode,
    begin_list_item: Option<usize>,
    last_line_blank: bool,
    table: Option<TableCheckpoint>,
    // Closed只来自原前沿真实EOF收尾，恢复时不能把它重建成尚待分类的Block。
    closed: bool,
}

// 恢复是一次固定控制操作，并结束有限游标/派生阶段及至多512页的指针链。
// 此费用属于状态机工作模型，不是allocator字节或实际CPU指令；预算不随源长度增长。
const RESET_WORK: usize = GLOBAL_CONTROL_WORK + PREFIX_WORKSPACE_WORK;

impl GlobalFrontier {
    /// 调用方放弃旧目标后，从真实源根重放同一前沿；不恢复旧游标，也不重发累计fuel。
    /// 活跃语义前探仍负有恢复义务，不能借重定位把它遗失；源根0则无需伪造旧块闭合票。
    pub fn restart_from_root(
        &mut self,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<GlobalWork, GlobalFrontierError> {
        self.semantic_authority(authority, budget)?;
        if self.semantic_boundary.is_some() {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        let work = self.semantic_work(RESET_WORK)?;
        let root = SemanticBoundary {
            position: Position::default(),
            classified: (0, 0),
            spine: self.spine.empty_shared(),
            mode: Mode::Block,
            begin_list_item: None,
            last_line_blank: false,
            table: None,
            closed: false,
        };
        // 原未发布阶段随明确的新目标结束；旧Fact的独立scope与共享预算仍由其真实owner持有。
        self.install_semantic_boundary(root);
        Ok(work)
    }

    /// 在真实闭合块、已提交表行或真实EOF终点保留唯一恢复点；不复制正文或祖先数组。
    /// 非边界返回None且不改变前沿，调用方必须继续真实结构工作，不能把None当作空语义。
    /// 引用Fact后的尾行仍在解释中，不属于Block边界；旧恢复点也绝不被新请求覆盖。
    pub fn retain_semantic_boundary(
        &mut self,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<Option<GlobalWork>, GlobalFrontierError> {
        self.semantic_authority(authority, budget)?;
        if self.semantic_boundary.is_some() {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        let table = match self.phase {
            Phase::Table => match self
                .table
                .as_ref()
                .and_then(TableContinuationState::checkpoint)
            {
                Some(table) => Some(table),
                None => return Ok(None), // 活动行没有提交点，不能据lookahead封存其起点。
            },
            Phase::Ready(Ready::Begin)
                if matches!(self.mode, Mode::Block)
                    && self.inline_block.is_none()
                    && self.leaf_block.is_none()
                    && self.table.is_none()
                    && self.reference.is_none()
                    && self.reference_fact.is_none()
                    && self.inline_fact.is_none()
                    && self.leaf_fact.is_none()
                    && self.prefix.is_none() =>
            {
                None
            }
            Phase::Closed
                if self.position.byte == self.source_end
                    && self.position.utf8_left == 0
                    && self.spine.is_empty()
                    && self.inline_block.is_none()
                    && self.leaf_block.is_none()
                    && self.table.is_none()
                    && self.reference.is_none()
                    && self.reference_fact.is_none()
                    && self.inline_fact.is_none()
                    && self.leaf_fact.is_none()
                    && self.prefix.is_none() =>
            {
                None // 已完成的主来源也需语义前探；真实EOF事实随原恢复点保留。
            }
            _ => return Ok(None), // Paragraph/Opaque/Reference continuation必须沿其原协议闭合。
        };
        let work = self.semantic_work(GLOBAL_CONTROL_WORK)?;
        self.semantic_boundary = Some(SemanticBoundary {
            position: self.position,
            classified: self.classified,
            spine: self.spine.clone(),
            mode: self.mode,
            begin_list_item: self.begin_list_item,
            last_line_blank: self.last_line_blank,
            table,
            closed: matches!(self.phase, Phase::Closed),
        });
        Ok(Some(work))
    }

    /// 仅已有主恢复点时，让同一前沿从真实源根重新分类语义上下文。
    /// 空根继续共享原SpineBudget；前探/重放工作累计，已发Fact和主恢复点的scope保持有效。
    pub fn rewind_semantic_source_to_start(
        &mut self,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<GlobalWork, GlobalFrontierError> {
        self.semantic_authority(authority, budget)?;
        if self.semantic_boundary.is_none() {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        let work = self.semantic_work(RESET_WORK)?;
        let root = SemanticBoundary {
            position: Position::default(),
            classified: (0, 0),
            spine: self.spine.empty_shared(),
            mode: Mode::Block,
            begin_list_item: None,
            last_line_blank: false,
            table: None,
            closed: false,
        };
        self.install_semantic_boundary(root);
        Ok(work)
    }

    /// 语义配对完成后一次性恢复主来源；前探的引用/表格/未交付Fact只归还，不交给主消费者。
    /// 调用方须先结束原SourceFrame并核实际源健康，revision本身不能替代DocRoot权限。
    pub fn restore_semantic_boundary(
        &mut self,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<GlobalWork, GlobalFrontierError> {
        self.semantic_authority(authority, budget)?;
        if self.semantic_boundary.is_none() {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        let work = self.semantic_work(RESET_WORK)?;
        let boundary = self
            .semantic_boundary
            .take()
            .expect("原恢复点已在同次借用中核验");
        self.install_semantic_boundary(boundary);
        Ok(work)
    }

    /// 只查询原owner是否仍持恢复义务；不把位置或可伪造的恢复载荷暴露给调用方。
    pub fn semantic_boundary_retained(&self) -> bool {
        self.semantic_boundary.is_some()
    }

    /// 取消和代次失效为sticky首因，必须同时撤恢复点；错误预算仍可用合法值重试。
    fn semantic_authority(
        &mut self,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<(), GlobalFrontierError> {
        let cause = self.failure.or_else(|| {
            if authority.cancelled {
                Some(GlobalFrontierError::Cancelled)
            } else if authority.current_revision != self.revision {
                Some(GlobalFrontierError::SourceSuperseded)
            } else {
                self.spine_error.take().map(GlobalFrontierError::Spine)
            }
        });
        if let Some(cause) = cause {
            self.semantic_boundary = None;
            self.leaf_block = None;
            self.leaf_fact = None;
            self.failure = Some(cause);
            return Err(cause);
        }
        if !(2048..=MAX_LINE_START_STEP).contains(&budget) {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        Ok(())
    }

    /// 只收取本次真实控制操作；历史byte/transition/SpineBudget一律不随位置恢复。
    fn semantic_work(&mut self, transitions: usize) -> Result<GlobalWork, GlobalFrontierError> {
        let next = self
            .work
            .transitions
            .checked_add(transitions)
            .ok_or(GlobalFrontierError::ArithmeticOverflow);
        let result = next.and_then(|next| {
            if self.work.byte_reads > self.limits.max_byte_work
                || next > self.limits.max_transitions
            {
                Err(GlobalFrontierError::WorkFuel)
            } else {
                self.work.transitions = next;
                Ok(GlobalWork {
                    byte_reads: 0,
                    transitions,
                })
            }
        });
        if let Err(cause) = result {
            self.semantic_boundary = None;
            self.failure = Some(cause);
        } // 真实fuel失败不可恢复旧边界后继续；未成功的操作不假增实际工作。
        result
    }

    /// 只安装内部真实恢复载荷；所有前探临时阶段在装回原spine前归还，没有第二初始化图。
    fn install_semantic_boundary(&mut self, boundary: SemanticBoundary) {
        // 先封闭前探阶段并结束其临时scope；这些owner只含派生游标，不移交给主来源消费者。
        self.phase = Phase::Closed;
        self.prefix = None;
        self.table = None;
        self.reference = None;
        self.reference_fact = None;
        self.reference_line = None;
        self.reference_spine = None;
        self.inline_block = None;
        self.inline_fact = None;
        self.leaf_block = None;
        self.leaf_fact = None;
        // 原位置、分类水位与容器事实一起恢复；前探到达的更远水位不再有主来源发布权。
        self.position = boundary.position;
        self.classified = boundary.classified;
        self.spine = boundary.spine;
        self.mode = boundary.mode;
        self.begin_list_item = boundary.begin_list_item;
        self.last_line_blank = boundary.last_line_blank;
        // 下一Begin沿原规则重建LineStart/tab状态；保留前探行状态会把后继prefix接到错误origin。
        self.origin = boundary.position.byte;
        self.state = LineStartState::default();
        self.saved = LineStartState::default();
        self.container_saved = LineStartState::default();
        self.matched = 0;
        self.outer_indent = 0;
        self.indent = 0;
        // 候选头、引用和marker仅属于前探阶段；清标量避免后继真实解析读到旧候选残留。
        self.header = None;
        self.header_line = 0;
        self.header_physical_start = 0;
        self.separator = None;
        self.metadata_start = 0;
        self.metadata_marker = 0;
        self.interrupt_line = None;
        self.header_interrupt = false;
        self.opaque_body_state = LineStartState::default();
        self.header_origin = 0;
        self.header_state = LineStartState::default();
        self.reference_origin = 0;
        self.reference_state = LineStartState::default();
        self.prefix_last = None;
        self.footnote_interrupt = false;
        self.step_prefix_workspace_work = 0;
        self.phase = match boundary.table {
            Some(table) => {
                self.table = Some(TableContinuationState::resume(table));
                Phase::Table
            }
            None if boundary.closed => Phase::Closed,
            None => Phase::Ready(Ready::Begin),
        }; // 只有原已提交表行才能恢复Table；引用尾、活动行和半个Leaf没有这种内部载荷。
    }
}

#[cfg(test)]
mod tests;
