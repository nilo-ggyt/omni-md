// Copyright 2015 Google Inc. All rights reserved.
// 真实闭合块的有限 Plain 证明沿固定 pulldown-cmark 0.13.4；完整 MIT 条款见 LICENSE。

//! 保留原 Fact 的 opaque 语义来源；仅证明 Root 纯文本前缀，不代替 Core 样式 publication。

use std::ops::Range;

use super::{
    GlobalFrontierError, GlobalSourceAuthority, GlobalWork, InlineBlockKind,
    VerifiedGlobalInlineBlock,
};
use crate::block_frontier::{SourceFrame, MAX_FRONTIER_STEP_BYTES};
use crate::structural_spine::{
    StructuralCursorStep, StructuralKind, StructuralSpineCursor, StructuralSpineSnapshot,
};
use crate::{HeadingLevel, Options};

const MIN_STEP: usize = 2048;
const CONTROL_WORK: usize = 128;
const WORKSPACE_WORK: usize = 1024;

#[path = "inline_code.rs"]
mod code;
pub use code::{
    InlineCodeCursor, InlineCodeStep, InlineLinkDestinationDecoder, InlineLinkDestinationStep,
    VerifiedInlineCodeSpan, VerifiedInlineEmphasisRun, VerifiedInlineLink,
};

/// Plain 与 ATX 共用原 special-bytes 表及逐 byte 规则；候选只要求完整 inline，绝不自行配对。
pub(super) fn scan_plain_bytes(
    bytes: &[u8],
    start: usize,
    last_content_end: usize,
    lut: &[bool; 256],
    tail_start: &mut Option<usize>,
) -> (usize, bool) {
    for (offset, &byte) in bytes.iter().enumerate() {
        let at = start + offset;
        if matches!(byte, b'\r' | b'\n') {
            // 原 Plain 终行判断需要保留尾空白；真实 ATX 正文边界由原 Leaf 先确定。
            if at < last_content_end {
                *tail_start = None;
            }
        } else if lut[usize::from(byte)] {
            return (offset + 1, true); // 该候选 byte 已实际读取并计费，不能发布它后面的 Plain 权限。
        } else if crate::scanners::is_ascii_whitespace_no_nl(byte) {
            tail_start.get_or_insert(at);
        } else {
            *tail_start = None;
        }
    }
    (bytes.len(), false)
}

#[derive(Clone, Debug)]
/// 只由原闭合 Fact 配对 factory 创建；不接受 Range、外部 scope 或归一化正文来制造权限。
pub struct VerifiedInlineSemanticSeed {
    pub(crate) revision: u64,
    pub(crate) source_end: usize,
    pub(crate) options: Options,
    pub(crate) kind: InlineBlockKind,
    pub(crate) physical_start: usize,
    // 真 Paragraph/Setext 的首行已由 opening 消费；Cell 没有这种块前缀事实。
    pub(crate) opening: Option<(usize, crate::line_start_stream::LineStartState)>,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) lexical_end: usize,
    pub(crate) last_content_end: usize,
    pub(crate) last_line_end: usize,
    pub(crate) spine: StructuralSpineSnapshot,
    pub(crate) cell: Option<TableCellInlineContext>,
}

#[derive(Clone, Copy, Debug)]
/// 只有原表格格扫描能创建；原行入口经过前列真实扫描后保留的数学状态不能在短串解析时重置。
pub(crate) struct TableCellInlineContext {
    pub(crate) header: usize,
    pub(crate) row: usize,
    pub(crate) column: usize,
    pub(crate) math: crate::math_context::MathContext,
}

impl VerifiedInlineSemanticSeed {
    /// 有限完整块的原 Parser 入口使用本 Fact 的真实选项；调用方不能补造容器或开启另一套语法。
    pub fn options(&self) -> Options {
        self.options
    }

    /// 仅真实 cell seed 有表身份；Root 块不会因相同范围或字符串被解释成表格。
    pub fn table_cell_coordinate(&self) -> Option<(usize, usize, usize)> {
        self.cell.map(|cell| (cell.header, cell.row, cell.column))
    }

    /// 供同一个原格 Parser 入口消费；该状态来自行上下文及前列扫描，不是重置后的默认 math。
    pub fn table_cell_math_context(&self) -> Option<crate::math_context::MathContext> {
        self.cell.map(|cell| cell.math)
    }

