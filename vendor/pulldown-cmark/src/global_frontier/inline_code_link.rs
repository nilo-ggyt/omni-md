// Copyright 2015 Google Inc. All rights reserved.
// 原链接目标/标题 scanner 的分帧消费；完整 MIT 许可见 LICENSE。

//! 只认证同一 Root Paragraph 中的直接 inline link；源正文始终留在调用者的 mmap 帧。
//!
//! 这里接原顺序游标的真实 `[`，不接任意搜索结果或局部截段。闭合判断调用原
//! DestinationScanner/TitleScanner，code和HTML优先级也借原内核。源修订、真实块末端
//! 与当次权限均来自外层同一个seed，短帧末端不成为任何语法的EOF。
//!
//! 一次只拥有一个活动链接：先确认完整owner，再由原游标重借label，产生code/强调。
//! 这个顺序使目标与标题中的标点永不进入label样式栈，也让LinkEnd可以恢复外层匹配。
//! 前瞻不持label String、Parser事件Vec或括号历史，目标值也只在Core原库存中存在。
//!
//! 引用、image和nested link仍需原Global/LinkStack事实；缺事实时返回General。
//! 特别不能把未解析的shortcut当普通字面，否则它可能禁用一个外层链接。保留未知
//! 比错误发出正面proof更重要，消费者不得把General或半个目标当作Ready。
//!
//! 所有范围均为原UTF8 byte坐标，UTF16投影与selection活动状态由同Source的Core负责。
//! 解码器只接受原目标范围内的帧；空帧仅用于已读carry归还，不允许扩大源权限。

use super::*;
use crate::definition_scan::{DestinationResult, DestinationScanner, TitleScanner, TitleStep};

#[derive(Clone, Debug)]
/// 只有原顺序游标及完整闭合目标能签发；范围不是一个重新解析的局部 Paragraph。
pub struct VerifiedInlineLink {
    pub(super) revision: u64,
    pub(super) source_end: usize,
    pub(super) source: Range<usize>,
    pub(super) label: Range<usize>,
    pub(super) destination: Range<usize>,
    pub(super) title: Option<Range<usize>>,
}

impl VerifiedInlineLink {
    /// 与原Source同一修订；消费者还须核其真实健康，数字相同不创造新Source权限。
    pub fn source_revision(&self) -> u64 {
        self.revision
    }
    /// 原Doc真实byte末端，不是owner末端或这次借入帧末端。
    pub fn source_end(&self) -> usize {
        self.source_end
    }
    /// 完整 owner 用于选择显隐；不能把可见 label 交集当成整个链接。
    pub fn source_range(&self) -> Range<usize> {
        self.source.clone()
    }
    /// 完整显示label不含外括号；长label不会被requested viewport交集截成新owner。
    pub fn label_range(&self) -> Range<usize> {
        self.label.clone()
    }
    /// 真实 scanner 的 content 范围不含尖括号；Core 只需有限读目标而非复制整个 label。
    pub fn destination_range(&self) -> Range<usize> {
        self.destination.clone()
    }
    /// 只交真实原文范围，不把未规范化title冒充用户值；当前Core链接动作不消费标题。
    pub fn title_range(&self) -> Option<Range<usize>> {
        self.title.clone()
    }
    /// 目标沿真实范围分帧规范化；不复制label、不给URL另设2048字节拒绝门。
    pub fn destination_decoder(&self) -> InlineLinkDestinationDecoder {
        InlineLinkDestinationDecoder {
            revision: self.revision,
            source_end: self.source_end,
            end: self.destination.end,
            next: self.destination.start,
            stream: crate::scanners::UnescapeStream::new(false),
            utf8: [0; 4],
            utf8_len: 0,
            complete: false,
            failure: None,
        }
    }
}

