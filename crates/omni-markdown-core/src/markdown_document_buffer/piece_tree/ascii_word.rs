//! 同一全ASCII冻结树的邻接字母数字段；游标只保原节点栈，所有实际访问消费原量子。

use super::*;

enum PendingNode {
    Node(Arc<MarkdownPieceTreeNode>, usize),
    Span {
        span: OriginalSpan,
        ordinal: u8,
        edge: usize,
        byte_edge: usize,
        prefix: Option<u8>,
    },
    Leaf(MarkdownPiece, usize),
}

/// 只在原Source已认证Dense GCB时消费；不保Source、正文映射或派生Reader。
pub(crate) struct MarkdownAsciiWordRun {
    authority: crate::MarkdownLayoutTextAuthority,
    pending: Vec<PendingNode>,
    position: usize,
    forward: bool,
    complete: bool,
}

/// bytes计实际请求的局部lease，scalars计真正检查的字符；metadata计原节点与Span目录访问。
pub(crate) struct MarkdownAsciiWordStep {
    pub(crate) bytes: usize,
    pub(crate) scalars: usize,
    pub(crate) metadata: usize,
    pub(crate) position: usize,
    pub(crate) complete: bool,
}

/// 原L/N统计在可打印ASCII域中精确等于alnum；单run且两端在run内证明整个节点同类。
fn all_alnum(metrics: MarkdownDocumentMetrics) -> bool {
    metrics.all_printable_ascii.is_printable()
        && metrics.word_count == 1
        && metrics.starts_in_word
        && metrics.ends_in_word
}

impl MarkdownAsciiWordRun {
    pub(in crate::markdown_document_buffer) fn new(
        root: &PieceTreeLink,
        position: usize,
        forward: bool,
        authority: crate::MarkdownLayoutTextAuthority,
    ) -> Self {
        let pending = root.as_ref().map_or_else(Vec::new, |root| {
            vec![PendingNode::Node(Arc::clone(root), 0)]
        });
        Self {
            authority,
            pending,
            position,
            forward,
            complete: false,
        }
    }

    pub(crate) fn authority(&self) -> &crate::MarkdownLayoutTextAuthority {
        &self.authority
    }