    pub(super) fn from_verified(block: &VerifiedGlobalInlineBlock) -> Self {
        // 仅复制固定标量与一个原页 claim；不复制正文、AST 或 flatten 原 scope。
        Self {
            revision: block.revision,
            source_end: block.source_end,
            options: block.options,
            kind: block.kind,
            physical_start: block.physical_start,
            opening: Some((block.first_state_origin, block.first_state)),
            start: block.start,
            end: block.end,
            lexical_end: block.lexical_end,
            last_content_end: block.last_content_end,
            last_line_end: block.last_line_end,
            spine: block.spine.clone(),
            cell: None,
        }
    }

    pub fn source_range(&self) -> Range<usize> {
        self.start..self.end
    }
    pub fn physical_range(&self) -> Range<usize> {
        self.physical_start..self.end
    }
    pub fn lexical_range(&self) -> Range<usize> {
        self.start..self.lexical_end
    }
    pub fn kind(&self) -> InlineBlockKind {
        self.kind
    }
    pub fn scope_depth(&self) -> usize {
        self.spine.len()
    }

    /// requested_end 仅选择需要证明的前缀末端；块、Root 与终行权限均仍来自原 capture。
    /// 构造只留固定状态，实际 LUT/4KiB 游标初始化在有预算的第一步发生。
    pub fn plain_cursor(
        &self,
        requested_end: usize,
        limits: InlinePlainLimits,
    ) -> Result<InlinePlainCursor, GlobalFrontierError> {
        if requested_end < self.physical_start || requested_end > self.end {
            return Err(GlobalFrontierError::InvalidFrame);
        }
        if limits.max_byte_work == 0 || limits.max_transitions < MIN_STEP {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        Ok(InlinePlainCursor {
            seed: Self {
                revision: self.revision,
                source_end: self.source_end,
                options: self.options,
                kind: self.kind,
                physical_start: self.physical_start,
                opening: self.opening,
                start: self.start,
                end: self.end,
                lexical_end: self.lexical_end,
                last_content_end: self.last_content_end,
                last_line_end: self.last_line_end,
                spine: self.spine.clone(),
                cell: self.cell,
            },
            limits,
            total: GlobalWork::default(),
            requested_end,
            next: self.start,
            phase: Phase::Init,
            scope: None,
            scope_seen: false,
            lut: None,
            tail_start: None,
            marker_start: None,
            marker_tail: false,
            marker_eol: 0,
            real_eof_seen: false,
            failure: None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// 只说明原 capture 中实际出现的祖先种类；列表编号、tight 终态和 checkbox 不由此推断。
pub struct InlinePlainContainers {
    pub quote: bool,
    pub unordered_list: bool,
    pub ordered_list: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct InlinePlainLimits {
    pub max_byte_work: usize,
    pub max_transitions: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 仅在完整词法末端已被真实扫描时存在；终行真实 EOL 不授予最终 Soft/HardBreak。
pub struct InlinePlainTerminal {
    pub content_end: usize,
    pub trimmed_text_end: usize,
    pub eol_end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 真实 Root 前缀无 enabled inline special 的证明；未解析引用/HTML，也没有样式或隐藏权限。
pub struct VerifiedInlinePlainProof {
    revision: u64,
    source_end: usize,
    physical: Range<usize>,
    source: Range<usize>,
    proven_end: usize,
    kind: InlineBlockKind,
    marker: Option<Range<usize>>,
    terminal: Option<InlinePlainTerminal>,
}

impl VerifiedInlinePlainProof {
    pub fn source_revision(&self) -> u64 {
        self.revision
    }
    pub fn source_end(&self) -> usize {
        self.source_end
    }
    pub fn physical_range(&self) -> Range<usize> {
        self.physical.clone()
    }
    pub fn source_range(&self) -> Range<usize> {
        self.source.clone()
    }
    pub fn proven_end(&self) -> usize {
        self.proven_end
    }
    pub fn kind(&self) -> InlineBlockKind {
        self.kind
    }
    pub fn marker_range(&self) -> Option<Range<usize>> {
        self.marker.clone()
    }
    pub fn terminal(&self) -> Option<InlinePlainTerminal> {
        self.terminal
    }
}

#[derive(Debug)]
pub enum InlinePlainStep {
    Progress {
        work: GlobalWork,
    },
    NeedInput {
        next_byte: usize,
        work: GlobalWork,
    },
    NeedsGeneralInline {
        work: GlobalWork,
    },
    NonRootScope {
        work: GlobalWork,
    },
    Proven {
        proof: VerifiedInlinePlainProof,
        work: GlobalWork,
    },
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Init,
    Scope,
    Body,
    Marker,
    Emit,
    Proven,
    General,
    NonRoot,
    Failed,
}

/// 一个前缀游标保同一原 scope/role ledger；续批只扩大末端，不重发总 fuel 或重建 Global。
pub struct InlinePlainCursor {
    seed: VerifiedInlineSemanticSeed,
    limits: InlinePlainLimits,
    total: GlobalWork,
    requested_end: usize,
    next: usize,
    phase: Phase,
    scope: Option<StructuralSpineCursor>,
    scope_seen: bool,
    lut: Option<[bool; 256]>,
    tail_start: Option<usize>,
    marker_start: Option<usize>,
    marker_tail: bool,
    marker_eol: u8,
    real_eof_seen: bool,
    failure: Option<GlobalFrontierError>,
}

impl std::fmt::Debug for InlinePlainCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 只展示有限状态；不递归共享页或把正文放进诊断。
        formatter
            .debug_struct("InlinePlainCursor")
            .field("next", &self.next)
            .field("phase", &self.phase)
            .field("work", &self.total)
            .field("failure", &self.failure)
            .finish()
    }
}

impl InlinePlainCursor {
    /// 原 Paragraph scope 或已认证 TableCell 来源完成后才借固定短块，不以 depth 或裸 Range 伪造语法入口。
    pub fn bounded_inline_range(&self) -> Option<Range<usize>> {
        let range = self.seed.physical_range();
        (self.failure.is_none() && self.scope_seen && range.len() <= 2048).then_some(range)
    }
    pub fn next_byte(&self) -> usize {
        self.next
    }
    pub fn total_work(&self) -> GlobalWork {
        self.total
    }
    pub fn requested_end(&self) -> usize {
        self.requested_end
    }

    /// 只延伸同一已证明前缀；调用方仍须核原 source/context/full selection/publication。
    pub fn extend_to(&mut self, end: usize) -> Result<GlobalWork, GlobalFrontierError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if end < self.requested_end || end > self.seed.end || !matches!(self.phase, Phase::Proven) {
            // 逆向或未完状态不能通过重设 next 取得回放权限；由 Core 给出 checkpoint/状态错误。
            return Err(GlobalFrontierError::InvalidFrame);
        }
        let next_total = self
            .total
            .transitions
            .checked_add(CONTROL_WORK)
            .ok_or(GlobalFrontierError::ArithmeticOverflow)?;
        if next_total > self.limits.max_transitions {
            return Err(self.fail(GlobalFrontierError::WorkFuel));
        }
        self.total.transitions = next_total;
        self.requested_end = end;
        self.phase = if self.next < self.body_end(end) {
            Phase::Body
        } else if end > self.seed.lexical_end {
            Phase::Marker
        } else {
            Phase::Emit
        };
        Ok(GlobalWork {
            byte_reads: 0,
            transitions: CONTROL_WORK,
        })
    }

    fn fail(&mut self, error: GlobalFrontierError) -> GlobalFrontierError {
        // 失败 tombstone 不冒充 Proven；清工作区后 getter 只依赖有限 next 标量。
        self.failure = Some(error);
        self.phase = Phase::Failed;
        self.scope = None;
        self.lut = None;
        error
    }

    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<InlinePlainStep, GlobalFrontierError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if authority.cancelled {
            return Err(self.fail(GlobalFrontierError::Cancelled));
        }
        if authority.current_revision != self.seed.revision {
            return Err(self.fail(GlobalFrontierError::SourceSuperseded));
        }
        if !(MIN_STEP..=MAX_FRONTIER_STEP_BYTES).contains(&budget) {
            return Err(GlobalFrontierError::InvalidBudget);
        }
        // 假 EOF/错 frame 在任何消费前拒绝；frame 尾与闭合块末端不是完整 source EOF。
        crate::source_frontier::admitted_bytes(
            &frame,
            self.seed.revision,
            self.seed.source_end,
            self.next,
            budget,
        )?;
        match self.advance_inner(frame, budget) {
            Ok(step) => Ok(step),
            Err(error) => Err(self.fail(error)),
        }
    }

    fn advance_inner(
        &mut self,
        frame: SourceFrame<'_>,
        budget: usize,
    ) -> Result<InlinePlainStep, GlobalFrontierError> {
        if matches!(self.phase, Phase::Proven) {
            return Ok(InlinePlainStep::Proven {
                proof: self.proof(),
                work: GlobalWork::default(),
            });
        }
        if matches!(self.phase, Phase::General) {
            return Ok(InlinePlainStep::NeedsGeneralInline {
                work: GlobalWork::default(),
            });
        }
        if matches!(self.phase, Phase::NonRoot) {
            return Ok(InlinePlainStep::NonRootScope {
                work: GlobalWork::default(),
            });
        }
        let fuel = budget.min(
            self.limits
                .max_transitions
                .saturating_sub(self.total.transitions),
        );
        let bytes_left = self
            .limits
            .max_byte_work
            .saturating_sub(self.total.byte_reads);
        if fuel < MIN_STEP || bytes_left == 0 {
            return Err(GlobalFrontierError::WorkFuel);
        }
        let before = self.seed.spine.work_used();
        let mut work = GlobalWork {
            byte_reads: 0,
            transitions: CONTROL_WORK,
        };
        let mut need = false;
        self.real_eof_seen |= frame.real_eof;
        match self.phase {
            Phase::Init => {
                if self.seed.cell.is_some() {
                    // 原行/格词法已保留真实 Table scope；cell 没有 paragraph 前缀，不伪造 sole Paragraph。
                    self.lut = Some(crate::firstpass::special_bytes(&self.seed.options));
                    self.scope_seen = true;
                    work.transitions += 256;
                    self.phase = Phase::Body;
                } else if self.seed.spine.len() != 1 {
                    // 旧块入口仍只接受 Root；容器必须另持原 Origin 行，不能仅删除深度限制扩权。
                    self.phase = Phase::NonRoot;
                } else {
                    // 原 LUT 与固定 page-pointer 工作区只在真实初始化时收费，不按每个 frame 重收。
                    self.lut = Some(crate::firstpass::special_bytes(&self.seed.options));
                    self.scope = Some(self.seed.spine.cursor());
                    work.transitions += WORKSPACE_WORK + 256;
                    self.phase = Phase::Scope;
                }
            }
            Phase::Scope => {
                // 每步只消费一个真实页/role；共享原 ledger 的增量在尾部恰好计入一次。
                match self
                    .scope
                    .as_mut()
                    .ok_or(GlobalFrontierError::InvalidFrame)?
                    .next()?
                {
                    StructuralCursorStep::Building => (),
                    StructuralCursorStep::Entry { index, entry } => {
                        let paragraph = self.seed.spine.len() - 1;
                        if index == paragraph {
                            if entry.kind != StructuralKind::Paragraph
                                || entry.source_start != self.seed.start
                            {
                                return Err(GlobalFrontierError::InvalidFrame);
                            }
                            self.scope_seen = true;
                        } else {
                            return Err(GlobalFrontierError::InvalidFrame);
                        }
                    }
                    StructuralCursorStep::End => {
                        if !self.scope_seen {
                            return Err(GlobalFrontierError::InvalidFrame);
                        }
                        // 完成时真实释放固定工作区；后续只保原 seed 的单个共享页 claim。
                        self.scope = None;
                        work.transitions += WORKSPACE_WORK;
                        self.phase = Phase::Body;
                    }
                }
                work.transitions += 1;
            }
            Phase::Body => {
                let end = self.body_end(self.requested_end);
                let count = frame
                    .bytes
                    .len()
                    .min(bytes_left)
                    .min((fuel - CONTROL_WORK) / 2)
                    .min(end.saturating_sub(self.next));
                let (read, general) = scan_plain_bytes(
                    &frame.bytes[..count],
                    self.next,
                    self.seed.last_content_end,
                    self.lut.as_ref().ok_or(GlobalFrontierError::InvalidFrame)?,
                    &mut self.tail_start,
                );
                work.byte_reads += read;
                work.transitions += read;
                self.next += read;
                if general {
                    // 任一 enabled candidate 都交 general-inline 路径；与 ATX 使用同一规则。
                    self.phase = Phase::General;
                }
                if matches!(self.phase, Phase::Body) && self.next == end {
                    self.phase = if self.requested_end > self.seed.lexical_end {
                        Phase::Marker
                    } else {
                        Phase::Emit
                    };
                } else if matches!(self.phase, Phase::Body) && count == frame.bytes.len() {
                    need = true; // 短 frame 尚未完成真实前缀；不得把其末尾当 paragraph 结束。
                }
            }
            Phase::Marker => {
                let marker = match self.seed.kind {
                    InlineBlockKind::SetextHeading(HeadingLevel::H1) => b'=',
                    InlineBlockKind::SetextHeading(HeadingLevel::H2) => b'-',
                    _ => return Err(GlobalFrontierError::InvalidFrame),
                };
                let count = frame
                    .bytes
                    .len()
                    .min(bytes_left)
                    .min((fuel - CONTROL_WORK) / 2)
                    .min(self.seed.end.saturating_sub(self.next));
                for &byte in &frame.bytes[..count] {
                    let at = self.next;
                    work.byte_reads += 1;
                    work.transitions += 1;
                    self.next += 1;
                    if self.marker_eol != 0 {
                        if self.marker_eol != 1 || byte != b'\n' {
                            return Err(GlobalFrontierError::InvalidFrame);
                        }
                        self.marker_eol = 2;
                    } else if matches!(byte, b'\r' | b'\n') {
                        self.marker_eol = if byte == b'\r' { 1 } else { 2 };
                    } else if matches!(byte, b' ' | b'\t') {
                        self.marker_tail |= self.marker_start.is_some();
                    } else if byte == marker && !self.marker_tail {
                        self.marker_start.get_or_insert(at);
                    } else {
                        // 即使原 kind 已确定，内部 capture 与真实 marker bytes 失配也整体拒绝。
                        return Err(GlobalFrontierError::InvalidFrame);
                    }
                }
                if self.next == self.seed.end {
                    if self.marker_start.is_none() {
                        return Err(GlobalFrontierError::InvalidFrame);
                    }
                    self.phase = Phase::Emit;
                } else if count == frame.bytes.len() {
                    need = true;
                }
            }
            Phase::Emit => {
                if self.requested_end == self.seed.source_end && !self.real_eof_seen {
                    need = true; // 只有真实 source EOF 可关闭末端证明，逻辑 required_end 不伪造 EOF。
                } else {
                    self.phase = Phase::Proven;
                }
            }
            Phase::Proven | Phase::General | Phase::NonRoot => unreachable!("终态已在入口处理"),
            Phase::Failed => unreachable!("失败首因已在入口处理"),
        }
        work.transitions = work
            .transitions
            .checked_add(
                self.seed
                    .spine
                    .work_used()
                    .checked_sub(before)
                    .ok_or(GlobalFrontierError::ArithmeticOverflow)?,
            )
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
            return Err(GlobalFrontierError::WorkFuel);
        }
        Ok(match self.phase {
            Phase::Proven => InlinePlainStep::Proven {
                proof: self.proof(),
                work,
            },
            Phase::General => InlinePlainStep::NeedsGeneralInline { work },
            Phase::NonRoot => InlinePlainStep::NonRootScope { work },
            _ if need => InlinePlainStep::NeedInput {
                next_byte: self.next,
                work,
            },
            _ => InlinePlainStep::Progress { work },
        })
    }

    fn proof(&self) -> VerifiedInlinePlainProof {
        VerifiedInlinePlainProof {
            revision: self.seed.revision,
            source_end: self.seed.source_end,
            physical: self.seed.physical_start..self.seed.end,
            source: self.seed.start..self.seed.end,
            proven_end: self.requested_end,
            kind: self.seed.kind,
            marker: self.marker_start.map(|start| start..self.seed.end),
            terminal: (self.requested_end >= self.seed.lexical_end).then_some(
                InlinePlainTerminal {
                    content_end: self.seed.last_content_end,
                    trimmed_text_end: self.tail_start.unwrap_or(self.seed.last_content_end),
                    eol_end: self.seed.last_line_end,
                },
            ),
        }
    }

    fn body_end(&self, requested_end: usize) -> usize {
        requested_end
            .min(self.seed.lexical_end)
            .max(self.seed.start)
    }
}

#[cfg(test)]
mod tests;