#[derive(Debug)]
/// 输出只是本量子已规范化的完整UTF8；String总容量和既有target上限仍由实际Core拥有。
// work包含本次扫描、实体比较、UTF8检查与本地copy；Core另收真实借帧和目标库存费用。
// written只授权output的这段完整UTF8；complete以前的片段不能单独作为可激活目标。
pub struct InlineLinkDestinationStep {
    pub work: GlobalWork,
    pub written: usize,
    pub complete: bool,
}

#[derive(Debug)]
// 不持Doc、mmap、Reader或源帧，取消不会留下第二正文owner；错误首因只在本实例固定。
// utf8只跨片保留一个未完成scalar，实体carry由原共享内核拥有，二者均有固定上界。
pub struct InlineLinkDestinationDecoder {
    revision: u64,
    source_end: usize,
    end: usize,
    next: usize,
    stream: crate::scanners::UnescapeStream,
    utf8: [u8; 4],
    utf8_len: usize,
    complete: bool,
    failure: Option<Error>,
}

impl InlineLinkDestinationDecoder {
    /// 返回真实已消费端点；有待排出输出时可已到end，调用方此时无需再做源I/O。
    pub fn next_byte(&self) -> usize {
        self.next
    }

    /// actual target末端已闭合，故可以空帧归还carry；real_eof仍只承认原Doc真实EOF。
    /// 源撤权、取消、UTF8和原首错都不能以部分规范化字符串冒充完成。
    ///
    /// 原authority先于任何写入；包括已complete的重复查询，也不能借撤销票继续取值。
    /// output来自消费者固定栈，最小8B足以容纳两个scalar；不在这里分配目标String。
    /// 输入扫描和输出检查共享budget，保留CONTROL及最多一个scalar的固定复制费用。
    /// Core仍按本步实际借帧长度付I/O，不应把consumed等同于整帧实际读取长度。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        authority: GlobalSourceAuthority,
        output: &mut [u8],
        budget: usize,
    ) -> Result<InlineLinkDestinationStep, Error> {
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
            if !(2048..=MAX_FRONTIER_STEP_BYTES).contains(&budget) || output.len() < 8 {
                return Err(Error::InvalidBudget);
            }
            crate::source_frontier::admitted_bytes(
                &frame,
                self.revision,
                self.source_end,
                self.next,
                budget,
            )?;
            if self.complete {
                return Ok(InlineLinkDestinationStep {
                    work: GlobalWork::default(),
                    written: 0,
                    complete: true,
                });
            }
            // source_end是Doc权限，end是已闭合目标；借帧可能覆盖其后字节但决不消费。
            // 终止理由来自原proof，real_end不是根据一个暂时短帧猜出的“当前输入完毕”。
            let count = frame.bytes.len().min(self.end - self.next);
            output[..self.utf8_len].copy_from_slice(&self.utf8[..self.utf8_len]);
            let (consumed, written, transitions, complete) = self.stream.feed(
                &frame.bytes[..count],
                self.next + count == self.end,
                &mut output[self.utf8_len..],
                (budget - CONTROL - 4) / 2,
            );
            // 只有共享内核实际消费的字节推进next；输出缓冲满不会越过尚未处理的源。
            // carry复制计入固定控制开销，produced的UTF8检查则按真实长度逐片收费。
            self.next += consumed;
            let produced = self.utf8_len + written;
            // 一次最多检查固定输出片；跨片只保未完成scalar的至多3B，不收尾重扫整个URL。
            let valid = match std::str::from_utf8(&output[..produced]) {
                Ok(_) => produced,
                Err(error) if error.error_len().is_none() => error.valid_up_to(),
                Err(_) => return Err(Error::SourceUtf8),
            };
            self.utf8_len = produced - valid;
            if self.utf8_len > 3 || complete && self.utf8_len != 0 {
                return Err(Error::SourceUtf8);
            }
            self.utf8[..self.utf8_len].copy_from_slice(&output[valid..produced]);
            // complete同时要求共享实体/escape状态及UTF8尾部全部排空；不能丢尾字符。
            // 调用方据此把唯一目标String直接移交，不必再全量复制或做收尾UTF8扫描。
            self.complete = complete;
            Ok(InlineLinkDestinationStep {
                work: GlobalWork {
                    byte_reads: consumed,
                    transitions: CONTROL + transitions + produced,
                },
                written: valid,
                complete,
            })
        })();
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }
}

