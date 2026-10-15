//! Borrowless scalar and physical-line traversal over canonical pieces, with fixed transient metadata.

use super::*;

/// A scalar cursor owns immutable piece metadata, so each read can end its byte borrow immediately.
pub(super) struct SourceCursor {
    pieces: SourcePieceCursor,
    piece: Option<super::super::super::formatting::streaming::SourcePieceRange>,
    byte: usize,
    unit: usize,
    pending: Option<(char, usize, usize)>,
}

impl SourceCursor {
    /// Start at independently validated scalar-aligned native/byte coordinates.
    pub(super) fn new(document: &MarkdownDocumentBuffer, bytes: Range<usize>, unit: usize) -> Self {
        Self {
            pieces: SourcePieceCursor::new(document.root.clone(), bytes.clone()),
            piece: None,
            byte: bytes.start,
            unit,
            pending: None,
        }
    }

    /// Decode one complete scalar; source leaves already preserve UTF-8 boundaries.
    pub(super) fn next(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Option<(char, usize, usize)> {
        if let Some(value) = self.pending.take() {
            // Lookahead already advanced the physical cursor and transfers its saved scalar exactly once.
            return Some(value);
        }
        loop {
            // Empty/exhausted leaves are replaced with the next ordered overlap, never a copied body.
            if let Some(piece) = &mut self.piece
                && piece.local.start < piece.local.end
            {
                // The temporary immutable source borrow ends before the caller can append canonical bytes.
                let source = document.piece_bytes(&piece.piece);
                let first = source[piece.local.start];
                let width = if first < 0x80 {
                    // ASCII prefix/body scans decode one byte without revalidating the remaining leaf.
                    1
                } else if first < 0xe0 {
                    // Canonical validated UTF-8 leads this scalar with a two-byte sequence.
                    2
                } else if first < 0xf0 {
                    // BMP scalars outside the ASCII/two-byte ranges occupy three source bytes.
                    3
                } else {
                    // Supplementary scalars remain complete within scalar-aligned source leaves.
                    4
                };
                let scalar =
                    std::str::from_utf8(&source[piece.local.start..piece.local.start + width])
                        .expect("canonical complete UTF-8 scalar")
                        .chars()
                        .next()
                        .expect("one scalar");
                let value = (scalar, self.byte, self.unit);
                piece.local.start += scalar.len_utf8();
                self.byte += scalar.len_utf8();
                self.unit += scalar.len_utf16();
                return Some(value);
            }
            self.piece = Some(self.pieces.next_piece()?);
        }
    }

    /// Inspect a scalar without losing it at a CRLF or marker grammar boundary.
    pub(super) fn peek(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Option<(char, usize, usize)> {
        if self.pending.is_none() {
            // Decode once; the next consumer takes the same saved scalar and original coordinates.
            self.pending = self.next(document);
        }
        self.pending
    }
}

/// One physical source line retains exact native offsets and no text buffer.
#[derive(Clone, Copy, Debug)]
pub(super) struct SourceLine {
    pub(super) number: usize,
    pub(super) bytes: (usize, usize),
    pub(super) units: (usize, usize),
}

impl SourceLine {
    /// Read one indexed command line without copying its content or normalizing its separator.
    pub(super) fn at(
        document: &MarkdownDocumentBuffer,
        number: usize,
    ) -> Result<Self, MarkdownDocumentBufferError> {
        let start = document.byte_offset_for_line(number)?;
        let start_unit = document.utf16_offset_for_byte(start)?;
        let next = if number + 1 < document.line_count() {
            // The next aggregate line boundary includes this line's physical separator bytes.
            document.byte_offset_for_line(number + 1)?
        } else {
            // A final unterminated line ends at the canonical byte extent.
            document.byte_len()
        };
        let next_unit = document.utf16_offset_for_byte(next)?;
        let mut tail = [0; 2];
        let length = (next_unit - start_unit).min(2);
        document.copy_utf16_code_units(next_unit - length, &mut tail[..length])?;
        let removed = if length > 0 && tail[length - 1] == 10 {
            // CRLF is one logical separator while both stored units remain canonical coordinates.
            1 + usize::from(length == 2 && tail[0] == 13)
        } else if length > 0 && tail[length - 1] == 13 {
            // Standalone CR is also a logical break in the existing source model.
            1
        } else {
            // No separator bytes belong to a final unterminated source line.
            0
        };
        Ok(Self {
            number,
            bytes: (start, next - removed),
            units: (start_unit, next_unit - removed),
        })
    }

    /// Read a source-local interval as scalars while preserving canonical physical offsets.
    pub(super) fn cursor(
        self,
        document: &MarkdownDocumentBuffer,
        local: usize,
    ) -> Result<SourceCursor, MarkdownDocumentBufferError> {
        let unit = self.units.0 + local;
        let byte = if local == 0 {
            // Sequential line records already provide this exact scalar-aligned byte/native boundary.
            self.bytes.0
        } else {
            // General caret callers may cross multibyte text and require the canonical UTF-16 conversion.
            document.byte_offset_for_utf16(unit)?
        };
        Ok(SourceCursor::new(document, byte..self.bytes.1, unit))
    }
    /// Start inside a previously classified ASCII marker/padding prefix without an indexed leaf rescan.
    pub(super) fn ascii_cursor(
        self,
        document: &MarkdownDocumentBuffer,
        local: usize,
    ) -> SourceCursor {
        // Marker callers advance only ASCII source units; byte/native offsets are identical in that prefix.
        SourceCursor::new(
            document,
            self.bytes.0 + local..self.bytes.1,
            self.units.0 + local,
        )
    }

    /// Classify an entire logical line using its already known physical range and finite scalar state.
    pub(super) fn whitespace(self, document: &MarkdownDocumentBuffer, quotes: bool) -> bool {
        let mut cursor = SourceCursor::new(document, self.bytes.0..self.bytes.1, self.units.0);
        while let Some((scalar, _, _)) = cursor.next(document) {
            // Ordinary content terminates immediately; only a genuinely blank giant line is fully scanned.
            if !indentation_whitespace(scalar) && !(quotes && scalar == '>') {
                // Any body content disproves blank-line status before the remaining text needs decoding.
                return false;
            }
        }
        true
    }
}

/// Sequential line boundaries hold immutable piece descriptors and no borrow across output append.
pub(super) struct SourceLines {
    cursor: SourceCursor,
    remaining: usize,
    number: usize,
    byte: usize,
    unit: usize,
}

impl SourceLines {
    /// One explicit command owns a depth-first source cursor and exact physical separator coordinates.
    pub(super) fn new(document: &MarkdownDocumentBuffer) -> Self {
        Self {
            cursor: SourceCursor::new(document, 0..document.byte_len(), 0),
            remaining: document.line_count(),
            number: 0,
            byte: 0,
            unit: 0,
        }
    }

    /// Restart at an aggregate line boundary without scanning the preceding/current giant body.
    pub(super) fn from_line(
        document: &MarkdownDocumentBuffer,
        number: usize,
    ) -> Result<Self, MarkdownDocumentBufferError> {
        let byte = document.byte_offset_for_line(number)?;
        let unit = document.utf16_offset_for_byte(byte)?;
        Ok(Self {
            cursor: SourceCursor::new(document, byte..document.byte_len(), unit),
            remaining: document.line_count() - number,
            number,
            byte,
            unit,
        })
    }

    /// Stream one complete line once, including CRLF split across canonical piece boundaries.
    pub(super) fn next(&mut self, document: &MarkdownDocumentBuffer) -> Option<SourceLine> {
        if self.remaining == 0 {
            // A final empty source line is represented by the aggregate count, never a synthetic sentinel.
            return None;
        }
        self.remaining -= 1;
        let start_byte = self.byte;
        let start_unit = self.unit;
        let mut end_byte = document.byte_len();
        let mut end_unit = document.utf16_len();
        while let Some((scalar, byte, unit)) = self.cursor.next(document) {
            // Scalar decoding advances ordered source once and never retains the intervening body.
            if matches!(scalar, '\r' | '\n') {
                // Content endpoints exclude separators while subsequent starts preserve every physical unit.
                end_byte = byte;
                end_unit = unit;
                self.byte = byte + 1;
                self.unit = unit + 1;
                if scalar == '\r'
                    && self
                        .cursor
                        .peek(document)
                        .is_some_and(|value| value.0 == '\n')
                {
                    // Consume one LF with its preceding CR even when their bytes belong to different leaves.
                    self.cursor.next(document);
                    self.byte += 1;
                    self.unit += 1;
                }
                break;
            }
        }
        let line = SourceLine {
            number: self.number,
            bytes: (start_byte, end_byte),
            units: (start_unit, end_unit),
        };
        self.number += 1;
        Some(line)
    }
}

/// Match JavaScript's command whitespace while optionally permitting literal quote markers.
pub(super) fn range_is_whitespace(
    document: &MarkdownDocumentBuffer,
    range: Range<usize>,
    quotes: bool,
) -> Result<bool, MarkdownDocumentBufferError> {
    let mut cursor = SourceCursor::new(
        document,
        document.byte_offset_for_utf16(range.start)?..document.byte_offset_for_utf16(range.end)?,
        range.start,
    );
    while let Some((scalar, _, _)) = cursor.next(document) {
        // Quotes are ignored only for the baseline blank-line/container tests that explicitly allow them.
        if !indentation_whitespace(scalar)
            && !matches!(scalar, '\r' | '\n')
            && !(quotes && scalar == '>')
        {
            return Ok(false);
        }
    }
    Ok(true)
}
