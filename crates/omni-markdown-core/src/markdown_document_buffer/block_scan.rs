//! Restartable Markdown block scanning and bounded viewport checkpoint state.

use super::*;

#[cfg(any(test, feature = "native-markdown-candidate"))]
mod streaming;
#[cfg(any(test, feature = "native-markdown-candidate"))]
pub(super) use streaming::InitialBlockScanner;

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// A complete line boundary and minimal restart state, never retained source text or AST nodes.
pub(super) struct MarkdownBlockCheckpoint {
    pub(super) utf16_offset: usize,
    pub(super) byte_offset: usize,
    pub(super) state: MarkdownBlockScannerState,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Debug)]
/// Sparse ordered checkpoints invalidated from the first affected edit position.
pub(super) struct MarkdownBlockCheckpointIndex {
    pub(super) checkpoints: Vec<MarkdownBlockCheckpoint>,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
impl MarkdownBlockCheckpointIndex {
    /// Build sparse restart checkpoints while the original mapped text is already being indexed.
    #[cfg(test)]
    pub(super) fn from_text(text: &str) -> Self {
        let initial = MarkdownBlockCheckpoint {
            utf16_offset: 0,
            byte_offset: 0,
            state: MarkdownBlockScannerState::document_start(),
        };
        let scan = scan_markdown_block_slice(text, 0, 0, initial.state, true, false);
        let mut checkpoints = Vec::with_capacity(scan.checkpoints.len().saturating_add(1));
        checkpoints.push(initial);
        checkpoints.extend(scan.checkpoints);
        Self { checkpoints }
    }

    /// Find the latest known scanner state preceding a requested UTF-16 position.
    pub(super) fn checkpoint_at_or_before(&self, utf16_offset: usize) -> MarkdownBlockCheckpoint {
        let index = self
            .checkpoints
            .partition_point(|checkpoint| {
                // Partition ordered checkpoints at the latest restart position unaffected by the requested coordinate.
                checkpoint.utf16_offset <= utf16_offset
            })
            .saturating_sub(1);
        self.checkpoints[index]
    }

    /// Discard checkpoints affected by an edit while always retaining the document-start state.
    pub(super) fn invalidate_after(&mut self, utf16_offset: usize) {
        let keep = self
            .checkpoints
            .partition_point(|checkpoint| {
                // Partition ordered checkpoints at the latest restart position unaffected by the requested coordinate.
                checkpoint.utf16_offset <= utf16_offset
            })
            .max(1);
        self.checkpoints.truncate(keep);
    }

