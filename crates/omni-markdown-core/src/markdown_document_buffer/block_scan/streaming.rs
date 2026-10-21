//! Accumulate fixed line facts scalar by scalar, without retaining huge physical lines or rescanning prefixes.

use super::*;

#[derive(Default)]
pub(super) struct MarkdownBlockLineFacts {
    // Each field is a finite grammar fact or run length for the current line.
    // Source bytes, leases and any surrounding document remain owned by the caller.
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
    // Recognize only short exact delimiter tokens. Length five means the token
    // cannot match; long input never grows a token buffer.
    length: usize,
    bytes: [char; 4],
    pending_space: bool,
}

impl Token {
    fn push(&mut self, character: char) {
        if character.is_whitespace() {
            // Leading/trailing trim whitespace needs no storage. Interior
            // whitespace needs only a rejection flag, not a copied whitespace run.
            if self.length > 0 {
                self.pending_space = true;
            }
            return;
        }
        if self.pending_space {
            // A nonspace after interior whitespace rules out an exact delimiter;
            // the saturated length preserves that negative fact for the rest of the line.
            self.length = 5;
        }
        if self.length < 4 {
            self.bytes[self.length] = character;
        }
        self.length = self.length.saturating_add(1).min(5);
    }
    fn is(&self, value: char) -> bool {
        // Front-matter delimiters must be exactly three copies of one character,
        // not a prefix of a longer token or a token with internal whitespace.
        self.length == 3 && self.bytes[..3].iter().all(|character| *character == value)
    }
}

#[derive(Default)]
struct Delimiter {
    // Keep only the current delimiter cell's grammar and completed column count.
    // Pending pipes/whitespace distinguish actual interior cells from outer trim.
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
            // One malformed cell rejects this row permanently. No later bytes
            // can turn a partial delimiter row into a valid table declaration.
            return;
        }
        if character.is_whitespace() {
            // Trailing outer trim is not cell content until a later nonspace
            // makes that pending whitespace an actual interior separator.
            if self.started || self.leading_pipe {
                self.pending_space = true;
            }
            return;
        }
        if self.pending_space {
            // Cell grammar needs only the existence of interior horizontal space,
            // so one representative space replaces an arbitrary pending run.
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
                // Whitespace after an outer leading pipe belongs to its cell;
                // a later pipe must not be mistaken for another contiguous outer pipe.
                self.leading_pipe = true;
            }
            // Contiguous outer pipes are trimmed, rather than creating empty
            // edge cells. Pending interior pipes are resolved by the next nonpipe.
            return;
        }
        if self.pending_pipes > 0 {
            // An interior boundary needs exactly one pipe and a complete prior
            // cell. Reject empty intermediate cells before starting the next one.
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
        // Cell stages allow surrounding spaces, one leading colon, at least
        // three hyphens and one trailing colon. Anything else invalidates the row.
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
        // A started hyphen run is required; colons and whitespace alone cannot
        // contribute a column even if its exterior resembles a delimiter.
        self.hyphens >= 3 && matches!(self.cell_stage, 2..=4)
    }
    fn columns(&self) -> Option<usize> {
        // Commit the last real cell only after complete line facts are available;
        // unconsumed outer pipes do not create additional columns.
        (!self.failed && self.started && self.valid_cell()).then_some(self.columns + 1)
    }
}

