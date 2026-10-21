//! Compact immutable directories for original 4-KiB logical pieces. Grouping
//! physical leaves changes neither read-lease limits nor edited-piece boundaries.

use super::*;

pub(in crate::markdown_document_buffer) const ORIGINAL_SPAN_PIECES: usize = 16;
// Charge locating the shared directory start and visiting up to 16 records;
// directory work must not disappear inside one reported AVL traversal step.
#[cfg(feature = "s2-table-window-prototype")]
pub(in crate::markdown_document_buffer) const ORIGINAL_SPAN_METADATA_STEPS: usize =
    ORIGINAL_SPAN_PIECES * 2;

#[derive(Clone, Copy, Debug, Default)]
struct CompactMetrics {
    bytes: u16,
    utf16: u16,
    characters: u16,
    words: u16,
    lines: u16,
    terminators: u16,
    first: u16,
    last: u16,
    flags: u16,
    // Byte-encode the u32 set to avoid alignment padding for every 4-KiB piece.
    bidi_classes: [u8; 4],
    // Two bytes per logical piece describe the character-domain envelope; this
    // directory contains no copied text or per-character bitset.
    printable_ascii: [u8; 2],
}
impl CompactMetrics {
    fn capture(metrics: MarkdownDocumentMetrics) -> Result<Self, MarkdownDocumentBufferError> {
        // Compress only a nonempty local piece up to 4 KiB. Whole-tree aggregates
        // remain `usize`, so local compression adds no file-length limit.
        if metrics.byte_len == 0 || metrics.byte_len > ORIGINAL_PIECE_BYTES {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let small = |value: usize| {
            // Every compact field must be representable. Reject inconsistent metrics
            // rather than truncating values to obtain a smaller index.
            u16::try_from(value).map_err(|_| MarkdownDocumentBufferError::RangeOutsideDocument)
        };
        let mut flags = 0_u16;
        for (bit, value) in [
            metrics.starts_in_word,
            metrics.ends_in_word,
            metrics.starts_with_line_feed,
            metrics.ends_with_carriage_return,
            metrics.contains_carriage_return,
            metrics.contains_crlf,
            metrics.contains_ascii_less_than,
            metrics.all_printable_ascii.is_printable(),
            metrics.all_printable_ascii.has_letters() == Some(true),
        ]
        .into_iter()
        .enumerate()
        {
            // Decoding uses this exact bit order. Cross-piece words, CRLF boundaries,
            // and syntax properties still follow the original aggregate join rules.
            flags |= u16::from(value) << bit;
        }
        // Bits 9-14 preserve six exact separator classes without replacing existing
        // boundary facts or the printable-ASCII three-state classification.
        flags |= u16::from(metrics.unicode_properties.separators) << 9;
        // Terminators retain offset-plus-one encoding, with zero meaning absent.
        // Raw CR/LF facts must not collapse into a physical-line count.
        Ok(Self {
            bytes: small(metrics.byte_len)?,
            utf16: small(metrics.utf16_len)?,
            characters: small(metrics.character_count)?,
            words: small(metrics.word_count)?,
            lines: small(metrics.line_breaks)?,
            terminators: small(metrics.text_checking_terminators)?,
            first: small(
                metrics
                    .first_text_checking_terminator
                    .map_or(0, NonZeroUsize::get),
            )?,
            last: small(
                metrics
                    .last_text_checking_terminator
                    .map_or(0, NonZeroUsize::get),
            )?,
            flags,
            bidi_classes: metrics.unicode_properties.bidi_classes.to_le_bytes(),
            printable_ascii: [
                metrics.unicode_properties.printable_ascii_min,
                metrics.unicode_properties.printable_ascii_max,
            ],
        })
    }
    fn full(self) -> MarkdownDocumentMetrics {
        // Decode one already validated small piece. Higher aggregates remain
        // `usize` and are not constrained by these local u16 fields.
        let flag = |bit: u32| self.flags & (1_u16 << bit) != 0;
        MarkdownDocumentMetrics {
            byte_len: usize::from(self.bytes),
            utf16_len: usize::from(self.utf16),
            character_count: usize::from(self.characters),
            word_count: usize::from(self.words),
            starts_in_word: flag(0),
            ends_in_word: flag(1),
            line_breaks: usize::from(self.lines),
            text_checking_terminators: usize::from(self.terminators),
            first_text_checking_terminator: NonZeroUsize::new(usize::from(self.first)),
            last_text_checking_terminator: NonZeroUsize::new(usize::from(self.last)),
            starts_with_line_feed: flag(2),
            ends_with_carriage_return: flag(3),
            contains_carriage_return: flag(4),
            contains_crlf: flag(5),
            contains_ascii_less_than: flag(6),
            all_printable_ascii: if !flag(7) {
                PrintableAsciiClass::NonPrintable
            } else if flag(8) {
                PrintableAsciiClass::WithLetters
            } else {
                PrintableAsciiClass::WithoutLetters
            }, // Bit 8 still stores the letter fact; added properties preserve the three states.
            unicode_properties: MarkdownUnicodeProperties {
                bidi_classes: u32::from_le_bytes(self.bidi_classes),
                separators: ((self.flags >> 9) & 0x3f) as u8,
                printable_ascii_min: self.printable_ascii[0],
                printable_ascii_max: self.printable_ascii[1],
            },
        }
    }
}

/// A separate `Arc` directory keeps the node's largest variant small. Every
/// entry describes contiguous scalar-aligned pieces of one original payload.
#[derive(Debug)]
struct OriginalSpanBlock {
    byte_start: usize,
    records: [CompactMetrics; ORIGINAL_SPAN_PIECES],
    count: u8,
}

#[derive(Clone, Debug)]
pub(in crate::markdown_document_buffer) struct OriginalSpan {
    block: Arc<OriginalSpanBlock>,
    start: u8,
    end: u8,
    pub(in crate::markdown_document_buffer) metrics: MarkdownDocumentMetrics,
}

pub(in crate::markdown_document_buffer) struct OriginalSpanPosition {
    pub(in crate::markdown_document_buffer) piece: MarkdownPiece,
    /// Position within the hit piece, measured in the caller's byte or UTF-16 units.
    pub(in crate::markdown_document_buffer) local: usize,
    pub(in crate::markdown_document_buffer) prefix: MarkdownDocumentMetrics,
    pub(in crate::markdown_document_buffer) ordinal: u8,
}

impl OriginalSpan {
    /// Word cursors charge original-directory prefix records per quantum instead
    /// of hiding up to 16 prefix records inside a piece lookup.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_document_buffer) fn word_block_byte_start(&self) -> usize {
        self.block.byte_start
    }

