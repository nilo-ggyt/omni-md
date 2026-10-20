// Copyright 2015 Google Inc. All rights reserved.
// 原 LinkStack 的有限外存括号组与源序 replay；完整 MIT 许可见 LICENSE。

//! 每次只保当前语法组的固定状态，原文件拥有所有深度记录与真实读写费用。
//! 直接目标/标题/code/HTML 沿原 scanner，引用在同 Global 回答前保持真实暂停。
//! 文件记录不是事件 AST；只有原语法闭合且 replay 到其源位置时才签不可构造的 proof。

use super::*;
use crate::definition_scan::{DestinationResult, DestinationScanner, TitleScanner, TitleStep};

#[derive(Clone, Copy, Debug)]
enum LinkPhase {
    Label,
    Bang,
    Escape,
    Ticks,
    Code,
    Html,
    OpenTarget,
    BeforeDestination,
    Destination,
    AfterDestination,
    Title,
    AfterTitle,
    ReferenceStart,
    ReferenceLabel,
    AwaitReference,
    Replay,
}
#[derive(Debug)]
pub(super) enum LinkStep {
    Progress,
    NeedInput,
    Storage(InlineLinkStorageRequest),
    Reference(VerifiedInlineReferenceRequest),
    Replay,
    General,
}
#[derive(Debug)]
pub(super) enum StoragePurpose {
    Append,
    Connect {
        id: InlineLinkRecordId,
        record: InlineLinkRecord,
    },
    Closed,
    Parent,
    ReplayFirst,
    ReplayNext,
    ReplayParent,
}
#[derive(Debug)]
pub(super) enum ReplayStep {
    Progress,
    Storage(InlineLinkStorageRequest),
    Begin(VerifiedInlineLink),
    End {
        source: Range<usize>,
        label: Range<usize>,
        next: usize,
        previous: char,
    },
    Done {
        next: usize,
        suppressed_close: Option<usize>,
    },
    None,
}

