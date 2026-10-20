// Copyright 2017 Google Inc. All rights reserved.
// 原代码匹配与 HTML scanner 的有限来源适配；MIT 许可见 LICENSE。

//! 一个真实 Root 闭合块的 code 优先级证明，不创建正文、AST 或分隔符历史目录。

use super::{GlobalFrontierError as Error, GlobalSourceAuthority, GlobalWork, InlinePlainLimits};
use crate::block_frontier::{SourceFrame, MAX_FRONTIER_STEP_BYTES};
use crate::inline_frontier::{
    CodeDelimiterCandidate, CodeSpanCandidate, CodeSpanProbe, CodeSpanStep,
};
use std::ops::Range;

#[path = "inline_code_scope.rs"]
mod scope;
use scope::ScopedCode;
#[path = "inline_code_link.rs"]
mod link;
pub use link::{
    InlineLinkDestinationDecoder, InlineLinkDestinationStep, InlineLinkKind, InlineLinkRecord,
    InlineLinkRecordId, InlineLinkRecordStatus, InlineLinkStorageOperation, InlineLinkStorageReply,
    InlineLinkStorageRequest, InlineLinkStorageStep, InlineLinkTarget, InlineReferenceDecoder,
    InlineReferenceKind, InlineReferenceResolution, VerifiedInlineLink,
    VerifiedInlineReferenceRequest,
};
#[path = "inline_code_link_group.rs"]
mod link_group;
use link_group::{LinkProbe, LinkStep, ReplayStep};

const CONTROL: usize = 128;
const HTML_BYTES: usize = 256;

#[derive(Debug)]
/// 原 Root 顺序中、排除 HTML/code 后的真实 delimiter run；尚未配对，不能单独当样式证明。
pub struct VerifiedInlineEmphasisRun {
    source: Range<usize>,
    scope: Range<usize>,
    delimiter: u8,
    can_open: bool,
    can_close: bool,
}
impl VerifiedInlineEmphasisRun {
    /// 坐标仍属于原闭合块，包含整条原始 run，不因随后消费而改变 run_length。
    pub fn source_range(&self) -> Range<usize> {
        self.source.clone()
    }
    /// 同一原seed或已认证label的完整匹配域；不能把不同label的run放进同一强调栈。
    pub fn scope_range(&self) -> Range<usize> {
        self.scope.clone()
    }
    /// 已经过原 enabled-delimiter 与 escape 优先级，不接受消费者补造字节类别。
    pub fn delimiter(&self) -> u8 {
        self.delimiter
    }
    /// 真实前后 Unicode scalar 经原 flanking 内核确认的 opener 能力。
    pub fn can_open(&self) -> bool {
        self.can_open
    }
    /// 真实前后 Unicode scalar 经原 flanking 内核确认的 closer 能力。
    pub fn can_close(&self) -> bool {
        self.can_close
    }
}

#[derive(Clone, Debug)]
/// 只有真实 Root 顺序游标能签发；外部不能从 lexical candidate 或 Range 构造 HTML/code 权限。
pub struct VerifiedInlineCodeSpan {
    revision: u64,
    source_end: usize,
    source: Range<usize>,
    body: Range<usize>,
    trim: bool,
}
impl VerifiedInlineCodeSpan {
    /// canonical 原范围包含 opener/closer；escaped opener 的第一个 tick 已由原规则留作 literal。
    pub fn source_range(&self) -> Range<usize> {
        self.source.clone()
    }
    /// 原正文没有复制/归一化；消费者仍须按原 make_code_span 处理换行和首尾空格。
    pub fn raw_body_range(&self) -> Range<usize> {
        self.body.clone()
    }
    /// 原规则只在非全空格且归一化首尾均为空格时，各去掉一个逻辑空格。
    pub fn trims_boundary_spaces(&self) -> bool {
        self.trim
    }
    pub fn source_revision(&self) -> u64 {
        self.revision
    }
    pub fn source_end(&self) -> usize {
        self.source_end
    }
}

#[derive(Debug)]
pub enum InlineCodeStep {
    Progress {
        work: GlobalWork,
    },
    NeedInput {
        work: GlobalWork,
    },
    Span {
        proof: VerifiedInlineCodeSpan,
        work: GlobalWork,
    },
    Prefix {
        proof: super::super::VerifiedInlinePrefix,
        work: GlobalWork,
    },
    Emphasis {
        run: VerifiedInlineEmphasisRun,
        work: GlobalWork,
    },
    Link {
        proof: VerifiedInlineLink,
        work: GlobalWork,
    },
    Storage {
        request: InlineLinkStorageRequest,
        work: GlobalWork,
    },
    Reference {
        request: VerifiedInlineReferenceRequest,
        work: GlobalWork,
    },
    LinkEnd {
        source: Range<usize>,
        label: Range<usize>,
        work: GlobalWork,
    },
    Proven {
        requires_general_styles: bool,
        work: GlobalWork,
    },
    NeedsGeneralInline {
        work: GlobalWork,
    },
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Init,
    Scope,
    Normal,
    Backslash,
    Bang,
    Run,
    Html,
    Probe,
    Trim,
    EmphasisRun,
    EmphasisContext,
    Proven,
    General,
}