    /// Append newly discovered checkpoints without duplicating or reordering existing restart positions.
    pub(super) fn extend_from_scan(&mut self, checkpoints: &[MarkdownBlockCheckpoint]) {
        let mut last_utf16 = self.checkpoints.last().map_or(0, |checkpoint| {
            // Read only the checkpoint coordinate; scanner state and source ownership remain unchanged.
            checkpoint.utf16_offset
        });
        for checkpoint in checkpoints {
            // The scan may overlap known positions; examine its monotonically ordered checkpoints in sequence.
            if checkpoint.utf16_offset > last_utf16 {
                // Only positions strictly newer than the retained tail can extend the index.
                self.checkpoints.push(*checkpoint);
                last_utf16 = checkpoint.utf16_offset;
            }
        }
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Conservative active-block and previous-header state that can be copied into a checkpoint.
pub(super) struct MarkdownBlockScannerState {
    pub(super) active: Option<MarkdownActiveBlock>,
    pub(super) previous_table_header: Option<MarkdownTableHeaderCandidate>,
    pub(super) document_start: bool,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
impl MarkdownBlockScannerState {
    /// Construct the initial state in which front matter may begin on the first line.
    pub(super) const fn document_start() -> Self {
        Self {
            active: None,
            previous_table_header: None,
            document_start: true,
        }
    }

    /// Restart before a known active block without retaining stale header or active-block state.
    pub(super) const fn at_block_start(utf16_offset: usize, byte_offset: usize) -> Self {
        Self {
            active: None,
            previous_table_header: None,
            document_start: utf16_offset == 0 && byte_offset == 0,
        }
    }

    /// Advance the conservative block-envelope scanner by one complete logical line.
    pub(super) fn consume_line(
        &mut self,
        line: MarkdownBlockLine<'_>,
        completed_blocks: &mut MarkdownCompletedBlockCollection,
    ) {
        let facts = streaming::MarkdownBlockLineFacts::from_text(line.content);
        self.consume_line_facts(line.utf16_range, line.byte_range, &facts, completed_blocks);
    }

    /// Consume the same fixed line facts for slice and initial scans, without retaining a huge physical line.
    fn consume_line_facts(
        &mut self,
        utf16_range: Range<usize>,
        byte_range: Range<usize>,
        facts: &streaming::MarkdownBlockLineFacts,
        completed_blocks: &mut MarkdownCompletedBlockCollection,
    ) {
        if let Some(active) = self.active {
            // Existing multi-line constructs determine whether this line can start another block.
            match active.kind {
                // Fence contents remain one block until a compatible closing delimiter appears.
                MarkdownScannedBlockKind::FencedCode {
                    marker,
                    minimum_length,
                } => {
                    if facts.closes_fence(marker, minimum_length) {
                        // A matching closing fence completes the block including the closing line ending.
                        complete_markdown_block(
                            &mut self.active,
                            byte_range.end,
                            utf16_range.end,
                            completed_blocks,
                        );
                    }
                    self.previous_table_header = None;
                    self.document_start = false;
                    return;
                }
                // Front matter treats all interior text as opaque until its closing delimiter.
                MarkdownScannedBlockKind::FrontMatter => {
                    if facts.closes_front_matter() {
                        // A front-matter terminator completes the envelope including the terminator line.
                        complete_markdown_block(
                            &mut self.active,
                            byte_range.end,
                            utf16_range.end,
                            completed_blocks,
                        );
                    }
                    self.previous_table_header = None;
                    self.document_start = false;
                    return;
                }
                // A pipe-bearing nonblank line extends the current table and cannot start a competing block.
                MarkdownScannedBlockKind::Table { .. } if facts.table_body() => {
                    self.previous_table_header = None;
                    self.document_start = false;
                    return;
                }
                // The first non-table line ends the table before this line, then receives normal classification.
                MarkdownScannedBlockKind::Table { .. } => {
                    complete_markdown_block(
                        &mut self.active,
                        byte_range.start,
                        utf16_range.start,
                        completed_blocks,
                    );
                }
                // After a blank list separator, unindented non-list text ends the prior list before this line.
                MarkdownScannedBlockKind::List { saw_blank: true }
                    if !facts.list_continuation() =>
                {
                    complete_markdown_block(
                        &mut self.active,
                        byte_range.start,
                        utf16_range.start,
                        completed_blocks,
                    );
                }
                // Other active paragraphs and continuing lists proceed to ordinary line classification.
                _ => {}
            }
        }

        if facts.blank() {
            // A blank line ends ordinary blocks but may separate items inside a continuing list.
            if let Some(active) = self.active.as_mut() {
                // Only an existing block needs a boundary update for blank input.
                match &mut active.kind {
                    // Lists retain their envelope while recording that continuation now needs an explicit marker or indentation.
                    MarkdownScannedBlockKind::List { saw_blank } => *saw_blank = true,
                    // Non-list blocks end before the blank line, keeping the blank outside their semantic envelope.
                    _ => complete_markdown_block(
                        &mut self.active,
                        byte_range.start,
                        utf16_range.start,
                        completed_blocks,
                    ),
                }
            }
            self.previous_table_header = None;
            self.document_start = false;
            return;
        }

        if self.document_start && facts.opens_front_matter() {
            // Only the actual document start permits a front-matter opener.
            complete_markdown_block(
                &mut self.active,
                byte_range.start,
                utf16_range.start,
                completed_blocks,
            );
            self.active = Some(MarkdownActiveBlock {
                start_utf16: utf16_range.start,
                start_byte: byte_range.start,
                kind: MarkdownScannedBlockKind::FrontMatter,
            });
            self.previous_table_header = None;
            self.document_start = false;
            return;
        }

        if let Some((marker, minimum_length)) = facts.opening_fence() {
            // A valid fence starts a new opaque block after completing any prior paragraph or list.
            complete_markdown_block(
                &mut self.active,
                byte_range.start,
                utf16_range.start,
                completed_blocks,
            );
            self.active = Some(MarkdownActiveBlock {
                start_utf16: utf16_range.start,
                start_byte: byte_range.start,
                kind: MarkdownScannedBlockKind::FencedCode {
                    marker,
                    minimum_length,
                },
            });
            self.previous_table_header = None;
            self.document_start = false;
            return;
        }

        if let Some(column_count) = facts.delimiter_columns()
            && let Some(header) = self.previous_table_header
            && header.column_count == column_count
        {
            // A delimiter row promotes a matching preceding header candidate into a table envelope.
            if self.active.is_some_and(|active| {
                // A prior active block contains text before the promoted table header and needs a separate completed envelope.
                active.start_utf16 < header.start_utf16
            }) {
                // Any earlier paragraph content ends before the promoted table header.
                complete_markdown_block(
                    &mut self.active,
                    header.start_byte,
                    header.start_utf16,
                    completed_blocks,
                );
            }
            self.active = Some(MarkdownActiveBlock {
                start_utf16: header.start_utf16,
                start_byte: header.start_byte,
                kind: MarkdownScannedBlockKind::Table { column_count },
            });
            self.previous_table_header = None;
            self.document_start = false;
            return;
        }

        let line_kind = if facts.list_marker() {
            // A recognized list marker gives a newly started block list continuation semantics.
            MarkdownScannedBlockKind::List { saw_blank: false }
        } else {
            // Other ordinary nonblank text begins or continues a paragraph envelope.
            MarkdownScannedBlockKind::Paragraph
        };
        match self.active.as_mut() {
            // An existing paragraph or list retains its original start position.
            Some(active) => {
                if let MarkdownScannedBlockKind::List { saw_blank } = &mut active.kind {
                    // A continuing nonblank list line clears the blank-separator state.
                    *saw_blank = false;
                }
            }
            // Without an active block, capture this line as the new envelope start.
            None => {
                self.active = Some(MarkdownActiveBlock {
                    start_utf16: utf16_range.start,
                    start_byte: byte_range.start,
                    kind: line_kind,
                });
            }
        }

        // Retain only the latest header candidate; a following matching delimiter may promote it.
        self.previous_table_header = facts.header_columns().map(|column_count| {
            // Capture absolute starts so a later promotion can rewind to the true header.
            MarkdownTableHeaderCandidate {
                start_utf16: utf16_range.start,
                start_byte: byte_range.start,
                column_count,
            }
        });
        self.document_start = false;
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Coordinates and continuation rule of an unfinished block envelope.
pub(super) struct MarkdownActiveBlock {
    pub(super) start_utf16: usize,
    pub(super) start_byte: usize,
    pub(super) kind: MarkdownScannedBlockKind,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// The bounded scanner's continuation classes; detailed grammar stays in pulldown-cmark.
pub(super) enum MarkdownScannedBlockKind {
    Paragraph,
    List { saw_blank: bool },
    FencedCode { marker: u8, minimum_length: usize },
    Table { column_count: usize },
    FrontMatter,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One prior pipe-bearing line eligible for promotion by a matching delimiter row.
pub(super) struct MarkdownTableHeaderCandidate {
    pub(super) start_utf16: usize,
    pub(super) start_byte: usize,
    pub(super) column_count: usize,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Clone, Debug, Eq, PartialEq)]
/// Completed absolute byte and UTF-16 envelope; source bytes remain document-owned.
pub(super) struct MarkdownScannedBlock {
    pub(super) utf16_range: Range<usize>,
    pub(super) byte_range: Range<usize>,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Optional temporary viewport envelopes; original-file indexing disables their retention.
pub(super) struct MarkdownCompletedBlockCollection {
    pub(super) enabled: bool,
    pub(super) blocks: Vec<MarkdownScannedBlock>,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
impl MarkdownCompletedBlockCollection {
    /// Choose whether a scan retains completed envelopes or only sparse checkpoint state.
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            blocks: Vec::new(),
        }
    }

    /// Retain completed ranges only for a viewport scan that requested them.
    pub(super) fn record(&mut self, block: MarkdownScannedBlock) {
        if self.enabled {
            // Initial indexing omits completed-block storage so it cannot retain a document-sized range list.
            self.blocks.push(block);
        }
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Temporary scan result containing completed envelopes, restart state, and sparse checkpoint additions.
pub(super) struct MarkdownBlockSliceScan {
    pub(super) completed_blocks: Vec<MarkdownScannedBlock>,
    pub(super) final_state: MarkdownBlockScannerState,
    pub(super) scanned_utf16_end: usize,
    pub(super) checkpoints: Vec<MarkdownBlockCheckpoint>,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
#[derive(Clone)]
/// Borrowed line content plus absolute ranges including its original line-ending bytes.
pub(super) struct MarkdownBlockLine<'a> {
    pub(super) content: &'a str,
    pub(super) utf16_range: Range<usize>,
    pub(super) byte_range: Range<usize>,
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Scan complete lines from a bounded slice, preserving absolute coordinates and restart state.
pub(super) fn scan_markdown_block_slice(
    source: &str,
    absolute_utf16_start: usize,
    absolute_byte_start: usize,
    initial_state: MarkdownBlockScannerState,
    reaches_document_end: bool,
    collect_completed_blocks: bool,
) -> MarkdownBlockSliceScan {
    // The caller owns source identity, immutable-byte acquisition and cancellation.
    // This synchronous bounded slice contains only complete lines plus an optional
    // incomplete tail, and only the explicit true-EOF flag may close that tail.
    let mut state = initial_state;
    let mut completed_blocks = MarkdownCompletedBlockCollection::new(collect_completed_blocks);
    let mut checkpoints = Vec::new();
    let mut local_byte_start = 0;
    let mut absolute_utf16_offset = absolute_utf16_start;
    let mut scanned_utf16_end = absolute_utf16_start;
    let mut last_checkpoint_utf16 = absolute_utf16_start;
    let bytes = source.as_bytes();

    while local_byte_start < bytes.len() {
        // Each iteration advances through one logical line and computes its absolute UTF-16 boundary.
        let (content_end, line_end, has_line_ending) =
            markdown_line_end_offsets(bytes, local_byte_start);
        if !reaches_document_end && !has_line_ending && line_end == bytes.len() {
            // A truncated final line cannot safely change semantic state until the rest of that line is available.
            break;
        }
        let line_text = &source[local_byte_start..line_end];
        let line_utf16_end = absolute_utf16_offset + line_text.encode_utf16().count();
        state.consume_line(
            MarkdownBlockLine {
                content: &source[local_byte_start..content_end],
                utf16_range: absolute_utf16_offset..line_utf16_end,
                byte_range: absolute_byte_start + local_byte_start..absolute_byte_start + line_end,
            },
            &mut completed_blocks,
        );
        scanned_utf16_end = line_utf16_end;
        if scanned_utf16_end.saturating_sub(last_checkpoint_utf16)
            >= MARKDOWN_BLOCK_CHECKPOINT_STRIDE_UTF16_UNITS
            && (line_end < bytes.len() || !reaches_document_end)
        {
            // Record sparse states only at complete line boundaries and avoid a redundant document-end checkpoint.
            checkpoints.push(MarkdownBlockCheckpoint {
                utf16_offset: scanned_utf16_end,
                byte_offset: absolute_byte_start + line_end,
                state,
            });
            last_checkpoint_utf16 = scanned_utf16_end;
        }
        absolute_utf16_offset = line_utf16_end;
        local_byte_start = line_end;
    }

    if reaches_document_end && local_byte_start == bytes.len() {
        // The real end of the document closes an unfinished block; an arbitrary slice end does not.
        complete_markdown_block(
            &mut state.active,
            absolute_byte_start + bytes.len(),
            absolute_utf16_offset,
            &mut completed_blocks,
        );
        scanned_utf16_end = absolute_utf16_offset;
    }

    MarkdownBlockSliceScan {
        completed_blocks: completed_blocks.blocks,
        final_state: state,
        scanned_utf16_end,
        checkpoints,
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Take ownership of an active envelope and optionally retain its nonempty completed range.
pub(super) fn complete_markdown_block(
    active: &mut Option<MarkdownActiveBlock>,
    byte_end: usize,
    utf16_end: usize,
    completed_blocks: &mut MarkdownCompletedBlockCollection,
) {
    let Some(active) = active.take() else {
        // No active envelope means there is no ownership to complete or retain.
        return;
    };
    if active.start_utf16 < utf16_end && active.start_byte < byte_end {
        // Only nonempty byte and UTF-16 spans form usable semantic block envelopes.
        completed_blocks.record(MarkdownScannedBlock {
            utf16_range: active.start_utf16..utf16_end,
            byte_range: active.start_byte..byte_end,
        });
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Find content end and full line end while treating CRLF as one line ending.
pub(super) fn markdown_line_end_offsets(bytes: &[u8], start: usize) -> (usize, usize, bool) {
    let mut index = start;
    while index < bytes.len() {
        // Scan bytes until an ASCII line marker appears; UTF-8 continuation bytes cannot equal CR or LF.
        match bytes[index] {
            // A line feed ends content before the marker and includes that marker in the line range.
            b'\n' => return (index, index + 1, true),
            // A carriage return may own the following line feed as part of the same break.
            b'\r' => {
                let line_end = if bytes.get(index + 1) == Some(&b'\n') {
                    // A CRLF pair advances past both bytes.
                    index + 2
                } else {
                    // A standalone CR advances past its single byte.
                    index + 1
                };
                return (index, line_end, true);
            }
            // Other bytes are ordinary line content and scanning continues.
            _ => index += 1,
        }
    }
    (bytes.len(), bytes.len(), false)
}

#[cfg(test)]
/// Recognize Markdown blank-line whitespace without treating other Unicode text as empty.
pub(super) fn markdown_line_is_blank(line: &str) -> bool {
    line.bytes().all(|byte| {
        // Only ASCII horizontal whitespace belongs to this Markdown grammar boundary.
        matches!(byte, b' ' | b'\t')
    })
}

#[cfg(test)]
/// Recognize a first-line YAML-style delimiter with an optional UTF-8 BOM.
pub(super) fn markdown_opens_front_matter(line: &str) -> bool {
    line.strip_prefix('\u{feff}').unwrap_or(line).trim() == "---"
}

#[cfg(test)]
/// Recognize either supported front-matter terminator after surrounding whitespace.
pub(super) fn markdown_closes_front_matter(line: &str) -> bool {
    matches!(line.trim(), "---" | "...")
}

#[cfg(test)]
/// Recognize a conservative backtick or tilde fence and retain its closing threshold.
pub(super) fn markdown_opening_fence(line: &str) -> Option<(u8, usize)> {
    let bytes = line.as_bytes();
    let indentation = bytes
        .iter()
        .take_while(|byte| {
            // Count leading Markdown spaces without interpreting other content as indentation.
            **byte == b' '
        })
        .count();
    if indentation > 3 {
        // Four leading spaces represent indented content rather than a fenced-block opener.
        return None;
    }
    let marker = *bytes.get(indentation)?;
    if !matches!(marker, b'`' | b'~') {
        // Only backticks and tildes can establish a fence marker.
        return None;
    }
    let marker_length = bytes[indentation..]
        .iter()
        .take_while(|byte| {
            // Keep only the contiguous opening marker run required to establish the matching fence length.
            **byte == marker
        })
        .count();
    if marker_length < 3 || (marker == b'`' && bytes[indentation + marker_length..].contains(&b'`'))
    {
        // Short runs and backticks inside a backtick info string cannot open a valid fence.
        return None;
    }
    // Keep the complete run: saturating to u8 can let a shorter delimiter falsely close the block.
    Some((marker, marker_length))
}

#[cfg(test)]
/// Require a compatible fence run followed only by permitted horizontal whitespace.
pub(super) fn markdown_closes_fence(line: &str, marker: u8, minimum_length: usize) -> bool {
    let bytes = line.as_bytes();
    let indentation = bytes
        .iter()
        .take_while(|byte| {
            // Count leading Markdown spaces without interpreting other content as indentation.
            **byte == b' '
        })
        .count();
    if indentation > 3 {
        // Excess indentation prevents a line from closing the active fence.
        return false;
    }
    let marker_length = bytes[indentation..]
        .iter()
        .take_while(|byte| {
            // Keep only the contiguous opening marker run required to establish the matching fence length.
            **byte == marker
        })
        .count();
    marker_length >= minimum_length
        && bytes[indentation + marker_length..].iter().all(|byte| {
            // Only ASCII horizontal whitespace belongs to this Markdown grammar boundary.
            matches!(byte, b' ' | b'\t')
        })
}

#[cfg(test)]
/// Recognize a possible table header using unescaped, non-code pipe separators.
pub(super) fn markdown_table_header_column_count(line: &str) -> Option<usize> {
    let pipe_count = markdown_unescaped_table_pipe_count(line);
    (pipe_count > 0).then_some(markdown_table_column_count(line, pipe_count))
}

#[cfg(test)]
/// Validate each delimiter cell before promoting a header into a table.
pub(super) fn markdown_table_delimiter_column_count(line: &str) -> Option<usize> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        // An empty row has no delimiter cells and cannot establish a table.
        return None;
    }
    let mut column_count = 0;
    for cell in trimmed.trim_matches('|').split('|') {
        // Each candidate cell must consist of an optional alignment colon around at least three hyphens.
        let cell = cell.trim();
        let core = cell
            .strip_prefix(':')
            .unwrap_or(cell)
            .strip_suffix(':')
            .unwrap_or_else(|| {
                // Without a trailing alignment colon, retain the cell after removing only its optional leading colon.
                cell.strip_prefix(':').unwrap_or(cell)
            });
        if core.len() < 3
            || !core.bytes().all(|byte| {
                // Every remaining delimiter byte must be a hyphen; other content would invalidate the table row.
                byte == b'-'
            })
        {
            // One malformed cell rejects the entire delimiter row rather than producing partial table semantics.
            return None;
        }
        column_count += 1;
    }
    (column_count > 0).then_some(column_count)
}

#[cfg(test)]
/// Conservatively retain nonblank pipe-bearing lines inside a table envelope.
pub(super) fn markdown_table_body_line(line: &str) -> bool {
    !markdown_line_is_blank(line) && markdown_unescaped_table_pipe_count(line) > 0
}

#[cfg(test)]
/// Count structural pipe bytes while skipping escapes and matching inline-code runs.
pub(super) fn markdown_unescaped_table_pipe_count(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut count = 0;
    let mut index = 0;
    let mut code_delimiter_length = 0;
    while index < bytes.len() {
        // Every path advances the byte cursor so malformed Markdown cannot stall scanning.
        match bytes[index] {
            // An escaped byte cannot act as a table separator or code delimiter.
            b'\\' => index = index.saturating_add(2).min(bytes.len()),
            // A run of backticks opens or closes inline code according to its exact length.
            b'`' => {
                let delimiter_length = bytes[index..]
                    .iter()
                    .take_while(|byte| {
                        // Count the full backtick run so different-length runs inside code remain literal.
                        **byte == b'`'
                    })
                    .count();
                if code_delimiter_length == 0 {
                    // Outside code, remember this run length as the required closing delimiter.
                    code_delimiter_length = delimiter_length;
                } else if code_delimiter_length == delimiter_length {
                    // A matching run closes code so later pipes regain structural meaning.
                    code_delimiter_length = 0;
                }
                index += delimiter_length;
            }
            // Only pipes outside code contribute table columns.
            b'|' if code_delimiter_length == 0 => {
                count += 1;
                index += 1;
            }
            // All other bytes, including pipes inside code, are ordinary content.
            _ => index += 1,
        }
    }
    count
}

#[cfg(test)]
/// Convert separator count into cells while excluding optional outer table pipes.
pub(super) fn markdown_table_column_count(line: &str, pipe_count: usize) -> usize {
    let leading = usize::from(line.trim_start().starts_with('|'));
    let trailing = usize::from(line.trim_end().ends_with('|'));
    pipe_count
        .saturating_add(1)
        .saturating_sub(leading + trailing)
}

#[cfg(test)]
/// Recognize bullet or numeric list prefixes and report the complete marker width.
pub(super) fn markdown_list_marker_length(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let indentation = bytes
        .iter()
        .take_while(|byte| {
            // Only ASCII horizontal whitespace belongs to this Markdown grammar boundary.
            matches!(byte, b' ' | b'\t')
        })
        .count();
    let content = bytes.get(indentation..)?;
    if matches!(content.first(), Some(b'-' | b'+' | b'*'))
        && content.get(1).is_some_and(u8::is_ascii_whitespace)
    {
        // A bullet followed by whitespace establishes an unordered list item.
        return Some(indentation + 2);
    }
    let digits = content
        .iter()
        .take_while(|byte| {
            // A numeric list prefix ends at the first non-ASCII digit.
            byte.is_ascii_digit()
        })
        .count();
    if digits > 0
        && matches!(content.get(digits), Some(b'.' | b')'))
        && content.get(digits + 1).is_some_and(u8::is_ascii_whitespace)
    {
        // Digits plus a supported terminator and whitespace establish an ordered list item.
        Some(indentation + digits + 2)
    } else {
        // Text that lacks the complete list prefix remains ordinary paragraph content.
        None
    }
}

#[cfg(test)]
/// Keep list envelopes open for another item marker or sufficiently indented continuation.
pub(super) fn markdown_list_continuation_line(line: &str) -> bool {
    markdown_list_marker_length(line).is_some()
        || line
            .as_bytes()
            .iter()
            .take_while(|byte| {
                // Only ASCII horizontal whitespace belongs to this Markdown grammar boundary.
                matches!(byte, b' ' | b'\t')
            })
            .count()
            >= 2
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Test envelope overlap, including a collapsed caret located on either block boundary.
pub(super) fn markdown_utf16_ranges_intersect(left: &Range<usize>, right: &Range<usize>) -> bool {
    if right.start == right.end {
        // A caret uses inclusive endpoints so an adjacent block can supply its parse envelope.
        left.start <= right.start && right.start <= left.end
    } else {
        // Nonempty ranges use ordinary half-open overlap semantics.
        left.start < right.end && right.start < left.end
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Align an absolute UTF-16 position downward without splitting a surrogate pair.
pub(super) fn markdown_utf16_boundary_down_in_text(
    source: &str,
    absolute_start: usize,
    absolute_offset: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    let mut local_offset = absolute_offset.saturating_sub(absolute_start);
    loop {
        // Only an interior surrogate unit needs correction; unrelated errors propagate.
        match byte_offset_for_utf16_text(source, local_offset) {
            // A valid local boundary maps directly back to document coordinates.
            Ok(_) => return Ok(absolute_start + local_offset),
            // Retreat the interior unit to the scalar's leading UTF-16 boundary.
            Err(MarkdownDocumentBufferError::NotUnicodeBoundary) if local_offset > 0 => {
                local_offset -= 1;
            }
            // Out-of-range or other failures cannot be repaired by alignment.
            Err(error) => return Err(error),
        }
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Align an absolute UTF-16 position upward within the supplied bounded text.
pub(super) fn markdown_utf16_boundary_up_in_text(
    source: &str,
    absolute_start: usize,
    absolute_offset: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    let source_utf16_len = source.encode_utf16().count();
    let mut local_offset = absolute_offset
        .saturating_sub(absolute_start)
        .min(source_utf16_len);
    loop {
        // Advance only while the candidate splits a surrogate pair and remains inside the slice.
        match byte_offset_for_utf16_text(source, local_offset) {
            // A valid local boundary is ready for absolute-coordinate publication.
            Ok(_) => return Ok(absolute_start + local_offset),
            // Include the trailing unit so the selected scalar stays complete.
            Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
                if local_offset < source_utf16_len =>
            {
                local_offset += 1;
            }
            // Other coordinate failures propagate rather than widening the slice arbitrarily.
            Err(error) => return Err(error),
        }
    }
}