#[derive(Debug)]
/// 括号记录只落 Core 原文件；内存只保当前 opener、源序尾和当前 replay 项。
/// 先完成同一括号组的 LinkStack 决策，再按源序重借 label，保证外层 Begin 先于内层内容。
/// 已关闭的子项也留在原文件 source-order 链，既不建括号 Vec，也不存一份 Parser 事件图。
/// 每个存储暂停保原请求，不能在另一 Source/请求回复上继续；真实 errno 归文件拥有者。
pub(super) struct LinkProbe {
    revision: u64,
    source_end: usize,
    block: Range<usize>,
    options: crate::Options,
    next: usize,
    phase: LinkPhase,
    decision: crate::inline_scan::LinkStackDecision,
    current: Option<(InlineLinkRecordId, InlineLinkRecord)>,
    first: Option<InlineLinkRecordId>,
    last: Option<(InlineLinkRecordId, InlineLinkRecord)>,
    pub(super) pending: Option<(InlineLinkStorageRequest, StoragePurpose)>,
    request_serial: usize,
    reference_request: Option<VerifiedInlineReferenceRequest>,
    reference_scanner: Option<crate::linklabel::LabelScanner>,
    reference_start: usize,
    reference_owner_end: usize,
    reference_kind: InlineReferenceKind,
    reference_first: Option<u8>,
    reference_footnote: bool,
    suppressed_close: Option<usize>,
    ticks: usize,
    tick_start: usize,
    escaped_tick: bool,
    code: Option<CodeSpanProbe>,
    html: [u8; HTML_BYTES],
    html_len: usize,
    destination: DestinationScanner,
    destination_start: usize,
    destination_range: Range<usize>,
    title: TitleScanner,
    title_start: usize,
    title_range: Option<Range<usize>>,
    separator_eol: bool,
    separator_cr: bool,
    replay_end: usize,
    replay_candidate: Option<(InlineLinkRecordId, InlineLinkRecord)>,
    replay_active: Option<(InlineLinkRecordId, InlineLinkRecord)>,
    replay_loading: bool,
}
impl LinkProbe {
    /// 真正 root opener 才能建立括号组。源范围/开关沿原 seed，初始 append 也必须暂停到原文件。
    /// 没有文件回复时不读 label，更不能提前产生 Link proof；错误由外层 cursor 固定为首错。
    pub(super) fn new(
        revision: u64,
        source_end: usize,
        block: Range<usize>,
        start: usize,
        image: bool,
        options: crate::Options,
        suppressed_close: Option<usize>,
    ) -> Result<Self, Error> {
        let mut probe = Self {
            revision,
            source_end,
            block,
            options,
            next: start + if image { 2 } else { 1 },
            phase: LinkPhase::Label,
            decision: Default::default(),
            current: None,
            first: None,
            last: None,
            pending: None,
            request_serial: 0,
            reference_request: None,
            reference_scanner: None,
            reference_start: 0,
            reference_owner_end: 0,
            reference_kind: InlineReferenceKind::Shortcut,
            reference_first: None,
            reference_footnote: false,
            suppressed_close,
            ticks: 0,
            tick_start: 0,
            escaped_tick: false,
            code: None,
            html: [0; HTML_BYTES],
            html_len: 0,
            destination: DestinationScanner::new(crate::parse::LINK_MAX_NESTED_PARENS),
            destination_start: 0,
            destination_range: 0..0,
            title: TitleScanner::new(),
            title_start: 0,
            title_range: None,
            separator_eol: false,
            separator_cr: false,
            replay_end: 0,
            replay_candidate: None,
            replay_active: None,
            replay_loading: false,
        };
        probe.open(start, image)?;
        Ok(probe)
    }
    pub(super) fn next_byte(&self) -> usize {
        self.code
            .as_ref()
            .map_or(self.next, CodeSpanProbe::next_byte)
    }
    pub(super) fn replaying(&self) -> bool {
        matches!(self.phase, LinkPhase::Replay)
    }
    pub(super) fn label_range(&self) -> Option<Range<usize>> {
        self.replay_active
            .as_ref()
            .map(|(_, record)| record.label_start..record.label_end)
    }
    /// 一个游标同时只有一个真实文件操作。请求序号与完整操作值共同核对，
    /// Progress 不替换它；其实际 flush 工作只收费一次，后继继续同一个操作。
    fn store(
        &mut self,
        operation: InlineLinkStorageOperation,
        purpose: StoragePurpose,
    ) -> Result<(), Error> {
        if self.pending.is_some() {
            return Err(Error::InvalidFrame);
        }
        self.request_serial = self
            .request_serial
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow)?;
        self.pending = Some((
            InlineLinkStorageRequest {
                request_id: self.request_serial,
                operation,
            },
            purpose,
        ));
        Ok(())
    }
    /// previous 指向当前真实括号父节点，next 暂不确定；与强调 checkpoint 的物理槽相邻无关。
    /// 先 append，再更新源序尾 next，最后才切换当前 opener，避免半完成链被 replay 消费。
    fn open(&mut self, start: usize, image: bool) -> Result<(), Error> {
        let serial = self.decision.open().ok_or(Error::ArithmeticOverflow)?;
        let record = InlineLinkRecord {
            previous: self.current.as_ref().map(|(id, _)| *id),
            next: None,
            serial,
            source_start: start,
            label_start: start + if image { 2 } else { 1 },
            label_end: 0,
            source_end: 0,
            destination: 0..0,
            title: None,
            reference: None,
            kind: if image {
                InlineLinkKind::Image
            } else {
                InlineLinkKind::Link
            },
            status: InlineLinkRecordStatus::Open,
        };
        self.store(
            InlineLinkStorageOperation::Append(record),
            StoragePurpose::Append,
        )
    }
    /// 只有 append 与前一 source-order 边都完成才安装当前 opener。
    /// last 是唯一尾记录副本，之后关闭或接后继时同步更新，不作为第二历史目录。
    fn install_open(&mut self, id: InlineLinkRecordId, record: InlineLinkRecord) {
        self.first.get_or_insert(id);
        self.last = Some((id, record.clone()));
        self.current = Some((id, record));
        self.phase = LinkPhase::Label;
    }
    /// 文件操作完成只恢复该原请求；Progress 不能把未落盘的一半记录签成完成。
    /// 真实分配由借用端准入，不进入 scope workspace getter，避免两层重复占用 quota。
    pub(super) fn accept_storage(
        &mut self,
        request: &InlineLinkStorageRequest,
        reply: InlineLinkStorageReply,
    ) -> Result<(), Error> {
        let (pending, purpose) = self.pending.take().ok_or(Error::InvalidFrame)?;
        if pending != *request {
            return Err(Error::InvalidFrame);
        }
        match (purpose, reply, pending.operation) {
            (
                StoragePurpose::Append,
                InlineLinkStorageReply::Appended { id },
                InlineLinkStorageOperation::Append(record),
            ) => {
                if let Some((last_id, mut last)) = self.last.clone() {
                    last.next = Some(id);
                    if self
                        .current
                        .as_ref()
                        .is_some_and(|(current, _)| *current == last_id)
                    {
                        self.current = Some((last_id, last.clone()));
                    }
                    self.store(
                        InlineLinkStorageOperation::Update {
                            id: last_id,
                            record: last,
                        },
                        StoragePurpose::Connect { id, record },
                    )?;
                } else {
                    self.install_open(id, record);
                }
            }
            (
                StoragePurpose::Connect { id, record },
                InlineLinkStorageReply::Updated,
                InlineLinkStorageOperation::Update { .. },
            ) => self.install_open(id, record),
            (
                StoragePurpose::Closed,
                InlineLinkStorageReply::Updated,
                InlineLinkStorageOperation::Update { id, record },
            ) => {
                if self.last.as_ref().is_some_and(|(last, _)| *last == id) {
                    self.last = Some((id, record.clone()));
                }
                // 记录已闭合；父 opener 从原文件读取，不能用尾记录（可能是内层孙项）冒充父。
                self.current = None;
                if let Some(parent) = record.previous {
                    self.store(
                        InlineLinkStorageOperation::Read(parent),
                        StoragePurpose::Parent,
                    )?;
                } else {
                    // 根组到此完成。即使根为 Literal，其先前接受的子项仍按源序 replay。
                    // main cursor 会从原 opener 重借，不从这里的前瞻末端继续发样式。
                    self.replay_end = self.next;
                    self.phase = LinkPhase::Replay;
                    let first = self.first.ok_or(Error::InvalidFrame)?;
                    self.store(
                        InlineLinkStorageOperation::Read(first),
                        StoragePurpose::ReplayFirst,
                    )?;
                }
            }
            (
                StoragePurpose::Parent,
                InlineLinkStorageReply::Read { record },
                InlineLinkStorageOperation::Read(id),
            ) => {
                self.check_record(&record)?;
                self.current = Some((id, record));
                self.phase = LinkPhase::Label;
            }
            (
                StoragePurpose::ReplayFirst | StoragePurpose::ReplayNext,
                InlineLinkStorageReply::Read { record },
                InlineLinkStorageOperation::Read(id),
            ) => {
                self.check_record(&record)?;
                self.replay_candidate = Some((id, record));
                self.replay_loading = false;
            }
            (
                StoragePurpose::ReplayParent,
                InlineLinkStorageReply::Read { record },
                InlineLinkStorageOperation::Read(id),
            ) => {
                self.check_record(&record)?;
                // 恢复最近的真正样式 scope；Literal 祖先不建 scope，必须沿真实父链继续找。
                // 这一步只做原文件 metadata I/O，不能重新读取父 label 或推算强调下界。
                if matches!(
                    record.status,
                    InlineLinkRecordStatus::Direct | InlineLinkRecordStatus::Reference
                ) {
                    self.replay_active = Some((id, record));
                } else if let Some(parent) = record.previous {
                    self.store(
                        InlineLinkStorageOperation::Read(parent),
                        StoragePurpose::ReplayParent,
                    )?;
                }
            }
            _ => return Err(Error::InvalidFrame),
        }
        Ok(())
    }
    fn check_record(&self, record: &InlineLinkRecord) -> Result<(), Error> {
        // 文件是同 Source 的实际 metadata 边界；坐标损坏不能借原 seed 变成新的权限。
        if record.source_start < self.block.start
            || record.label_start < record.source_start
            || record.label_start > self.block.end
            || record.label_end > self.block.end
            || record.source_end > self.block.end
            || record.serial == 0
            || (record.status != InlineLinkRecordStatus::Open
                && (record.label_end < record.label_start || record.source_end < record.label_end))
        {
            return Err(Error::InvalidFrame);
        }
        Ok(())
    }
    /// 引用请求从已闭合且验证过 key 的当前记录产生。后继 replay 仅重建同一身份，
    /// 不再解析/复制 key，也不创建另一张 Resolved/Missing 表。
    fn request_for(
        &self,
        id: InlineLinkRecordId,
        record: &InlineLinkRecord,
    ) -> Result<VerifiedInlineReferenceRequest, Error> {
        let (kind, reference) = record.reference.clone().ok_or(Error::InvalidFrame)?;
        Ok(VerifiedInlineReferenceRequest {
            revision: self.revision,
            source_end: self.source_end,
            block: self.block.clone(),
            record: id,
            source: record.source_start..record.source_end,
            label: record.label_start..record.label_end,
            reference,
            kind,
        })
    }
    /// Core 已在同 Source/Queries 完成真实查询后恢复；请求不匹配立即失败，
    /// 不能用另一个同字符串 key 的结果结束当前暂停，Missing 也不能触发盲重查。
    pub(super) fn accept_reference(
        &mut self,
        request: &VerifiedInlineReferenceRequest,
        resolution: InlineReferenceResolution,
    ) -> Result<(), Error> {
        if self.reference_request.as_ref() != Some(request) {
            return Err(Error::InvalidFrame);
        }
        self.reference_request = None;
        let (_, record) = self.current.as_mut().ok_or(Error::InvalidFrame)?;
        record.status = match resolution {
            InlineReferenceResolution::Resolved => InlineLinkRecordStatus::Reference,
            InlineReferenceResolution::Missing => InlineLinkRecordStatus::Literal,
        };
        if resolution == InlineReferenceResolution::Missing {
            // 原 Full reference 失败不吞第二个标签。它的 closing 不再尝试 shortcut/collapsed，
            // 但紧随其后的真实 (destination) 仍可使第二标签成为直接链接。
            self.next = record.label_end + 1;
        } else if record.kind == InlineLinkKind::Link {
            self.decision.accept_link();
        }
        self.close_current()
    }
    /// 关闭先写原记录，之后才弹父；如果文件失败，原拥有者结束请求，
    /// 不把未落盘状态当成功，也不尝试重新执行一个结局未知的写入。
    fn close_current(&mut self) -> Result<(), Error> {
        let (id, record) = self.current.as_ref().ok_or(Error::InvalidFrame)?;
        self.store(
            InlineLinkStorageOperation::Update {
                id: *id,
                record: record.clone(),
            },
            StoragePurpose::Closed,
        )
    }
    /// 失效 opener 仍按原字面被 main cursor 扫描。源序子项没有删除，
    /// 因而合法内层 Link/Image 不会因外层未闭合或被禁用而丢失。
    fn literal(&mut self) -> Result<(), Error> {
        let (_, record) = self.current.as_mut().ok_or(Error::InvalidFrame)?;
        record.status = InlineLinkRecordStatus::Literal;
        record.source_end = record.label_end + 1;
        self.next = record.source_end;
        self.close_current()
    }
    /// 先尝试紧邻的第二标签；若其词法不合法，再按原 Parser 回退外标签。
    /// raw key 含 closing bracket，让 frame 切分不影响关闭/空标签判定。
    fn begin_reference(&mut self, full: bool) -> Result<(), Error> {
        let (_, record) = self.current.as_ref().ok_or(Error::InvalidFrame)?;
        self.reference_kind = if full {
            InlineReferenceKind::Full
        } else {
            InlineReferenceKind::Shortcut
        };
        self.reference_start = if full {
            self.next + 1
        } else {
            record.label_start
        };
        self.reference_owner_end = record.label_end + 1;
        self.next = self.reference_start;
        self.reference_scanner = Some(crate::linklabel::LabelScanner::new());
        self.reference_first = None;
        self.reference_footnote = false;
        self.phase = LinkPhase::ReferenceLabel;
        Ok(())
    }
    /// 直接目标失败必须回到真实 label close 之后；先前的 target/title 前瞻
    /// 不能吞原正文，也不能把其中括号从后继普通扫描里消失。
    fn fallback_reference(&mut self) -> Result<(), Error> {
        let (_, record) = self.current.as_ref().ok_or(Error::InvalidFrame)?;
        self.next = record.label_end + 1;
        self.phase = LinkPhase::ReferenceStart;
        self.code = None;
        self.reference_scanner = None;
        Ok(())
    }
    /// 只有真实 closing parenthesis 已消费，才把候选升级为 Direct。
    /// 此时禁用原栈上的 Link 祖先，Image 保持可用；写入失败仍由文件拥有者终止。
    fn finish_direct(&mut self) -> Result<(), Error> {
        let (_, record) = self.current.as_mut().ok_or(Error::InvalidFrame)?;
        record.source_end = self.next;
        record.destination = self.destination_range.clone();
        record.title = self.title_range.clone();
        record.status = InlineLinkRecordStatus::Direct;
        if record.kind == InlineLinkKind::Link {
            self.decision.accept_link();
        }
        self.close_current()
    }
    /// 原共享 label scanner 先验证真正完整 key，Core 才能暂停到原 Global 查询。
    /// 无效 Full label 回退外 label；合法但 Missing 的 Full 不另试 shortcut，沿原 Parser 规则。
    fn advance_reference(
        &mut self,
        frame: &SourceFrame<'_>,
        budget: usize,
        work: &mut GlobalWork,
    ) -> Result<LinkStep, Error> {
        let scanner = self.reference_scanner.as_mut().ok_or(Error::InvalidFrame)?;
        let available = frame
            .bytes
            .get(self.next.saturating_sub(frame.byte_start)..)
            .unwrap_or(&[]);
        if self.reference_first.is_none() && !available.is_empty() {
            self.reference_first = Some(available[0]);
        }
        if self.reference_first == Some(b'^')
            && self.options.contains(crate::Options::ENABLE_FOOTNOTES)
        {
            // '^]' 仍是普通 key；只有原 scanner 的真实第二字符才能区分 Footnote。
            let second = if self.next == self.reference_start {
                available.get(1).copied()
            } else {
                available.first().copied()
            };
            self.reference_footnote |= second.is_some_and(|byte| byte != b']');
        }
        let child = budget.saturating_sub(work.byte_reads + work.transitions + 16) / 8;
        if child == 0 {
            return Ok(LinkStep::Progress);
        }
        let cap = available.len().min(child).min(self.block.end - self.next);
        let mut output = [0u8; 64];
        let before = scanner.transitions;
        let (read, written, step) = scanner.feed(
            &available[..cap],
            self.next + cap == self.block.end,
            &mut output,
        );
        self.next += read;
        work.byte_reads += read;
        work.transitions += scanner.transitions - before + written;
        match step {
            crate::linklabel::LabelStep::NeedLine => {
                scanner.continue_line(0).ok_or(Error::InvalidFrame)?;
                Ok(LinkStep::Progress)
            }
            crate::linklabel::LabelStep::Complete => {
                if self.reference_footnote {
                    // 原 scan_reference 的 UnexpectedFootnote 不会吞掉后继标签；先试外层
                    // shortcut。真正外层 Footnote 则保自己的语义路径，不能伪普通引用。
                    if self.reference_kind == InlineReferenceKind::Full {
                        self.begin_reference(false)?;
                        return Ok(LinkStep::Progress);
                    }
                    return Ok(LinkStep::General);
                }
                let (id, record) = self.current.as_mut().ok_or(Error::InvalidFrame)?;
                if self.reference_kind == InlineReferenceKind::Full {
                    self.suppressed_close = Some(self.next - 1);
                }
                record.reference = Some((self.reference_kind, self.reference_start..self.next));
                record.source_end = match self.reference_kind {
                    InlineReferenceKind::Shortcut => record.label_end + 1,
                    InlineReferenceKind::Collapsed => self.reference_owner_end,
                    InlineReferenceKind::Full => self.next,
                };
                self.next = record.source_end;
                let id = *id;
                let record = record.clone();
                self.reference_scanner = None;
                let request = self.request_for(id, &record)?;
                self.reference_request = Some(request.clone());
                self.phase = LinkPhase::AwaitReference;
                Ok(LinkStep::Reference(request))
            }
            crate::linklabel::LabelStep::Rejected => {
                if self.reference_kind == InlineReferenceKind::Full {
                    let collapsed =
                        self.next == self.reference_start + 1 && available.first() == Some(&b']');
                    let owner_end = self.next;
                    if collapsed
                        && self.current.as_ref().is_some_and(|(_, record)| {
                            self.suppressed_close == Some(record.label_end)
                        })
                    {
                        self.literal()?;
                    } else {
                        self.begin_reference(false)?;
                        if collapsed {
                            self.reference_kind = InlineReferenceKind::Collapsed;
                            self.reference_owner_end = owner_end;
                        }
                    }
                    Ok(LinkStep::Progress)
                } else {
                    self.literal()?;
                    Ok(LinkStep::Progress)
                }
            }
            crate::linklabel::LabelStep::NeedInput
                if cap == available.len() && self.next < self.block.end =>
            {
                Ok(LinkStep::NeedInput)
            }
            _ => Ok(LinkStep::Progress),
        }
    }
    /// 每步只读同 Source 当前 frame，delimiter/code/HTML/target 优先级与原 scanner 共用。
    /// 存储或引用请求一旦出现立即归还，不跨它们继续读正文，也不在回调里保留借帧。
    pub(super) fn advance(
        &mut self,
        frame: &SourceFrame<'_>,
        budget: usize,
        work: &mut GlobalWork,
    ) -> Result<LinkStep, Error> {
        if let Some((request, _)) = &self.pending {
            return Ok(LinkStep::Storage(request.clone()));
        }
        if let Some(request) = &self.reference_request {
            return Ok(LinkStep::Reference(request.clone()));
        }
        if self.replaying() {
            return Ok(LinkStep::Replay);
        }
        if matches!(self.phase, LinkPhase::ReferenceLabel) {
            return self.advance_reference(frame, budget, work);
        }
        if let Some(probe) = self.code.as_mut() {
            let child = budget.saturating_sub(work.byte_reads + work.transitions);
            if child < crate::inline_frontier::MIN_INLINE_STEP_WORK + 1 {
                return Ok(LinkStep::Progress);
            }
            let step = probe
                .advance(
                    SourceFrame {
                        source_revision: frame.source_revision,
                        byte_start: frame.byte_start,
                        bytes: frame.bytes,
                        real_eof: frame.real_eof,
                    },
                    child,
                )
                .map_err(inline_error)?;
            let measured = match &step {
                CodeSpanStep::Progress { work }
                | CodeSpanStep::NeedInput { work }
                | CodeSpanStep::Candidate { work, .. }
                | CodeSpanStep::Unmatched { work, .. } => *work,
            };
            work.byte_reads += measured.byte_reads;
            work.transitions += measured.transitions + measured.entity_lookup_byte_work_bound;
            match step {
                CodeSpanStep::Candidate { candidate, .. } => {
                    self.next = candidate.range().end;
                    self.code = None;
                    self.phase = LinkPhase::Label;
                }
                CodeSpanStep::Unmatched { replay_from, .. } => {
                    self.next = replay_from;
                    self.code = None;
                    self.phase = LinkPhase::Label;
                }
                CodeSpanStep::NeedInput { .. } => return Ok(LinkStep::NeedInput),
                CodeSpanStep::Progress { .. } => return Ok(LinkStep::Progress),
            }
            return Ok(LinkStep::Progress);
        }
        while work.byte_reads + work.transitions + 16 <= budget {
            if self.next == self.block.end {
                if matches!(self.phase, LinkPhase::Html) {
                    // 尚未完成原 HTML 优先级证明，label_end 也尚不存在；不能跳去目标
                    // 回退并用零坐标重读块前方。保留原独立 General 路径处理该语法。
                    return Ok(LinkStep::General);
                }
                if matches!(
                    self.phase,
                    LinkPhase::Label | LinkPhase::Bang | LinkPhase::Escape | LinkPhase::Ticks
                ) {
                    // 未闭合外括号为字面，但此前已经闭合的子链接必须保留，不可把整组降级丢弃。
                    let (_, record) = self.current.as_mut().ok_or(Error::InvalidFrame)?;
                    record.label_end = self.block.end;
                    record.source_end = self.block.end;
                    record.status = InlineLinkRecordStatus::Literal;
                    self.close_current()?;
                    return Ok(LinkStep::Progress);
                }
                if matches!(
                    self.phase,
                    LinkPhase::OpenTarget | LinkPhase::ReferenceStart
                ) {
                    if self
                        .current
                        .as_ref()
                        .is_some_and(|(_, record)| self.suppressed_close == Some(record.label_end))
                    {
                        self.literal()?;
                    } else {
                        self.begin_reference(false)?;
                    }
                } else {
                    self.fallback_reference()?;
                }
                return Ok(LinkStep::Progress);
            }
            let Some(&byte) = frame.bytes.get(self.next.saturating_sub(frame.byte_start)) else {
                return Ok(LinkStep::NeedInput);
            };
            work.byte_reads += 1;
            work.transitions += 3;
            match self.phase {
                LinkPhase::Label => match byte {
                    b'\\' => {
                        self.next += 1;
                        self.phase = LinkPhase::Escape;
                    }
                    b'`' => self.begin_ticks(false),
                    b'<' => {
                        self.html[0] = byte;
                        self.html_len = 1;
                        self.next += 1;
                        self.phase = LinkPhase::Html;
                    }
                    b'!' => {
                        self.next += 1;
                        self.phase = LinkPhase::Bang;
                    }
                    b'[' => {
                        let at = self.next;
                        self.next += 1;
                        self.open(at, false)?;
                        return Ok(LinkStep::Progress);
                    }
                    b']' => {
                        let (_, record) = self.current.as_mut().ok_or(Error::InvalidFrame)?;
                        record.label_end = self.next;
                        self.next += 1;
                        if !self
                            .decision
                            .enabled(record.serial, record.kind == InlineLinkKind::Image)
                        {
                            self.literal()?;
                            return Ok(LinkStep::Progress);
                        }
                        self.phase = LinkPhase::OpenTarget;
                    }
                    _ => self.next += 1,
                },
                // '!' 只在后继真实 '[' 时成为 Image opener；其它位置仍按原字面保留。
                // 两字节可跨帧，但不把第一帧尾的 '!' 当新语义边界。
                LinkPhase::Bang => {
                    self.phase = LinkPhase::Label;
                    if byte == b'[' {
                        let at = self.next - 1;
                        self.next += 1;
                        self.open(at, true)?;
                        return Ok(LinkStep::Progress);
                    }
                }
                LinkPhase::Escape => {
                    if byte == b'`' {
                        self.begin_ticks(true);
                    } else {
                        if byte.is_ascii_punctuation() {
                            self.next += 1;
                        }
                        self.phase = LinkPhase::Label;
                    }
                }
                // code 能跨越看似 closing bracket，必须先交原 CodeSpanProbe 决定。
                // 未匹配时按 replay_from 重借源，不能吞掉其后本应参与链接的括号。
                LinkPhase::Ticks => {
                    if byte == b'`' {
                        self.ticks = self.ticks.checked_add(1).ok_or(Error::ArithmeticOverflow)?;
                        self.next += 1;
                    } else {
                        self.code = CodeSpanProbe::from_opener(
                            CodeDelimiterCandidate {
                                revision: self.revision,
                                start: self.tick_start,
                                end: self.next,
                                raw_count: self.ticks,
                                escaped: self.escaped_tick,
                            },
                            self.source_end,
                            self.block.end,
                        )
                        .map_err(inline_error)?;
                        self.phase = if self.code.is_some() {
                            LinkPhase::Code
                        } else {
                            LinkPhase::Label
                        };
                        return Ok(LinkStep::Progress);
                    }
                }
                LinkPhase::Html => {
                    if self.html_len == HTML_BYTES {
                        return Ok(LinkStep::General);
                    }
                    if byte == b'>' {
                        let cost = (self.html_len + 1) * 4;
                        if work.byte_reads + work.transitions + cost > budget {
                            return Ok(LinkStep::Progress);
                        }
                        work.transitions += cost;
                    }
                    self.html[self.html_len] = byte;
                    self.html_len += 1;
                    self.next += 1;
                    if byte == b'>' {
                        if let Some(autolink) = root_html(&self.html[..self.html_len]) {
                            if autolink {
                                return Ok(LinkStep::General);
                            }
                            self.phase = LinkPhase::Label;
                        }
                    }
                }
                // 直接目标的 '(' 必须紧邻 ']'；空白不是自由跳过的 reference 前缀。
                // 直接目标失败后仍按原规则试引用，不能仅因后文像 URL 就签 Direct。
                LinkPhase::OpenTarget => {
                    if byte != b'(' {
                        self.phase = LinkPhase::ReferenceStart;
                        continue;
                    }
                    self.next += 1;
                    self.reset_separator();
                    self.destination =
                        DestinationScanner::new(crate::parse::LINK_MAX_NESTED_PARENS);
                    self.title = TitleScanner::new();
                    self.title_range = None;
                    self.phase = LinkPhase::BeforeDestination;
                }
                LinkPhase::BeforeDestination
                | LinkPhase::AfterDestination
                | LinkPhase::AfterTitle => {
                    if self.separator(byte) {
                        continue;
                    }
                    match self.phase {
                        LinkPhase::BeforeDestination => {
                            self.destination_start = self.next;
                            self.phase = LinkPhase::Destination;
                        }
                        LinkPhase::AfterDestination if byte != b')' => {
                            self.title_start = self.next;
                            self.phase = LinkPhase::Title;
                        }
                        _ if byte == b')' => {
                            self.next += 1;
                            self.finish_direct()?;
                            return Ok(LinkStep::Progress);
                        }
                        _ => {
                            self.fallback_reference()?;
                            return Ok(LinkStep::Progress);
                        }
                    }
                }
                // DestinationScanner 与 whole Parser 共用，空目标完成可不消费当前 ')'。
                // 每次真实读取照常收费，title 分隔与直接闭合继续留给后继阶段处理。
                LinkPhase::Destination => {
                    let (used, result) = self.destination.feed(std::slice::from_ref(&byte), false);
                    self.next += used;
                    match result {
                        DestinationResult::NeedInput => {}
                        DestinationResult::Rejected => {
                            self.fallback_reference()?;
                            return Ok(LinkStep::Progress);
                        }
                        DestinationResult::Complete {
                            content_start,
                            content_end,
                            ..
                        } => {
                            self.destination_range = self.destination_start + content_start
                                ..self.destination_start + content_end;
                            self.reset_separator();
                            self.phase = LinkPhase::AfterDestination;
                        }
                    }
                }
                // 标题只记原范围，不在这里复制或规范化；其 '*' / '[' 不能进入标签样式栈。
                // Root 的 NeedLine 无容器前缀，继续零跳过；原真实 EOL 权限仍在 scanner 中。
                LinkPhase::Title => {
                    let (used, step) = self.title.feed(std::slice::from_ref(&byte), false);
                    self.next += used;
                    match step {
                        TitleStep::NeedInput => {}
                        TitleStep::Rejected => {
                            self.fallback_reference()?;
                            return Ok(LinkStep::Progress);
                        }
                        TitleStep::NeedLine { .. } => self.title.continue_line(0),
                        TitleStep::Complete { byte_count, .. } => {
                            self.title_range =
                                Some(self.title_start + 1..self.title_start + byte_count - 1);
                            self.reset_separator();
                            self.phase = LinkPhase::AfterTitle;
                        }
                    }
                }
                // Full reference 总能继续尝试，即使这个 close 已被前一次 full 扫描标为非 shortcut。
                // 这个区别保护 [a][b](url) 在首引用 Missing 后仍能识别真正的第二直接链接。
                LinkPhase::ReferenceStart => {
                    let (_, record) = self.current.as_ref().ok_or(Error::InvalidFrame)?;
                    if byte == b'[' {
                        self.begin_reference(true)?;
                        return Ok(LinkStep::Progress);
                    }
                    if self.suppressed_close == Some(record.label_end) {
                        self.literal()?;
                        return Ok(LinkStep::Progress);
                    }
                    self.begin_reference(false)?;
                    return Ok(LinkStep::Progress);
                }
                _ => return Err(Error::InvalidFrame),
            }
        }
        Ok(LinkStep::Progress)
    }
    fn begin_ticks(&mut self, escaped: bool) {
        self.tick_start = self.next;
        self.ticks = 1;
        self.escaped_tick = escaped;
        self.next += 1;
        self.phase = LinkPhase::Ticks;
    }
    fn reset_separator(&mut self) {
        self.separator_eol = false;
        self.separator_cr = false;
    }
    fn separator(&mut self, byte: u8) -> bool {
        if self.separator_cr {
            self.separator_cr = false;
            if byte == b'\n' {
                self.next += 1;
                return true;
            }
        }
        if crate::scanners::is_ascii_whitespace_no_nl(byte) {
            self.next += 1;
            return true;
        }
        if !self.separator_eol && matches!(byte, b'\r' | b'\n') {
            self.separator_eol = true;
            self.separator_cr = byte == b'\r';
            self.next += 1;
            return true;
        }
        false
    }
    /// 原 Parser 的 Collapsed 节点只覆盖首个 `[label]`，但节点后继已经越过空 `[]`。
    /// record.source_end 保留实际消费末端供 replay 前进；样式 owner 单独沿同一已认证
    /// Collapsed 事实取首个 closing bracket，不能给空后缀赋 LINK/marker，也不能重扫它。
    fn semantic_source(record: &InlineLinkRecord) -> Range<usize> {
        let end = if record.status == InlineLinkRecordStatus::Reference
            && matches!(record.reference, Some((InlineReferenceKind::Collapsed, _)))
        {
            record.label_end + 1
        } else {
            record.source_end
        };
        record.source_start..end
    }

    /// 原语法决策和实际记录读取都完成后签发不可外部构造的 proof。
    /// Direct 的范围只供原共享 decoder，Reference 只供原查询身份，二者不互相兜底。
    fn proof(
        &self,
        id: InlineLinkRecordId,
        record: &InlineLinkRecord,
    ) -> Result<VerifiedInlineLink, Error> {
        let target = match record.status {
            InlineLinkRecordStatus::Direct => InlineLinkTarget::Direct {
                destination: record.destination.clone(),
                title: record.title.clone(),
            },
            InlineLinkRecordStatus::Reference => InlineLinkTarget::Reference {
                request: self.request_for(id, record)?,
            },
            _ => return Err(Error::InvalidFrame),
        };
        Ok(VerifiedInlineLink {
            revision: self.revision,
            source_end: self.source_end,
            source: Self::semantic_source(record),
            label: record.label_start..record.label_end,
            kind: record.kind,
            target,
        })
    }
    /// 只按原 source-order 链推进，不凭 record ID 相邻猜语法子树。Literal opener 不建 scope，
    /// 因而外层非法链接不会隔断正常强调；被接受的 Image/Link 才有成对 Begin/End。
    pub(super) fn replay(&mut self, next: usize) -> Result<ReplayStep, Error> {
        if let Some((request, _)) = &self.pending {
            return Ok(ReplayStep::Storage(request.clone()));
        }
        if let Some((_, record)) = &self.replay_active {
            if next == record.label_end {
                let (_, record) = self.replay_active.take().unwrap();
                if let Some(parent) = record.previous {
                    self.store(
                        InlineLinkStorageOperation::Read(parent),
                        StoragePurpose::ReplayParent,
                    )?;
                }
                return Ok(ReplayStep::End {
                    source: Self::semantic_source(&record),
                    label: record.label_start..record.label_end,
                    next: record.source_end,
                    previous: if record.status == InlineLinkRecordStatus::Direct {
                        ')'
                    } else {
                        ']'
                    },
                });
            }
        }
        if let Some((id, record)) = self.replay_candidate.clone() {
            if record.source_start <= next {
                self.replay_candidate = None;
                if let Some(after) = record.next {
                    self.replay_loading = true;
                    self.store(
                        InlineLinkStorageOperation::Read(after),
                        StoragePurpose::ReplayNext,
                    )?;
                }
                if matches!(
                    record.status,
                    InlineLinkRecordStatus::Direct | InlineLinkRecordStatus::Reference
                ) {
                    let proof = self.proof(id, &record)?;
                    self.replay_active = Some((id, record));
                    return Ok(ReplayStep::Begin(proof));
                }
                return Ok(ReplayStep::Progress);
            }
        }
        if self.replay_active.is_none()
            && self.replay_candidate.is_none()
            && !self.replay_loading
            && next >= self.replay_end
        {
            return Ok(ReplayStep::Done {
                next: self.replay_end,
                suppressed_close: self.suppressed_close,
            });
        }
        Ok(ReplayStep::None)
    }
}
