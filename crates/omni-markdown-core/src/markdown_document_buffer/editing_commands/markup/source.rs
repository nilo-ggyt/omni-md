//! Borrowless scalar and physical-line traversal over canonical pieces, with fixed transient metadata.

use super::*;

/// 按最多4KiB读取一个原piece区间；true仅表示调用方已找到终止事实，仍须先真实回收成功。
pub(super) fn scan_piece(
    document: &MarkdownDocumentBuffer,
    source: &super::super::super::formatting::streaming::SourcePieceRange,
    mut visit: impl FnMut(&[u8]) -> Result<bool, MarkdownDocumentBufferError>,
) -> Result<bool, MarkdownDocumentBufferError> {
    document.original_backing_health()?;
    if source.local.start > source.local.end || source.local.end > source.piece.byte_len {
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    }
    let mut offset = source.local.start;
    while offset < source.local.end {
        let length = (source.local.end - offset).min(ORIGINAL_PIECE_BYTES);
        let mut window = source.piece.clone();
        window.byte_start = window
            .byte_start
            .checked_add(offset)
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        window.byte_len = length; // 只作byte lease描述，不把旧metrics作为新树叶发布。
        if document.with_piece_bytes(&window, &mut visit)? {
            return Ok(true); // callback的终止事实只有finish成功后才可交给语法调用方。
        }
        offset += length;
    }
    Ok(false)
}

