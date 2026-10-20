// Copyright 2018 Google LLC
// 新状态机共用固定0.13.4 linklabel match判定；完整MIT许可见LICENSE。

//! 分段label extent扫描。不复制label正文，不把其语法通过当作真实definition/block权限。
//! 原Unicode大小写与空白规范化仍由有限本批resolver复用上游实现；本模块只报告源锚点。

use crate::block_frontier::{FrontierError, MAX_FRONTIER_FRAME_BYTES, MAX_FRONTIER_STEP_BYTES};
use crate::definition_scan::{label_lexeme, LabelLexeme};
use crate::scanners::is_ascii_whitespace;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelStep {
    Progress {
        consumed: usize,
    },
    NeedInput {
        consumed: usize,
    },
    /// EOL已完整读入；外层按真实容器/paragraph中断规则准许或拒绝续行，再调用continue_line。
    NeedLineContinuation {
        consumed: usize,
        after_eol: usize,
    },
    Complete {
        consumed: usize,
        byte_count: usize,
    },
    Rejected {
        consumed: usize,
    },
}

#[derive(Clone, Copy, Debug)]
struct WhitespaceRun {
    start: usize,
    weight: usize,
    linebreaks: usize,
    pending_cr: bool,
}
#[derive(Clone, Copy, Debug)]
enum Phase {
    Body,
    Backslash,
    Whitespace,
    AwaitContinuation,
    Complete,
    Rejected,
}

#[derive(Clone, Copy, Debug)]
pub struct LabelScanner {
    in_table: bool,
    phase: Phase,
    offset: usize,
    codepoints: usize,
    ordinary_ascii: usize,
    only_whitespace: bool,
    previous: Option<u8>,
    whitespace: WhitespaceRun,
}

impl LabelScanner {
    /// opening `[`由外层消费；offset从label正文起点计数，成功byte_count包含closing `]`。
    pub fn new(in_table: bool) -> Self {
        Self {
            in_table,
            phase: Phase::Body,
            offset: 0,
            codepoints: 0,
            ordinary_ascii: 0,
            only_whitespace: true,
            previous: None,
            whitespace: WhitespaceRun {
                start: 0,
                weight: 0,
                linebreaks: 0,
                pending_cr: false,
            },
        }
    }

    /// global label(in_table=false)中这些ASCII字节不会被空白规范化或Unicode折叠删除。
    /// 成功时raw长度≤ordinary_ascii+codepoints，且原closing前检查保证codepoints<1000。
    /// resolver可先用有限query折叠长度排除巨大ASCII label，再准入有限raw读取，不能先clone后拒绝。
    pub fn ordinary_ascii_bytes(&self) -> usize {
        self.ordinary_ascii
    }

    pub fn codepoint_counter(&self) -> usize {
        self.codepoints
    }

    /// 续行结果只能由真实共享容器/中断扫描器提供；skip包含被去掉的原始prefix字节。
    pub fn continue_line(
        &mut self,
        allowed: Option<(usize, Option<u8>)>,
    ) -> Result<(), FrontierError> {
        if !matches!(self.phase, Phase::AwaitContinuation) {
            return Err(FrontierError::InvalidFrame);
        }
        match allowed {
            Some((skip, last_source_byte)) => {
                self.offset = self
                    .offset
                    .checked_add(skip)
                    .ok_or(FrontierError::InvalidFrame)?;
                if skip > 0 {
                    self.previous = last_source_byte;
                }
                self.whitespace.weight += 2;
                self.phase = Phase::Whitespace;
            }
            None => self.phase = Phase::Rejected,
        }
        Ok(())
    }