#[derive(Debug)]
struct Trim {
    candidate: CodeSpanCandidate,
    next: usize,
    first_space: Option<bool>,
    last_space: bool,
    all_spaces: bool,
    after_cr: bool,
}

#[derive(Debug)]
pub struct InlineCodeCursor {
    revision: u64,
    source_end: usize,
    end: usize,
    scope_range: Range<usize>,
    requested_end: usize,
    options: crate::Options,
    next: usize,
    phase: Phase,
    run_start: usize,
    run_count: usize,
    run_escaped: bool,
    html: [u8; HTML_BYTES],
    html_len: usize,
    probe: Option<CodeSpanProbe>,
    trim: Option<Trim>,
    general_styles: bool,
    limits: InlinePlainLimits,
    total: GlobalWork,
    failure: Option<Error>,
    spine: Option<crate::structural_spine::StructuralSpineSnapshot>,
    table_cell: bool,
    scoped: Option<ScopedCode>,
    emphasis: bool,
    previous: Option<char>,
    previous_bytes: [u8; 4],
    previous_len: usize,
    delimiter: u8,
    delimiter_before: Option<char>,
    // 链接前瞻只保当前记录/真实暂停；完整决定后按同文件源序链重借 label，不复制正文。
    // 外层请求末端独立保留，所有嵌套 End 后才恢复它，不能签发一个局部 label EOF。
    link_probe: Option<LinkProbe>,
    link_outer_end: Option<usize>,
    suppressed_reference_close: Option<usize>,
}

impl super::VerifiedInlineSemanticSeed {
    /// 只从原真实 Root Fact 建立同源游标；容器仍须原 origin 投影，不能丢掉 prefix 后假装 Root。
    pub fn code_cursor(
        &self,
        requested_end: usize,
        limits: InlinePlainLimits,
    ) -> Result<Option<InlineCodeCursor>, Error> {
        self.code_cursor_for_origin(None, requested_end, limits)
    }

    /// 容器 inline 只接同一 Global 真正发布的 Origin；坐标相似、相同文本或深度不能代替该票。
    pub fn emphasis_origin_cursor(
        &self,
        origin: &super::super::BlockOriginLine,
        requested_end: usize,
        limits: InlinePlainLimits,
    ) -> Result<Option<InlineCodeCursor>, Error> {
        let mut cursor = self.code_cursor_for_origin(Some(origin), requested_end, limits)?;
        if let Some(cursor) = cursor.as_mut() {
            cursor.emphasis = true;
        }
        Ok(cursor)
    }

    fn code_cursor_for_origin(
        &self,
        origin: Option<&super::super::BlockOriginLine>,
        requested_end: usize,
        limits: InlinePlainLimits,
    ) -> Result<Option<InlineCodeCursor>, Error> {
        let scoped = if let Some(origin) = origin {
            if self.cell.is_some()
                || self.kind != super::super::InlineBlockKind::Paragraph
                || self.spine.len() <= 1
                || origin.revision != self.revision
                || origin.source_end != self.source_end
                || origin.block_source != (self.start..self.end)
                || origin.block_kind() != super::super::BlockOriginKind::Inline(self.kind)
                || origin.total_depth() != self.spine.len()
            {
                return Err(Error::InvalidFrame);
            }
            Some(ScopedCode::new(self.clone()))
        } else {
            if self.cell.is_none() && self.spine.len() != 1 {
                return Ok(None);
            }
            None
        };
        if requested_end < self.physical_start || requested_end > self.end {
            return Err(Error::InvalidFrame);
        }
        if limits.max_byte_work == 0 || limits.max_transitions < 2048 {
            return Err(Error::InvalidBudget);
        }
        Ok(Some(InlineCodeCursor {
            revision: self.revision,
            source_end: self.source_end,
            end: self.lexical_end,
            scope_range: self.source_range(),
            requested_end: requested_end.min(self.lexical_end),
            options: self.options,
            next: self.start,
            phase: Phase::Init,
            run_start: 0,
            run_count: 0,
            run_escaped: false,
            html: [0; HTML_BYTES],
            html_len: 0,
            probe: None,
            trim: None,
            general_styles: false,
            limits,
            total: GlobalWork::default(),
            failure: None,
            spine: Some(self.spine.clone()),
            table_cell: self.cell.is_some(),
            scoped,
            emphasis: false,
            previous: None,
            previous_bytes: [0; 4],
            previous_len: 0,
            delimiter: 0,
            delimiter_before: None,
            link_probe: None,
            link_outer_end: None,
            suppressed_reference_close: None,
        }))
    }