/// A scalar cursor owns immutable piece metadata, so each read can end its byte borrow immediately.
pub(super) struct SourceCursor {
    pieces: SourcePieceCursor,
    piece: Option<super::super::super::formatting::streaming::SourcePieceRange>,
    byte: usize,
    unit: usize,
    pending: Option<(char, usize, usize)>,
    scratch: [u8; ORIGINAL_PIECE_BYTES],
    scratch_at: usize,
    scratch_len: usize,
    failed: Option<MarkdownDocumentBufferError>,
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
            scratch: [0; ORIGINAL_PIECE_BYTES],
            scratch_at: 0,
            scratch_len: 0,
            failed: None,
        }
    }

    /// Decode one complete scalar; source leaves already preserve UTF-8 boundaries.
    pub(super) fn next(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<(char, usize, usize)>, MarkdownDocumentBufferError> {
        if let Some(error) = self.failed {
            return Err(error); // 原lease失败永久撤销此cursor，不能在后来一次peek中伪装成EOF。
        }
        if let Err(error) = document.original_backing_health() {
            self.failed = Some(error);
            return Err(error); // 其他原读取已撤权时，也不能凭本cursor的旧scratch继续发布字符。
        }
        if let Some(value) = self.pending.take() {
            // Lookahead already advanced the physical cursor and transfers its saved scalar exactly once.
            return Ok(Some(value));
        }
        let result = self.decode(document);
        if let Err(error) = result {
            self.failed = Some(error);
        }
        result
    }

    fn decode(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<(char, usize, usize)>, MarkdownDocumentBufferError> {
        let start_byte = self.byte;
        let start_unit = self.unit;
        let Some(first) = self.byte(document)? else {
            return Ok(None);
        };
        let width = match first {
            0..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Err(MarkdownDocumentBufferError::InvalidUtf8Document),
        };
        let mut scalar = [first, 0, 0, 0];
        for target in &mut scalar[1..width] {
            *target = self
                .byte(document)?
                .ok_or(MarkdownDocumentBufferError::InvalidUtf8Document)?;
        } // 4KiB边界可以切入scalar，但只保至多4字节残余，不跨lease借用原内存。
        let scalar = std::str::from_utf8(&scalar[..width])
            .map_err(|_| MarkdownDocumentBufferError::InvalidUtf8Document)?
            .chars()
            .next()
            .ok_or(MarkdownDocumentBufferError::InvalidUtf8Document)?;
        self.unit = self
            .unit
            .checked_add(scalar.len_utf16())
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        Ok(Some((scalar, start_byte, start_unit)))
    }

    fn byte(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<u8>, MarkdownDocumentBufferError> {
        loop {
            if self.scratch_at < self.scratch_len {
                let byte = self.scratch[self.scratch_at];
                self.scratch_at += 1;
                self.byte = self
                    .byte
                    .checked_add(1)
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                return Ok(Some(byte)); // 一个已回收成功的4KiB窗口供多个scalar复用，不逐scalar msync。
            }
            if let Some(piece) = &mut self.piece
                && piece.local.start < piece.local.end
            {
                let length = (piece.local.end - piece.local.start).min(self.scratch.len());
                let mut window = piece.piece.clone();
                window.byte_start = window
                    .byte_start
                    .checked_add(piece.local.start)
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                window.byte_len = length;
                document.with_piece_bytes(&window, |bytes| {
                    if bytes.len() != length {
                        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                    }
                    self.scratch[..length].copy_from_slice(bytes);
                    Ok(())
                })?; // 只有真实finish成功才发布scratch长度；失败不能消费复制出的前缀。
                piece.local.start += length;
                self.scratch_at = 0;
                self.scratch_len = length;
                continue;
            }
            self.piece = self.pieces.next_piece();
            if self.piece.is_none() {
                return Ok(None);
            } // 只有真实元数据遍历终点才是EOF。
        }
    }

    /// Inspect a scalar without losing it at a CRLF or marker grammar boundary.
    pub(super) fn peek(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<(char, usize, usize)>, MarkdownDocumentBufferError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if let Err(error) = document.original_backing_health() {
            self.failed = Some(error);
            return Err(error); // 缓存的lookahead也服从同一原backing首错，不重签已知前缀。
        }
        if self.pending.is_none() {
            // Decode once; the next consumer takes the same saved scalar and original coordinates.
            self.pending = self.next(document)?;
        }
        Ok(self.pending)
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
    pub(super) fn whitespace(
        self,
        document: &MarkdownDocumentBuffer,
        quotes: bool,
    ) -> Result<bool, MarkdownDocumentBufferError> {
        let mut cursor = SourceCursor::new(document, self.bytes.0..self.bytes.1, self.units.0);
        while let Some((scalar, _, _)) = cursor.next(document)? {
            // Ordinary content terminates immediately; only a genuinely blank giant line is fully scanned.
            if !indentation_whitespace(scalar) && !(quotes && scalar == '>') {
                // Any body content disproves blank-line status before the remaining text needs decoding.
                return Ok(false);
            }
        }
        Ok(true)
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
    pub(super) fn next(
        &mut self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<SourceLine>, MarkdownDocumentBufferError> {
        document.original_backing_health()?; // 已观察的原回收失败不能被空行/EOF短路吞掉。
        if self.remaining == 0 {
            // A final empty source line is represented by the aggregate count, never a synthetic sentinel.
            return Ok(None);
        }
        let start_byte = self.byte;
        let start_unit = self.unit;
        let mut end_byte = document.byte_len();
        let mut end_unit = document.utf16_len();
        while let Some((scalar, byte, unit)) = self.cursor.next(document)? {
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
                        .peek(document)?
                        .is_some_and(|value| value.0 == '\n')
                {
                    // Consume one LF with its preceding CR even when their bytes belong to different leaves.
                    self.cursor.next(document)?;
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
        self.remaining -= 1; // 失败的行不消费remaining；后续调用继续返回同一cursor首错。
        Ok(Some(line))
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
    while let Some((scalar, _, _)) = cursor.next(document)? {
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

#[cfg(test)]
mod cursor_tests {
    use super::*;

    fn original(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source-cursor.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        (directory, document)
    }

    #[test]
    fn edited_piece_crosses_scratch_inside_scalar_without_changing_global_coordinates() {
        let (_directory, mut document) = original("");
        let text = format!("{}😀\r\n中e\u{301}", "a".repeat(4095));
        let units = text.encode_utf16().count();
        document
            .insert_utf16(
                0,
                &text,
                MarkdownTextSelection {
                    anchor_utf16: units,
                    head_utf16: units,
                },
            )
            .unwrap();
        // 插入段的真实叶可超过4KiB；emoji首字节恰在4095，确实跨本cursor的scratch边界。
        let mut cursor = SourceCursor::new(&document, 0..text.len(), 0);
        let mut unit = 0;
        for (byte, scalar) in text.char_indices() {
            let expected = Some((scalar, byte, unit));
            assert_eq!(cursor.peek(&document).unwrap(), expected);
            assert_eq!(cursor.peek(&document).unwrap(), expected); // 相同lookahead不重复消费原坐标。
            assert_eq!(cursor.next(&document).unwrap(), expected);
            unit += scalar.len_utf16();
        }
        assert!(cursor.next(&document).unwrap().is_none());
        assert_eq!(unit, units);
    }

    #[test]
    fn crlf_split_at_scratch_boundary_keeps_exact_line_ranges() {
        let (_directory, mut document) = original("");
        let text = format!("{}\r\n😀", "a".repeat(4095));
        let units = text.encode_utf16().count();
        document
            .insert_utf16(
                0,
                &text,
                MarkdownTextSelection {
                    anchor_utf16: units,
                    head_utf16: units,
                },
            )
            .unwrap();
        let mut lines = SourceLines::new(&document);
        let first = lines.next(&document).unwrap().unwrap();
        assert_eq!(
            (first.number, first.bytes, first.units),
            (0, (0, 4095), (0, 4095))
        );
        let second = lines.next(&document).unwrap().unwrap();
        assert_eq!(
            (second.number, second.bytes, second.units),
            (1, (4097, 4101), (4097, 4099))
        );
        assert!(lines.next(&document).unwrap().is_none());
        assert_eq!(newline_separator(&document).unwrap(), "\r\n");
    }

    #[test]
    fn actual_lease_budget_failure_is_sticky_and_never_becomes_eof_or_committed_edit() {
        let (_directory, mut document) = original("- a");
        let backing = Arc::clone(document.original_snapshot.as_ref().unwrap());
        let leases = (0..32)
            .map(|_| backing.lease(0..1).unwrap())
            .collect::<Vec<_>>();
        let mut cursor = SourceCursor::new(&document, 0..document.byte_len(), 0);
        let failure = cursor.next(&document).unwrap_err();
        assert_eq!(
            failure,
            MarkdownDocumentBufferError::OriginalReadLeaseLimitExceeded
        );
        assert_eq!(cursor.peek(&document).unwrap_err(), failure);
        let generation = document.generation();
        let root = document.root.clone();
        let used = document.edits.used_bytes;
        let result = apply_markup_command(
            &mut document,
            MarkdownEditingCommand::InsertNewline,
            MarkdownTextSelection {
                anchor_utf16: 3,
                head_utf16: 3,
            },
        );
        assert!(matches!(
            result,
            Err(MarkdownDocumentBufferError::OriginalReadLeaseLimitExceeded)
        ));
        assert_eq!(document.generation(), generation);
        assert_eq!(document.edits.used_bytes, used);
        assert!(Arc::ptr_eq(
            root.as_ref().unwrap(),
            document.root.as_ref().unwrap()
        ));
        drop(leases);
        assert_eq!(cursor.next(&document).unwrap_err(), failure); // 后来可用也不能复活已失败cursor。
        assert!(document.undo.is_empty() && document.redo.is_empty());
    }
}
