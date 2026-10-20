//! 同一冻结树的真实B类段端点；没有B的子树只消费元数据，命中叶按原量子逐scalar读取。

use super::*;
use crate::unicode_bidi_data::UnicodeBidiData;
use unicode_bidi::{BidiClass, BidiDataSource};

#[derive(Clone, Copy, PartialEq)]
enum Seek {
    Boundary,
    Previous,
    Next,
    Complete,
}

enum Pending {
    Node(Arc<MarkdownPieceTreeNode>, usize, usize),
    Span {
        span: OriginalSpan,
        ordinal: u8,
        base: usize,
        byte_edge: usize,
        source_byte: usize,
        prefix: Option<u8>,
    },
    Leaf(Leaf),
}

struct Leaf {
    piece: MarkdownPiece,
    utf16: usize,
    end: usize,
    byte: usize,
    source_byte: usize,
    pending: [u8; 4],
    used: usize,
    needed: usize,
    previous: Option<(usize, usize)>,
}

/// 仅来自已完成的真实 scalar 解码；不保 lease、正文或尚未认证的 UTF8 残余。
struct ScalarPoint {
    piece: MarkdownPiece,
    utf16: usize,
    // 同一次命中叶的全局末端；不能按 UTF8 byte 差推测剩余 UTF16 范围。
    end: usize,
    byte: usize,
    source_byte: usize,
}

/// 只保原冻结树的路径与一个scalar残余；Weak标记原Frozen身份，不增加正文或Reader owner。
pub(crate) struct FrozenBidiParagraphCursor {
    origin: std::sync::Weak<FrozenText>,
    // 显示源必须走自己的输出目录；其 root=None 不能落入原树空值分支。
    #[cfg(feature = "s2-table-window-prototype")]
    projection: Option<Box<display_projection::algorithms::ProjectionBidiParagraphCursor>>,
    root: PieceTreeLink,
    pending: Vec<Pending>,
    position: usize,
    start: usize,
    end: usize,
    start_byte: usize,
    end_byte: usize,
    content_end: usize,
    seek: Seek,
    // 原 B 段已就绪后，只认证新的前进端点；段范围和这个点均出自本游标的同次查询。
    boundary_only: bool,
    forward: Option<ScalarPoint>,
}

pub(crate) struct FrozenBidiParagraphStep {
    pub(crate) bytes: usize,
    pub(crate) scalars: usize,
    pub(crate) metadata: usize,
    pub(crate) range: Option<Range<usize>>,
    // 同一次真实 B 段定位同时保留 canonical byte 端点，后继 UBA 不必另读边叶转换。
    pub(crate) byte_range: Option<Range<usize>>,
    pub(crate) content_end: usize,
    pub(crate) maximum_borrow: usize,
}

