// Copyright 2015 Google Inc. All rights reserved.
// 原pipe/换行/分隔行规则来自固定pulldown-cmark0.13.4；完整MIT条款见LICENSE。

//! 物理行只保锚点和结构计数；普通巨段、opaque行和表头都不需要整行Vec。

use crate::scanners::is_ascii_whitespace_no_nl;

#[derive(Debug)]
/// 原分隔行的逐字节续体；列计数和列对齐共用这里的真实 colon/pipe 转换。
/// 调用者必须已由原 LineStart 排除容器前缀和缩进，并在真实 content_end 才结束。
pub struct TableDelimiterScanner {
    // 无效是本行终态；后继字节不能把已经失败的分隔语法重新拼成表格。
    valid: bool,
    initial: bool,
    start_col: bool,
    found_pipe: bool,
    found_hyphen: bool,
    col_hyphen: bool,
    columns: usize,
    // 仅保当前列，已关闭列由真实消费者立即取走；不会按巨行长度分配数组。
    alignment: crate::Alignment,
}

impl TableDelimiterScanner {
    pub fn new() -> Self {
        // 默认方向只表示原 Parser 的无 colon 列；不是未知输入已经获得 Left 权限。
        Self {
            valid: true,
            initial: true,
            start_col: true,
            found_pipe: false,
            found_hyphen: false,
            col_hyphen: false,
            columns: 0,
            alignment: crate::Alignment::None,
        }
    }

    /// 返回本次真实 pipe 关闭的列；未到真实末尾时它不授整行有效或列数完整权限。
    pub fn push(&mut self, byte: u8) -> Option<(usize, crate::Alignment)> {
        if !self.valid {
            // 原物理行仍可继续被 LineReader 消费，分隔语义不再产生任何候选。
            return None;
        }
        if self.initial {
            self.initial = false;
            if byte == b'|' {
                // 唯一可选 leading pipe 不关闭空列；之后每条 pipe 才结束一个真实列。
                self.found_pipe = true;
                return None;
            }
        }
        match byte {
            b' ' => (), // 原语法仅略过 ASCII space；tab 等输入仍按原 scan_table_head 拒绝。
            b':' => {
                // 保原 scan_table_head 的非常规连续 colon 规则，不另用首尾字符猜方向。
                self.alignment = match (self.start_col, self.alignment) {
                    (true, crate::Alignment::None) => crate::Alignment::Left,
                    (false, crate::Alignment::Left) => crate::Alignment::Center,
                    (false, crate::Alignment::None) => crate::Alignment::Right,
                    _ => self.alignment,
                };
                self.start_col = false;
            }
            b'-' => {
                // 一列至少有一个 hyphen 才能被后继 pipe 关闭，整行也至少需一个 hyphen。
                self.start_col = false;
                self.found_hyphen = true;
                self.col_hyphen = true;
            }
            b'|' => {
                self.found_pipe = true;
                let closed = (self.columns, self.alignment);
                // 先交旧列再重置新列，不能让新列起始 colon 继承上一列的 Left/Center。
                self.columns += 1;
                self.start_col = true;
                self.alignment = crate::Alignment::None;
                if !self.col_hyphen {
                    self.valid = false;
                }
                self.col_hyphen = false;
                return Some(closed);
            }
            _ => self.valid = false,
        }
        None
    }

    /// 只观察真实行末，不改变游标；末列可以没有尾 pipe，但仍须整行有效才准入。
    pub fn final_column(&self) -> Option<(usize, crate::Alignment)> {
        (!self.start_col).then_some((self.columns, self.alignment))
    }

    pub fn columns(&self) -> Option<usize> {
        // 真正 EOF 或 content_end 才可据此授整行列数；调用者不能把分片边缘当结束。
        (self.valid && self.found_pipe && self.found_hyphen)
            .then_some(self.columns + usize::from(!self.start_col))
    }
}