    /// 每次最多准入64KiB，单byte至多经历body/whitespace或backslash回交两个转换。
    pub fn feed(
        &mut self,
        bytes: &[u8],
        real_eof: bool,
        budget: usize,
    ) -> Result<LabelStep, FrontierError> {
        if bytes.len() > MAX_FRONTIER_FRAME_BYTES {
            return Err(FrontierError::InvalidFrame);
        }
        if budget == 0 || budget > MAX_FRONTIER_STEP_BYTES {
            return Err(FrontierError::InvalidBudget);
        }
        let limit = bytes.len().min(budget);
        let mut consumed = 0;
        while consumed < limit {
            let byte = bytes[consumed];
            match self.phase {
                Phase::Complete | Phase::Rejected | Phase::AwaitContinuation => {
                    return Ok(self.step(consumed));
                }
                Phase::Backslash => {
                    match label_lexeme(b'\\', None, Some(byte), true, self.in_table) {
                        LabelLexeme::EscapedTablePipe => self.codepoints += 1,
                        LabelLexeme::EscapedPunctuation => self.codepoints += 2,
                        LabelLexeme::Ordinary(_) => {
                            // 后继不是punctuation：slash本身普通，当前byte回交body且不重复消费。
                            self.only_whitespace = false;
                            self.ordinary_ascii += 1;
                            self.phase = Phase::Body;
                            continue;
                        }
                        _ => unreachable!("确定后继byte的slash只有转义或普通分支"),
                    }
                    self.only_whitespace = false;
                    self.phase = Phase::Body;
                }
                Phase::Body => {
                    if self.codepoints >= 1000 {
                        self.phase = Phase::Rejected;
                        return Ok(self.step(consumed));
                    }
                    if byte == b'\\' {
                        // 先消费slash再等待实际后继，避免要求调用者为frame末尾重放已读前缀。
                        self.phase = Phase::Backslash;
                    } else {
                        match label_lexeme(byte, self.previous, None, false, self.in_table) {
                            LabelLexeme::Reject => {
                                self.phase = Phase::Rejected;
                                return Ok(self.step(consumed));
                            }
                            LabelLexeme::Close => {
                                self.offset += 1;
                                consumed += 1;
                                self.phase = if self.only_whitespace {
                                    Phase::Rejected
                                } else {
                                    Phase::Complete
                                };
                                return Ok(self.step(consumed));
                            }
                            LabelLexeme::TablePipe => self.only_whitespace = false,
                            LabelLexeme::Whitespace => {
                                self.whitespace = WhitespaceRun {
                                    start: self.offset,
                                    weight: 0,
                                    linebreaks: 0,
                                    pending_cr: false,
                                };
                                self.phase = Phase::Whitespace;
                                continue;
                            }
                            LabelLexeme::Ordinary(byte) => {
                                // 与原实现逐byte一致：ASCII普通字节不增codepoints，高位byte逐个计数。
                                self.only_whitespace = false;
                                if byte & 0x80 != 0 {
                                    self.codepoints += 1;
                                } else {
                                    self.ordinary_ascii += 1;
                                }
                            }
                            _ => unreachable!("非slash不能进入escape/lookahead分支"),
                        }
                    }
                }
                Phase::Whitespace => {
                    if self.whitespace.pending_cr {
                        self.whitespace.pending_cr = false;
                        if byte == b'\n' {
                            self.consume(byte);
                            consumed += 1;
                        }
                        self.phase = Phase::AwaitContinuation;
                        return Ok(self.step(consumed));
                    }
                    if matches!(byte, b'\r' | b'\n') {
                        self.whitespace.linebreaks += 1;
                        if self.whitespace.linebreaks > 1 {
                            // 同一空白run最多一个实际EOL；第二行不因frame切分获得额外label权限。
                            self.phase = Phase::Rejected;
                            return Ok(self.step(consumed));
                        }
                        self.consume(byte);
                        consumed += 1;
                        if byte == b'\r' {
                            self.whitespace.pending_cr = true;
                            continue;
                        }
                        self.phase = Phase::AwaitContinuation;
                        return Ok(self.step(consumed));
                    }
                    if is_ascii_whitespace(byte) {
                        self.whitespace.weight += if byte == b' ' { 1 } else { 2 };
                    } else {
                        self.codepoints += if self.whitespace.weight > 1 {
                            self.offset - self.whitespace.start
                        } else {
                            1
                        };
                        self.phase = Phase::Body;
                        continue;
                    }
                }
            }
            self.consume(byte);
            consumed += 1;
        }
        if consumed == bytes.len() && real_eof {
            if matches!(self.phase, Phase::Whitespace) && self.whitespace.pending_cr {
                // 跨frame CR在真实EOF才确定单独EOL；续行handler仍必须得到一次明确请求。
                self.whitespace.pending_cr = false;
                self.phase = Phase::AwaitContinuation;
            } else if !matches!(
                self.phase,
                Phase::Complete | Phase::Rejected | Phase::AwaitContinuation
            ) {
                self.phase = Phase::Rejected;
            }
            return Ok(self.step(consumed));
        }
        if matches!(
            self.phase,
            Phase::Complete | Phase::Rejected | Phase::AwaitContinuation
        ) {
            return Ok(self.step(consumed));
        }
        Ok(if consumed == bytes.len() {
            LabelStep::NeedInput { consumed }
        } else {
            LabelStep::Progress { consumed }
        })
    }