    /// 同一真实 Root 来源开启原强调事实输出；首次遇 delimiter 才把目标扩大到真块末端。
    pub fn emphasis_cursor(
        &self,
        requested_end: usize,
        limits: InlinePlainLimits,
    ) -> Result<Option<InlineCodeCursor>, Error> {
        let mut cursor = self.code_cursor(requested_end, limits)?;
        if let Some(cursor) = cursor.as_mut() {
            cursor.emphasis = true;
        }
        Ok(cursor)
    }
}

impl crate::global_frontier::VerifiedGlobalInlineBlock {
    /// 全局语义前探直接消费一个真实块票据；不建立第二个 Global 或假范围证明。
    pub fn into_code_cursor(
        self,
        requested_end: usize,
        limits: InlinePlainLimits,
    ) -> Result<Option<InlineCodeCursor>, Error> {
        super::VerifiedInlineSemanticSeed::from_verified(&self).code_cursor(requested_end, limits)
    }
}

impl InlineCodeCursor {
    /// 文件借用端已经结束本次 I/O 并首先完成 SourceFrame.finish 后，才可恢复这个原请求。
    /// 实际存储错误不进入这里；调用者保 Core errno 并停止游标，不把错误解释成字面。
    pub fn accept_storage(
        &mut self,
        request: &InlineLinkStorageRequest,
        step: InlineLinkStorageStep,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<GlobalWork, Error> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = (|| {
            if authority.cancelled {
                return Err(Error::Cancelled);
            }
            if authority.current_revision != self.revision {
                return Err(Error::SourceSuperseded);
            }
            let (work, reply) = match step {
                InlineLinkStorageStep::Progress { work } => (work, None),
                InlineLinkStorageStep::Ready { reply, work } => (work, Some(reply)),
            };
            if work.byte_reads != 0 || work.transitions > budget || budget > MAX_FRONTIER_STEP_BYTES
            {
                return Err(Error::InvalidBudget);
            }
            let probe = self.link_probe.as_mut().ok_or(Error::InvalidFrame)?;
            if probe.pending.as_ref().map(|(pending, _)| pending) != Some(request) {
                return Err(Error::InvalidFrame);
            }
            let scanning = !probe.replaying();
            if let Some(reply) = reply {
                probe.accept_storage(request, reply)?;
            }
            if scanning && probe.replaying() {
                self.next = self.run_start;
                self.previous_len = 0;
                self.phase = Phase::Normal;
            }
            self.account(work)?;
            Ok(work)
        })();
        result.map_err(|error| self.fail(error))
    }
    /// 只接受原请求在同一 Queries/Global 中真正得到的 Resolved/Missing；不能以局部未见代替 Missing。
    pub fn accept_reference(
        &mut self,
        request: &VerifiedInlineReferenceRequest,
        resolution: InlineReferenceResolution,
        authority: GlobalSourceAuthority,
    ) -> Result<GlobalWork, Error> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = (|| {
            if authority.cancelled {
                return Err(Error::Cancelled);
            }
            if authority.current_revision != self.revision {
                return Err(Error::SourceSuperseded);
            }
            self.link_probe
                .as_mut()
                .ok_or(Error::InvalidFrame)?
                .accept_reference(request, resolution)?;
            let work = GlobalWork {
                byte_reads: 0,
                transitions: CONTROL,
            };
            self.account(work)?;
            Ok(work)
        })();
        result.map_err(|error| self.fail(error))
    }
    fn start_link(&mut self, image: bool) -> Result<(), Error> {
        let start = if image { self.run_start } else { self.next };
        self.run_start = start;
        self.link_outer_end = Some(self.requested_end);
        self.link_probe = Some(LinkProbe::new(
            self.revision,
            self.source_end,
            self.scope_range.clone(),
            start,
            image,
            self.options,
            self.suppressed_reference_close,
        )?);
        Ok(())
    }
    fn advance_link_replay(&mut self, work: GlobalWork) -> Result<Option<InlineCodeStep>, Error> {
        let step = self
            .link_probe
            .as_mut()
            .ok_or(Error::InvalidFrame)?
            .replay(self.next)?;
        Ok(match step {
            ReplayStep::Storage(request) => Some(InlineCodeStep::Storage { request, work }),
            ReplayStep::Progress => Some(InlineCodeStep::Progress { work }),
            ReplayStep::Begin(proof) => {
                self.next = proof.label.start;
                self.previous = Some('[');
                self.previous_len = 0;
                Some(InlineCodeStep::Link { proof, work })
            }
            ReplayStep::End {
                source,
                label,
                next,
                previous,
            } => {
                self.next = next;
                self.previous = Some(previous);
                self.previous_len = 0;
                Some(InlineCodeStep::LinkEnd {
                    source,
                    label,
                    work,
                })
            }
            ReplayStep::Done {
                next,
                suppressed_close,
            } => {
                self.next = next;
                self.suppressed_reference_close = suppressed_close;
                self.link_probe = None;
                self.requested_end = self.link_outer_end.take().ok_or(Error::InvalidFrame)?;
                Some(InlineCodeStep::Progress { work })
            }
            ReplayStep::None => None,
        })
    }
    /// 返回真实下一次所需源字节；lookahead/trim 都不会假装 canonical 已提交到此处。
    pub fn next_byte(&self) -> usize {
        if let Some(prefix) = self
            .scoped
            .as_ref()
            .and_then(|scoped| scoped.prefix.as_ref())
        {
            prefix.next_byte()
        } else if let Some(probe) = self.link_probe.as_ref().filter(|probe| !probe.replaying()) {
            probe.next_byte()
        } else if let Some(probe) = self.probe.as_ref() {
            probe.next_byte()
        } else if let Some(trim) = self.trim.as_ref() {
            trim.next
        } else if matches!(self.phase, Phase::EmphasisContext) {
            self.next + self.html_len
        } else {
            self.next
        }
    }
    /// 同一 source 的后继 viewport 只扩大目标；累计 fuel 和已扫描的真实状态不回退。
    pub fn extend_to(&mut self, end: usize) -> Result<GlobalWork, Error> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if !matches!(self.phase, Phase::Proven) || end < self.requested_end || end > self.end {
            return Err(Error::InvalidFrame);
        }
        let work = GlobalWork {
            byte_reads: 0,
            transitions: CONTROL,
        };
        self.account(work)?;
        self.requested_end = end;
        self.phase = Phase::Normal;
        Ok(work)
    }
    /// 原源取消/失败永久撤销后继；scratch 与原 probe 的有限资源立即归还。
    fn fail(&mut self, error: Error) -> Error {
        self.failure = Some(error);
        self.probe = None;
        self.trim = None;
        self.link_probe = None;
        self.link_outer_end = None;
        self.html_len = 0;
        self.spine = None;
        self.scoped = None;
        error
    }
    fn account(&mut self, work: GlobalWork) -> Result<(), Error> {
        self.total.byte_reads = self
            .total
            .byte_reads
            .checked_add(work.byte_reads)
            .ok_or(Error::ArithmeticOverflow)?;
        self.total.transitions = self
            .total
            .transitions
            .checked_add(work.transitions)
            .ok_or(Error::ArithmeticOverflow)?;
        if self.total.byte_reads > self.limits.max_byte_work
            || self.total.transitions > self.limits.max_transitions
        {
            return Err(self.fail(Error::WorkFuel));
        }
        Ok(())
    }
    /// 每步只借原 SourceFrame，最多预算内字节；跨 frame 的 tick/HTML/UTF8 字节不创建虚拟 EOF。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<InlineCodeStep, Error> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if authority.cancelled {
            return Err(self.fail(Error::Cancelled));
        }
        if authority.current_revision != self.revision {
            return Err(self.fail(Error::SourceSuperseded));
        }
        if !(2048..=MAX_FRONTIER_STEP_BYTES).contains(&budget) {
            return Err(Error::InvalidBudget);
        }
        if let Err(error) = crate::source_frontier::admitted_bytes(
            &frame,
            self.revision,
            self.source_end,
            self.next_byte(),
            budget,
        ) {
            return Err(self.fail(Error::Frontier(error)));
        }
        let result = self.advance_inner(frame, budget);
        match result {
            Ok(step) => {
                let work = match &step {
                    InlineCodeStep::Progress { work }
                    | InlineCodeStep::NeedInput { work }
                    | InlineCodeStep::Span { work, .. }
                    | InlineCodeStep::Prefix { work, .. }
                    | InlineCodeStep::Emphasis { work, .. }
                    | InlineCodeStep::Link { work, .. }
                    | InlineCodeStep::Storage { work, .. }
                    | InlineCodeStep::Reference { work, .. }
                    | InlineCodeStep::LinkEnd { work, .. }
                    | InlineCodeStep::Proven { work, .. }
                    | InlineCodeStep::NeedsGeneralInline { work } => *work,
                };
                self.account(work)?;
                Ok(step)
            }
            Err(error) => Err(self.fail(error)),
        }
    }
    fn advance_inner(
        &mut self,
        frame: SourceFrame<'_>,
        budget: usize,
    ) -> Result<InlineCodeStep, Error> {
        let mut work = GlobalWork {
            byte_reads: 0,
            transitions: CONTROL,
        };
        if matches!(self.phase, Phase::Proven) {
            return Ok(InlineCodeStep::Proven {
                requires_general_styles: self.general_styles,
                work: GlobalWork::default(),
            });
        }
        if matches!(self.phase, Phase::General) {
            return Ok(InlineCodeStep::NeedsGeneralInline { work });
        }
        if let Some(step) = self.advance_prefix(&frame, budget)? {
            return Ok(step);
        }
        if matches!(self.phase, Phase::Scope) {
            return self.advance_scope();
        }
        if matches!(self.phase, Phase::Init) {
            if self.scoped.is_some() {
                self.spine = None;
                self.phase = Phase::Scope;
                return Ok(InlineCodeStep::Progress { work });
            }
            if !self.table_cell {
                let entry = self
                    .spine
                    .as_ref()
                    .unwrap()
                    .sole_entry()?
                    .ok_or(Error::InvalidFrame)?;
                if entry.kind != crate::structural_spine::StructuralKind::Paragraph
                    || entry.source_start != self.next
                {
                    return Err(Error::InvalidFrame);
                }
                work.transitions += 2;
            }
            // 共享角色预算已经真实收取一页一项；从此只保闭合块固定标量，不留另一个 scope 页缓存。
            self.spine = None;
            self.phase = Phase::Normal;
            return Ok(InlineCodeStep::Progress { work });
        }
        if self
            .link_probe
            .as_ref()
            .is_some_and(|probe| !probe.replaying())
        {
            let probe = self.link_probe.as_mut().unwrap();
            let step = probe.advance(&frame, budget, &mut work)?;
            return Ok(match step {
                LinkStep::Progress => InlineCodeStep::Progress { work },
                LinkStep::NeedInput => InlineCodeStep::NeedInput { work },
                LinkStep::Storage(request) => InlineCodeStep::Storage { request, work },
                LinkStep::Reference(request) => InlineCodeStep::Reference { request, work },
                LinkStep::General => {
                    self.link_probe = None;
                    self.phase = Phase::General;
                    InlineCodeStep::NeedsGeneralInline { work }
                }
                LinkStep::Replay => {
                    // 语法先完整决定，原 main cursor 才重借 opener 起点；不以闭合顺序发嵌套样式。
                    self.next = self.run_start;
                    self.previous_len = 0;
                    self.phase = Phase::Normal;
                    InlineCodeStep::Progress { work }
                }
            });
        }
        // replay 的元数据回复也只在真实 Source guard 后消费。普通源扫描不跨一个未完成的文件操作。
        if self.link_probe.as_ref().is_some_and(LinkProbe::replaying)
            && matches!(self.phase, Phase::Normal | Phase::Backslash | Phase::Bang)
        {
            if let Some(step) = self.advance_link_replay(work)? {
                return Ok(step);
            }
        }
        if matches!(self.phase, Phase::Probe) {
            let (probe_frame, boundary) = self.probe_frame(&frame, budget, &mut work)?;
            let child = budget - work.byte_reads - work.transitions;
            let step = self
                .probe
                .as_mut()
                .expect("Probe 持原实际 opener")
                .advance(probe_frame, child)
                .map_err(inline_error)?;
            let measured = match &step {
                CodeSpanStep::Progress { work }
                | CodeSpanStep::NeedInput { work }
                | CodeSpanStep::Candidate { work, .. }
                | CodeSpanStep::Unmatched { work, .. } => *work,
            };
            work.byte_reads += measured.byte_reads;
            work.transitions += measured.transitions + measured.entity_lookup_byte_work_bound;
            if let Some((at, byte)) = boundary {
                if self.probe.as_ref().unwrap().next_byte() == at
                    && matches!(
                        step,
                        CodeSpanStep::NeedInput { .. } | CodeSpanStep::Progress { .. }
                    )
                {
                    self.arm_continuation(at, byte);
                }
            }
            return Ok(match step {
                CodeSpanStep::Candidate { candidate, .. } => {
                    self.probe = None;
                    self.trim = Some(Trim {
                        next: candidate.raw_body_range().start,
                        candidate,
                        first_space: None,
                        last_space: false,
                        all_spaces: true,
                        after_cr: false,
                    });
                    self.phase = Phase::Trim;
                    InlineCodeStep::Progress { work }
                }
                CodeSpanStep::Unmatched { replay_from, .. } => {
                    self.probe = None;
                    self.next = replay_from;
                    self.phase = Phase::Normal;
                    InlineCodeStep::Progress { work }
                }
                CodeSpanStep::NeedInput { .. } => InlineCodeStep::NeedInput { work },
                CodeSpanStep::Progress { .. } => InlineCodeStep::Progress { work },
            });
        }
        if matches!(self.phase, Phase::Trim) {
            let trim = self.trim.as_mut().expect("Trim 只保已闭合 candidate");
            let end = trim.candidate.raw_body_range().end;
            while trim.next < end && work.byte_reads + work.transitions + 2 <= budget {
                let Some(&byte) = frame.bytes.get(trim.next - frame.byte_start) else {
                    return Ok(InlineCodeStep::NeedInput { work });
                };
                trim.next += 1;
                work.byte_reads += 1;
                work.transitions += 1;
                if byte == b'\n' && trim.after_cr {
                    trim.after_cr = false;
                    if self.scoped.is_some() {
                        let at = trim.next;
                        self.arm_continuation(at, byte);
                        return Ok(InlineCodeStep::Progress { work });
                    }
                    continue;
                }
                let space = matches!(byte, b' ' | b'\r' | b'\n');
                trim.first_space.get_or_insert(space);
                trim.last_space = space;
                trim.all_spaces &= space;
                trim.after_cr = byte == b'\r';
                if self.scoped.is_some() && matches!(byte, b'\r' | b'\n') {
                    let at = trim.next;
                    self.arm_continuation(at, byte);
                    return Ok(InlineCodeStep::Progress { work });
                }
            }
            if trim.next != end {
                return Ok(InlineCodeStep::Progress { work });
            }
            let trim = self.trim.take().unwrap();
            let proof = VerifiedInlineCodeSpan {
                revision: self.revision,
                source_end: self.source_end,
                source: trim.candidate.range(),
                body: trim.candidate.raw_body_range(),
                trim: !trim.all_spaces && trim.first_space == Some(true) && trim.last_space,
            };
            self.next = proof.source.end;
            self.previous = Some('`');
            self.previous_len = 0;
            self.phase = Phase::Normal;
            return Ok(InlineCodeStep::Span { proof, work });
        }
        if matches!(self.phase, Phase::EmphasisContext) {
            if self.next == self.scan_end() {
                let boundary = self
                    .link_probe
                    .as_ref()
                    .and_then(LinkProbe::label_range)
                    .map(|_| ']');
                return Ok(self.finish_emphasis(boundary, work));
            }
            while work.byte_reads + work.transitions + 8 <= budget {
                let at = self.next + self.html_len;
                let Some(&byte) = frame.bytes.get(at - frame.byte_start) else {
                    return Ok(InlineCodeStep::NeedInput { work });
                };
                self.html[self.html_len] = byte;
                self.html_len += 1;
                work.byte_reads += 1;
                work.transitions += 5;
                match std::str::from_utf8(&self.html[..self.html_len]) {
                    Ok(text) => {
                        let next = text.chars().next();
                        return Ok(self.finish_emphasis(next, work));
                    }
                    Err(error) if error.error_len().is_none() && self.html_len < 4 => {}
                    Err(_) => return Err(Error::InvalidFrame),
                }
            }
            return Ok(InlineCodeStep::Progress { work });
        }
        loop {
            if self.link_probe.as_ref().is_some_and(LinkProbe::replaying)
                && matches!(self.phase, Phase::Normal | Phase::Backslash | Phase::Bang)
            {
                if let Some(step) = self.advance_link_replay(work)? {
                    return Ok(step);
                }
            }
            if self.next >= self.requested_end
                && self.link_probe.is_none()
                && matches!(self.phase, Phase::Normal | Phase::Backslash | Phase::Bang)
            {
                self.phase = Phase::Proven;
                return Ok(InlineCodeStep::Proven {
                    requires_general_styles: self.general_styles,
                    work,
                });
            }
            if self.next == self.scan_end() {
                if matches!(self.phase, Phase::Run) {
                    self.begin_probe()?;
                    return Ok(InlineCodeStep::Progress { work });
                }
                if matches!(self.phase, Phase::Html) {
                    self.phase = Phase::General;
                    return Ok(InlineCodeStep::NeedsGeneralInline { work });
                }
                if matches!(self.phase, Phase::EmphasisRun) {
                    self.html_len = 0;
                    self.phase = Phase::EmphasisContext;
                    return Ok(InlineCodeStep::Progress { work });
                }
            }
            if work.byte_reads + work.transitions + 8 > budget {
                return Ok(InlineCodeStep::Progress { work });
            }
            let Some(&byte) = frame.bytes.get(self.next - frame.byte_start) else {
                return Ok(InlineCodeStep::NeedInput { work });
            };
            work.byte_reads += 1;
            work.transitions += 1;
            if matches!(self.phase, Phase::Run) && byte != b'`' {
                self.begin_probe()?;
                return Ok(InlineCodeStep::Progress { work }); // 原非 tick byte 尚未消费，后继按真实 opener.end 续读。
            }
            if matches!(self.phase, Phase::EmphasisRun) && byte != self.delimiter {
                self.html_len = 0;
                self.phase = Phase::EmphasisContext;
                return Ok(InlineCodeStep::Progress { work }); // 后继scalar仅前瞻，canonical cursor仍停在run末端。
            }
            match self.phase {
                Phase::Run => {
                    self.run_count = self
                        .run_count
                        .checked_add(1)
                        .ok_or(Error::ArithmeticOverflow)?;
                    self.next += 1;
                }
                Phase::Bang => {
                    self.phase = Phase::Normal;
                    if byte == b'[' {
                        self.start_link(true)?;
                        return Ok(InlineCodeStep::Progress { work });
                    }
                    self.previous = Some('!');
                    self.previous_len = 0;
                }
                Phase::Backslash => {
                    if byte == b'`' {
                        self.start_run(true);
                        self.next += 1;
                    } else if byte.is_ascii_punctuation() {
                        self.next += 1;
                        self.observe_previous(byte)?;
                        work.transitions += usize::from(self.emphasis) * 4;
                        self.phase = Phase::Normal;
                    } else {
                        self.phase = Phase::Normal;
                    }
                }
                Phase::Normal => {
                    if byte == b'`' {
                        self.start_run(false);
                        self.next += 1;
                    } else if byte == b'\\' {
                        self.observe_previous(byte)?;
                        work.transitions += usize::from(self.emphasis) * 4;
                        self.next += 1;
                        self.phase = Phase::Backslash;
                    } else if byte == b'<' {
                        self.html[0] = byte;
                        self.html_len = 1;
                        self.next += 1;
                        self.phase = Phase::Html;
                    } else if byte == b'['
                        && self.link_probe.is_none()
                        && self.scoped.is_none()
                        && !self.table_cell
                    {
                        self.start_link(false)?;
                        return Ok(InlineCodeStep::Progress { work });
                    } else if byte == b'!'
                        && self.link_probe.is_none()
                        && self.scoped.is_none()
                        && !self.table_cell
                    {
                        // 只有随后真实 '[' 才建立 Image opener；普通感叹号仍是原字面。
                        self.run_start = self.next;
                        self.next += 1;
                        self.phase = Phase::Bang;
                    } else if (matches!(byte, b'[' | b']')
                        && self.link_probe.is_none()
                        && (self.scoped.is_some() || self.table_cell))
                        || (self.scoped.is_some() && byte == b'&')
                        || (self.options.contains(crate::Options::ENABLE_MATH)
                            && matches!(byte, b'$' | b'{' | b'}'))
                    {
                        self.phase = Phase::General;
                        return Ok(InlineCodeStep::NeedsGeneralInline { work });
                    } else if self.emphasis
                        && (matches!(byte, b'*' | b'_')
                            || byte == b'~'
                                && self.options.intersects(
                                    crate::Options::ENABLE_STRIKETHROUGH
                                        | crate::Options::ENABLE_SUBSCRIPT,
                                )
                            || byte == b'^'
                                && self.options.contains(crate::Options::ENABLE_SUPERSCRIPT))
                    {
                        self.delimiter = byte;
                        self.delimiter_before = self.previous;
                        self.run_start = self.next;
                        self.run_count = 1;
                        self.next += 1;
                        self.requested_end = self.scan_end(); // 原块/已认证label才是真匹配域，视口尾不是EOF。
                        self.phase = Phase::EmphasisRun;
                    } else {
                        self.general_styles |= matches!(byte, b'*' | b'_')
                            || (byte == b'~'
                                && self.options.intersects(
                                    crate::Options::ENABLE_STRIKETHROUGH
                                        | crate::Options::ENABLE_SUBSCRIPT,
                                ))
                            || (byte == b'^'
                                && self.options.contains(crate::Options::ENABLE_SUPERSCRIPT));
                        self.next += 1;
                        self.observe_previous(byte)?;
                        work.transitions += usize::from(self.emphasis) * 4;
                        if self.scoped.is_some() && matches!(byte, b'\r' | b'\n') {
                            self.arm_continuation(self.next, byte);
                            return Ok(InlineCodeStep::Progress { work });
                        }
                    }
                }
                Phase::EmphasisRun => {
                    self.run_count += 1;
                    self.next += 1;
                }
                Phase::Html => {
                    if self.scoped.is_some() && matches!(byte, b'\r' | b'\n') {
                        self.phase = Phase::General;
                        return Ok(InlineCodeStep::NeedsGeneralInline { work });
                    } // 多行 HTML 尚未完成原 origin 投影时保留真实未知，不把容器标记拼进 HTML。
                    if self.html_len == HTML_BYTES {
                        self.phase = Phase::General;
                        return Ok(InlineCodeStep::NeedsGeneralInline { work });
                    }
                    if byte == b'>' {
                        let cost = (self.html_len + 1) * 4;
                        if work.byte_reads + work.transitions + cost > budget {
                            return Ok(InlineCodeStep::Progress { work });
                        }
                        work.transitions += cost;
                        self.html[self.html_len] = byte;
                        let length = self.html_len + 1;
                        if let Some(autolink) = root_html(&self.html[..length]) {
                            self.general_styles |= autolink;
                            self.next += 1;
                            self.previous = Some('>');
                            self.previous_len = 0;
                            self.html_len = 0;
                            self.phase = Phase::Normal;
                            continue;
                        }
                    }
                    self.html[self.html_len] = byte;
                    self.html_len += 1;
                    self.next += 1;
                }
                _ => return Err(Error::InvalidFrame),
            }
        }
    }
    /// opener 只能从顺序主游标的实际 tick run 产生，HTML/链接前瞻没有制造它的入口。
    fn start_run(&mut self, escaped: bool) {
        self.run_start = self.next;
        self.run_count = 1;
        self.run_escaped = escaped;
        self.phase = Phase::Run;
    }

    /// 有界保存真实前一个Unicode scalar；HTML/code跳跃在其真实末字节处另行重设。
    fn observe_previous(&mut self, byte: u8) -> Result<(), Error> {
        if !self.emphasis {
            return Ok(());
        }
        if byte.is_ascii() {
            self.previous = Some(char::from(byte));
            self.previous_len = 0;
            return Ok(());
        }
        self.previous_bytes[self.previous_len] = byte;
        self.previous_len += 1;
        match std::str::from_utf8(&self.previous_bytes[..self.previous_len]) {
            Ok(text) => {
                self.previous = text.chars().next();
                self.previous_len = 0;
            }
            Err(error) if error.error_len().is_none() && self.previous_len < 4 => {}
            Err(_) => return Err(Error::InvalidFrame),
        }
        Ok(())
    }

    /// 只有完整后继scalar或真块末端才能交付run；原flanking和扩展开关与firstpass共用。
    fn finish_emphasis(&mut self, next: Option<char>, work: GlobalWork) -> InlineCodeStep {
        let valid = self.delimiter != b'~' || self.run_count <= 2;
        let run = VerifiedInlineEmphasisRun {
            source: self.run_start..self.next,
            scope: self
                .link_probe
                .as_ref()
                .and_then(LinkProbe::label_range)
                .unwrap_or_else(|| self.scope_range.clone()),
            delimiter: self.delimiter,
            can_open: valid
                && crate::inline_emphasis::can_open(
                    self.delimiter_before,
                    next,
                    self.delimiter,
                    self.run_count,
                    false,
                    false,
                    self.options,
                ),
            can_close: valid
                && crate::inline_emphasis::can_close(
                    self.delimiter_before,
                    next,
                    self.delimiter,
                    self.run_count,
                    false,
                    false,
                    self.options,
                ),
        };
        self.previous = Some(char::from(self.delimiter));
        self.previous_len = 0;
        self.html_len = 0;
        self.phase = Phase::Normal;
        InlineCodeStep::Emphasis { run, work }
    }
    /// 只有完整认证过的label可以收窄匹配域；请求窗和短帧绝不能成为code的闭合边界。
    fn scan_end(&self) -> usize {
        self.link_probe
            .as_ref()
            .and_then(LinkProbe::label_range)
            .map_or(self.end, |label| label.end)
    }
    fn begin_probe(&mut self) -> Result<(), Error> {
        self.previous = Some('`');
        self.previous_len = 0;
        let candidate = CodeDelimiterCandidate {
            revision: self.revision,
            start: self.run_start,
            end: self.next,
            raw_count: self.run_count,
            escaped: self.run_escaped,
        };
        self.probe = CodeSpanProbe::from_opener(candidate, self.source_end, self.scan_end())
            .map_err(inline_error)?;
        self.phase = if self.probe.is_some() {
            Phase::Probe
        } else {
            Phase::Normal
        };
        Ok(())
    }
}

