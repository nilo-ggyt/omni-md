//! 逐标量收集一行的固定语法事实；巨物理行也不保留正文或重新扫描前缀。

use super::*;

#[derive(Default)]
pub(super) struct MarkdownBlockLineFacts {
    blank: bool,
    first: Option<char>,
    last: Option<char>,
    trimmed: Token,
    front: Token,
    first_scalar: bool,
    spaces: usize,
    indent_done: bool,
    fence_marker: Option<u8>,
    fence_length: usize,
    fence_run_done: bool,
    fence_tail_horizontal: bool,
    fence_tail_backtick: bool,
    horizontal_indent: usize,
    list_state: u8,
    list_marker: bool,
    escape: bool,
    ticks: usize,
    code_ticks: usize,
    pipes: usize,
    delimiter: Delimiter,
}

#[derive(Default)]
struct Token {
    length: usize,
    bytes: [char; 4],
    pending_space: bool,
}

impl Token {
    fn push(&mut self, character: char) {
        if character.is_whitespace() {
            // 开头与结尾trim都不保留；中间空白只需一个拒绝标志，不复制任意长空白串。
            if self.length > 0 {
                self.pending_space = true;
            }
            return;
        }
        if self.pending_space {
            self.length = 5;
        }
        if self.length < 4 {
            self.bytes[self.length] = character;
        }
        self.length = self.length.saturating_add(1).min(5);
    }
    fn is(&self, value: char) -> bool {
        self.length == 3 && self.bytes[..3].iter().all(|character| *character == value)
    }
}

#[derive(Default)]
struct Delimiter {
    started: bool,
    leading_pipe: bool,
    pending_space: bool,
    pending_pipes: usize,
    columns: usize,
    hyphens: usize,
    cell_stage: u8,
    failed: bool,
}

impl Delimiter {
    fn push(&mut self, character: char) {
        if self.failed {
            return;
        }
        if character.is_whitespace() {
            // 外层trim的尾空白直到后续非空白出现才成为真实cell内容。
            if self.started || self.leading_pipe {
                self.pending_space = true;
            }
            return;
        }
        if self.pending_space {
            self.raw(' ');
            self.pending_space = false;
        }
        self.raw(character);
    }
    fn raw(&mut self, character: char) {
        if character == '|' {
            if self.started {
                self.pending_pipes += 1;
            } else {
                // 首pipe后空白已经属于trim_matches之后的cell；它会阻止后继pipe继续冒充连续首pipe。
                self.leading_pipe = true;
            }
            // trim_matches('|')允许任意多个真实首尾pipe；它们不是空的首尾cell。
            return;
        }
        if self.pending_pipes > 0 {
            if self.pending_pipes != 1 || !self.valid_cell() {
                self.failed = true;
                return;
            }
            self.columns += 1;
            self.hyphens = 0;
            self.cell_stage = 0;
            self.pending_pipes = 0;
        }
        self.started = true;
        match character {
            ' ' => {
                if self.cell_stage > 0 {
                    self.cell_stage = 4;
                }
            }
            ':' if self.cell_stage == 0 => self.cell_stage = 1,
            '-' if self.cell_stage <= 2 => {
                self.hyphens += 1;
                self.cell_stage = 2;
            }
            ':' if self.cell_stage == 2 => self.cell_stage = 3,
            _ => self.failed = true,
        }
    }
    fn valid_cell(&self) -> bool {
        self.hyphens >= 3 && matches!(self.cell_stage, 2..=4)
    }
    fn columns(&self) -> Option<usize> {
        (!self.failed && self.started && self.valid_cell()).then_some(self.columns + 1)
    }
}

