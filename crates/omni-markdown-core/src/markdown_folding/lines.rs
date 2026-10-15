//! Bounded source-line records and sequential UTF-16 traversal for explicit fold queries.
//!
//! Line bodies stay in the document's canonical pieces. Paint hints retain finite prefixes only;
//! explicit queries acquire complete delimiter metadata using the separate fixed-scratch scanner.

use super::delimiters::{FenceDelimiter, HtmlBlock, marker_line_metadata, scan_fence_delimiter};
use super::{MarkdownDocumentBuffer, MarkdownDocumentBufferError, PREFIX_UNITS};

/// A temporary line record retains only the prefix needed to recognize Markdown block delimiters.
pub(super) struct Line {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) prefix: String,
    pub(super) blank: bool,
    delimiter: Option<FenceDelimiter>,
    marker_line: (Option<usize>, bool),
}

impl Line {
    /// Inspect one logical line without copying its possibly enormous body.
    pub(super) fn read(
        document: &MarkdownDocumentBuffer,
        number: usize,
    ) -> Result<Self, MarkdownDocumentBufferError> {
        Self::read_with_blank_scan(document, number, true)
    }

    /// Viewport hints stop after the fixed prefix; explicit fold queries may classify a giant blank line.
    pub(super) fn read_with_blank_scan(
        document: &MarkdownDocumentBuffer,
        number: usize,
        scan_blank: bool,
    ) -> Result<Self, MarkdownDocumentBufferError> {
        let start = document.utf16_offset_for_byte(document.byte_offset_for_line(number)?)?;
        let next = if number + 1 < document.line_count() {
            // Tree aggregates find the next logical start without scanning intervening source bytes.
            document.utf16_offset_for_byte(document.byte_offset_for_line(number + 1)?)?
        } else {
            // The final line ends at the canonical length, including an empty trailing line.
            document.utf16_len()
        };
        let mut end = next;
        let mut tail = [0_u16; 2];
        let tail_length = (next - start).min(tail.len());
        document.copy_utf16_code_units(next - tail_length, &mut tail[..tail_length])?;
        if tail_length != 0 && matches!(tail[tail_length - 1], 10 | 13) {
            // Strip one logical line ending, preserving exact CRLF source coordinates.
            end -= 1;
            if tail_length == 2 && tail[0] == 13 && tail[1] == 10 {
                // A CRLF pair is one logical break but occupies two native UTF-16 positions.
                end -= 1;
            }
        }
        let mut scratch = [0_u16; PREFIX_UNITS];
        let count = (end - start).min(scratch.len());
        document.copy_utf16_code_units(start, &mut scratch[..count])?;
        let prefix = String::from_utf16_lossy(&scratch[..count]);
        let mut blank = scratch[..count].iter().all(|unit| matches!(unit, 9 | 32));
        let mut offset = start + count;
        while scan_blank && blank && offset < end {
            // A giant whitespace line is classified using the same fixed buffer, without retaining it.
            let count = (end - offset).min(scratch.len());
            document.copy_utf16_code_units(offset, &mut scratch[..count])?;
            blank = scratch[..count].iter().all(|unit| matches!(unit, 9 | 32));
            offset += count;
        }
        Ok(Self {
            start,
            end,
            delimiter: if scan_blank && prefix.trim_start_matches(' ').starts_with(['`', '~']) {
                // Only delimiter candidates need a complete bounded-scratch info/run scan.
                scan_fence_delimiter(document, start, end)?
            } else {
                // Ordinary lines never pay for a complete nonblank body scan.
                None
            },
            marker_line: marker_line_metadata(document, start, end, &prefix, scan_blank)?,
            prefix,
            blank,
        })
    }

    /// Count indentation columns in the finite retained prefix, including Markdown tab-stop padding.
    pub(super) fn indent(&self) -> usize {
        let mut columns = 0;
        for byte in self.prefix.bytes() {
            // Indentation ends at the first content byte; only leading ASCII spacing affects block context.
            match byte {
                // Tabs advance to the next four-column stop, matching Markdown indentation.
                b'\t' => columns += 4 - columns % 4,
                // Spaces contribute one indentation column.
                b' ' => columns += 1,
                // The first content byte ends indentation counting.
                _ => break,
            }
        }
        columns
    }

