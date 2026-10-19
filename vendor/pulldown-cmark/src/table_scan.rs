// Copyright 2015 Google Inc. All rights reserved.
// 原始escape分支提取自pulldown-cmark0.13.4 firstpass.rs；MIT条款见LICENSE。
//! AST lexer与有限table frontier共享的原始pipe/escape边界。
#[cfg(feature = "omni-block-frontier")]
use crate::scanners::is_ascii_whitespace_no_nl;
use crate::scanners::{
    is_ascii_punctuation, scan_blank_line, scan_ch_repeat, scan_whitespace_no_nl,
};
#[cfg(feature = "omni-block-frontier")]
use std::ops::Range;

#[cfg(feature = "omni-block-frontier")]
#[derive(Clone, Debug, Eq, PartialEq)]
/// 同一GFM行词法产生的真实格端点；只表达字节，不授予成员、样式或编辑权限。
pub struct TableCellBoundary {
    pub raw: Range<usize>,
    pub trimmed: Range<usize>,
}

#[cfg(feature = "omni-block-frontier")]
#[derive(Debug)]
/// 原row_inner的有限续体；不保正文，也不以backtick或反斜线奇偶重写原pipe规则。
pub struct TableCellScanner {
    phase: u8,
    previous: Option<u8>,
    next: usize,
    raw_start: usize,
    trimmed_start: usize,
    trimmed_end: usize,
    cells: usize,
    trailing_pipe: bool,
}

#[cfg(feature = "omni-block-frontier")]
impl TableCellScanner {
    /// 起点由调用方的真实container prefix决定；这个纯词法游标不能自行证明表格身份。
    pub fn new(start: usize) -> Self {
        Self {
            phase: 0,
            previous: None,
            next: start,
            raw_start: start,
            trimmed_start: start,
            trimmed_end: start,
            cells: 0,
            trailing_pipe: false,
        }
    }

    pub fn cell_count(&self) -> usize {
        self.cells
    }

    /// 当前trim端点供同一次源扫描同步计量UTF16；端点本身不要求回读已扫过的正文。
    pub fn trimmed_range(&self) -> Range<usize> {
        self.trimmed_start..self.trimmed_end.max(self.trimmed_start)
    }

    /// 原row_inner先消费一条leading pipe，再跳ASCII横向空白，才创建真实格。
    /// 一个pipe可结束前格并成为下格leading pipe；连续pipe因而保留显式空格。
    pub fn push(&mut self, byte: u8) -> Option<TableCellBoundary> {
        let at = self.next;
        self.next += 1;
        let mut boundary = None;
        for _ in 0..3 {
            match self.phase {
                0 => {
                    self.phase = 1;
                    if byte == b'|' {
                        self.raw_start = at + 1;
                        self.trimmed_start = at + 1;
                        self.trimmed_end = at + 1;
                        self.trailing_pipe = true;
                        break;
                    }
                }
                1 => {
                    if is_ascii_whitespace_no_nl(byte) {
                        if matches!(byte, b' ' | b'\t') && self.trimmed_start == at {
                            self.trimmed_start = at + 1;
                        } else if !matches!(byte, b' ' | b'\t') {
                            // 原Parser跳过VT/FF以寻找cell，但presentation只trim空格/tab，仍须保留它们。
                            self.trimmed_end = at + 1;
                        }
                        break; // 仅padding而无正文的尾部不创建新的真实格。
                    }
                    self.cells += 1;
                    self.phase = 2;
                    // presentation只trim空格/tab，VT/FF仍属原Parser给出的cell范围。
                    self.trimmed_end = self.trimmed_end.max(self.trimmed_start);
                }
                _ => {
                    if byte == b'|' && self.previous != Some(b'\\') {
                        boundary = Some(TableCellBoundary {
                            raw: self.raw_start..at,
                            trimmed: self.trimmed_start..self.trimmed_end,
                        });
                        self.phase = 0;
                    } else {
                        if !matches!(byte, b' ' | b'\t') {
                            self.trimmed_end = at + 1;
                        }
                        self.trailing_pipe = false;
                        break;
                    }
                }
            }
        }
        self.previous = Some(byte);
        boundary
    }

    /// 只能在原LineReader认证的content_end完成；EOL字节不得喂入cell词法。
    pub fn finish(&mut self) -> Option<TableCellBoundary> {
        if self.phase != 2 {
            return None;
        }
        self.phase = 1;
        Some(TableCellBoundary {
            raw: self.raw_start..self.next,
            trimmed: self.trimmed_start..self.trimmed_end,
        })
    }