/// 使用原 autolink/HTML/comment/PI scanner 的确切接受条件；超固定 scratch 的 HTML 保持未知。
fn root_html(bytes: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(bytes).ok()?;
    if crate::scanners::scan_autolink(text, 1).is_some_and(|(end, _, _)| end == bytes.len()) {
        return Some(true);
    }
    let mut guard = crate::parse::HtmlScanGuard::default();
    let end = match bytes.get(1) {
        Some(b'!') => crate::scanners::scan_inline_html_comment(bytes, 2, &mut guard),
        Some(b'?') => crate::scanners::scan_inline_html_processing(bytes, 2, &mut guard),
        _ => crate::scanners::scan_html_block_inner(bytes, Some(&|_| 0)).map(|(_, end)| end),
    }?;
    (end == bytes.len()).then_some(false)
}

/// 子 lexical 游标错误保留真实失效种类，不把预算、取消或坐标错误伪装成未匹配代码。
fn inline_error(error: crate::inline_frontier::InlineError) -> Error {
    use crate::inline_frontier::InlineError;
    match error {
        InlineError::Cancelled => Error::Cancelled,
        InlineError::Overflow => Error::ArithmeticOverflow,
        InlineError::InvalidBudget => Error::InvalidBudget,
        InlineError::InvalidFrame | InlineError::InvalidRange | InlineError::Closed => {
            Error::InvalidFrame
        }
    }
}
