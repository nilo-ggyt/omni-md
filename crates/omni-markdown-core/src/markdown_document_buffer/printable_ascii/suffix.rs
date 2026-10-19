//! 同一冻结树的可打印ASCII后缀证明；只保树路径和一个UTF8残余，不捕获第二份正文。

use super::*;

enum Pending {
    Node(Arc<MarkdownPieceTreeNode>, usize),
    Span {
        span: OriginalSpan,
        ordinal: u8,
        base: usize,
        byte_edge: usize,
        prefix: Option<u8>,
    },
    Leaf {
        piece: MarkdownPiece,
        utf16: usize,
        byte: usize,
        pending: [u8; 4],
        used: usize,
        needed: usize,
    },
}

/// 起点到调用方真实端点的原树事实；Weak只认证原Frozen，路径不会增加正文或Reader所有权。
pub(crate) struct FrozenPrintableAsciiSuffix {
    origin: std::sync::Weak<FrozenText>,
    pending: Vec<Pending>,
    range: Range<usize>,
    rejected: bool,
    answer: Option<bool>,
    minimum: u8,
    maximum: u8,
}

pub(crate) struct FrozenPrintableAsciiSuffixStep {
    pub(crate) bytes: usize,
    pub(crate) scalars: usize,
    pub(crate) metadata: usize,
    pub(crate) maximum_borrow: usize,
    pub(crate) answer: Option<bool>,
    // 只有answer=true才授完整范围的保守包络；部分正节点可贡献更宽域，不能据此反推精确字符集合。
    pub(crate) minimum: u8,
    pub(crate) maximum: u8,
}

impl FrozenPrintableAsciiSuffix {
    pub(in super::super) fn new(snapshot: &MarkdownTextSnapshot, range: Range<usize>) -> Self {
        Self {
            origin: Arc::downgrade(&snapshot.text),
            pending: snapshot
                .text
                .root
                .iter()
                .map(|root| Pending::Node(Arc::clone(root), 0))
                .collect(),
            range,
            rejected: false,
            answer: None,
            minimum: 0,
            maximum: 0,
        }
    }

    fn include(&mut self, minimum: u8, maximum: u8) {
        if minimum != 0 {
            self.minimum = if self.minimum == 0 {
                minimum
            } else {
                self.minimum.min(minimum)
            };
            self.maximum = self.maximum.max(maximum);
        } // 空节点没有字符；不能把空集合哨兵当作范围内真实NUL。
    }