impl MarkdownBlockLineFacts {
    pub(super) fn new() -> Self {
        // Empty line defaults are positive only for blankness and permitted
        // fence-tail whitespace; every actual scalar refines those facts.
        Self {
            blank: true,
            first_scalar: true,
            fence_tail_horizontal: true,
            ..Self::default()
        }
    }
    pub(super) fn from_text(text: &str) -> Self {
        // Bounded slice scanning and initial streaming share this same scalar
        // accumulator, so their block-state transitions consume identical facts.
        let mut facts = Self::new();
        for character in text.chars() {
            facts.push(character);
        }
        facts.finish();
        facts
    }
    pub(super) fn push(&mut self, character: char) {
        // Update independent line facts before pipe/code handling may return.
        // Only the first line scalar may supply the optional BOM fact; the block
        // scanner separately restricts front matter to the actual document start.
        self.blank &= matches!(character, ' ' | '\t');
        self.trimmed.push(character);
        if !(self.first_scalar && character == '\u{feff}') {
            self.front.push(character);
        }
        self.first_scalar = false;
        self.delimiter.push(character);
        if !character.is_whitespace() {
            // The actual trimmed edge characters determine optional outer table
            // pipes; no complete trimmed line is retained.
            if self.first.is_none() {
                self.first = Some(character);
            }
            self.last = Some(character);
        }
        if !self.indent_done && character == ' ' {
            // Fenced code allows at most three leading ASCII spaces. The whole
            // marker run stays a length, including runs larger than one byte can encode.
            self.spaces += 1;
        } else if !self.indent_done {
            self.indent_done = true;
            if self.spaces <= 3 && matches!(character, '`' | '~') {
                self.fence_marker = Some(character as u8);
                self.fence_length = 1;
            }
        } else if let Some(marker) = self.fence_marker {
            // Once the opening marker run ends, its tail separately records
            // closure whitespace and invalid backticks in a backtick info string.
            if !self.fence_run_done && character as u32 == u32::from(marker) {
                self.fence_length += 1;
            } else {
                self.fence_run_done = true;
                self.fence_tail_horizontal &= matches!(character, ' ' | '\t');
                self.fence_tail_backtick |= character == '`';
            }
        }
        if self.list_state == 0 && matches!(character, ' ' | '\t') {
            // List continuation uses the actual horizontal indentation count;
            // the marker FSM begins only when the first nonindent scalar arrives.
            self.horizontal_indent += 1;
        } else {
            // Recognize bullet+whitespace or digits+terminator+whitespace. Once
            // rejected/completed, later line content cannot establish another prefix.
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
        // Escape/backtick facts preserve the existing byte grammar. UTF-8
        // continuation bytes cannot themselves be pipes, backslashes or backticks.
        if self.escape {
            self.escape = false;
            return;
        }
        if character == '`' {
            // Keep a complete backtick run before deciding whether it opens or
            // closes inline code; a different-length run remains code content.
            self.ticks += 1;
            return;
        }
        self.flush_ticks();
        // A pipe is structural only outside an exact matching inline-code run
        // and without a preceding escape; these facts require no retained text.
        if character == '\\' {
            self.escape = true;
        } else if character == '|' && self.code_ticks == 0 {
            self.pipes += 1;
        }
    }
    fn flush_ticks(&mut self) {
        // A completed run opens code or closes only the same delimiter length.
        // Always return the run counter so subsequent runs are independent.
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
        // The final backtick run belongs to this complete line even when no
        // following ordinary scalar arrived to flush it during streaming.
        self.flush_ticks();
    }
    pub(super) fn opening_fence(&self) -> Option<(u8, usize)> {
        // A backtick info string cannot contain another backtick; tilde fences
        // keep their original marker length without that extra restriction.
        let marker = self.fence_marker?;
        (self.fence_length >= 3 && !(marker == b'`' && self.fence_tail_backtick))
            .then_some((marker, self.fence_length))
    }
    pub(super) fn closes_fence(&self, marker: u8, minimum: usize) -> bool {
        // Closing requires the original marker, sufficient full run length and
        // only permitted horizontal tail whitespace, not merely a matching prefix.
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
        // Remove actual outer pipes from structural separator count. Escaped and
        // inline-code pipes were excluded while accumulating the original line.
        (self.pipes > 0).then_some(self.pipes.saturating_add(1).saturating_sub(
            usize::from(self.first == Some('|')) + usize::from(self.last == Some('|')),
        ))
    }
    pub(super) fn delimiter_columns(&self) -> Option<usize> {
        self.delimiter.columns()
    }
    pub(super) fn table_body(&self) -> bool {
        // The conservative block envelope extends only through nonblank lines
        // with an actual structural pipe; detailed cell grammar belongs to parsing.
        !self.blank && self.pipes > 0
    }
    pub(super) fn list_marker(&self) -> bool {
        self.list_marker
    }
    pub(super) fn list_continuation(&self) -> bool {
        // A new marker or sufficient original indentation continues the envelope;
        // no synthetic indentation or copied line prefix is needed.
        self.list_marker || self.horizontal_indent >= 2
    }
}

/// Initial indexing retains fixed current-line facts and sparse checkpoints; cross-chunk CRLF stays one line.
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
        // The document-start checkpoint is always present. This scanner owns
        // coordinates and grammar state, never the caller's source lease or file.
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
        // The caller supplies validated scalar-aligned UTF-8 chunks and owns
        // source health/cancellation. Chunk boundaries do not imply line or source end.
        for character in text.chars() {
            if self.pending_cr {
                self.pending_cr = false;
                if character == '\n' {
                    // Join the previous chunk's CR with this actual LF. The LF
                    // extends the same line range but never enters grammar content.
                    self.byte_offset += 1;
                    self.utf16_offset += 1;
                    self.end_line();
                    continue;
                }
                // A non-LF confirms the pending CR was a standalone line ending;
                // finish it before consuming this scalar as the next line's content.
                self.end_line();
            }
            if let Some(checkpoint) = self.pending_checkpoint.take() {
                // Retain a checkpoint only after an actual successor scalar is
                // observed; true EOF must not create a redundant final checkpoint.
                self.last_checkpoint_utf16 = checkpoint.utf16_offset;
                self.checkpoints.push(checkpoint);
            }
            // Advance byte and UTF-16 coordinates independently. CR waits for the
            // next scalar so its potential LF cannot be split by input chunking.
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
        // Complete grammar facts for this actual logical line, feeding the same
        // state transition as bounded slice scans. Initial indexing retains no block list.
        self.facts.finish();
        let mut collection = MarkdownCompletedBlockCollection::new(false);
        self.state.consume_line_facts(
            self.line_start_utf16..self.utf16_offset,
            self.line_start_byte..self.byte_offset,
            &self.facts,
            &mut collection,
        );
        // Return all prior-line facts before beginning the next physical extent;
        // no previous content or token buffer grows with the number of lines.
        self.facts = MarkdownBlockLineFacts::new();
        self.line_start_byte = self.byte_offset;
        self.line_start_utf16 = self.utf16_offset;
        if self.utf16_offset.saturating_sub(self.last_checkpoint_utf16)
            >= MARKDOWN_BLOCK_CHECKPOINT_STRIDE_UTF16_UNITS
        {
            // Stage a complete-line restart state. Its later real successor,
            // rather than this chunk boundary, decides whether it joins the index.
            self.pending_checkpoint = Some(MarkdownBlockCheckpoint {
                utf16_offset: self.utf16_offset,
                byte_offset: self.byte_offset,
                state: self.state,
            });
        }
    }
    pub(in crate::markdown_document_buffer) fn finish(mut self) -> MarkdownBlockCheckpointIndex {
        if self.pending_cr || self.line_start_byte < self.byte_offset {
            // Only explicit finish closes the actual remaining line or pending
            // CR. Do not add an invented empty line or treat each chunk as EOF.
            self.end_line();
        }
        // Initial indexing retains restart states only. Closing the last semantic
        // block has no retained output here, and the final staged checkpoint is omitted.
        MarkdownBlockCheckpointIndex {
            checkpoints: self.checkpoints,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compare fixed line facts with the existing complete-line recognizers, not another copy of the FSM.
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
        // Explicit cases cover trimming, optional BOM, pipe/code escapes, fence
        // thresholds and list prefixes before the deterministic finite input corpus.
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
        // This finite synthetic corpus varies grammar interactions without
        // introducing a replacement source, production state or random external input.
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
        // Cross the real checkpoint stride with front matter, giant lines, CRLF,
        // tables and long fences; an empty checkpoint result would not exercise restart state.
        source.push_str(&"中😀`|\\x".repeat(12_000));
        source.push_str("\r\n\n| a | b |\n|---|---|\n|1|2|\r\n\n````rust\n");
        source.push_str(&"opaque|中".repeat(12_000));
        source.push_str("\r\n````\n- x\n  continuation\n\nend\r\n");
        let expected = MarkdownBlockCheckpointIndex::from_text(&source).checkpoints;
        // Vary chunk sizes while preserving original scalar boundaries. Every
        // split must produce the same complete-line checkpoint coordinates and states.
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
