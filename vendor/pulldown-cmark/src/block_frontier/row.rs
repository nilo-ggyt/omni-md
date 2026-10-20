// Copyright 2015 Google Inc. All rights reserved.
// 原ActiveTable成员规则取自固定0.13.4 firstpass；完整MIT条款见LICENSE。

//! 一行只有源锚点、词法状态和UTF16计量；超长行不分配整行Vec，也不一次扫描完分类。

use super::*;
use crate::global_frontier::{ContainerPrefix, GlobalFrontierError, PrefixProgress};
use crate::label_frontier::{LabelScanner, LabelStep};
use crate::leaf_scan::LeafStart;
use crate::leaf_stream::{LeafProbe, LeafProbeStep};
use crate::line_frontier::{LineFacts, LineReadStep, LineReader, SourcePosition};
use crate::line_start_stream::{
    LineStartFrame, LineStartOperation, LineStartProgress, LineStartState, LineStartStream,
    LineStartValue,
};

enum Phase {
    Prefix,
    Space(LineStartStream),
    Leaf(LeafProbe),
    List(LineStartStream),
    Quote(LineStartStream),
    Peek {
        next: usize,
        bytes: [u8; 2],
        used: usize,
    },
    Label {
        scanner: LabelScanner,
        next: usize,
    },
    Colon(usize),
    Read(LineReader),
    Commit {
        target: usize,
        line: LineFacts,
    },
    Done,
}
pub(super) enum RowStep {
    Progress,
    NeedInput,
    NotMember,
    Member {
        line: LineFacts,
        position: SourcePosition,
    },
}
#[derive(Clone, Copy, Debug, Default)]
pub struct TableWork {
    pub byte_reads: usize,
    pub transitions: usize,
}

pub(super) struct ActiveRow {
    revision: u64,
    source_end: usize,
    origin: usize,
    content: usize,
    prefix: usize,
    depth: usize,
    options: Options,
    phase: Phase,
    position: SourcePosition,
    work: TableWork,
    prefix_scan: Option<ContainerPrefix>,
}

impl ActiveRow {
    pub(super) fn new(
        scalars: TableScalars,
        spine: &StructuralSpineSnapshot,
        options: Options,
    ) -> Self {
        let origin = scalars.next_byte;
        Self {
            revision: scalars.source_revision,
            source_end: scalars.source_end,
            origin,
            content: origin,
            prefix: 0,
            depth: spine.len(),
            options,
            phase: Phase::Prefix,
            prefix_scan: Some(ContainerPrefix::new(
                scalars.source_revision,
                origin,
                scalars.source_end,
                spine,
                options,
            )),
            position: SourcePosition {
                byte: origin,
                utf16: scalars.next_utf16,
                line: scalars.next_source_line,
                ..SourcePosition::default()
            },
            work: TableWork::default(),
        }
    }
    pub(super) fn next_byte(&self) -> usize {
        match &self.phase {
            Phase::Prefix => self
                .prefix_scan
                .as_ref()
                .expect("Prefix阶段拥有游标")
                .next_byte(),
            Phase::Space(operation) | Phase::List(operation) | Phase::Quote(operation) => {
                operation.next_byte()
            }
            Phase::Leaf(probe) => probe.next_byte(),
            Phase::Peek { next, .. } | Phase::Label { next, .. } | Phase::Colon(next) => *next,
            Phase::Read(reader) => reader.next_byte(),
            Phase::Commit { .. } | Phase::Done => self.position.byte,
        }
    }
    pub(super) fn prefix(&self) -> usize {
        self.prefix
    }
    pub(super) fn last_work(&self) -> TableWork {
        self.work
    }
    fn start_read(&mut self) {
        self.phase = Phase::Read(LineReader::new(self.content, self.source_end, false));
    }
    fn operation(&self, operation: LineStartOperation) -> Result<LineStartStream, FrontierError> {
        LineStartStream::new(
            self.revision,
            self.content,
            self.source_end,
            LineStartState::default(),
            operation,
        )
        .map_err(|_| FrontierError::InvalidFrame)
    }
    fn error(error: GlobalFrontierError) -> FrontierError {
        match error {
            GlobalFrontierError::Frontier(error) => error,
            _ => FrontierError::WorkFuel,
        }
    }

