//! 原始4KiB逻辑片的不可变小目录；物理树叶合组，正文借用与编辑片大小保持原合同。

use super::*;

pub(in crate::markdown_document_buffer) const ORIGINAL_SPAN_PIECES: usize = 16;
// 路径成本含定位共享目录起点与至多16项查找；不把目录工作藏进一次AVL步骤。
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
    // 按字节保存u32集合，避免为每个4KiB逻辑片引入额外对齐空洞。
    bidi_classes: [u8; 4],
    // 每个4KiB逻辑片仅增两字节域包络；原目录没有正文副本或字符位集。
    printable_ascii: [u8; 2],
}
impl CompactMetrics {
    fn capture(metrics: MarkdownDocumentMetrics) -> Result<Self, MarkdownDocumentBufferError> {
        // 仅局部≤4KiB片可压缩；全树metrics继续usize，不因此引入文件长度上限。
        if metrics.byte_len == 0 || metrics.byte_len > ORIGINAL_PIECE_BYTES {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let small = |value: usize| {
            // 紧凑字段必须逐项可表示；内部计量失配立即拒绝，不用截断换取较小索引。
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
            // 位序在恢复处一一对应；跨片的单词、CRLF与语法风险仍交原joined规则处理。
            flags |= u16::from(value) << bit;
        }
        // 原flags第9..14位保存六种精确分隔字符，未挤占既有边界与ASCII三态事实。
        flags |= u16::from(metrics.unicode_properties.separators) << 9;
        // 终止符保存原offset+1编码，零仍表示None；不能把CR与LF合并成物理行计数。
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
        // 只恢复一个已验证小片的值；所有上层累计仍为usize，不受u16局部压缩限制。
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
            }, // 原第8位继续保存字母事实；新属性不改变原三态判定。
            unicode_properties: MarkdownUnicodeProperties {
                bidi_classes: u32::from_le_bytes(self.bidi_classes),
                separators: ((self.flags >> 9) & 0x3f) as u8,
                printable_ascii_min: self.printable_ascii[0],
                printable_ascii_max: self.printable_ascii[1],
            },
        }
    }
}

/// 独立Arc目录不放进Node最大variant；所有条目只描述同一原始payload的连续标量片。
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
    /// 相对命中小片的位置；单位与调用byte/UTF16查询一致。
    pub(in crate::markdown_document_buffer) local: usize,
    pub(in crate::markdown_document_buffer) prefix: MarkdownDocumentMetrics,
    pub(in crate::markdown_document_buffer) ordinal: u8,
}

impl OriginalSpan {
    /// Word游标按量子逐条计量原目录前缀，不调用piece里至多16条的内部前缀累计。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_document_buffer) fn word_block_byte_start(&self) -> usize {
        self.block.byte_start
    }

    /// 一次只恢复一条真实compact记录；原目录前缀只用于物理字节定位，不授予正文范围。
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
            // 不允许空或越过真实目录尾的Span；空树由None表达，避免伪造非空叶。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        // 子Span重用原目录范围；最多16个缓存计量合并，不读取正文或构造临时子树。
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
            // 目录虽共享，当前Span只能给出自己仍拥有的条目，不能越过编辑后的逻辑边界。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        // 目录至多16项，只有计数相加；不读取Source、不展开临时树或分配每小片Arc。
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
        // 先定位共享目录的实际源起点，再单次前行；避免每个候选重复累计先前条目。
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
                // 编辑边界可归前片尾，单元读取必须落在真实片内；两种合同由调用者显式选择。
                return Ok(OriginalSpanPosition {
                    piece,
                    local: offset,
                    prefix,
                    ordinal,
                });
            }
            // 完整跳过当前小片时只累计缓存值；byte源坐标与所查单位分别前进。
            offset -= length;
            byte_start = byte_start
                .checked_add(metrics.byte_len)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
            prefix = MarkdownDocumentMetrics::joined(prefix, metrics);
        }
        // 包括不允许的EOF单元读取；不能用最后一个条目或截断坐标代替真实拒绝。
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    }
    pub(in crate::markdown_document_buffer) fn subrange(
        &self,
        range: Range<u8>,
    ) -> Result<PieceTreeLink, MarkdownDocumentBufferError> {
        if range.start > range.end || range.start < self.start || range.end > self.end {
            // 子范围只能缩小当前来源权限，不能重新取回已从该Span删除的目录条目。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if range.is_empty() {
            // 空的一侧不占树叶、目录Arc或任何正文资源。
            return Ok(None);
        }
        if range.end - range.start == 1 {
            // 单小片沿原Leaf路径；不为仅一个条目额外保留整个Block强边。
            return Ok(Some(Arc::new(MarkdownPieceTreeNode::Leaf(
                self.piece(range.start)?,
            ))));
        }
        // 多小片仅新建固定Span值，目录Arc与旧root/Undo/Frozen继续共享不可变记录。
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
            // 合组仅接受同一原始payload中连续且计量一致的小片；编辑片保持原追加段路径。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if self.count == 0 {
            // 新组的源起点只由第一片确定；后续片必须通过上面的连续性检查。
            self.byte_start = piece.byte_start;
        }
        // 只保存紧凑元数据，不保留当前Source借用或指向正文页的切片。
        self.records[usize::from(self.count)] = compact;
        self.count += 1;
        self.bytes = self
            .bytes
            .checked_add(piece.byte_len)
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        if usize::from(self.count) == ORIGINAL_SPAN_PIECES {
            // 固定16项即发布一个物理树叶，构造期间也不累计整份原始小片Vec。
            self.flush(output)?;
        }
        Ok(())
    }
    pub(in crate::markdown_document_buffer) fn flush(
        &mut self,
        output: &mut Vec<Arc<MarkdownPieceTreeNode>>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if self.count == 0 {
            // 恰好满组后的EOF无需额外空叶，重复flush也保持无副作用。
            return Ok(());
        }
        if self.count == 1 {
            // 极小文稿或单个尾片复用原Leaf布局，避免小文稿为目录增加一次Arc分配。
            let metrics = self.records[0].full();
            output.push(Arc::new(MarkdownPieceTreeNode::Leaf(MarkdownPiece {
                source: MarkdownPieceSource::Original,
                byte_start: self.byte_start,
                byte_len: metrics.byte_len,
                metrics,
            })));
        } else {
            // 多片目录在Node之外独立分配，防止所有普通编辑节点的枚举载荷一起变大。
            let block = Arc::new(OriginalSpanBlock {
                byte_start: self.byte_start,
                records: std::mem::take(&mut self.records),
                count: self.count,
            });
            output.push(Arc::new(MarkdownPieceTreeNode::OriginalSpan(
                OriginalSpan::new(block, 0, self.count)?,
            )));
        }
        // 发布后仅清空有限构造状态；真实目录与正文寿命分别归新root与原Backing所有。
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
            } // 真实compact目录不存空片，空集合由树默认聚合负责。
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