    fn consume(&mut self, byte: u8) {
        self.offset += 1;
        self.previous = Some(byte);
    }
    fn step(&self, consumed: usize) -> LabelStep {
        match self.phase {
            Phase::Complete => LabelStep::Complete {
                consumed,
                byte_count: self.offset,
            },
            Phase::Rejected => LabelStep::Rejected { consumed },
            Phase::AwaitContinuation => LabelStep::NeedLineContinuation {
                consumed,
                after_eol: self.offset,
            },
            _ => unreachable!("只有terminal/continuation阶段产生对应结果"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanners::scan_eol;
    use crate::strings::CowStr;

    use crate::scanners::is_ascii_punctuation;
    fn original_label<'t>(
        text: &'t str,
        linebreak_handler: &dyn Fn(&[u8]) -> Option<usize>,
        is_in_table: bool,
    ) -> Option<(usize, CowStr<'t>)> {
        let bytes = text.as_bytes();
        let mut ix = 0;
        let mut only_white_space = true;
        let mut codepoints = 0;
        // no worries, doesn't allocate until we push things onto it
        let mut label = String::new();
        let mut mark = 0;

        loop {
            if codepoints >= 1000 {
                return None;
            }
            match *bytes.get(ix)? {
                b'[' => return None,
                b']' => break,
                // Backslash escapes in link references are normally untouched, but
                // tables are an exception, because they're parsed as-if the tables
                // were parsed in a discrete pass, changing `\|` to `|`, and then
                // passing the changed string to the inline parser.
                b'|' if is_in_table && ix != 0 && bytes.get(ix - 1) == Some(&b'\\') => {
                    // only way to reach this spot is to have `\\|` (even number of `\` before `|`)
                    label.push_str(&text[mark..ix - 1]);
                    label.push('|');
                    ix += 1;
                    only_white_space = false;
                    mark = ix;
                }
                b'\\' if is_in_table && bytes.get(ix + 1) == Some(&b'|') => {
                    // only way to reach this spot is to have `\|` (odd number of `\` before `|`)
                    label.push_str(&text[mark..ix]);
                    label.push('|');
                    ix += 2;
                    codepoints += 1;
                    only_white_space = false;
                    mark = ix;
                }
                b'\\' if is_ascii_punctuation(*bytes.get(ix + 1)?) => {
                    ix += 2;
                    codepoints += 2;
                    only_white_space = false;
                }
                b if is_ascii_whitespace(b) => {
                    // normalize labels by collapsing whitespaces, including linebreaks
                    let mut whitespaces = 0;
                    let mut linebreaks = 0;
                    let whitespace_start = ix;

                    while ix < bytes.len() && is_ascii_whitespace(bytes[ix]) {
                        if let Some(eol_bytes) = scan_eol(&bytes[ix..]) {
                            linebreaks += 1;
                            if linebreaks > 1 {
                                return None;
                            }
                            ix += eol_bytes;
                            ix += linebreak_handler(&bytes[ix..])?;
                            whitespaces += 2; // indicate that we need to replace
                        } else {
                            whitespaces += if bytes[ix] == b' ' { 1 } else { 2 };
                            ix += 1;
                        }
                    }
                    if whitespaces > 1 {
                        label.push_str(&text[mark..whitespace_start]);
                        label.push(' ');
                        mark = ix;
                        codepoints += ix - whitespace_start;
                    } else {
                        codepoints += 1;
                    }
                }
                b => {
                    only_white_space = false;
                    ix += 1;
                    if b & 0b1000_0000 != 0 {
                        codepoints += 1;
                    }
                }
            }
        }

        if only_white_space {
            None
        } else {
            let cow = if mark == 0 {
                let asciiws = &[' ', '\r', '\n', '\t'][..];
                text[..ix].trim_matches(asciiws).into()
            } else {
                label.push_str(&text[mark..ix]);
                while matches!(
                    label.as_bytes().last(),
                    Some(&b' ' | &b'\r' | &b'\n' | &b'\t')
                ) {
                    label.pop();
                }
                while matches!(
                    label.as_bytes().first(),
                    Some(&b' ' | &b'\r' | &b'\n' | &b'\t')
                ) {
                    label.remove(0);
                }
                label.into()
            };
            Some((ix + 1, cow))
        }
    }

    fn run(source: &str, in_table: bool, chunk: usize, prefix: bool) -> Option<usize> {
        let mut scanner = LabelScanner::new(in_table);
        let mut offset = 0;
        loop {
            let end = (offset + chunk).min(source.len());
            let step = scanner
                .feed(&source.as_bytes()[offset..end], end == source.len(), 7)
                .unwrap();
            let consumed = match step {
                LabelStep::Progress { consumed }
                | LabelStep::NeedInput { consumed }
                | LabelStep::NeedLineContinuation { consumed, .. }
                | LabelStep::Complete { consumed, .. }
                | LabelStep::Rejected { consumed } => consumed,
            };
            assert!(consumed <= 7);
            offset += consumed;
            match step {
                LabelStep::Complete { byte_count, .. } => return Some(byte_count),
                LabelStep::Rejected { .. } => return None,
                LabelStep::NeedLineContinuation { after_eol, .. } => {
                    assert_eq!(offset, after_eol);
                    let skip: usize = if prefix && source.as_bytes()[offset..].starts_with(b"> ") {
                        2
                    } else {
                        0
                    };
                    let last = skip.checked_sub(1).map(|n| source.as_bytes()[offset + n]);
                    scanner.continue_line(Some((skip, last))).unwrap();
                    offset += skip;
                }
                _ => assert!(consumed > 0),
            }
        }
    }

    #[test]
    fn label_all_byte_chunks_match_original_extent_with_unicode_whitespace_and_escapes() {
        let alphabet = ['a', ']', '[', '|', '\\', '\t', '\n', '\r', ' ', '中', '😀'];
        let mut random = 0x296d45a9bc7e0f13_u64;
        for case in 0..4096 {
            let mut source = String::new();
            for _ in 0..(case % 53) {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                source.push(alphabet[random as usize % alphabet.len()]);
            }
            source.push(']');
            for in_table in [false, true] {
                let expected = original_label(&source, &|_| Some(0), in_table).map(|value| value.0);
                for chunk in 1..=source.len().min(13) {
                    assert_eq!(
                        run(&source, in_table, chunk, false),
                        expected,
                        "case={case} chunk={chunk}"
                    );
                }
            }
        }
    }

    #[test]
    fn skipped_prefix_is_in_raw_extent_and_codepoint_budget_but_no_label_body_is_held() {
        for source in [
            "first\r\n> second]",
            "first\n> second\r\n> third]",
            " \t label ]",
        ] {
            let expected = original_label(
                source,
                &|bytes| Some(if bytes.starts_with(b"> ") { 2 } else { 0 }),
                false,
            )
            .map(|value| value.0);
            for chunk in 1..=source.len() {
                assert_eq!(run(source, false, chunk, true), expected);
            }
        }
        let mut scanner = LabelScanner::new(false);
        let chunk = [b'a'; 4096];
        for _ in 0..1024 {
            // 固定上游对ASCII普通字节不增加codepoints；不能凭999的名字假设label只有999bytes。
            assert_eq!(
                scanner.feed(&chunk, false, chunk.len()).unwrap(),
                LabelStep::NeedInput {
                    consumed: chunk.len()
                }
            );
        }
        assert_eq!(
            scanner.feed(b"]", false, 1).unwrap(),
            LabelStep::Complete {
                consumed: 1,
                byte_count: 4 * 1024 * 1024 + 1
            }
        );
        assert!(std::mem::size_of::<LabelScanner>() <= 128);
    }
}