impl MarkdownBlockLineFacts {
    pub(super) fn new() -> Self {
        Self {
            blank: true,
            first_scalar: true,
            fence_tail_horizontal: true,
            ..Self::default()
        }
    }
    pub(super) fn from_text(text: &str) -> Self {
        let mut facts = Self::new();
        for character in text.chars() {
            facts.push(character);
        }
        facts.finish();
        facts
    }
    pub(super) fn push(&mut self, character: char) {
        self.blank &= matches!(character, ' ' | '\t');
        self.trimmed.push(character);
        if !(self.first_scalar && character == '\u{feff}') {
            self.front.push(character);
        }
        self.first_scalar = false;
        self.delimiter.push(character);
        if !character.is_whitespace() {
            if self.first.is_none() {
                self.first = Some(character);
            }
            self.last = Some(character);
        }
        if !self.indent_done && character == ' ' {
            self.spaces += 1;
        } else if !self.indent_done {
            self.indent_done = true;
            if self.spaces <= 3 && matches!(character, '`' | '~') {
                self.fence_marker = Some(character as u8);
                self.fence_length = 1;
            }
        } else if let Some(marker) = self.fence_marker {
            if !self.fence_run_done && character as u32 == u32::from(marker) {
                self.fence_length += 1;
            } else {
                self.fence_run_done = true;
                self.fence_tail_horizontal &= matches!(character, ' ' | '\t');
                self.fence_tail_backtick |= character == '`';
            }
        }
        if self.list_state == 0 && matches!(character, ' ' | '\t') {
            self.horizontal_indent += 1;
        } else {
            match self.list_state {
                0 if matches!(character, '-' | '+' | '*') => self.list_state = 1,
                0 if character.is_ascii_digit() => {
                    self.list_state = 2;
                }
                1 => {
                    self.list_marker = character.is_ascii_whitespace();
                    self.list_state = 4;
                }
                2 if character.is_ascii_digit() => {}
                2 if matches!(character, '.' | ')') => self.list_state = 3,
                3 => {
                    self.list_marker = character.is_ascii_whitespace();
                    self.list_state = 4;
                }
                _ => self.list_state = 4,
            }
        }
        // 逃逸与backtick状态对应旧逐byte算法；非ASCII续字节不可能是pipe、反斜线或backtick。
        if self.escape {
            self.escape = false;
            return;
        }
        if character == '`' {
            self.ticks += 1;
            return;
        }
        self.flush_ticks();
        if character == '\\' {
            self.escape = true;
        } else if character == '|' && self.code_ticks == 0 {
            self.pipes += 1;
        }
    }
    fn flush_ticks(&mut self) {
        if self.ticks > 0 {
            if self.code_ticks == 0 {
                self.code_ticks = self.ticks;
            } else if self.code_ticks == self.ticks {
                self.code_ticks = 0;
            }
            self.ticks = 0;
        }
    }
    fn finish(&mut self) {
        self.flush_ticks();
    }
    pub(super) fn opening_fence(&self) -> Option<(u8, usize)> {
        let marker = self.fence_marker?;
        (self.fence_length >= 3 && !(marker == b'`' && self.fence_tail_backtick))
            .then_some((marker, self.fence_length))
    }
    pub(super) fn closes_fence(&self, marker: u8, minimum: usize) -> bool {
        self.fence_marker == Some(marker)
            && self.fence_length >= minimum
            && self.fence_tail_horizontal
    }
    pub(super) fn blank(&self) -> bool {
        self.blank
    }
    pub(super) fn opens_front_matter(&self) -> bool {
        self.front.is('-')
    }
    pub(super) fn closes_front_matter(&self) -> bool {
        self.trimmed.is('-') || self.trimmed.is('.')
    }
    pub(super) fn header_columns(&self) -> Option<usize> {
        (self.pipes > 0).then_some(self.pipes.saturating_add(1).saturating_sub(
            usize::from(self.first == Some('|')) + usize::from(self.last == Some('|')),
        ))
    }
    pub(super) fn delimiter_columns(&self) -> Option<usize> {
        self.delimiter.columns()
    }
    pub(super) fn table_body(&self) -> bool {
        !self.blank && self.pipes > 0
    }
    pub(super) fn list_marker(&self) -> bool {
        self.list_marker
    }
    pub(super) fn list_continuation(&self) -> bool {
        self.list_marker || self.horizontal_indent >= 2
    }
}

