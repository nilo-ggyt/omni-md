// Copyright 2015 Google Inc. All rights reserved.
// 本地分段适配复用固定0.13.4 label/space/destination/title规则；完整MIT许可见LICENSE。

//! 真实block producer使用的refdef候选扫描；不单凭字面形状授予global definition权限。
//! 返回值只有原始源锚点。调用者仍须绑定实际spine、document/root身份与generation，
//! 并按真实源顺序处理first-definition-wins。这里不保存label、URL或title正文。

use crate::block_frontier::{FrontierError, SourceFrame};
use crate::definition_scan::{
    DestinationResult, DestinationScanner, RefdefSpaceScanner, SpaceStep, TitleScanner, TitleStep,
};
use crate::label_frontier::{LabelScanner, LabelStep};
use crate::scanners::is_ascii_whitespace_no_nl;
use crate::source_frontier::admitted_bytes;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceLinePurpose {
    Label,
    Whitespace,
    Title,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceCandidate {
    pub source_revision: u64,
    pub span_start: usize,
    pub span_end: usize,
    pub label_start: usize,
    pub label_end: usize,
    /// 这两个计数只用于有限query的分配前排除，不能代替原UniCase的最终label比较。
    pub label_ordinary_ascii: usize,
    pub label_codepoint_counter: usize,
    pub destination_start: usize,
    pub destination_end: usize,
    /// 包含原始多行prefix的范围；有限resolver不能把它直接当成规范化后的title。
    pub title: Option<(usize, usize)>,
}

impl ReferenceCandidate {
    /// 仅规范化已验证candidate的完整有限label；此方法本身不授予global block身份。
    /// raw必须从同revision真实源的label_start读至label_end之后的`]`，续行权限由拥有
    /// candidate起点spine的producer提供。返回值最多持此有限label，不持全文或全局目录。
    pub fn normalized_label<'a>(
        &self,
        raw_with_closer: &'a str,
        continuation_prefix: impl Fn(&[u8]) -> Option<usize>,
    ) -> Result<crate::CowStr<'a>, FrontierError> {
        let expected = self
            .label_end
            .checked_sub(self.label_start)
            .and_then(|len| len.checked_add(1));
        if expected != Some(raw_with_closer.len())
            || raw_with_closer.len() > crate::block_frontier::MAX_FRONTIER_FRAME_BYTES
            || !raw_with_closer.ends_with(']')
        {
            // 完整range与单次有限预算在原scanner调用前检查；截断raw不能获得虚拟EOF权限。
            return Err(FrontierError::InvalidFrame);
        }
        let prefix = |after_eol: &[u8]| {
            let skip = continuation_prefix(after_eol)?;
            let relative = raw_with_closer.len().checked_sub(after_eol.len())?;
            let end = relative.checked_add(skip)?;
            if skip <= after_eol.len() && raw_with_closer.is_char_boundary(end) {
                // 只传回真实prefix的边界；错误外部长度不能让原whole-slice scanner越界切UTF8。
                Some(skip)
            } else {
                None
            }
        };
        let Some((consumed, normalized)) =
            crate::linklabel::scan_link_label_rest(raw_with_closer, &prefix, false)
        else {
            // 候选与原规则不再一致时整批拒绝，不自行trim/fold或猜第二种label语法。
            return Err(FrontierError::InvalidFrame);
        };
        if consumed != raw_with_closer.len() {
            // 内部提前closer不能隐藏未消费正文；caller必须提供候选精确range。
            return Err(FrontierError::InvalidFrame);
        }
        Ok(normalized)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceStep {
    Progress {
        consumed: usize,
    },
    NeedInput {
        consumed: usize,
    },
    NeedLine {
        consumed: usize,
        after_eol: usize,
        purpose: ReferenceLinePurpose,
    },
    Candidate {
        consumed: usize,
        candidate: ReferenceCandidate,
    },
    Rejected {
        consumed: usize,
        replay_from: usize,
    },
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Start,
    Label(LabelScanner),
    Colon,
    BeforeDestination(RefdefSpaceScanner),
    Destination(DestinationScanner),
    AfterDestination(RefdefSpaceScanner),
    Title(TitleScanner),
    TrailingBlank,
    Finished,
    Rejected,
}

#[derive(Clone, Copy, Debug)]
pub struct ReferenceDefinitionScanner {
    revision: u64,
    source_end: usize,
    next_byte: usize,
    phase_start: usize,
    destination_raw_end: usize,
    title_raw_end: usize,
    title_newlines: usize,
    phase: Phase,
    pending_line: Option<ReferenceLinePurpose>,
    candidate: ReferenceCandidate,
}

impl ReferenceDefinitionScanner {
    /// 该入口只能接真实block起点候选；语法通过与block/global authority是不同的结果。
    pub fn new(revision: u64, source_end: usize, start: usize) -> Result<Self, FrontierError> {
        if start > source_end {
            return Err(FrontierError::InvalidFrame);
        }
        Ok(Self {
            revision,
            source_end,
            next_byte: start,
            phase_start: start,
            destination_raw_end: start,
            title_raw_end: start,
            title_newlines: 0,
            phase: Phase::Start,
            pending_line: None,
            candidate: ReferenceCandidate {
                source_revision: revision,
                span_start: start,
                span_end: start,
                label_start: start,
                label_end: start,
                label_ordinary_ascii: 0,
                label_codepoint_counter: 0,
                destination_start: start,
                destination_end: start,
                title: None,
            },
        })
    }

    pub fn next_byte(&self) -> usize {
        self.next_byte
    }

    /// 前瞻期间原始定义起点保持未提交；失败/backup不会吞掉已读后继正文。
    pub fn committed_byte(&self) -> usize {
        self.candidate.span_start
    }

    /// 外层先用共享容器与paragraph中断规则核实真实续行，再交回prefix长度和末字节。
    pub fn continue_line(
        &mut self,
        allowed: Option<(usize, Option<u8>)>,
    ) -> Result<(), FrontierError> {
        let purpose = self.pending_line.ok_or(FrontierError::InvalidFrame)?;
        if let Some((skip, last)) = allowed {
            let next = self.next_byte.checked_add(skip);
            if !next.is_some_and(|next| next <= self.source_end) || (skip > 0 && last.is_none()) {
                return Err(FrontierError::InvalidFrame);
            }
            match (&mut self.phase, purpose) {
                (Phase::Label(scanner), ReferenceLinePurpose::Label) => {
                    scanner.continue_line(Some((skip, last)))?;
                }
                (
                    Phase::BeforeDestination(scanner) | Phase::AfterDestination(scanner),
                    ReferenceLinePurpose::Whitespace,
                ) => scanner.continue_line(skip),
                (Phase::Title(scanner), ReferenceLinePurpose::Title) => scanner.continue_line(skip),
                _ => return Err(FrontierError::InvalidFrame),
            }
            self.next_byte = next.unwrap();
        } else {
            match self.phase {
                Phase::AfterDestination(_) => self.finish(false),
                Phase::Title(_) => self.reject_title_or_backup(),
                // label/URL前空白失败时没有合法backup；保留起点供外层按paragraph重新分类。
                _ => self.phase = Phase::Rejected,
            }
        }
        self.pending_line = None;
        Ok(())
    }

    /// 每个step借用有界frame。一个候选的phase只向前走；遇续行权限请求立即退出而不忙重试。
    pub fn advance(
        &mut self,
        frame: SourceFrame<'_>,
        budget: usize,
    ) -> Result<ReferenceStep, FrontierError> {
        let limit = admitted_bytes(
            &frame,
            self.revision,
            self.source_end,
            self.next_byte,
            budget,
        )?;
        if let Some(purpose) = self.pending_line {
            return Ok(ReferenceStep::NeedLine {
                consumed: 0,
                after_eol: self.next_byte,
                purpose,
            });
        }
        if matches!(self.phase, Phase::Finished | Phase::Rejected) {
            return Ok(self.step(0));
        }
        let real_eof = frame.real_eof && limit == frame.bytes.len();
        let mut consumed = 0;
        let mut transitions = 0;
        while consumed < limit || real_eof {
            transitions += 1;
            // 每次最多经过九个向前phase；helper各自每byte至多两次转换，不形成零消费循环。
            debug_assert!(transitions <= 12);
            let bytes = &frame.bytes[consumed..limit];
            match &mut self.phase {
                Phase::Finished | Phase::Rejected => return Ok(self.step(consumed)),
                Phase::Start => {
                    if bytes.first() != Some(&b'[') {
                        self.phase = Phase::Rejected;
                        return Ok(self.step(consumed));
                    }
                    self.next_byte += 1;
                    consumed += 1;
                    self.phase_start = self.next_byte;
                    self.candidate.label_start = self.next_byte;
                    self.phase = Phase::Label(LabelScanner::new(false));
                }
                Phase::Label(scanner) => {
                    let step = scanner.feed(bytes, real_eof, budget)?;
                    let count = match step {
                        LabelStep::Progress { consumed }
                        | LabelStep::NeedInput { consumed }
                        | LabelStep::NeedLineContinuation { consumed, .. }
                        | LabelStep::Complete { consumed, .. }
                        | LabelStep::Rejected { consumed } => consumed,
                    };
                    self.next_byte += count;
                    consumed += count;
                    match step {
                        LabelStep::Complete { byte_count, .. } => {
                            self.candidate.label_ordinary_ascii = scanner.ordinary_ascii_bytes();
                            self.candidate.label_codepoint_counter = scanner.codepoint_counter();
                            self.candidate.label_end = self.phase_start + byte_count - 1;
                            debug_assert!(
                                self.candidate.label_end - self.candidate.label_start
                                    <= self.candidate.label_ordinary_ascii
                                        + self.candidate.label_codepoint_counter
                            );
                            self.phase = Phase::Colon;
                        }
                        LabelStep::NeedLineContinuation { after_eol, .. } => {
                            debug_assert_eq!(self.next_byte, self.phase_start + after_eol);
                            return Ok(self.request_line(consumed, ReferenceLinePurpose::Label));
                        }
                        LabelStep::Rejected { .. } => {
                            self.phase = Phase::Rejected;
                            return Ok(self.step(consumed));
                        }
                        _ => return Ok(self.wait_step(consumed, frame.bytes.len())),
                    }
                }
                Phase::Colon => {
                    if bytes.first() != Some(&b':') {
                        self.phase = Phase::Rejected;
                        return Ok(self.step(consumed));
                    }
                    self.next_byte += 1;
                    consumed += 1;
                    self.phase_start = self.next_byte;
                    self.phase = Phase::BeforeDestination(RefdefSpaceScanner::new());
                }
                Phase::BeforeDestination(scanner) => {
                    let (count, step) = scanner.feed(bytes, real_eof);
                    self.next_byte += count;
                    consumed += count;
                    match step {
                        SpaceStep::Complete { .. } => {
                            self.phase_start = self.next_byte;
                            self.phase = Phase::Destination(DestinationScanner::new(32));
                        }
                        SpaceStep::NeedLine { after_eol } => {
                            debug_assert_eq!(self.next_byte, self.phase_start + after_eol);
                            return Ok(
                                self.request_line(consumed, ReferenceLinePurpose::Whitespace)
                            );
                        }
                        SpaceStep::Rejected => {
                            self.phase = Phase::Rejected;
                            return Ok(self.step(consumed));
                        }
                        SpaceStep::NeedInput => {
                            return Ok(self.wait_step(consumed, frame.bytes.len()))
                        }
                    }
                }
                Phase::Destination(scanner) => {
                    let (count, step) = scanner.feed(bytes, real_eof);
                    self.next_byte += count;
                    consumed += count;
                    match step {
                        DestinationResult::Complete {
                            byte_count,
                            content_start,
                            content_end,
                        } if byte_count > 0 => {
                            self.candidate.destination_start = self.phase_start + content_start;
                            self.candidate.destination_end = self.phase_start + content_end;
                            self.destination_raw_end = self.next_byte;
                            self.phase_start = self.next_byte;
                            self.phase = Phase::AfterDestination(RefdefSpaceScanner::new());
                        }
                        DestinationResult::Complete { .. } | DestinationResult::Rejected => {
                            // 空destination的拒绝属于refdef外层；共享inline scanner仍保原空URL语义。
                            self.phase = Phase::Rejected;
                            return Ok(self.step(consumed));
                        }
                        DestinationResult::NeedInput => {
                            return Ok(self.wait_step(consumed, frame.bytes.len()))
                        }
                    }
                }
                Phase::AfterDestination(scanner) => {
                    let (count, step) = scanner.feed(bytes, real_eof);
                    self.next_byte += count;
                    consumed += count;
                    match step {
                        SpaceStep::Complete {
                            byte_count,
                            newlines,
                        } => {
                            if byte_count == 0 && newlines == 0 {
                                self.phase = Phase::Rejected;
                                return Ok(self.step(consumed));
                            }
                            self.title_newlines = newlines;
                            self.phase_start = self.next_byte;
                            self.phase = Phase::Title(TitleScanner::new());
                        }
                        SpaceStep::NeedLine { after_eol } => {
                            debug_assert_eq!(self.next_byte, self.phase_start + after_eol);
                            return Ok(
                                self.request_line(consumed, ReferenceLinePurpose::Whitespace)
                            );
                        }
                        SpaceStep::Rejected => {
                            self.finish(false);
                            return Ok(self.step(consumed));
                        }
                        SpaceStep::NeedInput => {
                            return Ok(self.wait_step(consumed, frame.bytes.len()))
                        }
                    }
                }
                Phase::Title(scanner) => {
                    let (count, step) = scanner.feed(bytes, real_eof);
                    self.next_byte += count;
                    consumed += count;
                    match step {
                        TitleStep::Complete { byte_count, .. } => {
                            self.title_raw_end = self.phase_start + byte_count;
                            self.candidate.title =
                                Some((self.phase_start + 1, self.title_raw_end - 1));
                            self.phase = Phase::TrailingBlank;
                        }
                        TitleStep::NeedLine { after_eol, .. } => {
                            debug_assert_eq!(self.next_byte, self.phase_start + after_eol);
                            return Ok(self.request_line(consumed, ReferenceLinePurpose::Title));
                        }
                        TitleStep::Rejected => {
                            self.reject_title_or_backup();
                            return Ok(self.step(consumed));
                        }
                        TitleStep::NeedInput => {
                            return Ok(self.wait_step(consumed, frame.bytes.len()))
                        }
                    }
                }
                Phase::TrailingBlank => {
                    let count = bytes
                        .iter()
                        .take_while(|byte| is_ascii_whitespace_no_nl(**byte))
                        .count();
                    self.next_byte += count;
                    consumed += count;
                    if consumed < limit {
                        if matches!(frame.bytes[consumed], b'\r' | b'\n') {
                            self.finish(true);
                        } else {
                            self.reject_title_or_backup();
                        }
                        return Ok(self.step(consumed));
                    }
                    if real_eof {
                        self.finish(true);
                        return Ok(self.step(consumed));
                    }
                    return Ok(self.wait_step(consumed, frame.bytes.len()));
                }
            }
        }
        Ok(self.wait_step(consumed, frame.bytes.len()))
    }

    fn finish(&mut self, title: bool) {
        self.candidate.span_end = if title {
            self.title_raw_end
        } else {
            self.destination_raw_end
        };
        if !title {
            self.candidate.title = None;
        }
        self.phase = Phase::Finished;
    }
    fn reject_title_or_backup(&mut self) {
        if self.title_newlines > 0 {
            self.finish(false);
        } else {
            self.phase = Phase::Rejected;
        }
    }
    fn request_line(&mut self, consumed: usize, purpose: ReferenceLinePurpose) -> ReferenceStep {
        self.pending_line = Some(purpose);
        ReferenceStep::NeedLine {
            consumed,
            after_eol: self.next_byte,
            purpose,
        }
    }
    fn wait_step(&self, consumed: usize, frame_len: usize) -> ReferenceStep {
        if consumed == frame_len {
            ReferenceStep::NeedInput { consumed }
        } else {
            ReferenceStep::Progress { consumed }
        }
    }
    fn step(&self, consumed: usize) -> ReferenceStep {
        match self.phase {
            Phase::Finished => ReferenceStep::Candidate {
                consumed,
                candidate: self.candidate,
            },
            Phase::Rejected => ReferenceStep::Rejected {
                consumed,
                replay_from: self.candidate.span_start,
            },
            _ => unreachable!("只有terminal阶段可以生成definition结果"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_context::{
        scan_heavy_table_interrupt, scan_paragraph_interrupt_no_table, ContainerRole,
        ContainerSpine, ParagraphContext,
    };
    use crate::scanners::{scan_blank_line, scan_setext_heading, LineStart};
    use crate::{Options, Parser};

    struct RootScope;
    impl ContainerSpine for RootScope {
        fn visit_roles(&self, _: impl FnMut(ContainerRole) -> bool) {}
    }
    impl ParagraphContext for RootScope {
        fn is_in_table(&self) -> bool {
            false
        }
        fn parent_is_paragraph(&self) -> bool {
            false
        }
        fn spine_entry_is_definition(&self, _: usize) -> bool {
            false
        }
    }
    fn options() -> Options {
        Options::ENABLE_TABLES | Options::ENABLE_FOOTNOTES | Options::ENABLE_GFM
    }

    /// 仅top-level纯fixture的真实context handler；正式producer还须提供实际开放spine与root身份。
    fn continuation(
        source: &str,
        after_eol: usize,
        purpose: ReferenceLinePurpose,
    ) -> Option<(usize, Option<u8>)> {
        let bytes = &source.as_bytes()[after_eol..];
        let mut start = LineStart::new(bytes);
        if !start.scan_space(4) {
            let suffix = &bytes[start.bytes_scanned()..];
            if scan_paragraph_interrupt_no_table(suffix, true, true, false, &RootScope, 0)
                || scan_heavy_table_interrupt(suffix, &RootScope, 0, options())
                || scan_setext_heading(suffix).is_some()
            {
                return None;
            }
        }
        if purpose == ReferenceLinePurpose::Title {
            start.scan_all_space();
            if scan_blank_line(&bytes[start.bytes_scanned()..]).is_some() {
                return None;
            }
        }
        let skip = start.bytes_scanned();
        Some((skip, skip.checked_sub(1).map(|index| bytes[index])))
    }

    fn run(source: &str, chunk: usize, budget: usize) -> (Option<ReferenceCandidate>, usize) {
        let mut scanner = ReferenceDefinitionScanner::new(27, source.len(), 0).unwrap();
        loop {
            let start = scanner.next_byte();
            let end = (start + chunk).min(source.len());
            let step = scanner
                .advance(
                    SourceFrame {
                        source_revision: 27,
                        byte_start: start,
                        bytes: &source.as_bytes()[start..end],
                        real_eof: end == source.len(),
                    },
                    budget,
                )
                .unwrap();
            match step {
                ReferenceStep::Candidate { candidate, .. } => {
                    return (Some(candidate), scanner.next_byte())
                }
                ReferenceStep::Rejected { replay_from, .. } => {
                    assert_eq!(replay_from, 0);
                    return (None, scanner.next_byte());
                }
                ReferenceStep::NeedLine {
                    after_eol, purpose, ..
                } => {
                    assert_eq!(after_eol, scanner.next_byte());
                    scanner
                        .continue_line(continuation(source, after_eol, purpose))
                        .unwrap();
                }
                ReferenceStep::Progress { consumed } | ReferenceStep::NeedInput { consumed } => {
                    assert!(
                        consumed > 0,
                        "输入不足必须让外层取新frame；本fixture直接提供真实EOF"
                    );
                    assert!(consumed <= budget);
                    assert_eq!(scanner.committed_byte(), 0);
                }
            }
        }
    }

    #[test]
    fn complete_reference_candidate_matches_real_parser_with_all_byte_chunks_and_title_backup() {
        for source in [
            "[r]: https://example.test\n",
            "[r]: <>",
            "[r]: a(b(c)) \"title\"\n",
            "[r]: URL bad\n",
            "[r]: URL\nbad",
            "[r]: URL\n\"unclosed\n| H |\n| - |\n",
            "[r]: URL\n\"title\"\n",
            "[r]: URL \"first\nsecond\"\n",
            "[r]: URL \"first\n# interrupt\"\n",
            "[r]: <URL\\> x>\n",
            "[r]: URL\\( \n",
            "[r]: URL\\\n",
            "[r]: URL \n\n",
            "[r]: URL\n  \"title\" extra\n",
            "[r]: URL \"title\" extra\n",
            "[中😀]: <https://x.test/中> '中😀'\r\n",
            "[r\n label]: URL\n",
            "[r]:\r\nURL\r\n",
        ] {
            let parser = Parser::new_ext(source, options());
            let expected = parser
                .reference_definitions()
                .iter()
                .find(|(_, definition)| definition.span.start == 0);
            for chunk in 1..=source.len() {
                let (actual, _) = run(source, chunk, 5);
                assert_eq!(
                    actual.is_some(),
                    expected.is_some(),
                    "{source:?} chunk={chunk}"
                );
                if let (Some(actual), Some((_, expected))) = (actual, expected) {
                    assert_eq!(
                        actual.span_end, expected.span.end,
                        "{source:?} chunk={chunk}"
                    );
                    assert_eq!(actual.title.is_some(), expected.title.is_some());
                    let raw = &source[actual.label_start..actual.label_end + 1];
                    let label = actual.normalized_label(raw, |_| Some(0)).unwrap();
                    assert!(parser.reference_definitions().get(&label).is_some());
                    assert_eq!(
                        crate::scanners::unescape(
                            &source[actual.destination_start..actual.destination_end],
                            false
                        )
                        .as_ref(),
                        expected.dest.as_ref()
                    );
                }
            }
        }
    }

    #[test]
    fn deterministic_refdef_inputs_preserve_real_parser_span_and_title_admission() {
        let alphabet = [
            'a', '(', ')', '<', '>', '\\', '\t', '\n', '\r', ' ', '"', '\'', '[', ']', '|', '中',
            '😀',
        ];
        let mut random = 0x5799b3419ee7fa23_u64;
        for case in 0..4096 {
            let mut source = String::from("[r]: ");
            for _ in 0..(case % 89) {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                source.push(alphabet[random as usize % alphabet.len()]);
            }
            source.push('\n');
            let parser = Parser::new_ext(&source, options());
            let expected = parser
                .reference_definitions()
                .iter()
                .find(|(_, definition)| definition.span.start == 0);
            let (actual, _) = run(&source, 3, 2);
            assert_eq!(
                actual.is_some(),
                expected.is_some(),
                "case={case} source={source:?}"
            );
            if let (Some(actual), Some((_, expected))) = (actual, expected) {
                assert_eq!(actual.span_end, expected.span.end, "case={case}");
                assert_eq!(
                    actual.title.is_some(),
                    expected.title.is_some(),
                    "case={case}"
                );
            }
        }
    }

    #[test]
    fn long_failed_title_returns_url_backup_and_never_owns_pending_body() {
        let mut source = String::from("[r]: URL\n\"");
        source.extend(std::iter::repeat('x').take(4 * 1024 * 1024));
        let (candidate, read_end) = run(&source, 4096, 4096);
        let candidate = candidate.unwrap();
        assert_eq!(candidate.span_end, 8);
        assert!(candidate.title.is_none());
        assert_eq!(read_end, source.len());
        assert!(
            candidate.span_end < read_end,
            "外层从backup末尾重放，不能把lookahead末尾提交成definition末尾"
        );
        assert!(std::mem::size_of::<ReferenceDefinitionScanner>() <= 320);
        assert!(!std::mem::needs_drop::<ReferenceDefinitionScanner>());
    }

    #[test]
    fn finite_label_bridge_preserves_real_prefix_and_rejects_wrong_utf8_boundaries() {
        let source = "[Maße\n  世界]: /first\n";
        let (candidate, _) = run(source, 1, 1);
        let candidate = candidate.unwrap();
        let raw = &source[candidate.label_start..candidate.label_end + 1];
        // 实际container producer会给相同prefix事实；这里仅验证薄桥没有第二套规范化规则。
        assert_eq!(
            candidate
                .normalized_label(raw, |_| Some(2))
                .unwrap()
                .as_ref(),
            "Maße 世界"
        );
        assert!(candidate.normalized_label(raw, |_| Some(3)).is_err());
        assert!(candidate
            .normalized_label(raw, |_| Some(usize::MAX))
            .is_err());
        assert!(candidate
            .normalized_label(&raw[..raw.len() - 1], |_| Some(2))
            .is_err());
    }
}
