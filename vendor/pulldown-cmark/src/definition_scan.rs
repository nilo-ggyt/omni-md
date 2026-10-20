// Copyright 2015 Google Inc. All rights reserved.
// 原始destination规则提取自固定0.13.4 scanners.rs；完整MIT条款见LICENSE。

//! definition/inline destination的分段结构识别；只保锚点与括号/escape状态，不保存URL正文。

use crate::scanners::is_ascii_punctuation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LabelLexeme {
    Reject,
    Close,
    TablePipe,
    EscapedTablePipe,
    EscapedPunctuation,
    Whitespace,
    Ordinary(u8),
    NeedLookahead,
}

/// 原label match的优先级；stream传入next_known=false时不得把frame尾反斜杠误判为EOF。
pub(crate) fn label_lexeme(
    byte: u8,
    previous: Option<u8>,
    next: Option<u8>,
    next_known: bool,
    in_table: bool,
) -> LabelLexeme {
    use crate::scanners::is_ascii_whitespace;
    match byte {
        b'[' => LabelLexeme::Reject,
        b']' => LabelLexeme::Close,
        b'|' if in_table && previous == Some(b'\\') => LabelLexeme::TablePipe,
        b'\\' if in_table && next == Some(b'|') => LabelLexeme::EscapedTablePipe,
        b'\\' if next.is_some_and(is_ascii_punctuation) => LabelLexeme::EscapedPunctuation,
        b'\\' if next.is_none() => {
            if next_known {
                LabelLexeme::Reject
            } else {
                LabelLexeme::NeedLookahead
            }
        }
        byte if is_ascii_whitespace(byte) => LabelLexeme::Whitespace,
        byte => LabelLexeme::Ordinary(byte),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DestinationResult {
    NeedInput,
    Rejected,
    Complete {
        byte_count: usize,
        content_start: usize,
        content_end: usize,
    },
}

#[derive(Clone, Copy, Debug)]
enum DestinationKind {
    Start,
    Pointy,
    Bare,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DestinationScanner {
    kind: DestinationKind,
    maximum_nesting: usize,
    nesting: usize,
    offset: usize,
    pending_backslash: bool,
    result: DestinationResult,
}

impl DestinationScanner {
    pub(crate) fn new(maximum_nesting: usize) -> Self {
        Self {
            kind: DestinationKind::Start,
            maximum_nesting,
            nesting: 0,
            offset: 0,
            pending_backslash: false,
            result: DestinationResult::NeedInput,
        }
    }

    /// 返回本次真正消费量；bare终止空白/右括号不消费，交回外层后续title/label规则。
    pub(crate) fn feed(&mut self, bytes: &[u8], real_eof: bool) -> (usize, DestinationResult) {
        if self.result != DestinationResult::NeedInput {
            return (0, self.result);
        }
        let mut consumed = 0;
        for byte in bytes {
            if matches!(self.kind, DestinationKind::Start) {
                if *byte == b'<' {
                    self.kind = DestinationKind::Pointy;
                    self.offset += 1;
                    consumed += 1;
                    continue;
                }
                self.kind = DestinationKind::Bare;
            }
            if self.pending_backslash {
                self.pending_backslash = false;
                if is_ascii_punctuation(*byte) {
                    // 原scanner跳过escape后的punctuation，pointy closing/括号均不再参与结构判断。
                    self.offset += 1;
                    consumed += 1;
                    continue;
                }
                // 非punctuation不被转义；前一反斜杠已消费，本byte仍按真实终止/括号规则处理。
            }
            let terminal = match self.kind {
                DestinationKind::Pointy => match *byte {
                    b'\n' | b'\r' | b'<' => Some(DestinationResult::Rejected),
                    b'>' => {
                        let content_end = self.offset;
                        self.offset += 1;
                        consumed += 1;
                        Some(DestinationResult::Complete {
                            byte_count: self.offset,
                            content_start: 1,
                            content_end,
                        })
                    }
                    _ => None,
                },
                DestinationKind::Bare => match *byte {
                    0x00..=0x20 => Some(self.finish_bare()),
                    b'(' => {
                        if self.nesting > self.maximum_nesting {
                            // 保留原比较的严格大于次序：不是顺手把最大嵌套修成>=。
                            Some(DestinationResult::Rejected)
                        } else {
                            self.nesting += 1;
                            None
                        }
                    }
                    b')' if self.nesting == 0 => Some(self.finish_bare()),
                    b')' => {
                        self.nesting -= 1;
                        None
                    }
                    _ => None,
                },
                DestinationKind::Start => unreachable!("首byte已选择pointy/bare"),
            };
            if let Some(result) = terminal {
                self.result = result;
                return (consumed, result);
            }
            self.pending_backslash = *byte == b'\\';
            self.offset += 1;
            consumed += 1;
        }
        if real_eof {
            self.result = match self.kind {
                DestinationKind::Pointy => DestinationResult::Rejected,
                // 原bare空串返回长度0；refdef外层另行拒绝空destination，inline规则仍可接受。
                DestinationKind::Start | DestinationKind::Bare => self.finish_bare(),
            };
        }
        (consumed, self.result)
    }

    fn finish_bare(&self) -> DestinationResult {
        if self.nesting == 0 {
            DestinationResult::Complete {
                byte_count: self.offset,
                content_start: 0,
                content_end: self.offset,
            }
        } else {
            DestinationResult::Rejected
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TitleStep {
    NeedInput,
    Rejected,
    /// segment是此次换行前原始正文；外层只在需要title值时拼接/规范化，结构producer忽略其正文。
    NeedLine {
        after_eol: usize,
        segment_start: usize,
        segment_end: usize,
    },
    Complete {
        byte_count: usize,
        segment_start: usize,
        segment_end: usize,
    },
}

#[derive(Clone, Copy, Debug)]
enum TitlePhase {
    Start,
    Body,
    Backslash,
    PendingCr,
    AwaitLine,
    Complete,
    Rejected,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TitleScanner {
    phase: TitlePhase,
    closing: u8,
    offset: usize,
    segment_start: usize,
    segment_end: usize,
}

impl TitleScanner {
    pub(crate) fn new() -> Self {
        Self {
            phase: TitlePhase::Start,
            closing: 0,
            offset: 0,
            segment_start: 1,
            segment_end: 1,
        }
    }

    /// 外层完成真实容器与中断/blank检查后，才允许跳过原始prefix；本scanner不猜其语法。
    pub(crate) fn continue_line(&mut self, prefix_bytes: usize) {
        assert!(matches!(self.phase, TitlePhase::AwaitLine));
        self.offset += prefix_bytes;
        self.segment_start = self.offset;
        self.phase = TitlePhase::Body;
    }

    /// 每byte最多两个转换；frame不足只NeedInput，尾反斜杠与CR等待实际后继/真实EOF。
    pub(crate) fn feed(&mut self, bytes: &[u8], real_eof: bool) -> (usize, TitleStep) {
        let mut consumed = 0;
        while consumed < bytes.len() {
            let byte = bytes[consumed];
            match self.phase {
                TitlePhase::Complete | TitlePhase::Rejected | TitlePhase::AwaitLine => {
                    return (consumed, self.step());
                }
                TitlePhase::Start => {
                    self.closing = match byte {
                        b'\'' => b'\'',
                        b'"' => b'"',
                        b'(' => b')',
                        _ => {
                            self.phase = TitlePhase::Rejected;
                            return (consumed, self.step());
                        }
                    };
                    self.phase = TitlePhase::Body;
                }
                TitlePhase::Backslash => {
                    self.phase = TitlePhase::Body;
                    if matches!(byte, b'\r' | b'\n') {
                        continue;
                    }
                    // 原实现对所有非EOL后继跳过一byte，不限ASCII punctuation；不擅自改转义规则。
                }
                TitlePhase::PendingCr => {
                    if byte == b'\n' {
                        self.offset += 1;
                        consumed += 1;
                    }
                    self.phase = TitlePhase::AwaitLine;
                    return (consumed, self.step());
                }
                TitlePhase::Body => match byte {
                    b'(' if self.closing == b')' => {
                        self.phase = TitlePhase::Rejected;
                        return (consumed, self.step());
                    }
                    b'\r' | b'\n' => {
                        self.segment_end = self.offset;
                        self.offset += 1;
                        consumed += 1;
                        if byte == b'\r' {
                            self.phase = TitlePhase::PendingCr;
                            continue;
                        }
                        self.phase = TitlePhase::AwaitLine;
                        return (consumed, self.step());
                    }
                    b'\\' => self.phase = TitlePhase::Backslash,
                    byte if byte == self.closing => {
                        self.segment_end = self.offset;
                        self.offset += 1;
                        consumed += 1;
                        self.phase = TitlePhase::Complete;
                        return (consumed, self.step());
                    }
                    _ => {}
                },
            }
            self.offset += 1;
            consumed += 1;
        }
        if real_eof {
            if matches!(self.phase, TitlePhase::PendingCr) {
                self.phase = TitlePhase::AwaitLine;
            } else if !matches!(
                self.phase,
                TitlePhase::Complete | TitlePhase::Rejected | TitlePhase::AwaitLine
            ) {
                self.phase = TitlePhase::Rejected;
            }
            return (consumed, self.step());
        }
        if matches!(
            self.phase,
            TitlePhase::Complete | TitlePhase::Rejected | TitlePhase::AwaitLine
        ) {
            (consumed, self.step())
        } else {
            (consumed, TitleStep::NeedInput)
        }
    }

    fn step(&self) -> TitleStep {
        match self.phase {
            TitlePhase::Complete => TitleStep::Complete {
                byte_count: self.offset,
                segment_start: self.segment_start,
                segment_end: self.segment_end,
            },
            TitlePhase::Rejected => TitleStep::Rejected,
            TitlePhase::AwaitLine => TitleStep::NeedLine {
                after_eol: self.offset,
                segment_start: self.segment_start,
                segment_end: self.segment_end,
            },
            _ => unreachable!("仅terminal/continuation请求可转换为TitleStep"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SpaceStep {
    NeedInput,
    Rejected,
    NeedLine { after_eol: usize },
    Complete { byte_count: usize, newlines: usize },
}

#[derive(Clone, Copy, Debug)]
enum SpacePhase {
    Space,
    PendingCr,
    AwaitLine,
    Complete,
    Rejected,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RefdefSpaceScanner {
    phase: SpacePhase,
    offset: usize,
    newlines: usize,
}

impl RefdefSpaceScanner {
    pub(crate) fn new() -> Self {
        Self {
            phase: SpacePhase::Space,
            offset: 0,
            newlines: 0,
        }
    }
    pub(crate) fn continue_line(&mut self, prefix_bytes: usize) {
        assert!(matches!(self.phase, SpacePhase::AwaitLine));
        self.offset += prefix_bytes;
        self.phase = SpacePhase::Space;
    }
    fn finish_eol(&mut self) {
        self.newlines += 1;
        self.phase = if self.newlines > 1 {
            SpacePhase::Rejected
        } else {
            SpacePhase::AwaitLine
        };
    }
    pub(crate) fn feed(&mut self, bytes: &[u8], real_eof: bool) -> (usize, SpaceStep) {
        use crate::scanners::is_ascii_whitespace_no_nl;
        let mut consumed = 0;
        while consumed < bytes.len() {
            let byte = bytes[consumed];
            match self.phase {
                SpacePhase::Complete | SpacePhase::Rejected | SpacePhase::AwaitLine => {
                    return (consumed, self.step());
                }
                SpacePhase::PendingCr => {
                    if byte == b'\n' {
                        self.offset += 1;
                        consumed += 1;
                    }
                    self.finish_eol();
                    return (consumed, self.step());
                }
                SpacePhase::Space => {
                    if byte == b'\r' {
                        self.offset += 1;
                        consumed += 1;
                        self.phase = SpacePhase::PendingCr;
                        continue;
                    }
                    if byte == b'\n' {
                        self.offset += 1;
                        consumed += 1;
                        self.finish_eol();
                        return (consumed, self.step());
                    }
                    if !is_ascii_whitespace_no_nl(byte) {
                        self.phase = SpacePhase::Complete;
                        return (consumed, self.step());
                    }
                }
            }
            self.offset += 1;
            consumed += 1;
        }
        if real_eof {
            if matches!(self.phase, SpacePhase::Space | SpacePhase::PendingCr) {
                // 原scan_eol(empty)为Some(0)；真实EOF仍经同一续行权限，不把短frame认作它。
                self.finish_eol();
            }
            return (consumed, self.step());
        }
        if matches!(
            self.phase,
            SpacePhase::Complete | SpacePhase::Rejected | SpacePhase::AwaitLine
        ) {
            (consumed, self.step())
        } else {
            (consumed, SpaceStep::NeedInput)
        }
    }
    fn step(&self) -> SpaceStep {
        match self.phase {
            SpacePhase::Complete => SpaceStep::Complete {
                byte_count: self.offset,
                newlines: self.newlines,
            },
            SpacePhase::Rejected => SpaceStep::Rejected,
            SpacePhase::AwaitLine => SpaceStep::NeedLine {
                after_eol: self.offset,
            },
            _ => unreachable!("只有完成/续行/拒绝阶段可生成SpaceStep"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanners::scan_ch;

    /// 保留未改原函数作oracle，防止候选wrapper与候选state互相证明。
    fn original(data: &str, maximum: usize) -> Option<(usize, &str)> {
        let bytes = data.as_bytes();
        let mut i = scan_ch(bytes, b'<');
        if i != 0 {
            while i < bytes.len() {
                match bytes[i] {
                    b'\n' | b'\r' | b'<' => return None,
                    b'>' => return Some((i + 1, &data[1..i])),
                    b'\\' if i + 1 < bytes.len() && is_ascii_punctuation(bytes[i + 1]) => {
                        i += 1;
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        } else {
            let mut nest = 0;
            while i < bytes.len() {
                match bytes[i] {
                    0x0..=0x20 => break,
                    b'(' => {
                        if nest > maximum {
                            return None;
                        }
                        nest += 1;
                    }
                    b')' => {
                        if nest == 0 {
                            break;
                        }
                        nest -= 1;
                    }
                    b'\\' if i + 1 < bytes.len() && is_ascii_punctuation(bytes[i + 1]) => {
                        i += 1;
                    }
                    _ => {}
                }
                i += 1;
            }
            if nest != 0 {
                None
            } else {
                Some((i, &data[..i]))
            }
        }
    }

    #[test]
    fn destination_all_byte_splits_match_original_including_escape_and_nesting() {
        let alphabet = [
            'a', '(', ')', '<', '>', '\\', '\t', '\n', '\r', ' ', '中', '😀',
        ];
        let mut random = 0x3745468e1d2c7b99_u64;
        for case in 0..4096 {
            let mut source = String::new();
            for _ in 0..(case % 47) {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                source.push(alphabet[random as usize % alphabet.len()]);
            }
            for maximum in [0, 1, 32] {
                let expected = original(&source, maximum);
                for split in 0..=source.len() {
                    let mut scanner = DestinationScanner::new(maximum);
                    let (_, first) = scanner.feed(&source.as_bytes()[..split], false);
                    let (_, result) = if first == DestinationResult::NeedInput {
                        scanner.feed(&source.as_bytes()[split..], true)
                    } else {
                        (0, first)
                    };
                    let actual = match result {
                        DestinationResult::Complete {
                            byte_count,
                            content_start,
                            content_end,
                        } => Some((byte_count, &source[content_start..content_end])),
                        DestinationResult::Rejected => None,
                        DestinationResult::NeedInput => panic!("真实EOF必须完成或拒绝"),
                    };
                    assert_eq!(actual, expected, "case={case} split={split} max={maximum}");
                }
            }
        }
    }

    #[test]
    fn long_destination_has_no_body_snapshot_and_frame_end_is_not_eof() {
        let mut scanner = DestinationScanner::new(32);
        assert_eq!(scanner.feed(b"<", false).1, DestinationResult::NeedInput);
        let chunk = [b'a'; 4096];
        for _ in 0..1024 {
            assert_eq!(
                scanner.feed(&chunk, false),
                (chunk.len(), DestinationResult::NeedInput)
            );
        }
        assert_eq!(
            scanner.feed(b">suffix", false),
            (
                1,
                DestinationResult::Complete {
                    byte_count: 4 * 1024 * 1024 + 2,
                    content_start: 1,
                    content_end: 4 * 1024 * 1024 + 1,
                }
            )
        );
        assert!(std::mem::size_of::<DestinationScanner>() <= 80);
        let mut pending = DestinationScanner::new(32);
        assert_eq!(
            pending.feed(b"<still open", false).1,
            DestinationResult::NeedInput
        );
        assert_eq!(pending.feed(&[], true).1, DestinationResult::Rejected);
    }
    fn original_title<'t>(
        text: &'t str,
        linebreak: &dyn Fn(&[u8]) -> Option<usize>,
    ) -> Option<(usize, CowStr<'t>)> {
        let bytes = text.as_bytes();
        let closing_delim = match bytes.first()? {
            b'\'' => b'\'',
            b'"' => b'"',
            b'(' => b')',
            _ => return None,
        };
        let mut bytecount = 1;
        let mut linestart = 1;

        let mut linebuf = None;

        while let Some(&c) = bytes.get(bytecount) {
            match c {
                b'(' if closing_delim == b')' => {
                    // https://spec.commonmark.org/0.30/#link-title
                    // a sequence of zero or more characters between matching parentheses ((...)),
                    // including a ( or ) character only if it is backslash-escaped.
                    return None;
                }
                b'\n' | b'\r' => {
                    // push text to line buffer
                    // this is used to strip the block formatting:
                    //
                    // > [first]: http://example.com "
                    // > second"
                    //
                    // should get turned into `<a href="example.com" title=" second">first</a>`
                    let linebuf = if let Some(linebuf) = &mut linebuf {
                        linebuf
                    } else {
                        linebuf = Some(String::new());
                        linebuf.as_mut().unwrap()
                    };
                    linebuf.push_str(&text[linestart..bytecount]);
                    linebuf.push('\n'); // normalize line breaks
                                        // skip line break
                    bytecount += 1;
                    if c == b'\r' && bytes.get(bytecount) == Some(&b'\n') {
                        bytecount += 1;
                    }
                    bytecount += linebreak(&bytes[bytecount..])?;
                    linestart = bytecount;
                    if scan_blank_line(&bytes[bytecount..]).is_some() {
                        // blank line - not allowed
                        return None;
                    }
                }
                b'\\' => {
                    bytecount += 1;
                    if let Some(c) = bytes.get(bytecount) {
                        if c != &b'\r' && c != &b'\n' {
                            bytecount += 1;
                        }
                    }
                }
                c if c == closing_delim => {
                    let cow = if let Some(mut linebuf) = linebuf {
                        linebuf.push_str(&text[linestart..bytecount]);
                        CowStr::from(linebuf)
                    } else {
                        CowStr::from(&text[linestart..bytecount])
                    };
                    return Some((bytecount + 1, cow));
                }
                _ => {
                    bytecount += 1;
                }
            }
        }
        None
    }

    use crate::scanners::scan_blank_line;
    use crate::strings::CowStr;

    fn title_prefix(bytes: &[u8]) -> Option<usize> {
        let mut skip = if bytes.starts_with(b"> ") { 2 } else { 0 };
        skip += bytes[skip..]
            .iter()
            .take_while(|byte| matches!(**byte, b' ' | b'\t'))
            .count();
        if bytes.get(skip) == Some(&b'#') {
            None
        } else {
            Some(skip)
        }
    }

    fn streamed_title(source: &str, chunk: usize) -> Option<(usize, String)> {
        let mut scanner = TitleScanner::new();
        let mut offset = 0;
        let mut title = String::new();
        loop {
            let end = (offset + chunk).min(source.len());
            let (consumed, step) =
                scanner.feed(&source.as_bytes()[offset..end], end == source.len());
            offset += consumed;
            match step {
                TitleStep::NeedLine {
                    after_eol,
                    segment_start,
                    segment_end,
                } => {
                    assert_eq!(offset, after_eol);
                    title.push_str(&source[segment_start..segment_end]);
                    title.push('\n');
                    let skip = title_prefix(&source.as_bytes()[offset..])?;
                    offset += skip;
                    if scan_blank_line(&source.as_bytes()[offset..]).is_some() {
                        return None;
                    }
                    scanner.continue_line(skip);
                }
                TitleStep::Complete {
                    byte_count,
                    segment_start,
                    segment_end,
                } => {
                    title.push_str(&source[segment_start..segment_end]);
                    return Some((byte_count, title));
                }
                TitleStep::Rejected => return None,
                TitleStep::NeedInput => assert!(consumed > 0),
            }
        }
    }

    #[test]
    fn title_all_byte_chunks_match_original_values_ranges_and_multiline_backup_input() {
        let alphabet = [
            'a', '(', ')', '\'', '"', '\\', '\t', '\n', '\r', ' ', '中', '😀',
        ];
        let mut random = 0x329743abdf351c89_u64;
        for case in 0..2048 {
            let mut source = String::new();
            source.push(['"', '\'', '('][case % 3]);
            for _ in 0..(case % 67) {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                source.push(alphabet[random as usize % alphabet.len()]);
            }
            source.push(['"', '\'', ')'][case % 3]);
            let expected = original_title(&source, &title_prefix)
                .map(|(length, title)| (length, title.to_string()));
            for chunk in 1..=source.len().min(13) {
                assert_eq!(
                    streamed_title(&source, chunk),
                    expected,
                    "case={case} chunk={chunk}"
                );
            }
        }
        for source in [
            "\"first\r\n> 中😀 second\"tail",
            "(first\\)still)",
            "\"first\n# interrupt\"",
            "\"first\n\nend\"",
        ] {
            let expected = original_title(source, &title_prefix)
                .map(|(length, title)| (length, title.to_string()));
            for chunk in 1..=source.len() {
                assert_eq!(streamed_title(source, chunk), expected);
            }
        }
    }

    #[test]
    fn long_unclosed_title_streams_without_body_storage_until_real_eof() {
        let mut scanner = TitleScanner::new();
        assert_eq!(scanner.feed(b"\"", false), (1, TitleStep::NeedInput));
        let chunk = [b'x'; 4096];
        for _ in 0..1024 {
            assert_eq!(scanner.feed(&chunk, false), (4096, TitleStep::NeedInput));
        }
        assert_eq!(scanner.feed(&[], true), (0, TitleStep::Rejected));
        assert!(std::mem::size_of::<TitleScanner>() <= 48);
    }
}
