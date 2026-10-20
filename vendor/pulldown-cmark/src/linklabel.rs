// Copyright 2018 Google LLC
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.

//! Link label parsing and matching.

use unicase::UniCase;

use crate::definition_scan::{label_lexeme, LabelLexeme};
use crate::scanners::{is_ascii_whitespace, scan_eol};
use crate::strings::CowStr;

#[derive(Debug)]
pub(crate) enum ReferenceLabel<'a> {
    Link(CowStr<'a>),
    Footnote(CowStr<'a>),
}

pub(crate) type LinkLabel<'a> = UniCase<CowStr<'a>>;

pub(crate) type FootnoteLabel<'a> = UniCase<CowStr<'a>>;

/// Assumes the opening bracket has already been scanned.
/// The line break handler determines what happens when a linebreak
/// is found. It is passed the bytes following the line break and
/// either returns `Some(k)`, where `k` is the number of bytes to skip,
/// or `None` to abort parsing the label.
/// Returns the number of bytes read (including closing bracket) and label on success.
pub(crate) fn scan_link_label_rest<'t>(
    text: &'t str,
    linebreak_handler: &dyn Fn(&[u8]) -> Option<usize>,
    is_in_table: bool,
) -> Option<(usize, CowStr<'t>)> {
    // 表格的离散 pipe 投影仍走原专有路径；Root 引用与分帧游标共享下面的真实规范化器。
    // 原 Parser 的容器前缀回调在 NeedLine 处执行，不把引入帧的末尾当逻辑行末。
    if is_in_table {
        return scan_link_label_rest_table(text, linebreak_handler, true);
    }
    let mut scanner = LabelScanner::new();
    // 原有借用优化仍保留：输出与原前缀逐字节相同就不分配；首次真正改写才持有
    // 一个原 label String。该完整入口只供既有整段 Parser，不被流式 cursor 调用。
    let mut normalized: Option<String> = None;
    let mut output_len = 0;
    let mut carry = [0u8; 4];
    let mut carry_len = 0;
    loop {
        let mut output = [0u8; 256];
        output[..carry_len].copy_from_slice(&carry[..carry_len]);
        let (_, added, step) = scanner.feed(
            &text.as_bytes()[scanner.offset..],
            true,
            &mut output[carry_len..],
        );
        // 片尾可停在 UTF8 中间。只把真实完整 scalar 交给 String，最多三字节回到原
        // 固定 carry；不能每次为验证而把已完成的整个 key 再扫一遍。
        let produced = carry_len + added;
        let written = match std::str::from_utf8(&output[..produced]) {
            Ok(_) => produced,
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => return None,
        };
        carry_len = produced - written;
        carry[..carry_len].copy_from_slice(&output[written..produced]);
        if let Some(value) = normalized.as_mut() {
            value.push_str(std::str::from_utf8(&output[..written]).ok()?);
        } else if text.as_bytes().get(output_len..output_len + written) == Some(&output[..written])
        {
            output_len += written;
        } else {
            let mut value = text[..output_len].to_owned();
            value.push_str(std::str::from_utf8(&output[..written]).ok()?);
            normalized = Some(value);
        }
        match step {
            LabelStep::Complete => {
                if carry_len != 0 {
                    return None;
                }
                let label = normalized.map_or_else(|| text[..output_len].into(), Into::into);
                return Some((scanner.offset, label));
            }
            LabelStep::Rejected => return None,
            LabelStep::NeedLine => {
                // whole Parser 沿原容器边界跳 prefix；流式 Root 在同暂停点跳零，
                // 两者共用 label 语法，不能把容器标记压进规范化 identifier。
                let skip = linebreak_handler(&text.as_bytes()[scanner.offset..])?;
                scanner.continue_line(skip)?;
            }
            LabelStep::NeedInput if scanner.offset == text.len() => return None,
            LabelStep::NeedInput | LabelStep::Progress => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LabelStep {
    Progress,
    NeedInput,
    NeedLine,
    Complete,
    Rejected,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LabelPhase {
    Normal,
    Escape,
    Space,
    Cr,
    Line,
    Complete,
    Rejected,
}

#[derive(Clone, Debug)]
/// 原 label 规范化的恢复状态。只有最多一个 escape 和一段空白事实，无 identifier String。
/// codepoints 刻意沿原算法的计数规则：普通 ASCII 不加，非 ASCII 按原 byte 加，不能偷换上限。
/// 每段连续空白最多一个 EOL；容器 prefix 的实际跳过字节也进入原空白计数。
pub(crate) struct LabelScanner {
    pub(crate) offset: usize,
    pub(crate) transitions: usize,
    phase: LabelPhase,
    codepoints: usize,
    whitespace: usize,
    whitespace_weight: usize,
    linebreaks: usize,
    content: bool,
    pending_space: bool,
}
impl LabelScanner {
    pub(crate) fn new() -> Self {
        Self {
            offset: 0,
            transitions: 0,
            phase: LabelPhase::Normal,
            codepoints: 0,
            whitespace: 0,
            whitespace_weight: 0,
            linebreaks: 0,
            content: false,
            pending_space: false,
        }
    }
    /// 仅真正 EOL 后调用。prefix 是原调用者已验证的容器范围，而不是猜出的缩进。
    pub(crate) fn continue_line(&mut self, prefix: usize) -> Option<()> {
        if self.phase != LabelPhase::Line {
            return None;
        }
        self.offset = self.offset.checked_add(prefix)?;
        self.whitespace = self.whitespace.checked_add(prefix)?;
        self.phase = LabelPhase::Space;
        Some(())
    }
    /// 返回本帧实际消耗/输出；输出停在完整 scalar 边界的责任由公有 decoder 的四字节 carry 承担。
    /// whole Parser 传足够 UTF8 片时也不能在片尾直接 from_utf8，因此其外层另保同一 carry。
    pub(crate) fn feed(
        &mut self,
        input: &[u8],
        eof: bool,
        output: &mut [u8],
    ) -> (usize, usize, LabelStep) {
        let mut read = 0;
        let mut written = 0;
        loop {
            self.transitions += 1;
            match self.phase {
                // 完成/拒绝是终态。空输出片不构造一次新的 label 或重新消费 closing。
                LabelPhase::Complete => return (read, written, LabelStep::Complete),
                LabelPhase::Rejected => return (read, written, LabelStep::Rejected),
                LabelPhase::Line => return (read, written, LabelStep::NeedLine),
                _ => {}
            }
            // 暂时没有输入不是 EOF。尤其 CR 需要等可能的 LF，反斜杠需要真实后继，
            // 否则按一个 frame 的结束签合法 key 会改变完整 Parser 的语义。
            let Some(&byte) = input.get(read) else {
                if self.phase == LabelPhase::Cr {
                    if eof {
                        self.phase = LabelPhase::Line;
                        continue;
                    }
                } else if eof && written + 3 <= output.len() {
                    self.phase = LabelPhase::Rejected;
                    continue;
                }
                return (read, written, LabelStep::NeedInput);
            };
            if written + 3 > output.len() {
                return (read, written, LabelStep::Progress);
            }
            match self.phase {
                LabelPhase::Cr => {
                    // CRLF 是一个 EOL；二者实际 byte 都进入原空白计数，回调只发生一次。
                    // 非 LF 后继不消费，交真实容器 prefix/空白阶段继续处理。
                    if byte == b'\n' {
                        read += 1;
                        self.offset += 1;
                        self.whitespace += 1;
                    }
                    self.phase = LabelPhase::Line;
                    continue;
                }
                LabelPhase::Space if is_ascii_whitespace(byte) => {
                    // 连续空白只保存长度和 EOL 事实，不积累空白正文。两次 EOL 会拒绝
                    // 这个引用 label，不能延伸穿越原块分隔边界去找后来的 closing。

                    self.whitespace += 1;
                    self.whitespace_weight += if byte == b' ' { 1 } else { 2 };
                    read += 1;
                    self.offset += 1;
                    if matches!(byte, b'\r' | b'\n') {
                        self.linebreaks += 1;
                        if self.linebreaks > 1 {
                            self.phase = LabelPhase::Rejected;
                        } else {
                            self.phase = if byte == b'\r' {
                                LabelPhase::Cr
                            } else {
                                LabelPhase::Line
                            };
                        }
                    }
                    continue;
                }
                LabelPhase::Space => {
                    // 原算法对单个普通空格与需改写的空白分别计数；保持这条真实上限。
                    // 仅在已有内容之后暂存一个空格，closing 时不输出它，原样实现边界 trim。
                    self.codepoints += if self.whitespace_weight > 1 {
                        self.whitespace
                    } else {
                        1
                    };
                    self.pending_space = self.content;
                    self.phase = LabelPhase::Normal;
                    continue;
                }
                LabelPhase::Escape => {
                    // 原引用保留 escape 的反斜杠，实体也原样；URL 的 unescape 规则不能借用到 key。
                    if self.pending_space {
                        output[written] = b' ';
                        written += 1;
                        self.pending_space = false;
                    }
                    output[written] = b'\\';
                    written += 1;
                    self.content = true;
                    self.phase = LabelPhase::Normal;
                    if byte.is_ascii_punctuation() {
                        output[written] = byte;
                        written += 1;
                        self.codepoints += 2;
                        read += 1;
                        self.offset += 1;
                    }
                    continue;
                }
                LabelPhase::Normal => {
                    // 上限按原外循环在处理下一个 lexeme 前检查。因此 1000 的拒绝点
                    // 包含 closing bracket，不能为了放宽结果把检查挪到关闭之后。
                    if self.codepoints >= 1000 || byte == b'[' {
                        self.phase = LabelPhase::Rejected;
                        continue;
                    }
                    // 只有未被 escape 消费的真正 ']' 能完成；只空白的标签没有内容，
                    // 仍必须拒绝，不能把它加入唯一 Queries 当成一个空 key。
                    if byte == b']' {
                        read += 1;
                        self.offset += 1;
                        self.phase = if self.content {
                            LabelPhase::Complete
                        } else {
                            LabelPhase::Rejected
                        };
                        continue;
                    }
                    if is_ascii_whitespace(byte) {
                        self.phase = LabelPhase::Space;
                        self.whitespace = 0;
                        self.whitespace_weight = 0;
                        self.linebreaks = 0;
                        continue;
                    }
                    read += 1;
                    self.offset += 1;
                    // 反斜杠先单独消费，真实后继决定是否原样复制两个标点。
                    // 不在这里调用 URI unescape，更不能把实体提前变成另一个引用名字。
                    if byte == b'\\' {
                        self.phase = LabelPhase::Escape;
                        continue;
                    }
                    if self.pending_space {
                        output[written] = b' ';
                        written += 1;
                        self.pending_space = false;
                    }
                    output[written] = byte;
                    written += 1;
                    self.content = true;
                    self.codepoints += usize::from(!byte.is_ascii());
                }
                _ => unreachable!("已在入口处理的 label 终态"),
            }
        }
    }
}

fn scan_link_label_rest_table<'t>(
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
        match label_lexeme(
            *bytes.get(ix)?,
            ix.checked_sub(1).map(|at| bytes[at]),
            bytes.get(ix + 1).copied(),
            true,
            is_in_table,
        ) {
            LabelLexeme::Reject => return None,
            LabelLexeme::Close => break,
            // Backslash escapes in link references are normally untouched, but
            // tables are an exception, because they're parsed as-if the tables
            // were parsed in a discrete pass, changing `\|` to `|`, and then
            // passing the changed string to the inline parser.
            LabelLexeme::TablePipe => {
                // only way to reach this spot is to have `\\|` (even number of `\` before `|`)
                label.push_str(&text[mark..ix - 1]);
                label.push('|');
                ix += 1;
                only_white_space = false;
                mark = ix;
            }
            LabelLexeme::EscapedTablePipe => {
                // only way to reach this spot is to have `\|` (odd number of `\` before `|`)
                label.push_str(&text[mark..ix]);
                label.push('|');
                ix += 2;
                codepoints += 1;
                only_white_space = false;
                mark = ix;
            }
            LabelLexeme::EscapedPunctuation => {
                ix += 2;
                codepoints += 2;
                only_white_space = false;
            }
            LabelLexeme::Whitespace => {
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
            LabelLexeme::Ordinary(b) => {
                only_white_space = false;
                ix += 1;
                if b & 0b1000_0000 != 0 {
                    codepoints += 1;
                }
            }
            LabelLexeme::NeedLookahead => unreachable!("原whole-slice label scanner有真实EOF权限"),
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

#[cfg(test)]
mod test {
    use super::scan_link_label_rest;

    #[test]
    fn whitespace_normalization() {
        let input = "«\t\tBlurry Eyes\t\t»][blurry_eyes]";
        let expected_output = "« Blurry Eyes »"; // regular spaces!

        let (_bytes, normalized_label) = scan_link_label_rest(input, &|_| None, false).unwrap();
        assert_eq!(expected_output, normalized_label.as_ref());
    }

    #[test]
    fn return_carriage_linefeed_ok() {
        let input = "hello\r\nworld\r\n]";
        assert!(scan_link_label_rest(input, &|_| Some(0), false).is_some());
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::{scan_link_label_rest, LabelScanner, LabelStep};
    use crate::{Event, Parser, Tag};

    #[test]
    /// 规范化结果用固定字面和真实 Parser 引用解析双重验证；每个 byte 切分也不能丢反斜杠或 UTF8。
    /// 空白片只暂存“需要一个空格”的事实，测试不构造替代查询表或存储端口。
    fn reference_labels_keep_original_values_at_every_small_frame() {
        for (source, expected) in [
            ("  中文\t\t😀  ]", "中文 😀"),
            ("a\r\n b]", "a b"),
            (r"a\]b]", r"a\]b"),
            ("a&amp;b]", "a&amp;b"),
            ("前\t\r\n 后]", "前 后"),
        ] {
            let (consumed, normalized) = scan_link_label_rest(source, &|_| Some(0), false).unwrap();
            assert_eq!(consumed, source.len());
            assert_eq!(normalized.as_ref(), expected);
            for width in [1, 2, 7, 1024] {
                let mut scanner = LabelScanner::new();
                let mut actual = Vec::new();
                loop {
                    let start = scanner.offset;
                    let end = (start + width).min(source.len());
                    let mut output = [0u8; 8];
                    let (_, written, step) = scanner.feed(
                        &source.as_bytes()[start..end],
                        end == source.len(),
                        &mut output,
                    );
                    actual.extend_from_slice(&output[..written]);
                    match step {
                        LabelStep::Complete => break,
                        LabelStep::NeedLine => {
                            scanner.continue_line(0).unwrap();
                        }
                        LabelStep::Rejected => {
                            panic!("合法原 reference label 不应因 frame 切分失效")
                        }
                        _ => {}
                    }
                }
                assert_eq!(String::from_utf8(actual).unwrap(), expected);
            }
            // 真正完整 Parser 仍能用同一原 key 查到定义；decoder 的局部结果不是自签链接目标。
            let document = format!("[文本][{source}\n\n[{expected}]: /fixed-target");
            let targets: Vec<_> = Parser::new(&document)
                .filter_map(|event| match event {
                    Event::Start(Tag::Link { dest_url, .. }) => Some(dest_url.into_string()),
                    _ => None,
                })
                .collect();
            assert_eq!(targets, ["/fixed-target"]);
        }
    }

    #[test]
    /// 原 codepoint 上限没有改成新长度门：普通 ASCII 与非 ASCII 计数仍保持历史 Parser 规则。
    /// 真正无效括号/空标签/多 EOL 不得到引用请求，不能把它们丢给 Global 伪 Missing。
    fn reference_label_limits_and_rejections_keep_original_parser_rules() {
        let ascii = format!("{}]", "x".repeat(5000));
        let unicode_ok = format!("{}]", "é".repeat(499));
        let unicode_rejected = format!("{}]", "é".repeat(500));
        assert!(scan_link_label_rest(&ascii, &|_| Some(0), false).is_some());
        assert!(scan_link_label_rest(&unicode_ok, &|_| Some(0), false).is_some());
        assert!(scan_link_label_rest(&unicode_rejected, &|_| Some(0), false).is_none());
        for source in ["]", " \t]", "a[b]", "a\n\nb]", "a\\"] {
            assert!(scan_link_label_rest(source, &|_| Some(0), false).is_none());
        }
    }
}
