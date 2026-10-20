// Copyright 2015 Google Inc. All rights reserved.
// 物理行来源沿固定 pulldown-cmark 0.13.4 paragraph 路径；完整 MIT 条款见 LICENSE。

//! 原闭合 Inline/Leaf 共用逐物理行内核；只发布源坐标，不授予样式、Ready或省略正文权限。

use std::ops::Range;

use super::{
    ContainerPrefix, GlobalFrontierError, GlobalSourceAuthority, GlobalWork, InlineBlockKind,
    LeafBlockKind, PrefixProgress, VerifiedGlobalInlineBlock, VerifiedGlobalLeafBlock,
};
use crate::block_frontier::{SourceFrame, MAX_FRONTIER_STEP_BYTES};
use crate::line_start_stream::{
    LineStartFrame, LineStartOperation, LineStartProgress, LineStartState, LineStartStream,
};

const MIN_STEP: usize = 2048;
// 具名逻辑控制费不等于 CPU 指令测量；工作区预留只有真实初始化/释放时才记为消费。
const CONTROL_WORK: usize = 128;
const PREFIX_WORKSPACE_WORK: usize = 1024;

#[derive(Clone, Copy, Debug)]
/// 同一游标从首行到终行累计的独立上限；advance 与后继行均不得重新发放总 fuel。
pub struct BlockOriginLimits {
    pub max_byte_work: usize,
    pub max_transitions: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 四个 LineStart 标量仍相对其实际 source_origin，不能把 ix 当作归一化文本偏移。
pub struct BlockOriginState {
    pub source_origin: usize,
    pub relative_byte: usize,
    pub tab_start: usize,
    pub spaces_remaining: usize,
    pub min_hrule_offset: usize,
}

impl BlockOriginState {
    fn capture(source_origin: usize, state: LineStartState) -> Self {
        Self {
            source_origin,
            relative_byte: state.ix,
            tab_start: state.tab_start,
            spaces_remaining: state.spaces_remaining,
            min_hrule_offset: state.min_hrule_offset,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 首行直接消费真实 opening 后的 capture；续行才重新匹配原容器与 paragraph 空白。
pub enum BlockOriginLineKind {
    First,
    Continuation,
}

/// 行的真实块种类随原票保留；Leaf不能伪装成Paragraph取得另一个语法入口。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockOriginKind {
    Inline(InlineBlockKind),
    Leaf(LeafBlockKind),
}

/// 两种真正闭合的source owner共享原spine和行算法，不建立第二套正文或可构造的范围协议。
enum OriginBlock {
    Inline(VerifiedGlobalInlineBlock),
    Leaf(VerifiedGlobalLeafBlock),
}

impl OriginBlock {
    fn revision(&self) -> u64 {
        match self {
            Self::Inline(block) => block.revision,
            Self::Leaf(block) => block.source_revision(),
        }
    }
    fn source_end(&self) -> usize {
        match self {
            Self::Inline(block) => block.source_end,
            Self::Leaf(block) => block.source_end(),
        }
    }
    fn start(&self) -> usize {
        match self {
            Self::Inline(block) => block.start,
            Self::Leaf(block) => block.source_range().start,
        }
    }
    fn end(&self) -> usize {
        match self {
            Self::Inline(block) => block.end,
            Self::Leaf(block) => block.source_range().end,
        }
    }
    fn lexical_end(&self) -> usize {
        match self {
            Self::Inline(block) => block.lexical_end,
            Self::Leaf(block) => block.source_range().end,
        }
    }
    fn options(&self) -> crate::Options {
        match self {
            Self::Inline(block) => block.options,
            Self::Leaf(block) => block.options(),
        }
    }
    fn first_state_origin(&self) -> usize {
        match self {
            Self::Inline(block) => block.first_state_origin,
            Self::Leaf(block) => block.first_state_origin,
        }
    }
    fn spine(&self) -> &crate::structural_spine::StructuralSpineSnapshot {
        match self {
            Self::Inline(block) => &block.spine,
            Self::Leaf(block) => &block.spine,
        }
    }
    fn kind(&self) -> BlockOriginKind {
        match self {
            Self::Inline(block) => BlockOriginKind::Inline(block.kind),
            Self::Leaf(block) => BlockOriginKind::Leaf(block.kind()),
        }
    }
    fn terminal_matches(&self, content_end: usize, line_end: usize) -> bool {
        match self {
            Self::Inline(block) => {
                content_end == block.last_content_end && line_end == block.last_line_end
            }
            Self::Leaf(block) => line_end == block.source_range().end,
        }
    }
    fn continuation_space(&self) -> usize {
        match self {
            Self::Inline(_) => 4,
            Self::Leaf(block) => match block.details() {
                super::LeafBlockDetails::FencedCode { indent, .. } => *indent,
                super::LeafBlockDetails::IndentedCode { .. } => 4,
                _ => 0,
            },
        }
    }
    fn paragraph_space(&self) -> bool {
        matches!(self, Self::Inline(_))
    }
    fn has_closed_line_end(&self) -> bool {
        matches!(self, Self::Leaf(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 只含绝对源 byte 元数据。终行 EOL 仍是真实 separator，不代表最终 Soft/HardBreak。
/// Setext underline 不属于本记录；终行 kind 与 block_kind 也不授予 inline 配对权限。
pub struct BlockOriginLine {
    // 同一真实闭合块的固定归属随行交付；语义游标不得拿另一块相似的 prefix/content 续权。
    pub(super) revision: u64,
    pub(super) source_end: usize,
    pub(super) block_source: Range<usize>,
    physical: Range<usize>,
    prefix: Range<usize>,
    content: Range<usize>,
    eol: Range<usize>,
    ordinal: usize,
    kind: BlockOriginLineKind,
    terminal: bool,
    block_kind: BlockOriginKind,
    matched_depth: usize,
    total_depth: usize,
    container_state: BlockOriginState,
    content_state: BlockOriginState,
}

impl BlockOriginLine {
    pub fn physical_range(&self) -> Range<usize> {
        self.physical.clone()
    }
    pub fn prefix_range(&self) -> Range<usize> {
        self.prefix.clone()
    }
    pub fn content_range(&self) -> Range<usize> {
        self.content.clone()
    }
    pub fn eol_range(&self) -> Range<usize> {
        self.eol.clone()
    }
    pub fn line_ordinal(&self) -> usize {
        self.ordinal
    }
    pub fn kind(&self) -> BlockOriginLineKind {
        self.kind
    }
    pub fn is_terminal(&self) -> bool {
        self.terminal
    }
    pub fn block_kind(&self) -> BlockOriginKind {
        self.block_kind
    }
    pub fn matched_depth(&self) -> usize {
        self.matched_depth
    }
    pub fn total_depth(&self) -> usize {
        self.total_depth
    }
    /// First 返回真实 opening 后原状态；Continuation 返回容器匹配结束时的原状态。
    pub fn container_state(&self) -> BlockOriginState {
        self.container_state
    }
    /// First 与 container_state 相同；Continuation 额外经过原 Space(4) 与 AllSpace。
    pub fn content_state(&self) -> BlockOriginState {
        self.content_state
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlockOriginStep {
    Progress {
        next_byte: usize,
        work: GlobalWork,
    },
    NeedInput {
        next_byte: usize,
        work: GlobalWork,
    },
    Line {
        line: BlockOriginLine,
        work: GlobalWork,
    },
    Complete {
        work: GlobalWork,
    },
}

#[derive(Clone, Copy)]
enum Phase {
    First,
    BeginContinuation,
    Containers,
    Space,
    AllSpace,
    Content,
    AfterCr { content_end: usize },
    Complete,
    Failed,
}

/// 独占一个真实 capture，另保一个固定 prefix 游标与一个 LineStart 续体；
/// 没有 source/整行 String、角色 Vec、行集合或 source/root 强 owner。
/// Core 仍必须绑定实际 DocRoot Weak；本 vendor 只能核 source revision 与真实源尾。
pub struct BlockOriginCursor {
    block: OriginBlock,
    limits: BlockOriginLimits,
    total: GlobalWork,
    phase: Phase,
    prefix: Option<ContainerPrefix>,
    operation: Option<LineStartStream>,
    physical_start: usize,
    content_start: usize,
    next: usize,
    ordinal: usize,
    matched: usize,
    container_state: LineStartState,
    content_state: LineStartState,
    failure: Option<GlobalFrontierError>,
    allocated: usize,
}

impl std::fmt::Debug for BlockOriginCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不沿共享页递归展开，也不读取正文生成诊断。
        formatter
            .debug_struct("BlockOriginCursor")
            .field("revision", &self.block.revision())
            .field("next_byte", &self.next_byte())
            .field("line_ordinal", &self.ordinal)
            .field("scope_depth", &self.block.spine().len())
            .field("work", &self.total)
            .field("failure", &self.failure)
            .finish()
    }
}

impl BlockOriginCursor {
    pub(super) fn from_verified(
        block: VerifiedGlobalInlineBlock,
        limits: BlockOriginLimits,
    ) -> Result<Self, GlobalFrontierError> {
        if limits.max_byte_work == 0 || limits.max_transitions < MIN_STEP {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        if block.physical_start > block.start
            || block.start > block.last_content_end
            || block.last_content_end > block.last_line_end
            || block.last_line_end != block.lexical_end
            || block.lexical_end > block.end
            || block.end > block.source_end
            || block.first_state_origin.checked_add(block.first_state.ix) != Some(block.start)
            || block.first_state.tab_start > block.first_state.ix
            || block.first_state.spaces_remaining > 3
        {
            // 只有真实 factory 能进入；内部 capture 失配仍拒绝，不通过重扫 opening 修补。
            return Err(GlobalFrontierError::InvalidFrame);
        }
        Ok(Self {
            physical_start: block.physical_start,
            content_start: block.start,
            next: block.start,
            ordinal: 0,
            matched: block.spine.len(),
            container_state: block.first_state,
            content_state: block.first_state,
            block: OriginBlock::Inline(block),
            limits,
            total: GlobalWork::default(),
            phase: Phase::First,
            prefix: None,
            operation: None,
            failure: None,
            allocated: 0,
        })
    }

    /// 只有同producer的真实Leaf能签发；首行状态直接继承opening，不重扫容器或按字节数猜Tab列。
    pub(super) fn from_leaf(
        block: &VerifiedGlobalLeafBlock,
        limits: BlockOriginLimits,
    ) -> Result<Self, GlobalFrontierError> {
        let source = block.source_range();
        if limits.max_byte_work == 0 || limits.max_transitions < MIN_STEP {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        if block.physical_start() > source.start
            || source.start > source.end
            || source.end > block.source_end()
            || block.first_state_origin.checked_add(block.first_state.ix) != Some(source.start)
            || block.first_state.tab_start > block.first_state.ix
        {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        Ok(Self {
            physical_start: block.physical_start(),
            content_start: source.start,
            next: source.start,
            ordinal: 0,
            matched: block.scope_depth(),
            container_state: block.first_state,
            content_state: block.first_state,
            block: OriginBlock::Leaf(block.clone()),
            limits,
            total: GlobalWork::default(),
            phase: Phase::First,
            prefix: None,
            operation: None,
            failure: None,
            allocated: 0,
        })
    }

    pub fn next_byte(&self) -> usize {
        // 活动阶段从真实子游标借当前位置；失败转换可能已经移走子对象，此时只返回保留的有限标量。
        // 该诊断位置不代表完成或续权，失败入口仍先返回固定首因。
        match self.phase {
            Phase::Containers => self
                .prefix
                .as_ref()
                .map_or(self.next, ContainerPrefix::next_byte),
            Phase::Space | Phase::AllSpace => self
                .operation
                .as_ref()
                .map_or(self.next, LineStartStream::next_byte),
            _ => self.next,
        }
    }

    /// 记录每条真实 prefix 临时目录的实际新增容量；归还旧目录不会重置原请求的累计费用。
    pub fn allocated_workspace_bytes(&self) -> usize {
        self.allocated
    }

    pub fn total_work(&self) -> GlobalWork {
        self.total
    }

    /// 诊断只说明正文缓冲为零；固定游标、共享页与调用方保存记录的容量仍须另外准入。
    pub fn buffered_source_capacity(&self) -> usize {
        0
    }

    fn fail(&mut self, error: GlobalFrontierError) -> GlobalFrontierError {
        // 先保留仍存在的子游标位置；operation 已在失败转换中移走时也不得 expect 或重建扫描器。
        self.next = self.next_byte();
        // 独立失败 tombstone 与真实 Complete 分开，清子资源后 getter/Debug 不再依赖活动阶段不变量。
        self.phase = Phase::Failed;
        self.failure = Some(error);
        self.prefix = None;
        self.operation = None;
        error
    }

    /// 每次只交付一条真实物理行；frame 可切断 CRLF/UTF-8，只有 source_end 才能声明 EOF。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<BlockOriginStep, GlobalFrontierError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if authority.cancelled {
            return Err(self.fail(GlobalFrontierError::Cancelled));
        }
        if authority.current_revision != self.block.revision() {
            return Err(self.fail(GlobalFrontierError::SourceSuperseded));
        }
        if !(MIN_STEP..=MAX_FRONTIER_STEP_BYTES).contains(&budget) {
            return Err(GlobalFrontierError::InvalidBudget); // 参数错误未消费源，可用合法预算重试。
        }
        // 与 GlobalFrontier 一样，未消费的 frame 准入错误可由调用方修正后重试；
        // 此处尚未改变状态、工作账本或输出，假 EOF 不会成为真实终行权限。
        // 已取消、源 authority 撤销及内部/fuel 失败则在各自入口保存 sticky 首因。
        crate::source_frontier::admitted_bytes(
            &frame,
            self.block.revision(),
            self.block.source_end(),
            self.next_byte(),
            budget,
        )?;
        if matches!(self.phase, Phase::Complete) {
            return Ok(BlockOriginStep::Complete {
                work: GlobalWork::default(),
            });
        }
        match self.advance_inner(frame, budget) {
            Ok(step) => Ok(step),
            Err(error) => Err(self.fail(error)),
        }
    }

    fn advance_inner(
        &mut self,
        frame: SourceFrame<'_>,
        budget: usize,
    ) -> Result<BlockOriginStep, GlobalFrontierError> {
        let remaining_transitions = self
            .limits
            .max_transitions
            .saturating_sub(self.total.transitions);
        let remaining_bytes = self
            .limits
            .max_byte_work
            .saturating_sub(self.total.byte_reads);
        let step_fuel = remaining_transitions.min(budget);
        if step_fuel < MIN_STEP || remaining_bytes == 0 {
            return Err(GlobalFrontierError::WorkFuel);
        }
        // 原 prefix 每次真实 role 访问同时记共享账本，故预留四分之一交给子操作，
        // 再把实际共享 role 增量纳入本游标累计账本；没有每行重设任一总上限。
        // 一阶段最多一个固定 prefix 工作区事件，先为该最坏情况与标量控制费共同准入。
        // 最小2048仍可给原共享操作224 fuel；未使用的1024不能在零字节阶段反复累计。
        let child_fuel = (step_fuel - PREFIX_WORKSPACE_WORK - CONTROL_WORK) / 4;
        let byte_fuel = remaining_bytes.min(child_fuel);
        let spine_before = self.block.spine().work_used();
        let mut work = GlobalWork {
            byte_reads: 0,
            transitions: CONTROL_WORK,
        };
        let mut workspace_work = 0;
        let mut need_input = false;
        let mut line = None;
        match self.phase {
            Phase::First => {
                // 真实首行已经在 Global 路径完成 opening；直接从 start 读取正文，
                // physical_start..start 与原 tab 状态均来自同一次 capture，禁止 continuation 重扫。
                self.phase = Phase::Content;
            }
            Phase::BeginContinuation => {
                if self.next >= self.block.lexical_end() {
                    return Err(GlobalFrontierError::InvalidFrame);
                }
                if self.prefix.is_some() {
                    // 不允许隐式覆盖仍活的旧工作区；否则一次准入会掩盖真实释放加创建两笔费用。
                    return Err(GlobalFrontierError::InvalidFrame);
                }
                self.physical_start = self.next;
                record_workspace_operation(&mut workspace_work)?;
                self.prefix = Some(ContainerPrefix::new(
                    self.block.revision(),
                    self.physical_start,
                    self.block.source_end(),
                    self.block.spine(),
                    self.block.options(),
                ));
                self.phase = Phase::Containers;
            }
            Phase::Containers => {
                let prefix = self.prefix.as_mut().expect("真实 prefix 已创建");
                let before = prefix.allocated_workspace_bytes();
                let result = prefix.advance(&frame, byte_fuel, child_fuel);
                self.allocated += prefix.allocated_workspace_bytes() - before;
                let (progress, child) = result?;
                work.byte_reads += child.byte_reads;
                work.transitions += child.transitions;
                match progress {
                    PrefixProgress::Complete { state, matched } => {
                        self.container_state = state;
                        self.matched = matched;
                        record_workspace_operation(&mut workspace_work)?;
                        self.prefix = None;
                        self.start_space(
                            state,
                            LineStartOperation::Space(self.block.continuation_space()),
                            Phase::Space,
                        )?;
                    }
                    PrefixProgress::Progress => (),
                    PrefixProgress::NeedInput { .. } => need_input = true,
                }
            }
            Phase::Space | Phase::AllSpace => {
                let all_space = matches!(self.phase, Phase::AllSpace);
                let step = self.operation.as_mut().expect("原空白续体已创建").advance(
                    LineStartFrame {
                        source_revision: frame.source_revision,
                        byte_start: frame.byte_start,
                        bytes: frame.bytes,
                        real_eof: frame.real_eof,
                    },
                    byte_fuel,
                    child_fuel,
                )?;
                work.byte_reads += step.work.byte_reads;
                work.transitions += step.work.transitions;
                match step.progress {
                    LineStartProgress::Complete { state, .. } => {
                        self.operation = None;
                        if all_space || !self.block.paragraph_space() {
                            // 原 parse_paragraph 在 scan_space(4) 的部分消费结果上再 scan_all_space；
                            // 未匹配完的容器仍是 lazy continuation，matched 不被改成总深度。
                            self.content_state = state;
                            self.content_start = self
                                .physical_start
                                .checked_add(state.ix)
                                .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
                            self.next = self.content_start;
                            self.phase = Phase::Content;
                        } else {
                            self.start_space(state, LineStartOperation::AllSpace, Phase::AllSpace)?;
                        }
                    }
                    LineStartProgress::Progress { .. } => (),
                    LineStartProgress::NeedInput { .. } => need_input = true,
                }
            }
            Phase::Content => {
                let limit = frame.bytes.len().min(byte_fuel).min(child_fuel);
                for &byte in &frame.bytes[..limit] {
                    if self.next == self.block.lexical_end() && self.block.has_closed_line_end() {
                        break; // 真Leaf closure允许停在closer尾，不能读取后继物理行前缀或虚构其EOF。
                    }
                    // 本层只识别 ASCII EOL；Unicode byte 保留绝对源坐标，不复制、归一化或切片为 str。
                    if self.next >= self.block.lexical_end() {
                        return Err(GlobalFrontierError::InvalidFrame);
                    }
                    work.byte_reads += 1;
                    work.transitions += 1;
                    let offset = self.next;
                    self.next = self
                        .next
                        .checked_add(1)
                        .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
                    match byte {
                        b'\r' => {
                            self.phase = Phase::AfterCr {
                                content_end: offset,
                            };
                            break; // 必须读取下一源 byte 或真实 EOF，不能把 frame 尾当独立 CR。
                        }
                        b'\n' => {
                            line = Some(self.finish_line(offset, self.next)?);
                            break;
                        }
                        _ => (), // 正文仍在原始源；不为跨 frame scalar 保存第二份字符缓冲。
                    }
                }
                if matches!(self.phase, Phase::Content) {
                    if self.next == self.block.lexical_end() && self.block.has_closed_line_end() {
                        // Leaf闭合末端来自原producer，可在显式closer尾处结束原行；这不是frame尾或伪EOF。
                        line = Some(self.finish_line(self.next, self.next)?);
                    } else {
                        let consumed = self.next - frame.byte_start;
                        if consumed == frame.bytes.len() {
                            if frame.real_eof {
                                // 无 EOL 的最后一行只由真实 source_end EOF 关闭；lexical_end 不是伪 EOF。
                                line = Some(self.finish_line(self.next, self.next)?);
                            } else {
                                need_input = true;
                            }
                        }
                    }
                }
            }
            Phase::AfterCr { content_end } => {
                match frame.bytes.first() {
                    Some(&byte) => {
                        work.byte_reads += 1;
                        work.transitions += 1;
                        if byte == b'\n' {
                            self.next = self
                                .next
                                .checked_add(1)
                                .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
                        }
                        // 非 LF 仅是 lookahead，没有消费；后继行仍从同一个 next 借回原字节。
                        line = Some(self.finish_line(content_end, self.next)?);
                    }
                    None if frame.real_eof => {
                        line = Some(self.finish_line(content_end, self.next)?)
                    }
                    None => need_input = true, // 分帧 CR/LF 尚未判定，保持未提交状态。
                }
            }
            Phase::Complete => unreachable!("公开入口先处理已完成状态"),
            Phase::Failed => unreachable!("公开入口先返回固定失败首因"),
        }
        let role_work = self
            .block
            .spine()
            .work_used()
            .checked_sub(spine_before)
            .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
        work.transitions = work
            .transitions
            .checked_add(workspace_work)
            .and_then(|work| work.checked_add(role_work))
            .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
        self.total.byte_reads = self
            .total
            .byte_reads
            .checked_add(work.byte_reads)
            .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
        self.total.transitions = self
            .total
            .transitions
            .checked_add(work.transitions)
            .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
        if work.byte_reads > budget
            || work.transitions > budget
            || self.total.byte_reads > self.limits.max_byte_work
            || self.total.transitions > self.limits.max_transitions
        {
            return Err(GlobalFrontierError::WorkFuel); // 失败时本次 line 不会移交给调用方。
        }
        if let Some(line) = line {
            Ok(BlockOriginStep::Line { line, work })
        } else if need_input {
            Ok(BlockOriginStep::NeedInput {
                next_byte: self.next_byte(),
                work,
            })
        } else {
            Ok(BlockOriginStep::Progress {
                next_byte: self.next_byte(),
                work,
            })
        }
    }

    fn start_space(
        &mut self,
        state: LineStartState,
        operation: LineStartOperation,
        phase: Phase,
    ) -> Result<(), GlobalFrontierError> {
        self.operation = Some(LineStartStream::new(
            self.block.revision(),
            self.physical_start,
            self.block.source_end(),
            state,
            operation,
        )?);
        self.phase = phase;
        Ok(())
    }

    fn finish_line(
        &mut self,
        content_end: usize,
        line_end: usize,
    ) -> Result<BlockOriginLine, GlobalFrontierError> {
        let terminal = line_end == self.block.lexical_end();
        if self.physical_start > self.content_start
            || self.content_start > content_end
            || content_end > line_end
            || line_end > self.block.lexical_end()
            || (terminal && !self.block.terminal_matches(content_end, line_end))
        {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        let origin = if self.ordinal == 0 {
            self.block.first_state_origin()
        } else {
            self.physical_start
        };
        let line = BlockOriginLine {
            revision: self.block.revision(),
            source_end: self.block.source_end(),
            block_source: self.block.start()..self.block.end(),
            physical: self.physical_start..line_end,
            prefix: self.physical_start..self.content_start,
            content: self.content_start..content_end,
            eol: content_end..line_end,
            ordinal: self.ordinal,
            kind: if self.ordinal == 0 {
                BlockOriginLineKind::First
            } else {
                BlockOriginLineKind::Continuation
            },
            terminal,
            block_kind: self.block.kind(),
            matched_depth: self.matched,
            total_depth: self.block.spine().len(),
            container_state: BlockOriginState::capture(origin, self.container_state),
            content_state: BlockOriginState::capture(origin, self.content_state),
        };
        self.ordinal = self
            .ordinal
            .checked_add(1)
            .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
        self.phase = if terminal {
            Phase::Complete
        } else {
            Phase::BeginContinuation
        };
        Ok(line)
    }
}

fn record_workspace_operation(work: &mut usize) -> Result<(), GlobalFrontierError> {
    if *work != 0 {
        // 当前单阶段只准入一次真实工作区操作；未来双操作不得静默丢费或超出原步上限。
        return Err(GlobalFrontierError::InvalidFrame);
    }
    *work = PREFIX_WORKSPACE_WORK;
    Ok(())
}

#[cfg(test)]
#[path = "inline_origin/tests.rs"]
mod tests;