    /// 所有目录收费后才展开；部分混合叶可按一字节量子续接，实际lease在本步返回前结束。
    pub(in super::super) fn advance(
        &mut self,
        snapshot: &MarkdownTextSnapshot,
        maximum_bytes: usize,
        maximum_scalars: usize,
        maximum_metadata: usize,
    ) -> Result<FrozenPrintableAsciiSuffixStep, MarkdownDocumentBufferError> {
        if !std::sync::Weak::ptr_eq(&self.origin, &Arc::downgrade(&snapshot.text)) {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        } // 同文本的另一Frozen不能替换实际来源及其资源健康。
        let mut step = FrozenPrintableAsciiSuffixStep {
            bytes: 0,
            scalars: 0,
            metadata: 0,
            maximum_borrow: 0,
            answer: self.answer,
            minimum: self.minimum,
            maximum: self.maximum,
        };
        while self.answer.is_none() {
            let Some(next) = self.pending.last() else {
                self.answer = Some(!self.rejected);
                break; // 原相交路径真正耗尽才取得完整范围事实，配额耗尽绝不产生正证明。
            };
            if matches!(next, Pending::Leaf { .. }) {
                if step.bytes == maximum_bytes || step.scalars == maximum_scalars {
                    break;
                }
                let Some(Pending::Leaf {
                    piece,
                    mut utf16,
                    mut byte,
                    mut pending,
                    mut used,
                    mut needed,
                }) = self.pending.pop()
                else {
                    unreachable!()
                };
                if utf16 == self.range.end {
                    self.answer = Some(!self.rejected);
                    break; // 两端均已由真实scalar或原节点边界验证，不再借范围外后继。
                }
                let count = (maximum_bytes - step.bytes)
                    .min(maximum_scalars - step.scalars)
                    .min(crate::MARKDOWN_LAYOUT_SOURCE_BYTES)
                    .min(piece.byte_len - byte)
                    .min(self.range.end - utf16);
                let bytes = snapshot
                    .text
                    .piece_range_bytes(&piece, byte..byte + count)?;
                step.bytes += bytes.len();
                step.maximum_borrow = step.maximum_borrow.max(bytes.len());
                let mut failure = None;
                for &value in &*bytes {
                    if used == 0 {
                        needed = match value {
                            0..=0x7f => 1,
                            0xc2..=0xdf => 2,
                            0xe0..=0xef => 3,
                            0xf0..=0xf4 => 4,
                            _ => {
                                failure = Some(MarkdownDocumentBufferError::InvalidUtf8Document);
                                break;
                            }
                        };
                    }
                    pending[used] = value;
                    used += 1;
                    byte += 1;
                    if used != needed {
                        continue;
                    }
                    let Some(scalar) = std::str::from_utf8(&pending[..used])
                        .ok()
                        .and_then(|text| text.chars().next())
                    else {
                        failure = Some(MarkdownDocumentBufferError::InvalidUtf8Document);
                        break;
                    };
                    step.scalars += 1;
                    let end = utf16 + scalar.len_utf16();
                    if (utf16 < self.range.start && self.range.start < end)
                        || (utf16 < self.range.end && self.range.end < end)
                    {
                        failure = Some(MarkdownDocumentBufferError::NotUnicodeBoundary);
                        break;
                    } // 任一端点切入代理项都不能被已有反例或正聚合掩盖。
                    if utf16 >= self.range.start {
                        if (' '..='~').contains(&scalar) {
                            self.include(scalar as u8, scalar as u8);
                        } else {
                            self.rejected = true;
                        }
                    } // 负事实不能短路末端标量校验；完整中间节点仍可只消费已有聚合。

                    utf16 = end;
                    used = 0;
                    if utf16 == self.range.end {
                        self.answer = Some(!self.rejected);
                        break;
                    }
                }
                bytes.finish()?;
                snapshot.text.source_health()?; // 真实归还失败优先于刚算出的候选答案。
                if let Some(error) = failure {
                    return Err(error);
                }
                if self.answer.is_none() && byte != piece.byte_len {
                    self.pending.push(Pending::Leaf {
                        piece,
                        utf16,
                        byte,
                        pending,
                        used,
                        needed,
                    });
                }
                continue;
            }
            if step.metadata == maximum_metadata {
                break;
            }
            step.metadata += 1;
            match self.pending.pop().expect("原待访问树节点") {
                Pending::Node(node, base) => {
                    let metrics = node.metrics();
                    let end = base + metrics.utf16_len;
                    if end <= self.range.start || base >= self.range.end {
                        continue; // 原节点边界是标量边界；完全不相交不借正文。
                    }
                    if metrics.all_printable_ascii.is_printable() {
                        self.include(
                            metrics.unicode_properties.printable_ascii_min,
                            metrics.unicode_properties.printable_ascii_max,
                        );
                        continue; // 完整节点包络覆盖本交集，允许保守扩大，绝不能漏未读尾部字符。
                    }
                    if self.range.start <= base && end <= self.range.end {
                        self.rejected = true;
                        continue; // 完整混合节点已有真实反例，仍沿树验证尚未到达的部分末叶。
                    }
                    match node.as_ref() {
                        MarkdownPieceTreeNode::Branch { left, right, .. } => {
                            self.pending.push(Pending::Node(
                                Arc::clone(right),
                                base + left.metrics().utf16_len,
                            ));
                            self.pending.push(Pending::Node(Arc::clone(left), base));
                        }
                        MarkdownPieceTreeNode::OriginalSpan(span) => {
                            let ordinal = span.record_range().start;
                            self.pending.push(Pending::Span {
                                span: span.clone(),
                                ordinal,
                                base,
                                byte_edge: span.word_block_byte_start(),
                                prefix: (ordinal != 0).then_some(0),
                            });
                        }
                        MarkdownPieceTreeNode::Leaf(piece) => self.pending.push(Pending::Leaf {
                            piece: piece.clone(),
                            utf16: base,
                            byte: 0,
                            pending: [0; 4],
                            used: 0,
                            needed: 0,
                        }),
                    }
                }
                Pending::Span {
                    span,
                    ordinal,
                    base,
                    byte_edge,
                    prefix,
                } => {
                    if let Some(prefix) = prefix {
                        let metrics = span.word_record_metrics(prefix)?;
                        self.pending.push(Pending::Span {
                            span,
                            ordinal,
                            base,
                            byte_edge: byte_edge + metrics.byte_len,
                            prefix: (prefix + 1 < ordinal).then_some(prefix + 1),
                        });
                        continue; // 真实目录物理前缀逐条收费，不能以一次元数据额度暗读十六项。
                    }
                    let metrics = span.word_record_metrics(ordinal)?;
                    if ordinal + 1 < span.record_range().end {
                        self.pending.push(Pending::Span {
                            span,
                            ordinal: ordinal + 1,
                            base: base + metrics.utf16_len,
                            byte_edge: byte_edge + metrics.byte_len,
                            prefix: None,
                        });
                    }
                    if base + metrics.utf16_len <= self.range.start || base >= self.range.end {
                        continue;
                    }
                    if metrics.all_printable_ascii.is_printable() {
                        self.include(
                            metrics.unicode_properties.printable_ascii_min,
                            metrics.unicode_properties.printable_ascii_max,
                        );
                        continue; // 真实compact还原的整片域覆盖相交部分，不再借正文。
                    }
                    if self.range.start <= base && base + metrics.utf16_len <= self.range.end {
                        self.rejected = true;
                        continue;
                    }
                    self.pending.push(Pending::Leaf {
                        piece: MarkdownPiece {
                            source: MarkdownPieceSource::Original,
                            byte_start: byte_edge,
                            byte_len: metrics.byte_len,
                            metrics,
                        },
                        utf16: base,
                        byte: 0,
                        pending: [0; 4],
                        used: 0,
                        needed: 0,
                    });
                }
                Pending::Leaf { .. } => unreachable!("实际字节叶已在上方处理"),
            }
        }
        if self.answer.is_some() {
            self.pending.clear();
        } // 候选完成即交还路径Arc，不把树保留为额外缓存。
        step.answer = self.answer;
        step.minimum = self.minimum;
        step.maximum = self.maximum;
        Ok(step)
    }
}