/// 初开只保存当前行的固定事实与原checkpoint；CRLF跨4KiB借用仍是同一完整行。
pub(in crate::markdown_document_buffer) struct InitialBlockScanner {
    state: MarkdownBlockScannerState,
    facts: MarkdownBlockLineFacts,
    line_start_byte: usize,
    line_start_utf16: usize,
    byte_offset: usize,
    utf16_offset: usize,
    pending_cr: bool,
    pending_checkpoint: Option<MarkdownBlockCheckpoint>,
    checkpoints: Vec<MarkdownBlockCheckpoint>,
    last_checkpoint_utf16: usize,
}

impl InitialBlockScanner {
    pub(in crate::markdown_document_buffer) fn new() -> Self {
        let state = MarkdownBlockScannerState::document_start();
        Self {
            state,
            facts: MarkdownBlockLineFacts::new(),
            line_start_byte: 0,
            line_start_utf16: 0,
            byte_offset: 0,
            utf16_offset: 0,
            pending_cr: false,
            pending_checkpoint: None,
            checkpoints: vec![MarkdownBlockCheckpoint {
                utf16_offset: 0,
                byte_offset: 0,
                state,
            }],
            last_checkpoint_utf16: 0,
        }
    }
    pub(in crate::markdown_document_buffer) fn push(&mut self, text: &str) {
        for character in text.chars() {
            if self.pending_cr {
                self.pending_cr = false;
                if character == '\n' {
                    // 前一块末尾CR与此LF合为同一行，LF不进入语法content。
                    self.byte_offset += 1;
                    self.utf16_offset += 1;
                    self.end_line();
                    continue;
                }
                self.end_line();
            }
            if let Some(checkpoint) = self.pending_checkpoint.take() {
                // 只有真实后继字符出现才保留checkpoint，EOF不能多出旧实现没有的尾checkpoint。
                self.last_checkpoint_utf16 = checkpoint.utf16_offset;
                self.checkpoints.push(checkpoint);
            }
            self.byte_offset += character.len_utf8();
            self.utf16_offset += character.len_utf16();
            match character {
                '\r' => self.pending_cr = true,
                '\n' => self.end_line(),
                _ => self.facts.push(character),
            }
        }
    }
    fn end_line(&mut self) {
        self.facts.finish();
        let mut collection = MarkdownCompletedBlockCollection::new(false);
        self.state.consume_line_facts(
            self.line_start_utf16..self.utf16_offset,
            self.line_start_byte..self.byte_offset,
            &self.facts,
            &mut collection,
        );
        self.facts = MarkdownBlockLineFacts::new();
        self.line_start_byte = self.byte_offset;
        self.line_start_utf16 = self.utf16_offset;
        if self.utf16_offset.saturating_sub(self.last_checkpoint_utf16)
            >= MARKDOWN_BLOCK_CHECKPOINT_STRIDE_UTF16_UNITS
        {
            self.pending_checkpoint = Some(MarkdownBlockCheckpoint {
                utf16_offset: self.utf16_offset,
                byte_offset: self.byte_offset,
                state: self.state,
            });
        }
    }
    pub(in crate::markdown_document_buffer) fn finish(mut self) -> MarkdownBlockCheckpointIndex {
        if self.pending_cr || self.line_start_byte < self.byte_offset {
            // EOF只关闭真实最后一行；没有额外空行，不把任意块末尾当EOF。
            self.end_line();
        }
        // 初始index只保存重启状态，EOF完成block没有可观察副作用；最终checkpoint也故意不加入。
        MarkdownBlockCheckpointIndex {
            checkpoints: self.checkpoints,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧完整行识别函数作为独立oracle，不以新FSM输出互相比较冒充语法等价。
    fn compare_line(line: &str) {
        let facts = MarkdownBlockLineFacts::from_text(line);
        assert_eq!(
            facts.blank(),
            markdown_line_is_blank(line),
            "blank {line:?}"
        );
        assert_eq!(
            facts.opens_front_matter(),
            markdown_opens_front_matter(line),
            "front {line:?}"
        );
        assert_eq!(
            facts.closes_front_matter(),
            markdown_closes_front_matter(line),
            "close {line:?}"
        );
        assert_eq!(
            facts.opening_fence(),
            markdown_opening_fence(line),
            "fence {line:?}"
        );
        assert_eq!(
            facts.header_columns(),
            markdown_table_header_column_count(line),
            "header {line:?}"
        );
        assert_eq!(
            facts.delimiter_columns(),
            markdown_table_delimiter_column_count(line),
            "delimiter {line:?}"
        );
        assert_eq!(
            facts.table_body(),
            markdown_table_body_line(line),
            "body {line:?}"
        );
        assert_eq!(
            facts.list_marker(),
            markdown_list_marker_length(line).is_some(),
            "list {line:?}"
        );
        assert_eq!(
            facts.list_continuation(),
            markdown_list_continuation_line(line),
            "continuation {line:?}"
        );
        for marker in *b"`~" {
            for minimum in 1..=6 {
                assert_eq!(
                    facts.closes_fence(marker, minimum),
                    markdown_closes_fence(line, marker, minimum),
                    "close fence {line:?}"
                );
            }
        }
    }

    #[test]
    fn finite_line_facts_match_original_complete_line_rules() {
        for line in [
            "",
            " ",
            "\t",
            "\u{feff}---",
            "\u{feff} --- ",
            " --- ",
            "---",
            "...",
            "\u{85}---\u{2028}",
            "|---|",
            "||---||",
            "| :---: | --- |",
            "--- | |",
            "---| ",
            "| |---",
            "---||---",
            "---:--",
            "--|---",
            "`|`|a",
            "``|`x``|a",
            "\\`|x",
            "\\\\|",
            "~~~/x",
            "```rust`",
            "```rust",
            "    ```",
            "   ```\t",
            "12. x",
            "12)x",
            "-\u{85}",
            "- x",
            "\t\t中",
        ] {
            compare_line(line);
        }
        let alphabet = [
            ' ', '\t', '-', ':', '|', '`', '~', '\\', '.', ')', '+', '*', '1', 'x', '中', '😀',
            '\u{85}', '\u{2028}', '\u{feff}',
        ];
        let mut state = 0x8d8a_71f6_u64;
        for _ in 0..25_000 {
            let mut line = String::new();
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let length = (state >> 32) as usize % 65;
            for _ in 0..length {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                line.push(alphabet[(state >> 32) as usize % alphabet.len()]);
            }
            compare_line(&line);
        }
    }

    #[test]
    fn streaming_checkpoints_match_whole_scan_at_every_scalar_split() {
        let mut source = String::from("\u{feff}---\r\nkey: 中😀\r\n...\n");
        // 真实稀疏阈值前后有front matter、giant行、CRLF、表格和长fence，不能只比较零checkpoint。
        source.push_str(&"中😀`|\\x".repeat(12_000));
        source.push_str("\r\n\n| a | b |\n|---|---|\n|1|2|\r\n\n````rust\n");
        source.push_str(&"opaque|中".repeat(12_000));
        source.push_str("\r\n````\n- x\n  continuation\n\nend\r\n");
        let expected = MarkdownBlockCheckpointIndex::from_text(&source).checkpoints;
        for chunk in [1, 2, 3, 7, 4093, 4096] {
            let mut scanner = InitialBlockScanner::new();
            let mut start = 0;
            while start < source.len() {
                let mut end = (start + chunk).min(source.len());
                while !source.is_char_boundary(end) {
                    end += 1;
                }
                scanner.push(&source[start..end]);
                start = end;
            }
            assert_eq!(scanner.finish().checkpoints, expected, "chunk={chunk}");
        }
    }
}