    /// Block markers permit at most three leading spaces outside list/quote containers.
    pub(super) fn marker_source(&self) -> &str {
        self.prefix.trim_start_matches(' ')
    }

    /// Recognize ATX levels without treating escaped or seven-hash text as a heading.
    pub(super) fn heading(&self) -> Option<usize> {
        if self.indent() > 3 {
            // Four-column indentation belongs to code rather than an ATX heading.
            return None;
        }
        let text = self.marker_source();
        let count = text.bytes().take_while(|byte| *byte == b'#').count();
        (count != 0 && count <= 6 && text.as_bytes().get(count).is_none_or(|byte| *byte == b' '))
            .then_some(count)
    }

    /// Complete metadata distinguishes a contiguous Setext underline from spaced thematic markers.
    pub(super) fn setext(&self) -> Option<usize> {
        self.marker_line.0
    }

    /// Opening authority requires the exact run and a valid complete backtick info string.
    pub(super) fn fence(&self) -> Option<(u8, usize)> {
        self.delimiter
            .filter(|delimiter| delimiter.valid_opening)
            .map(|delimiter| {
                // The delimiter came from a full-line scan, never a truncated marker prefix.
                (delimiter.marker, delimiter.length)
            })
    }

    /// A closing fence needs only spaces/tabs after a matching sufficiently long exact run.
    pub(super) fn closes_fence(&self, opening: (u8, usize)) -> bool {
        self.delimiter.is_some_and(|delimiter| {
            // Info text or a shorter run cannot end an existing opaque fenced context.
            delimiter.marker == opening.0 && delimiter.length >= opening.1 && delimiter.closing_tail
        })
    }

    /// Identify list-item starts while bounding decimal marker work to CommonMark's nine digits.
    pub(super) fn list_item(&self) -> bool {
        if self.indent() > 3 {
            // Four-column top-level indentation is code, so marker-looking code cannot consume lazy prose.
            return false;
        }
        let text = self.marker_source().as_bytes();
        if text.len() >= 2
            && matches!(text[0], b'-' | b'+' | b'*')
            && matches!(text[1], b' ' | b'\t')
        {
            // A bullet needs whitespace after its marker to avoid interpreting emphasis as a list.
            return true;
        }
        let digits = text.iter().take_while(|byte| byte.is_ascii_digit()).count();
        digits != 0
            && digits <= 9
            && text
                .get(digits)
                .is_some_and(|byte| matches!(byte, b'.' | b')'))
            && text
                .get(digits + 1)
                .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    }

    /// Explicit quote markers distinguish container starts from lazy paragraph continuation.
    pub(super) fn quote(&self) -> bool {
        self.indent() <= 3 && self.marker_source().starts_with('>')
    }