impl Default for TableDelimiterScanner {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LineFacts {
    pub start: usize,
    pub content_end: usize,
    pub end: usize,
    pub header_columns: usize,
    pub starts_pipe: bool,
    pub hard_break: bool,
    pub setext: bool,
    pub setext_level: Option<crate::HeadingLevel>,
    pub delimiter_columns: Option<usize>,
    pub metadata_marker: Option<u8>,
    pub actual_cells: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum LineReadStep {
    Progress,
    NeedInput,
    Complete(LineFacts),
}

/// delimiter起点必须已由真实LineStart排除四格缩进；raw_lf仅用于原opaque parse_nextline路径。
pub(crate) struct LineReader {
    start: usize,
    next: usize,
    source_end: usize,
    raw_lf: bool,
    cr: bool,
    content_end: Option<usize>,
    first_content: Option<u8>,
    previous: Option<u8>,
    pipes: usize,
    last_pipe_blank: bool,
    backslashes: usize,
    setext_marker: Option<u8>,
    setext_spaces: bool,
    setext_valid: bool,
    delimiter: TableDelimiterScanner,
    done: Option<LineFacts>,
    metadata_marker: Option<u8>,
    metadata_count: usize,
    metadata_space: bool,
    metadata_valid: bool,
    cells: crate::table_scan::TableCellScanner,
}

impl LineReader {
    pub(crate) fn new(start: usize, source_end: usize, raw_lf: bool) -> Self {
        Self {
            start,
            next: start,
            source_end,
            raw_lf,
            cr: false,
            content_end: None,
            first_content: None,
            previous: None,
            pipes: 0,
            last_pipe_blank: true,
            backslashes: 0,
            setext_marker: None,
            setext_spaces: false,
            setext_valid: true,
            delimiter: TableDelimiterScanner::new(),
            done: None,
            metadata_marker: None,
            metadata_count: 0,
            metadata_space: false,
            metadata_valid: true,
            cells: crate::table_scan::TableCellScanner::new(start),
        }
    }
    pub(crate) fn next_byte(&self) -> usize {
        self.next
    }

    /// 每byte一次固定字段更新；CRLF确认只查看一个后继，frame末绝不提前Complete。
    pub(crate) fn advance(
        &mut self,
        bytes: &[u8],
        real_eof: bool,
        budget: usize,
    ) -> (LineReadStep, usize, usize) {
        let mut used = 0;
        if let Some(facts) = self.done {
            return (LineReadStep::Complete(facts), 0, 0);
        }
        while used < bytes.len().min(budget) {
            let byte = bytes[used];
            if self.cr {
                if byte == b'\n' {
                    self.next += 1;
                    used += 1;
                }
                self.complete();
                return (
                    LineReadStep::Complete(self.done.unwrap()),
                    used,
                    used + usize::from(byte != b'\n'),
                );
            }
            self.next += 1;
            used += 1;
            if byte == b'\n' {
                self.content_end = Some(self.next - 1);
                self.complete();
                return (LineReadStep::Complete(self.done.unwrap()), used, used);
            }
            if byte == b'\r' && !self.raw_lf {
                self.content_end = Some(self.next - 1);
                self.cr = true;
                continue;
            }
            self.observe(byte);
        }
        if used == bytes.len() && real_eof {
            self.complete();
            return (
                LineReadStep::Complete(self.done.unwrap()),
                used,
                used.max(1),
            );
        }
        let step = if used == bytes.len() {
            LineReadStep::NeedInput
        } else {
            LineReadStep::Progress
        };
        (step, used, used)
    }

    fn observe(&mut self, byte: u8) {
        // 成员计数与后续原格编辑共用同一GFM词法续体，不能维护另一套pipe/补格规则。
        let _ = self.cells.push(byte);
        if self.metadata_marker.is_none() {
            if matches!(byte, b'-' | b'+') {
                self.metadata_marker = Some(byte);
            } else {
                self.metadata_valid = false;
            }
        }
        if self.metadata_valid {
            if !self.metadata_space && Some(byte) == self.metadata_marker {
                self.metadata_count += 1;
            } else {
                self.metadata_space = true;
                if !byte.is_ascii_whitespace() {
                    self.metadata_valid = false;
                }
            }
        }
        if self.first_content.is_none() && !is_ascii_whitespace_no_nl(byte) {
            self.first_content = Some(byte);
        }
        // 原lexer的真实pipe分支排除紧邻反斜杠；转义其它punctuation不会产生额外pipe。
        if byte == b'|' && self.previous != Some(b'\\') {
            self.pipes += 1;
            self.last_pipe_blank = true;
        } else if !is_ascii_whitespace_no_nl(byte) {
            self.last_pipe_blank = false;
        }
        self.backslashes = if byte == b'\\' {
            self.backslashes + 1
        } else {
            0
        };
        self.previous = Some(byte);
        match self.setext_marker {
            None => {
                if matches!(byte, b'=' | b'-') {
                    self.setext_marker = Some(byte);
                } else {
                    self.setext_valid = false;
                }
            }
            Some(marker) => {
                if is_ascii_whitespace_no_nl(byte) {
                    self.setext_spaces = true;
                } else if byte != marker || self.setext_spaces {
                    self.setext_valid = false;
                }
            }
        }
        let _ = self.delimiter.push(byte);
    }