#[derive(Clone, Copy, Debug)]
enum LinkPhase {
    Label,
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
}

#[derive(Debug)]
pub(super) enum LinkStep {
    Progress,
    NeedInput,
    Complete(VerifiedInlineLink),
    General,
}

#[derive(Debug)]
// 此probe只有固定状态，内嵌于原cursor；它既不新增分配，也不另开stack/spine文件。
// code前瞻可以读到较远处；未匹配必须按原replay_from回到真实候选后，不能跳过标签。
// destination/title只记真实字节范围，目标值的资源上限仍由Core唯一quota拥有。
pub(super) struct LinkProbe {
    revision: u64,
    source_end: usize,
    end: usize,
    start: usize,
    next: usize,
    label_end: usize,
    phase: LinkPhase,
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
}

impl LinkProbe {
    /// opener 来自原 Normal 中真正读到的 '['；所有后继读取继续用同一 revision/闭合末端。
    pub(super) fn new(revision: u64, source_end: usize, end: usize, start: usize) -> Self {
        Self {
            revision,
            source_end,
            end,
            start,
            next: start + 1,
            label_end: 0,
            phase: LinkPhase::Label,
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
        }
    }

    pub(super) fn next_byte(&self) -> usize {
        self.code
            .as_ref()
            .map_or(self.next, CodeSpanProbe::next_byte)
    }