    /// Parse at most the fixed line prefix to distinguish complete HTML starts from tag-name prefixes.
    pub(super) fn html(&self) -> Option<HtmlBlock> {
        let text = self.marker_source();
        if self.indent() > 3 || !text.starts_with('<') {
            // Non-HTML and indented code cannot begin an HTML block at this source level.
            return None;
        }
        let lower = text.to_ascii_lowercase();
        for (opening, closing) in [
            ("<script", "</script>"),
            ("<pre", "</pre>"),
            ("<style", "</style>"),
            ("<textarea", "</textarea>"),
        ] {
            // Raw-tag authority depends on a finite name boundary, even when the remaining line is huge.
            if lower.strip_prefix(opening).is_some_and(|tail| {
                // A name prefix like <scripture cannot borrow script's explicit termination rule.
                tail.is_empty() || tail.starts_with([' ', '\t', '>'])
            }) {
                // Exact raw-tag identity remains opaque across blanks until its complete closing tag.
                return Some(HtmlBlock::Until(closing));
            }
        }
        if text.starts_with("<!--") {
            // Comments may contain blanks and Markdown-looking headings before their explicit terminator.
            return Some(HtmlBlock::Until("-->"));
        }
        if text.starts_with("<![CDATA[") {
            // CDATA's complete opening identity is finite even when its same-line payload is enormous.
            return Some(HtmlBlock::Until("]]>"));
        }
        if text.starts_with("<?") {
            // Processing instructions likewise retain explicit termination across intervening blank lines.
            return Some(HtmlBlock::Until("?>"));
        }
        if text.starts_with("<!") && text.as_bytes().get(2).is_some_and(u8::is_ascii_uppercase) {
            // A declaration requires an ASCII uppercase name start and ends at the first closing angle.
            return Some(HtmlBlock::Until(">"));
        }
        if self.end - self.start > PREFIX_UNITS {
            // Other incomplete long tags cannot authorize hiding, but their context stays opaque until blank.
            return Some(HtmlBlock::Uncertain);
        }
        let recognized = pulldown_cmark::Parser::new(text).any(|event| {
            // This transient parser sees only one bounded line, not a full-body AST or retained index.
            matches!(
                event,
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::HtmlBlock)
            )
        });
        if recognized {
            // Other recognized block/generic tags continue through the first blank line.
            Some(HtmlBlock::UntilBlank)
        } else {
            // Incomplete custom names such as <divorce are paragraphs, rather than <div blocks.
            None
        }
    }

    /// A thematic break interrupts paragraphs but cannot become a synthetic multiline fold itself.
    pub(super) fn thematic_break(&self) -> bool {
        self.marker_line.1
    }

    /// Setext promotion is restricted to ordinary paragraph text, never another block opener.
    pub(super) fn plain_paragraph(&self) -> bool {
        !self.blank
            && self.indent() < 4
            && self.heading().is_none()
            && self.fence().is_none()
            && !self.list_item()
            && !self.quote()
            && self.html().is_none()
            && !self.thematic_break()
    }

    /// Read explicit container content without assuming lazy continuation after non-paragraph blocks.
    pub(super) fn container_body(&self, quote: bool) -> Option<usize> {
        let source = self.marker_source().as_bytes();
        let leading = self.prefix.len() - source.len();
        let marker_end = if quote {
            // One explicit quote marker and its optional following space define the child source start.
            if source.first() != Some(&b'>') {
                // Missing explicit quote syntax cannot be interpreted as a contained child.
                return None;
            }
            1 + usize::from(source.get(1) == Some(&b' '))
        } else if matches!(source.first(), Some(b'-' | b'+' | b'*')) {
            // A bullet marker is ASCII, so byte and native-unit offsets agree here.
            1
        } else {
            // Ordered markers contain at most nine digits followed by one ASCII punctuation unit.
            source
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count()
                + 1
        };
        let mut at = leading + marker_end;
        if !quote {
            // The first content scalar follows the list marker's finite space/tab padding.
            while self
                .prefix
                .as_bytes()
                .get(at)
                .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
            {
                // Padding is bounded by the retained prefix and contributes no child content.
                at += 1;
            }
        }
        Some(at)
    }

    /// Recognize whether the visible container starts a paragraph eligible for an unmarked lazy line.
    pub(super) fn lazy_paragraph_body(&self, offset: usize) -> bool {
        let body = self.prefix.get(offset..).unwrap_or("");
        pulldown_cmark::Parser::new(body)
            .find_map(|event| {
                // Descend only through bounded container events; the first real child determines laziness.
                match event {
                    pulldown_cmark::Event::Start(pulldown_cmark::Tag::Paragraph) => {
                        // Paragraphs, including those inside a nested list/quote, permit lazy continuation.
                        Some(true)
                    }
                    pulldown_cmark::Event::Start(
                        pulldown_cmark::Tag::Heading { .. }
                        | pulldown_cmark::Tag::CodeBlock(_)
                        | pulldown_cmark::Tag::HtmlBlock,
                    )
                    | pulldown_cmark::Event::Rule => {
                        // Non-paragraph children cannot borrow unmarked outside source as their body.
                        Some(false)
                    }
                    pulldown_cmark::Event::Text(_)
                    | pulldown_cmark::Event::Code(_)
                    | pulldown_cmark::Event::InlineHtml(_)
                    | pulldown_cmark::Event::SoftBreak
                    | pulldown_cmark::Event::HardBreak => {
                        // Tight-list paragraphs omit Paragraph events, but their first inline still permits laziness.
                        Some(true)
                    }
                    _ => {
                        // Container delimiters and their closures have no independent laziness authority.
                        None
                    }
                }
            })
            .unwrap_or(false)
    }
}