    /// 最后真实格后存在原结构pipe；紧邻反斜线的literal pipe不会在此获权。
    pub fn trailing_pipe(&self) -> bool {
        self.trailing_pipe
    }
}

/// 固定上游整表补格限制；原AST与frontier不得分别维护会漂移的常量。
pub(crate) const MAX_AUTOCOMPLETED_CELLS: usize = 1 << 18;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackslashTransition {
    CodeRun(usize),
    Pipe,
    DoubleBackslashPipe,
    Punctuation,
    Literal,
}

/// 只提取原有分支判定；文本节点的构造仍留在firstpass，frontier不创建inline节点。
pub(crate) fn backslash_transition(
    bytes: &[u8],
    ix: usize,
    active_table: bool,
) -> BackslashTransition {
    if ix + 1 >= bytes.len() || !is_ascii_punctuation(bytes[ix + 1]) {
        // 普通反斜杠没有可消费的转义字节；不能把下一真实pipe吞掉。
        return BackslashTransition::Literal;
    }
    if bytes[ix + 1] == b'`' {
        // 原lexer把转义后的整个连续backtick run作为一个MaybeCode，保持同样跳跃长度。
        BackslashTransition::CodeRun(1 + scan_ch_repeat(&bytes[ix + 2..], b'`'))
    } else if active_table && bytes[ix + 1] == b'|' {
        // GFM table预处理会去掉pipe前的反斜杠，该pipe不作为cell边界。
        BackslashTransition::Pipe
    } else if active_table && bytes.get(ix + 2) == Some(&b'|') && bytes[ix + 1] == b'\\' {
        // 原实现对双反斜杠+pipe单独处理；不能以常见奇偶猜测替代这一合同。
        BackslashTransition::DoubleBackslashPipe
    } else {
        BackslashTransition::Punctuation
    }
}

/// 原lexer的pipe分支也排除紧邻反斜杠的pipe；两个消费者共享同一判断。
pub(crate) fn is_table_pipe(bytes: &[u8], ix: usize) -> bool {
    bytes[ix] == b'|' && (ix == 0 || bytes[ix - 1] != b'\\')
}

/// Computes the number of header columns in a table line by computing the number of dividing pipes
/// that aren't followed or preceded by whitespace.
pub(crate) fn count_header_cols(
    bytes: &[u8],
    mut pipes: usize,
    mut start: usize,
    last_pipe_ix: usize,
) -> usize {
    // was first pipe preceded by whitespace? if so, subtract one
    start += scan_whitespace_no_nl(&bytes[start..]);
    if bytes[start] == b'|' {
        pipes -= 1;
    }

    // was last pipe followed by whitespace? if so, sub one
    if scan_blank_line(&bytes[(last_pipe_ix + 1)..]).is_some() {
        pipes
    } else {
        pipes + 1
    }
}

#[cfg(all(test, feature = "omni-block-frontier"))]
mod cell_tests {
    use super::*;
    use crate::{Event, Options, Parser, Tag};

    #[test]
    /// 原Parser只作有限oracle；同一流式词法必须保留空格、转义pipe和超列原始端点。
    fn streamed_cell_boundaries_match_fixed_parser_raw_ranges() {
        for row in [
            "| a | 中😀 |",
            "|a||",
            "a\\|b",
            "a\\\\|",
            "`a|b`",
            "a",
            "|a|  ",
            "| a\u{b} | b\u{c} |",
        ] {
            let header = "| A | B | C | D |\n| - | - | - | - |\n";
            let source = format!("{header}{row}\n");
            let oracle = Parser::new_ext(&source, Options::ENABLE_TABLES)
                .into_offset_iter()
                .filter_map(|(event, range)| {
                    matches!(event, Event::Start(Tag::TableCell)).then_some(range)
                })
                .skip(4)
                .collect::<Vec<_>>();
            let mut scanner = TableCellScanner::new(header.len());
            let mut cells = Vec::new();
            for byte in row.bytes() {
                if let Some(cell) = scanner.push(byte) {
                    cells.push(cell);
                }
            }
            if let Some(cell) = scanner.finish() {
                cells.push(cell);
            }
            for (cell, expected) in cells.iter().take(4).zip(&oracle) {
                assert_eq!(&cell.raw, expected, "{row:?}");
                let trimmed = source[cell.raw.clone()].trim_matches([' ', '\t']);
                assert_eq!(&source[cell.trimmed.clone()], trimmed, "{row:?}");
            }
            assert_eq!(scanner.cell_count(), cells.len());
        }
    }
}