    fn complete(&mut self) {
        let first_pipe = self.first_content == Some(b'|');
        let columns =
            self.pipes.saturating_sub(usize::from(first_pipe)) + usize::from(!self.last_pipe_blank);
        let delimiter_columns = self.delimiter.columns();
        self.done = Some(LineFacts {
            start: self.start,
            content_end: self.content_end.unwrap_or(self.next),
            end: self.next,
            header_columns: if self.pipes > 0 { columns } else { 0 },
            starts_pipe: first_pipe,
            hard_break: self.backslashes % 2 != 0 && self.next < self.source_end,
            setext: self.setext_valid && self.setext_marker.is_some(),
            // 只投影已经由同一原扫描状态判定成功的marker，不另行识别Setext语法。
            setext_level: if self.setext_valid {
                match self.setext_marker {
                    Some(b'=') => Some(crate::HeadingLevel::H1),
                    Some(b'-') => Some(crate::HeadingLevel::H2),
                    _ => None,
                }
            } else {
                None
            },
            delimiter_columns,
            metadata_marker: if self.metadata_valid && self.metadata_count == 3 {
                self.metadata_marker
            } else {
                None
            },
            actual_cells: self.cells.cell_count(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanners::{scan_eol, scan_table_head};

    #[test]
    fn fragmented_delimiters_match_original_unusual_colon_and_eol_rules() {
        for source in [
            "|-|",
            "-|-",
            " :-- :|--:: |\r\nrest",
            "---|:",
            "--||--",
            "|---|---|",
            "---|---\t",
            "---",
            "|--| :",
            "|-|\rX",
        ] {
            // delimiter的indent由上游LineStart负责，此测试只比较其后真实语法。
            let indent = source.bytes().take_while(|byte| *byte == b' ').count();
            let source = &source[indent..];
            let (count, align) = scan_table_head(source.as_bytes());
            let expected = (count != 0).then_some(align.len());
            for width in 1..=source.len().max(1) {
                let mut scanner = LineReader::new(0, source.len(), false);
                loop {
                    let start = scanner.next_byte();
                    let end = (start + width).min(source.len());
                    let (step, used, work) =
                        scanner.advance(&source.as_bytes()[start..end], end == source.len(), 1);
                    assert!(used <= 1 && work <= 1);
                    if let LineReadStep::Complete(facts) = step {
                        assert_eq!(facts.delimiter_columns, expected, "{source:?}");
                        break;
                    }
                }
            }
        }
    }

    #[test]
    fn giant_lines_have_only_scalar_storage_and_exact_crlf_anchors() {
        let source = format!("{}😀|x\\|z|\r\nrest", "a".repeat(1024 * 1024));
        let mut scanner = LineReader::new(0, source.len(), false);
        let facts = loop {
            let start = scanner.next_byte();
            let end = (start + 8191).min(source.len());
            let (step, used, work) =
                scanner.advance(&source.as_bytes()[start..end], end == source.len(), 4096);
            assert!(used <= 4096 && work <= 4096);
            if let LineReadStep::Complete(facts) = step {
                break facts;
            }
        };
        assert_eq!(facts.header_columns, 2);
        assert_eq!(facts.end, source.find("rest").unwrap());
        assert_eq!(scan_eol(&source.as_bytes()[facts.content_end..]), Some(2));
        assert!(std::mem::size_of::<LineReader>() <= 256);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PositionError {
    Utf8,
    Overflow,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SourcePosition {
    pub(crate) byte: usize,
    pub(crate) utf16: usize,
    pub(crate) line: usize,
    pub(crate) previous_cr: bool,
    pub(crate) utf8_left: u8,
    pub(crate) utf8_min: u8,
    pub(crate) utf8_max: u8,
}
impl SourcePosition {
    pub(crate) fn feed(&mut self, byte: u8) -> Result<(), PositionError> {
        if self.utf8_left != 0 {
            if byte < self.utf8_min || byte > self.utf8_max {
                return Err(PositionError::Utf8);
            }
            self.utf8_left -= 1;
            self.utf8_min = 0x80;
            self.utf8_max = 0xbf;
        } else {
            let (left, min, max, units) = match byte {
                0..=0x7f => (0, 0x80, 0xbf, 1),
                0xc2..=0xdf => (1, 0x80, 0xbf, 1),
                0xe0 => (2, 0xa0, 0xbf, 1),
                0xe1..=0xec | 0xee..=0xef => (2, 0x80, 0xbf, 1),
                0xed => (2, 0x80, 0x9f, 1),
                0xf0 => (3, 0x90, 0xbf, 2),
                0xf1..=0xf3 => (3, 0x80, 0xbf, 2),
                0xf4 => (3, 0x80, 0x8f, 2),
                _ => return Err(PositionError::Utf8),
            };
            self.utf8_left = left;
            self.utf8_min = min;
            self.utf8_max = max;
            self.utf16 = self
                .utf16
                .checked_add(units)
                .ok_or(PositionError::Overflow)?;
        }
        if byte == b'\r' || (byte == b'\n' && !self.previous_cr) {
            self.line += 1;
        }
        self.previous_cr = byte == b'\r';
        self.byte += 1;
        Ok(())
    }
}