    /// 整节点与Span记录各收费一次；借出的叶描述跨量子保留，不从root反复重走路径。
    pub(in crate::markdown_document_buffer) fn advance(
        &mut self,
        source: &impl PieceByteSource,
        maximum_bytes: usize,
        maximum_scalars: usize,
        maximum_metadata: usize,
    ) -> Result<MarkdownAsciiWordStep, MarkdownDocumentBufferError> {
        let mut step = MarkdownAsciiWordStep {
            bytes: 0,
            scalars: 0,
            metadata: 0,
            position: self.position,
            complete: self.complete,
        };
        while !self.complete {
            let Some(next) = self.pending.last() else {
                self.complete = true;
                break; // 实际有序栈已经耗尽，才是本方向的原树端点。
            };
            if matches!(next, PendingNode::Leaf(..)) {
                if step.bytes == maximum_bytes || step.scalars == maximum_scalars {
                    break;
                }
                let Some(PendingNode::Leaf(piece, base)) = self.pending.pop() else {
                    unreachable!("同一无重入栈内已经确认叶描述")
                };
                let local = self.position - base;
                let available = if self.forward {
                    piece.byte_len - local
                } else {
                    local
                };
                let count = (maximum_bytes - step.bytes)
                    .min(maximum_scalars - step.scalars)
                    .min(crate::MARKDOWN_LAYOUT_SOURCE_BYTES)
                    .min(available);
                if count == 0 {
                    continue; // 原叶已到本方向端点，不为下一兄弟节点申请一个空lease。
                }
                let range = if self.forward {
                    local..local + count
                } else {
                    local - count..local
                };
                let bytes = source.piece_range_bytes(&piece, range)?;
                // bytes计真实逻辑lease请求，不只计提前遇终止字符前解引用的那几个字节。
                // 底层可按系统页pin；页对齐驻留量是另一项资源事实，不能混作逻辑读取预算。
                step.bytes += bytes.len();
                let mut stopped = false;
                for offset in 0..bytes.len() {
                    let index = if self.forward {
                        offset
                    } else {
                        bytes.len() - 1 - offset
                    };
                    step.scalars += 1;
                    if !bytes[index].is_ascii_alphanumeric() {
                        stopped = true;
                        break; // 这个真实字符只用于终止alnum跳过，仍留给原GCB规则解释。
                    }
                    if self.forward {
                        self.position += 1;
                    } else {
                        self.position -= 1;
                    }
                }
                bytes.finish()?; // 原lease先真实归还；回收I/O失败不能被已算出的extent掩盖。
                source.source_health()?; // 包括原Cell权限；最后一叶归还期间的撤权也不能交出extent。
                if stopped {
                    self.complete = true;
                } else if (self.forward && self.position < base + piece.metrics.utf16_len)
                    || (!self.forward && self.position > base)
                {
                    self.pending.push(PendingNode::Leaf(piece, base)); // 同一个部分叶继续，不复制其正文。
                }
                continue;
            }
            if step.metadata == maximum_metadata {
                break;
            }
            step.metadata += 1;
            match self.pending.pop().expect("已确认真实待访问节点") {
                PendingNode::Node(node, base) => {
                    let end = base + node.metrics().utf16_len;
                    if (self.forward && self.position >= end)
                        || (!self.forward && self.position <= base)
                    {
                        continue; // 原caret另一侧的子树只剪枝，不读其正文。
                    }
                    if self.position < base || self.position > end {
                        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                    }
                    if all_alnum(node.metrics()) {
                        self.position = if self.forward { end } else { base };
                        continue; // 只消费原建叶/joined的严格正事实，不把可打印直接当Word。
                    }
                    match node.as_ref() {
                        MarkdownPieceTreeNode::Branch { left, right, .. } => {
                            let right_base = base + left.metrics().utf16_len;
                            if self.forward {
                                self.pending
                                    .push(PendingNode::Node(Arc::clone(right), right_base));
                                self.pending.push(PendingNode::Node(Arc::clone(left), base));
                            } else {
                                self.pending.push(PendingNode::Node(Arc::clone(left), base));
                                self.pending
                                    .push(PendingNode::Node(Arc::clone(right), right_base));
                            } // 有序DFS只留O(height)兄弟节点，节点Arc始终属于原冻结树。
                        }
                        MarkdownPieceTreeNode::OriginalSpan(span) => {
                            let ordinal = if self.forward {
                                span.record_range().start
                            } else {
                                span.record_range().end - 1
                            };
                            let edge = if self.forward { base } else { end };
                            let target = if self.forward { ordinal } else { ordinal + 1 };
                            self.pending.push(PendingNode::Span {
                                span: span.clone(),
                                ordinal,
                                edge,
                                byte_edge: span.word_block_byte_start(),
                                prefix: (target != 0).then_some(0),
                            });
                        }
                        MarkdownPieceTreeNode::Leaf(piece) => {
                            self.pending.push(PendingNode::Leaf(piece.clone(), base))
                        }
                    }
                }
                PendingNode::Span {
                    span,
                    ordinal,
                    edge,
                    byte_edge,
                    prefix,
                } => {
                    if let Some(prefix) = prefix {
                        let metrics = span.word_record_metrics(prefix)?;
                        let target = if self.forward { ordinal } else { ordinal + 1 };
                        self.pending.push(PendingNode::Span {
                            span,
                            ordinal,
                            edge,
                            byte_edge: byte_edge + metrics.byte_len,
                            prefix: (prefix + 1 < target).then_some(prefix + 1),
                        });
                        continue; // 物理block前缀也一条一收费，budget1可在原目录中真实推进。
                    }
                    let metrics = span.word_record_metrics(ordinal)?;
                    let byte_start = if self.forward {
                        byte_edge
                    } else {
                        byte_edge - metrics.byte_len
                    };
                    let piece = MarkdownPiece {
                        source: MarkdownPieceSource::Original,
                        byte_start,
                        byte_len: metrics.byte_len,
                        metrics,
                    };
                    let base = if self.forward {
                        edge
                    } else {
                        edge - piece.metrics.utf16_len
                    };
                    let next = if self.forward {
                        ordinal
                            .checked_add(1)
                            .filter(|next| *next < span.record_range().end)
                    } else {
                        ordinal
                            .checked_sub(1)
                            .filter(|next| *next >= span.record_range().start)
                    };
                    if let Some(next) = next {
                        let edge = if self.forward {
                            base + piece.metrics.utf16_len
                        } else {
                            base
                        };
                        let byte_edge = if self.forward {
                            byte_start + piece.byte_len
                        } else {
                            byte_start
                        };
                        self.pending.push(PendingNode::Span {
                            span,
                            ordinal: next,
                            edge,
                            byte_edge,
                            prefix: None,
                        });
                    }
                    let end = base + piece.metrics.utf16_len;
                    if (self.forward && self.position >= end)
                        || (!self.forward && self.position <= base)
                    {
                        continue;
                    }
                    if all_alnum(piece.metrics) {
                        self.position = if self.forward { end } else { base };
                    } else {
                        self.pending.push(PendingNode::Leaf(piece, base));
                    } // 目录每次只取一条真实compact记录，不把16条展开藏成一次metadata。
                }
                PendingNode::Leaf(..) => unreachable!("叶字节由上方独立预算分支消费"),
            }
        }
        step.position = self.position;
        step.complete = self.complete;
        Ok(step)
    }
}