impl FrozenBidiParagraphCursor {
    pub(super) fn new(snapshot: &MarkdownTextSnapshot, position: usize) -> Self {
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(projection) = snapshot.display_projection() {
            let metrics = projection.metrics();
            return Self {
                origin: Arc::downgrade(&snapshot.text),
                projection: Some(Box::new(
                    display_projection::algorithms::ProjectionBidiParagraphCursor::new(
                        projection, position,
                    ),
                )),
                root: None,
                pending: Vec::new(),
                position,
                start: 0,
                end: metrics.utf16_len,
                start_byte: 0,
                end_byte: metrics.byte_len,
                content_end: metrics.utf16_len,
                seek: Seek::Complete,
                boundary_only: false,
                forward: None,
            };
        }
        let metrics = piece_tree_metrics(&snapshot.text.root);
        let mut cursor = Self {
            origin: Arc::downgrade(&snapshot.text),
            #[cfg(feature = "s2-table-window-prototype")]
            projection: None,
            root: snapshot.text.root.clone(),
            pending: Vec::new(),
            position,
            start: 0,
            end: metrics.utf16_len,
            start_byte: 0,
            end_byte: metrics.byte_len,
            content_end: metrics.utf16_len,
            seek: Seek::Boundary,
            boundary_only: false,
            forward: None,
        };
        // 每字符都只占一个UTF16单位时任意整数端点均合法；否则沿同树验证命中scalar。
        if position == 0
            || position == metrics.utf16_len
            || metrics.character_count == metrics.utf16_len
        {
            cursor.restart(Seek::Previous);
        } else {
            cursor.restart(Seek::Boundary);
        }
        cursor
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    /// 仅在本游标真实交付 Ready 后由拥有者移交；投影没有 canonical 前进点证明，原路释放。
    pub(crate) fn into_forward_range(self) -> Option<Self> {
        #[cfg(feature = "s2-table-window-prototype")]
        if self.projection.is_some() {
            return None;
        }
        Some(self)
    }

    /// 同 Frozen、同 B 段的向前请求只续算标量边界；范围覆盖本身不能授权代理项内部。
    /// 后退、在途、另一段与显示投影不借用这个点，仍由调用方创建原冷游标。
    pub(crate) fn advance_position(
        &mut self,
        snapshot: &MarkdownTextSnapshot,
        position: usize,
    ) -> Result<bool, MarkdownDocumentBufferError> {
        snapshot.text.source_health()?;
        if !std::sync::Weak::ptr_eq(&self.origin, &Arc::downgrade(&snapshot.text)) {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        #[cfg(feature = "s2-table-window-prototype")]
        if self.projection.is_some() {
            return Ok(false); // 投影的输出目录与 canonical piece 不共享此端点证明。
        }
        if self.seek != Seek::Complete
            || position <= self.position
            || !(self.start..self.end).contains(&position)
        {
            return Ok(false);
        }
        // 已封闭的同段范围保持不变；这里只更换尚未交付的新位置，未验证前没有 Ready。
        self.position = position;
        self.boundary_only = true;
        if let Some(point) = &self.forward
            && point.utf16 < position
            && position <= point.end
        {
            // 只从原已认证点顺读同叶剩余字节；跨叶仍支付原树路径与命中叶的 Boundary 量子。
            self.pending.clear();
            self.seek = Seek::Boundary;
            self.pending.push(Pending::Leaf(Leaf {
                piece: point.piece.clone(),
                utf16: point.utf16,
                end: point.end,
                byte: point.byte,
                source_byte: point.source_byte,
                pending: [0; 4],
                used: 0,
                needed: 0,
                previous: None,
            }));
        } else {
            self.restart(Seek::Boundary);
        }
        snapshot.text.source_health()?;
        Ok(true)
    }

    fn finish_boundary(&mut self, point: ScalarPoint) {
        self.forward = Some(point);
        if self.boundary_only {
            // 新点与旧 B 两端属于同一不可变 Frozen；无需再次 Previous/Next 扫描端点叶。
            self.pending.clear();
            self.seek = Seek::Complete;
        } else {
            // 冷查询只认证了这个 scalar，仍须完成原 B 范围定位才可对外交付。
            self.restart(Seek::Previous);
        }
    }

    fn restart(&mut self, seek: Seek) {
        self.pending.clear();
        self.seek = seek;
        if let Some(root) = &self.root {
            self.pending.push(Pending::Node(Arc::clone(root), 0, 0));
        }
    }

    fn relevant(&self, metrics: MarkdownDocumentMetrics, base: usize) -> bool {
        let end = base + metrics.utf16_len;
        if self.seek == Seek::Boundary {
            return base <= self.position && self.position < end;
        }
        let has_b = metrics.unicode_properties.bidi_classes()
            & MarkdownUnicodeProperties::class_mask(BidiClass::B)
            != 0;
        has_b
            && if self.seek == Seek::Previous {
                base < self.position
            } else {
                end > self.position
            }
    }

    /// 实际Node/目录各收费一次；叶的UTF8残余可跨1byte量子，原lease在返回前真实结束。
    pub(super) fn advance(
        &mut self,
        snapshot: &MarkdownTextSnapshot,
        maximum_bytes: usize,
        maximum_scalars: usize,
        maximum_metadata: usize,
    ) -> Result<FrozenBidiParagraphStep, MarkdownDocumentBufferError> {
        let mut step = FrozenBidiParagraphStep {
            bytes: 0,
            scalars: 0,
            metadata: 0,
            range: None,
            byte_range: None,
            content_end: self.content_end,
            maximum_borrow: 0,
        };
        if !std::sync::Weak::ptr_eq(&self.origin, &Arc::downgrade(&snapshot.text)) {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        } // 内部游标也不能借同文本的另一Frozen替换实际读取资源。
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(cursor) = &mut self.projection {
            return cursor.advance(
                snapshot
                    .display_projection()
                    .expect("显示游标保留原冻结值身份"),
                maximum_bytes,
                maximum_scalars,
                maximum_metadata,
            );
        }
        while self.seek != Seek::Complete {
            let Some(next) = self.pending.last() else {
                match self.seek {
                    Seek::Boundary => return Err(MarkdownDocumentBufferError::NotUnicodeBoundary),
                    Seek::Previous => self.restart(Seek::Next),
                    Seek::Next => self.seek = Seek::Complete,
                    Seek::Complete => unreachable!(),
                }
                continue; // 只有原有序路径耗尽才取得BOF/EOF，不把一次配额当作段边界。
            };
            if matches!(next, Pending::Leaf(_)) {
                if step.bytes == maximum_bytes || step.scalars == maximum_scalars {
                    break;
                }
                let Some(Pending::Leaf(mut leaf)) = self.pending.pop() else {
                    unreachable!()
                };
                let mut complete = false;
                let mut found = None;
                let mut failure = None;
                if self.seek == Seek::Boundary && leaf.utf16 == self.position {
                    self.finish_boundary(ScalarPoint {
                        piece: leaf.piece,
                        utf16: leaf.utf16,
                        end: leaf.end,
                        byte: leaf.byte,
                        source_byte: leaf.source_byte,
                    });
                    continue; // 叶首原本就是scalar边界，无需借正文重新证明。
                }
                let mut count = (maximum_bytes - step.bytes)
                    .min(maximum_scalars - step.scalars)
                    .min(crate::MARKDOWN_LAYOUT_SOURCE_BYTES)
                    .min(leaf.piece.byte_len - leaf.byte);
                if self.seek == Seek::Boundary && self.boundary_only {
                    // 每 UTF16 单位最多三个 UTF8 字节。只借到目标附近，避免每个小 GCB
                    // 虽少解码却反复借满整个量子；低代理项仍必须读完真实 scalar 才能拒绝。
                    count = count.min(
                        (self.position - leaf.utf16).min(crate::MARKDOWN_LAYOUT_SOURCE_BYTES) * 3,
                    );
                }
                let bytes = snapshot
                    .text
                    .piece_range_bytes(&leaf.piece, leaf.byte..leaf.byte + count)?;
                step.bytes += bytes.len(); // 计实际lease，而非只计遇到B之前的少量解引用。
                step.maximum_borrow = step.maximum_borrow.max(bytes.len());
                for &byte in &*bytes {
                    if leaf.used == 0 {
                        leaf.needed = match byte {
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
                    leaf.pending[leaf.used] = byte;
                    leaf.used += 1;
                    leaf.byte += 1;
                    if leaf.used != leaf.needed {
                        continue;
                    }
                    let Some(scalar) = std::str::from_utf8(&leaf.pending[..leaf.used])
                        .ok()
                        .and_then(|text| text.chars().next())
                    else {
                        failure = Some(MarkdownDocumentBufferError::InvalidUtf8Document);
                        break;
                    };
                    step.scalars += 1;
                    let end = leaf.utf16 + scalar.len_utf16();
                    if self.seek == Seek::Boundary {
                        if self.position <= end {
                            complete = true;
                            let valid = self.position == leaf.utf16 || self.position == end;
                            // lease 可以预读到目标之后，保存点仍严格对应目标的已认证 byte，
                            // 不能把预读量或刚解码出的 scalar 末端移交给另一个 UTF16 位置。
                            let byte = if self.position == leaf.utf16 {
                                leaf.byte - leaf.used
                            } else {
                                leaf.byte
                            };
                            found = Some((valid, byte));
                        }
                    } else if UnicodeBidiData.bidi_class(scalar) == BidiClass::B {
                        if self.seek == Seek::Previous && leaf.utf16 < self.position {
                            leaf.previous = Some((end, leaf.source_byte + leaf.byte));
                        } else if self.seek == Seek::Next && leaf.utf16 >= self.position {
                            self.end = end;
                            self.end_byte = leaf.source_byte + leaf.byte;
                            self.content_end = leaf.utf16;
                            complete = true;
                        }
                    }
                    leaf.used = 0;
                    leaf.utf16 = end;
                    if self.seek == Seek::Previous && end >= self.position {
                        complete = true;
                    }
                    if complete {
                        break;
                    }
                }
                bytes.finish()?; // 原pin/驱逐失败优先于已经计算出的候选端点，不跨量子保留lease。
                snapshot.text.source_health()?;
                if let Some(error) = failure {
                    return Err(error);
                }
                if let Some((valid, byte)) = found {
                    if !valid {
                        return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
                    }
                    // lease.finish 与原 Source 健康核验已经完成，才保留这个实际解码端点。
                    self.finish_boundary(ScalarPoint {
                        piece: leaf.piece,
                        utf16: self.position,
                        end: leaf.end,
                        byte,
                        source_byte: leaf.source_byte,
                    });
                } else if self.seek == Seek::Boundary && leaf.utf16 == self.position {
                    self.finish_boundary(ScalarPoint {
                        piece: leaf.piece,
                        utf16: leaf.utf16,
                        end: leaf.end,
                        byte: leaf.byte,
                        source_byte: leaf.source_byte,
                    });
                } else if self.seek == Seek::Next && complete {
                    self.seek = Seek::Complete;
                } else if complete || leaf.byte == leaf.piece.byte_len {
                    if self.seek == Seek::Previous
                        && let Some((start, byte)) = leaf.previous
                    {
                        self.start = start;
                        self.start_byte = byte;
                        self.restart(Seek::Next);
                    }
                } else {
                    self.pending.push(Pending::Leaf(leaf)); // 原部分叶续算，不回到叶头，也不保存正文缓冲。
                }
                continue;
            }
            if step.metadata == maximum_metadata {
                break;
            }
            step.metadata += 1;
            match self.pending.pop().expect("已有原待访问节点") {
                Pending::Node(node, base, source_byte) => {
                    if !self.relevant(node.metrics(), base) {
                        continue;
                    }
                    match node.as_ref() {
                        MarkdownPieceTreeNode::Branch { left, right, .. } => {
                            let right_base = base + left.metrics().utf16_len;
                            let right_byte = source_byte + left.metrics().byte_len;
                            if self.seek == Seek::Previous {
                                self.pending.push(Pending::Node(
                                    Arc::clone(left),
                                    base,
                                    source_byte,
                                ));
                                self.pending.push(Pending::Node(
                                    Arc::clone(right),
                                    right_base,
                                    right_byte,
                                ));
                            } else {
                                self.pending.push(Pending::Node(
                                    Arc::clone(right),
                                    right_base,
                                    right_byte,
                                ));
                                self.pending.push(Pending::Node(
                                    Arc::clone(left),
                                    base,
                                    source_byte,
                                ));
                            }
                        }
                        MarkdownPieceTreeNode::OriginalSpan(span) => {
                            let reverse = self.seek == Seek::Previous;
                            let ordinal = if reverse {
                                span.record_range().end - 1
                            } else {
                                span.record_range().start
                            };
                            let target = if reverse { ordinal + 1 } else { ordinal };
                            self.pending.push(Pending::Span {
                                span: span.clone(),
                                ordinal,
                                base: if reverse {
                                    base + node.metrics().utf16_len
                                } else {
                                    base
                                },
                                byte_edge: span.word_block_byte_start(),
                                source_byte: if reverse {
                                    source_byte + node.metrics().byte_len
                                } else {
                                    source_byte
                                },
                                prefix: (target != 0).then_some(0),
                            });
                        }
                        MarkdownPieceTreeNode::Leaf(piece) => {
                            self.pending.push(Pending::Leaf(Leaf {
                                piece: piece.clone(),
                                utf16: base,
                                end: base + piece.metrics.utf16_len,
                                byte: 0,
                                source_byte,
                                pending: [0; 4],
                                used: 0,
                                needed: 0,
                                previous: None,
                            }))
                        }
                    }
                }
                Pending::Span {
                    span,
                    ordinal,
                    base,
                    byte_edge,
                    source_byte,
                    prefix,
                } => {
                    if let Some(prefix) = prefix {
                        let metrics = span.word_record_metrics(prefix)?;
                        let target = if self.seek == Seek::Previous {
                            ordinal + 1
                        } else {
                            ordinal
                        };
                        self.pending.push(Pending::Span {
                            span,
                            ordinal,
                            base,
                            byte_edge: byte_edge + metrics.byte_len,
                            source_byte,
                            prefix: (prefix + 1 < target).then_some(prefix + 1),
                        });
                        continue; // 实际小目录的物理前缀同样逐条计费，budget1不会暗中展开十六项。
                    }
                    let metrics = span.word_record_metrics(ordinal)?;
                    let reverse = self.seek == Seek::Previous;
                    let start = if reverse {
                        base - metrics.utf16_len
                    } else {
                        base
                    };
                    let byte = if reverse {
                        byte_edge - metrics.byte_len
                    } else {
                        byte_edge
                    };
                    let source_start = if reverse {
                        source_byte - metrics.byte_len
                    } else {
                        source_byte
                    };
                    let next = if reverse {
                        ordinal
                            .checked_sub(1)
                            .filter(|next| *next >= span.record_range().start)
                    } else {
                        (ordinal + 1 < span.record_range().end).then_some(ordinal + 1)
                    };
                    if let Some(next) = next {
                        self.pending.push(Pending::Span {
                            span,
                            ordinal: next,
                            base: if reverse {
                                start
                            } else {
                                base + metrics.utf16_len
                            },
                            byte_edge: if reverse {
                                byte
                            } else {
                                byte_edge + metrics.byte_len
                            },
                            source_byte: if reverse {
                                source_start
                            } else {
                                source_byte + metrics.byte_len
                            },
                            prefix: None,
                        });
                    }
                    if self.relevant(metrics, start) {
                        self.pending.push(Pending::Leaf(Leaf {
                            piece: MarkdownPiece {
                                source: MarkdownPieceSource::Original,
                                byte_start: byte,
                                byte_len: metrics.byte_len,
                                metrics,
                            },
                            utf16: start,
                            end: start + metrics.utf16_len,
                            byte: 0,
                            source_byte: source_start,
                            pending: [0; 4],
                            used: 0,
                            needed: 0,
                            previous: None,
                        }));
                    }
                }
                Pending::Leaf(_) => unreachable!("叶已在上方按实际字节预算消费"),
            }
        }
        if self.seek == Seek::Complete {
            self.pending.clear(); // 就绪后仅保原 root、单点与单段事实，不保已经用完的遍历路径。
            step.range = Some(self.start..self.end);
            step.byte_range = Some(self.start_byte..self.end_byte);
            step.content_end = self.content_end;
        }
        Ok(step)
    }
}