/// Sequential explicit-query scanner avoids repeated indexed leaf rescans on millions of short lines.
pub(super) struct LineCursor<'a> {
    document: &'a MarkdownDocumentBuffer,
    scratch: [u16; PREFIX_UNITS],
    used: usize,
    available: usize,
    next_read: usize,
    next_start: usize,
    remaining: usize,
    pending: Option<(u16, usize)>,
}

impl<'a> LineCursor<'a> {
    /// Keep one source cursor and a fixed unit chunk; no note body or line index is retained.
    pub(super) fn new(document: &'a MarkdownDocumentBuffer, start: usize, lines: usize) -> Self {
        Self {
            document,
            scratch: [0; PREFIX_UNITS],
            used: 0,
            available: 0,
            next_read: start,
            next_start: start,
            remaining: lines,
            pending: None,
        }
    }

    /// Consume one native source unit, including a deferred non-LF unit after standalone CR.
    pub(super) fn unit(&mut self) -> Result<Option<(u16, usize)>, MarkdownDocumentBufferError> {
        if let Some(unit) = self.pending.take() {
            // A standalone CR already read this first unit of the following logical line.
            return Ok(Some(unit));
        }
        if self.used == self.available {
            // Refill only after the previous fixed chunk was fully consumed.
            if self.next_read == self.document.utf16_len() {
                // EOF is distinct from the final empty logical line handled by next_line.
                return Ok(None);
            }
            // Each ordered copy visits only intersecting pieces; source prefix scans are linear in bytes.
            self.available = (self.document.utf16_len() - self.next_read).min(self.scratch.len());
            self.document
                .copy_utf16_code_units(self.next_read, &mut self.scratch[..self.available])?;
            self.used = 0;
            self.next_read += self.available;
        }
        let position = self.next_read - self.available + self.used;
        let unit = self.scratch[self.used];
        self.used += 1;
        Ok(Some((unit, position)))
    }

    /// Return one bounded line record, keeping exact native offsets while consuming its body once.
    pub(super) fn next_line(&mut self) -> Result<Option<Line>, MarkdownDocumentBufferError> {
        if self.remaining == 0 {
            // The caller's checked logical extent was consumed, including any final empty line.
            return Ok(None);
        }
        self.remaining -= 1;
        let start = self.next_start;
        let mut end = self.document.utf16_len();
        let mut prefix = [0_u16; PREFIX_UNITS];
        let mut length = 0;
        let mut blank = true;
        while let Some((unit, position)) = self.unit()? {
            // Every source unit advances exactly once; only the first finite prefix is retained.
            if matches!(unit, 10 | 13) {
                // Record content ends before its separator; canonical offsets still include separator units.
                end = position;
                self.next_start = position + 1;
                if unit == 13 {
                    // One-unit lookahead distinguishes CRLF from a standalone CR without losing source.
                    match self.unit()? {
                        Some((10, _)) => {
                            // CRLF occupies two native units but starts only one following logical line.
                            self.next_start += 1;
                        }
                        Some(other) => {
                            // A standalone CR leaves the already-read next scalar for the next record.
                            self.pending = Some(other);
                        }
                        None => {
                            // A terminal CR still leaves the aggregate's trailing empty logical line.
                        }
                    }
                }
                break;
            }
            blank &= matches!(unit, 9 | 32);
            if length < prefix.len() {
                // Prefix truncation affects hints only; exact delimiter metadata is scanned separately.
                prefix[length] = unit;
                length += 1;
            }
        }
        let prefix = String::from_utf16_lossy(&prefix[..length]);
        let delimiter = if prefix.trim_start_matches(' ').starts_with(['`', '~']) {
            // Fence run/info authority always includes the full source line, never the truncated prefix.
            scan_fence_delimiter(self.document, start, end)?
        } else {
            // Non-delimiter bodies need no second pass after sequential classification.
            None
        };
        let marker_line = marker_line_metadata(self.document, start, end, &prefix, true)?;
        Ok(Some(Line {
            start,
            end,
            prefix,
            blank,
            delimiter,
            marker_line,
        }))
    }
}