    /// 每次只借当前原帧；lookahead 的代码 probe 也交还同一预算，不保整段或 bracket Vec。
    /// 外层已核revision/authority，并把这次CONTROL纳入work；子probe只能花剩余额度。
    /// 一个未消费的终止byte可以被后继状态重读，重复读取也真实收费，不隐匿lookahead。
    /// NeedInput只要求原Source续帧，不代表失败重试或允许另建一个Source。
    pub(super) fn advance(
        &mut self,
        frame: &SourceFrame<'_>,
        budget: usize,
        work: &mut GlobalWork,
    ) -> Result<LinkStep, Error> {
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
            // 子词法工作包含实体表比较的原上界；不能只计正常byte而漏掉semicolon lookup。
            // 返回后立即交回本量子，后继帧仍按子probe实际next定位。
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
            if self.next == self.end {
                return Ok(LinkStep::General); // 真块末端也不能把未闭合标签、reference或目标签成链接。
            }
            let Some(&byte) = frame.bytes.get(self.next - frame.byte_start) else {
                return Ok(LinkStep::NeedInput);
            };
            work.byte_reads += 1;
            work.transitions += 3;
            match self.phase {
                LinkPhase::Label => match byte {
                    // escape先于bracket，真实转义的方括号不会改变标签闭合状态。
                    // backtick另交原code规则，因为escaped多个tick与单tick的优先级不同。
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
                    // 内层shortcut/reference是否有效依赖原Global。未借到该事实前连
                    // 看似普通的嵌套[]也不能签外层link，否则可能越过原disable_all_links。
                    b'[' => return Ok(LinkStep::General),
                    b']' => {
                        self.label_end = self.next;
                        self.next += 1;
                        self.phase = LinkPhase::OpenTarget;
                    }
                    // Image 与嵌套有效 link 的 pass1禁用规则尚未在这份 proof 中完成，保留未知。
                    b'!' => return Ok(LinkStep::General),
                    _ => self.next += 1,
                },
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
                // 只累计当前真实tick run，不建立所有code候选历史；不匹配由原probe重放。
                // code能跨越看似closing bracket，必须先排除它才能认证一个链接。
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
                            self.end,
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
                LinkPhase::Code => unreachable!("活动code probe在入口独立消费原帧"),
                LinkPhase::Html => {
                    if self.html_len == HTML_BYTES {
                        return Ok(LinkStep::General);
                    }
                    if byte == b'>' {
                        // HTML原scanner最多读固定scratch，实际比较上界先由本量子预留。
                        // fuel不足不消费closing byte；后继仍从同原frame端点继续。
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
                                return Ok(LinkStep::General); // 内层 autolink 不能误签外层直接链接。
                            }
                            self.phase = LinkPhase::Label;
                        }
                    }
                }
                // 目前只认证紧接label的直接目标。空白后reference、shortcut和image
                // 不能借此分支降级成plain，也不能让一个后来局部Parser替它作全局决定。
                LinkPhase::OpenTarget => {
                    if byte != b'(' {
                        return Ok(LinkStep::General); // reference必须等原Global，不能把它默认判成普通字面。
                    }
                    self.next += 1;
                    self.reset_separator();
                    self.phase = LinkPhase::BeforeDestination;
                }
                LinkPhase::BeforeDestination
                | LinkPhase::AfterDestination
                | LinkPhase::AfterTitle => {
                    if self.separator(byte) {
                        continue;
                    }
                    // 分隔符规则只越过一个真实EOL；title和closing parenthesis同属owner。
                    // 目标scanner可能零byte完成（空目标），该终止byte必须留给后继状态。
                    match self.phase {
                        LinkPhase::BeforeDestination => {
                            self.destination_start = self.next;
                            self.phase = LinkPhase::Destination;
                        }
                        LinkPhase::AfterDestination if byte != b')' => {
                            self.title_start = self.next;
                            self.phase = LinkPhase::Title;
                        }
                        // 只有真实closing已被消费才发布proof；此前的任何范围都只是内部状态。
                        // proof直接移交scalar/range，不复制label、destination或title内容。
                        _ if byte == b')' => {
                            self.next += 1;
                            return Ok(LinkStep::Complete(VerifiedInlineLink {
                                revision: self.revision,
                                source_end: self.source_end,
                                source: self.start..self.next,
                                label: self.start + 1..self.label_end,
                                destination: self.destination_range.clone(),
                                title: self.title_range.clone(),
                            }));
                        }
                        _ => return Ok(LinkStep::General),
                    }
                }
                LinkPhase::Destination => {
                    // 与 whole Parser 的 scan_link_dest 使用同一个分段 scanner；终止byte不误消费。
                    let (used, result) = self.destination.feed(std::slice::from_ref(&byte), false);
                    self.next += used;
                    match result {
                        DestinationResult::NeedInput => {}
                        DestinationResult::Rejected => return Ok(LinkStep::General),
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
                LinkPhase::Title => {
                    // TitleScanner已有完整原delimiter/escape/CRLF续行规则；Root无容器前缀可跳。
                    let (used, step) = self.title.feed(std::slice::from_ref(&byte), false);
                    self.next += used;
                    match step {
                        TitleStep::NeedInput => {}
                        TitleStep::Rejected => return Ok(LinkStep::General),
                        TitleStep::NeedLine { .. } => self.title.continue_line(0),
                        TitleStep::Complete { byte_count, .. } => {
                            self.title_range =
                                Some(self.title_start + 1..self.title_start + byte_count - 1);
                            self.reset_separator();
                            self.phase = LinkPhase::AfterTitle;
                        }
                    }
                }
            }
        }
        Ok(LinkStep::Progress)
    }

    /// opener来自刚实际读到的tick；escaped标记只影响原code_search_length，不造新规则。
    fn begin_ticks(&mut self, escaped: bool) {
        self.tick_start = self.next;
        self.ticks = 1;
        self.escaped_tick = escaped;
        self.next += 1;
        self.phase = LinkPhase::Ticks;
    }

    /// 每个语法分隔位置独立允许一个EOL；不能把上一个分隔位置的CRLF状态带到下一个。
    fn reset_separator(&mut self) {
        self.separator_eol = false;
        self.separator_cr = false;
    }

    /// 原 scan_separator 至多越过一个物理EOL；只保CRLF的一byte邻界，不读后方全文。
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
}