    /// Decodes one actual compact record. Directory prefixes locate physical bytes
    /// only and grant no authority to borrow a document range.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_document_buffer) fn word_record_metrics(
        &self,
        ordinal: u8,
    ) -> Result<MarkdownDocumentMetrics, MarkdownDocumentBufferError> {
        if ordinal >= self.block.count {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        Ok(self.block.records[usize::from(ordinal)].full())
    }

    fn new(
        block: Arc<OriginalSpanBlock>,
        start: u8,
        end: u8,
    ) -> Result<Self, MarkdownDocumentBufferError> {
        if start >= end || end > block.count {
            // Reject empty spans and ranges beyond the real directory. `None`
            // represents an empty tree without manufacturing a nonempty leaf.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        // Reuse the original directory for subspans. At most 16 cached metrics are
        // joined, without reading document bytes or constructing temporary trees.
        let metrics = block.records[usize::from(start)..usize::from(end)]
            .iter()
            .fold(MarkdownDocumentMetrics::default(), |sum, value| {
                MarkdownDocumentMetrics::joined(sum, value.full())
            });
        Ok(Self {
            block,
            start,
            end,
            metrics,
        })
    }
    pub(in crate::markdown_document_buffer) fn record_range(&self) -> Range<u8> {
        self.start..self.end
    }
    pub(in crate::markdown_document_buffer) fn piece(
        &self,
        ordinal: u8,
    ) -> Result<MarkdownPiece, MarkdownDocumentBufferError> {
        if !self.record_range().contains(&ordinal) {
            // Shared directory ownership does not grant records outside this span
            // or across logical boundaries established by edits.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        // Sum metadata across at most 16 entries. This reads no source bytes,
        // expands no temporary tree, and allocates no per-piece `Arc`.
        let prefix =
            self.block.records[..usize::from(ordinal)]
                .iter()
                .try_fold(0usize, |sum, entry| {
                    sum.checked_add(usize::from(entry.bytes))
                        .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)
                })?;
        let metrics = self.block.records[usize::from(ordinal)].full();
        Ok(MarkdownPiece {
            source: MarkdownPieceSource::Original,
            byte_start: self
                .block
                .byte_start
                .checked_add(prefix)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?,
            byte_len: metrics.byte_len,
            metrics,
        })
    }
    pub(in crate::markdown_document_buffer) fn locate_byte(
        &self,
        offset: usize,
        include_end: bool,
    ) -> Result<OriginalSpanPosition, MarkdownDocumentBufferError> {
        self.locate(offset, include_end, |metrics| metrics.byte_len)
    }
    pub(in crate::markdown_document_buffer) fn locate_utf16(
        &self,
        offset: usize,
        include_end: bool,
    ) -> Result<OriginalSpanPosition, MarkdownDocumentBufferError> {
        self.locate(offset, include_end, |metrics| metrics.utf16_len)
    }
    fn locate(
        &self,
        mut offset: usize,
        include_end: bool,
        measure: impl Fn(MarkdownDocumentMetrics) -> usize,
    ) -> Result<OriginalSpanPosition, MarkdownDocumentBufferError> {
        let mut prefix = MarkdownDocumentMetrics::default();
        // Locate the shared directory's actual source start once, then advance
        // forward without recomputing preceding entries for every candidate.
        let mut byte_start = self.piece(self.start)?.byte_start;
        for ordinal in self.record_range() {
            let metrics = self.block.records[usize::from(ordinal)].full();
            let piece = MarkdownPiece {
                source: MarkdownPieceSource::Original,
                byte_start,
                byte_len: metrics.byte_len,
                metrics,
            };
            let length = measure(metrics);
            if offset < length || (include_end && offset == length) {
                // Editing may include a preceding piece's end, while unit reads must
                // land inside real bytes. Callers explicitly choose that contract.
                return Ok(OriginalSpanPosition {
                    piece,
                    local: offset,
                    prefix,
                    ordinal,
                });
            }
            // Skipping a complete piece joins cached metrics only. Source-byte
            // coordinates advance independently of the queried coordinate unit.
            offset -= length;
            byte_start = byte_start
                .checked_add(metrics.byte_len)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
            prefix = MarkdownDocumentMetrics::joined(prefix, metrics);
        }
        // This includes forbidden EOF unit reads. Never substitute the last
        // record or clamp an invalid position to avoid a real rejection.
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    }
    pub(in crate::markdown_document_buffer) fn subrange(
        &self,
        range: Range<u8>,
    ) -> Result<PieceTreeLink, MarkdownDocumentBufferError> {
        if range.start > range.end || range.start < self.start || range.end > self.end {
            // A subrange can only narrow authority; it cannot regain directory
            // entries already removed from this logical span.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if range.is_empty() {
            // An empty side owns no leaf, directory `Arc`, or document-byte resource.
            return Ok(None);
        }
        if range.end - range.start == 1 {
            // A single piece uses the ordinary leaf representation rather than
            // retaining an entire directory block for one record.
            return Ok(Some(Arc::new(MarkdownPieceTreeNode::Leaf(
                self.piece(range.start)?,
            ))));
        }
        // Multiple pieces allocate only a fixed span; prior roots, undo history,
        // and frozen readers share the same immutable directory records.
        let span = Self::new(Arc::clone(&self.block), range.start, range.end)?;
        Ok(Some(Arc::new(MarkdownPieceTreeNode::OriginalSpan(span))))
    }
}

#[derive(Default)]
pub(in crate::markdown_document_buffer) struct OriginalSpanBuilder {
    records: [CompactMetrics; ORIGINAL_SPAN_PIECES],
    count: u8,
    bytes: usize,
    byte_start: usize,
}
impl OriginalSpanBuilder {
    pub(in crate::markdown_document_buffer) fn push(
        &mut self,
        piece: MarkdownPiece,
        output: &mut Vec<Arc<MarkdownPieceTreeNode>>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        let compact = CompactMetrics::capture(piece.metrics)?;
        if piece.source != MarkdownPieceSource::Original
            || piece.byte_len != piece.metrics.byte_len
            || (self.count != 0
                && self.byte_start.checked_add(self.bytes) != Some(piece.byte_start))
        {
            // Group only contiguous, consistently measured pieces from one original
            // payload. Edited pieces retain their append-segment representation.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if self.count == 0 {
            // The first piece fixes this group's source start; every subsequent
            // piece must pass the contiguity check above.
            self.byte_start = piece.byte_start;
        }
        // Store compact metadata only, retaining neither the current source lease
        // nor slices pointing into document pages.
        self.records[usize::from(self.count)] = compact;
        self.count += 1;
        self.bytes = self
            .bytes
            .checked_add(piece.byte_len)
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        if usize::from(self.count) == ORIGINAL_SPAN_PIECES {
            // Publish one physical leaf at 16 records. Construction never retains
            // a whole-document vector of original logical pieces.
            self.flush(output)?;
        }
        Ok(())
    }
    pub(in crate::markdown_document_buffer) fn flush(
        &mut self,
        output: &mut Vec<Arc<MarkdownPieceTreeNode>>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if self.count == 0 {
            // EOF after a full group needs no empty leaf; repeated flush is harmless.
            return Ok(());
        }
        if self.count == 1 {
            // Tiny documents and a single trailing piece reuse the ordinary leaf
            // layout, avoiding an unnecessary directory allocation.
            let metrics = self.records[0].full();
            output.push(Arc::new(MarkdownPieceTreeNode::Leaf(MarkdownPiece {
                source: MarkdownPieceSource::Original,
                byte_start: self.byte_start,
                byte_len: metrics.byte_len,
                metrics,
            })));
        } else {
            // Allocate multi-piece directories outside the node enum so ordinary
            // edited nodes do not all inherit the directory's larger payload.
            let block = Arc::new(OriginalSpanBlock {
                byte_start: self.byte_start,
                records: std::mem::take(&mut self.records),
                count: self.count,
            });
            output.push(Arc::new(MarkdownPieceTreeNode::OriginalSpan(
                OriginalSpan::new(block, 0, self.count)?,
            )));
        }
        // Clear only bounded builder state. The new root owns directory lifetime;
        // the original backing separately owns document-byte lifetime.
        self.count = 0;
        self.bytes = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_metrics_roundtrip_retains_crlf_words_and_raw_terminators() {
        for text in [
            "0123 ~[](){}",
            "~Printable0123 ",
            "a\r",
            "\nb \u{2029}😀<e\u{301}",
            "\r\n",
            "_1甲",
            "\u{85}\u{2028}",
        ] {
            let actual = MarkdownDocumentMetrics::from_text(text);
            assert_eq!(CompactMetrics::capture(actual).unwrap().full(), actual);
        }
    }

    #[test]
    fn subspans_share_one_actual_directory_and_preserve_local_prefixes() {
        let text = "a😀\r\nb";
        let metrics = MarkdownDocumentMetrics::from_text(text);
        let mut builder = OriginalSpanBuilder::default();
        let mut nodes = Vec::new();
        for ordinal in 0..16 {
            builder
                .push(
                    MarkdownPiece {
                        source: MarkdownPieceSource::Original,
                        byte_start: 512 + ordinal * text.len(),
                        byte_len: text.len(),
                        metrics,
                    },
                    &mut nodes,
                )
                .unwrap();
        }
        assert_eq!(nodes.len(), 1);
        let MarkdownPieceTreeNode::OriginalSpan(span) = nodes[0].as_ref() else {
            panic!("必须是实际合组Span")
        };
        let Some(left) = span.subrange(0..8).unwrap() else {
            panic!("真实前缀不能为空")
        };
        let Some(right) = span.subrange(8..16).unwrap() else {
            panic!("真实后缀不能为空")
        };
        let (MarkdownPieceTreeNode::OriginalSpan(left), MarkdownPieceTreeNode::OriginalSpan(right)) =
            (left.as_ref(), right.as_ref())
        else {
            panic!("多片前后缀必须继续共享小目录")
        };
        assert!(Arc::ptr_eq(&span.block, &left.block) && Arc::ptr_eq(&span.block, &right.block));
        let position = right.locate_utf16(metrics.utf16_len + 2, false).unwrap();
        assert_eq!(position.local, 2);
        assert_eq!(position.prefix, metrics);
        assert_eq!(position.piece.byte_start, 512 + 9 * text.len());
        assert_eq!(
            MarkdownDocumentMetrics::joined(left.metrics, right.metrics),
            span.metrics
        );
    }
    #[test]
    fn compact_records_preserve_complete_ascii_domain_and_character_metrics() {
        for text in ["", "XaXa", "fi", "中文Xa\r\n", "!\u{301}😀~"] {
            if text.is_empty() {
                continue;
            } // Compact directories contain no empty pieces; tree identity handles empty text.
            let metrics = MarkdownDocumentMetrics::from_text(text);
            assert_eq!(CompactMetrics::capture(metrics).unwrap().full(), metrics);
        }
        println!(
            "ascii_domain_compact_layout record_bytes={} block_bytes={} pieces_per_block={}",
            std::mem::size_of::<CompactMetrics>(),
            std::mem::size_of::<OriginalSpanBlock>(),
            ORIGINAL_SPAN_PIECES
        );
    }
}