    /// 固定1024转换预留支付有限状态收尾和4KiB游标槽；余量才交给可恢复scanner。
    /// 每次最多处理一个phase，不能在Complete后隐藏深度遍历或巨行二次扫描。
    pub(super) fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        byte_budget: usize,
        transition_budget: usize,
    ) -> Result<RowStep, FrontierError> {
        if !(2048..=MAX_FRONTIER_STEP_BYTES).contains(&transition_budget) {
            return Err(FrontierError::InvalidBudget);
        }
        let max = crate::source_frontier::admitted_bytes(
            &frame,
            self.revision,
            self.source_end,
            self.next_byte(),
            byte_budget,
        )?;
        let fuel = (transition_budget - 1024) / 4;
        let count = max.min(fuel);
        let before = std::mem::replace(&mut self.phase, Phase::Done);
        self.work = TableWork {
            byte_reads: 0,
            transitions: 1024,
        };
        let mut result = RowStep::Progress;
        match before {
            Phase::Prefix => {
                let (step, work) = self
                    .prefix_scan
                    .as_mut()
                    .ok_or(FrontierError::InvalidFrame)?
                    .advance(&frame, byte_budget.min(fuel), fuel)
                    .map_err(Self::error)?;
                self.work.byte_reads = work.byte_reads;
                self.work.transitions += work.transitions;
                match step {
                    PrefixProgress::Complete { state, matched } => {
                        self.prefix_scan = None;
                        if matched != self.depth {
                            return Ok(RowStep::NotMember);
                        }
                        self.phase = Phase::Space(
                            LineStartStream::new(
                                self.revision,
                                self.origin,
                                self.source_end,
                                state,
                                LineStartOperation::AllSpace,
                            )
                            .map_err(|_| FrontierError::InvalidFrame)?,
                        );
                    }
                    PrefixProgress::Progress => self.phase = Phase::Prefix,
                    PrefixProgress::NeedInput { .. } => {
                        result = RowStep::NeedInput;
                        self.phase = Phase::Prefix;
                    }
                }
            }
            Phase::Space(mut operation)
            | Phase::List(mut operation)
            | Phase::Quote(mut operation) => {
                let step = operation
                    .advance(
                        LineStartFrame {
                            source_revision: frame.source_revision,
                            byte_start: frame.byte_start,
                            bytes: frame.bytes,
                            real_eof: frame.real_eof,
                        },
                        byte_budget.min(fuel),
                        fuel,
                    )
                    .map_err(|_| FrontierError::InvalidFrame)?;
                self.work.byte_reads = step.work.byte_reads;
                self.work.transitions += step.work.transitions;
                match step.progress {
                    LineStartProgress::Complete { state, value } => match value {
                        LineStartValue::Unit => {
                            self.prefix = state.ix;
                            self.content = self.origin + state.ix;
                            self.phase = Phase::Leaf(LeafProbe::new(self.content));
                        }
                        LineStartValue::List(Some(_)) | LineStartValue::Bool(true) => {
                            return Ok(RowStep::NotMember);
                        }
                        LineStartValue::List(None) => {
                            self.phase = Phase::Quote(self.operation(LineStartOperation::Quote)?)
                        }
                        LineStartValue::Bool(false) => {
                            if self.options.contains(Options::ENABLE_FOOTNOTES) {
                                self.phase = Phase::Peek {
                                    next: self.content,
                                    bytes: [0; 2],
                                    used: 0,
                                };
                            } else {
                                self.start_read();
                            }
                        }
                        _ => unreachable!("ActiveRow只有AllSpace/List/Quote三种操作"),
                    },
                    LineStartProgress::Progress { .. } | LineStartProgress::NeedInput { .. } => {
                        result = if matches!(step.progress, LineStartProgress::NeedInput { .. }) {
                            RowStep::NeedInput
                        } else {
                            RowStep::Progress
                        };
                        // 操作本身保留确定的返回类型；续存不需要猜测原enum分支。
                        self.phase = Phase::Space(operation);
                    }
                }
            }
            Phase::Leaf(mut probe) => {
                let (step, read, work) = probe.advance(
                    &frame.bytes[..count],
                    frame.real_eof && count == frame.bytes.len(),
                    fuel,
                );
                self.work.byte_reads = read;
                self.work.transitions += work;
                match step {
                    LeafProbeStep::Complete(leaf) => {
                        if matches!(
                            leaf,
                            LeafStart::Rule(_)
                                | LeafStart::Heading(_)
                                | LeafStart::Fence { .. }
                                | LeafStart::HtmlTerminated(_)
                        ) || matches!(leaf, LeafStart::HtmlBlankTerminated)
                            && !probe.html_type7()
                        {
                            return Ok(RowStep::NotMember);
                        }
                        self.phase = Phase::List(self.operation(LineStartOperation::List(0))?);
                    }
                    LeafProbeStep::Progress => self.phase = Phase::Leaf(probe),
                    LeafProbeStep::NeedInput => {
                        result = RowStep::NeedInput;
                        self.phase = Phase::Leaf(probe);
                    }
                }
            }
            Phase::Peek {
                mut next,
                mut bytes,
                mut used,
            } => {
                let read = count.min(2 - used);
                bytes[used..used + read].copy_from_slice(&frame.bytes[..read]);
                used += read;
                next += read;
                self.work.byte_reads = read;
                self.work.transitions += read;
                if used == 2 || next == self.source_end {
                    if bytes == *b"[^" && used == 2 {
                        self.phase = Phase::Label {
                            scanner: LabelScanner::new(false),
                            next,
                        };
                    } else {
                        self.start_read();
                    }
                } else {
                    result = RowStep::NeedInput;
                    self.phase = Phase::Peek { next, bytes, used };
                }
            }
            Phase::Label {
                mut scanner,
                mut next,
            } => {
                let step = scanner.feed(
                    &frame.bytes[..count],
                    frame.real_eof && count == frame.bytes.len(),
                    count.max(1),
                )?;
                let used = match step {
                    LabelStep::Progress { consumed }
                    | LabelStep::NeedInput { consumed }
                    | LabelStep::NeedLineContinuation { consumed, .. }
                    | LabelStep::Complete { consumed, .. }
                    | LabelStep::Rejected { consumed } => consumed,
                };
                self.work.byte_reads = used;
                self.work.transitions += used * 2;
                next += used;
                match step {
                    LabelStep::Complete { .. } => self.phase = Phase::Colon(next),
                    LabelStep::Rejected { .. } | LabelStep::NeedLineContinuation { .. } => {
                        self.start_read()
                    }
                    _ => {
                        result = if matches!(step, LabelStep::NeedInput { .. }) {
                            RowStep::NeedInput
                        } else {
                            RowStep::Progress
                        };
                        self.phase = Phase::Label { scanner, next };
                    }
                }
            }
            Phase::Colon(_) => {
                if let Some(byte) = frame.bytes.first() {
                    self.work.byte_reads = 1;
                    self.work.transitions += 1;
                    if *byte == b':' {
                        return Ok(RowStep::NotMember);
                    }
                    self.start_read();
                } else if frame.real_eof {
                    self.start_read();
                } else {
                    self.phase = before;
                    result = RowStep::NeedInput;
                }
            }
            Phase::Read(mut reader) => {
                let (step, read, work) = reader.advance(
                    &frame.bytes[..count],
                    frame.real_eof && count == frame.bytes.len(),
                    fuel,
                );
                self.work.byte_reads = read;
                self.work.transitions += work * 3;
                match step {
                    LineReadStep::Complete(line) => {
                        if line.actual_cells == 0 {
                            return Ok(RowStep::NotMember);
                        }
                        self.phase = Phase::Commit {
                            target: line.end,
                            line,
                        };
                    }
                    LineReadStep::Progress => self.phase = Phase::Read(reader),
                    LineReadStep::NeedInput => {
                        result = RowStep::NeedInput;
                        self.phase = Phase::Read(reader);
                    }
                }
            }
            Phase::Commit { target, line } => {
                let read = count.min(target - self.position.byte);
                for byte in &frame.bytes[..read] {
                    self.position
                        .feed(*byte)
                        .map_err(|_| FrontierError::InvalidUtf8)?;
                }
                self.work.byte_reads = read;
                self.work.transitions += read;
                if self.position.byte == target {
                    if self.position.utf8_left != 0 {
                        return Err(FrontierError::InvalidUtf8);
                    }
                    return Ok(RowStep::Member {
                        line,
                        position: self.position,
                    });
                }
                result = if read == frame.bytes.len() {
                    RowStep::NeedInput
                } else {
                    RowStep::Progress
                };
                self.phase = Phase::Commit { target, line };
            }
            Phase::Done => return Err(FrontierError::Closed),
        }
        Ok(result)
    }
}
