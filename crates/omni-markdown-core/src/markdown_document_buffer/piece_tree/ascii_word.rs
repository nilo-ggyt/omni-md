//! Traverse adjacent alphanumeric text in the same proved ASCII source, retaining an original-node stack and charging actual step work.

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
    // A raw prefix before a displayed ASCII interval may contain Unicode; retain one partial scalar to locate actual bytes.
    ScopedLeaf {
        piece: MarkdownPiece,
        utf16: usize,
        byte: usize,
        pending: [u8; 4],
        used: usize,
        needed: usize,
        located: bool,
    },
}

/// Consume only through the original Source's proved ASCII/GCB gateway; retain no Source, text mapping, or derived reader.
pub(crate) struct MarkdownAsciiWordRun {
    authority: crate::MarkdownLayoutTextAuthority,
    // A raw run reads only the original range of a sealed display run; hidden punctuation outside it cannot stop a visible word.
    scope: Option<Range<usize>>,
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    projection: Option<Box<display_projection::algorithms::ProjectionAsciiWordRun>>,
    pending: Vec<PendingNode>,
    position: usize,
    forward: bool,
    complete: bool,
}

/// Count requested logical lease bytes, actually inspected scalars, and original node/span-directory visits separately.
pub(crate) struct MarkdownAsciiWordStep {
    pub(crate) bytes: usize,
    pub(crate) scalars: usize,
    pub(crate) metadata: usize,
    pub(crate) position: usize,
    pub(crate) complete: bool,
}

/// Original letter/number summaries equal alnum in the printable ASCII domain; one run touching both endpoints proves the entire node belongs to it.
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
            scope: None,
            #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
            projection: None,
            pending,
            position,
            forward,
            complete: false,
        }
    }

    /// Only the original Frozen gateway supplies a real display run's raw range, without rescanning its already sealed ASCII fact.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(in crate::markdown_document_buffer) fn new_range(
        root: &PieceTreeLink,
        position: usize,
        forward: bool,
        authority: crate::MarkdownLayoutTextAuthority,
        range: Range<usize>,
    ) -> Self {
        let mut run = Self::new(root, position, forward, authority);
        run.scope = Some(range);
        run
    }

    /// Keep display coordinates in the original projection cursor, never using an empty raw root to manufacture BOF/EOF.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(in crate::markdown_document_buffer) fn new_display(
        position: usize,
        forward: bool,
        authority: crate::MarkdownLayoutTextAuthority,
    ) -> Self {
        Self {
            projection: Some(Box::new(
                display_projection::algorithms::ProjectionAsciiWordRun::new(position, forward),
            )),
            scope: None,
            authority,
            pending: Vec::new(),
            position,
            forward,
            complete: false,
        }
    }

    /// Projection and original-tree continuation share one budget; the Source gateway checks original identity and actual health around each step.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(in crate::markdown_document_buffer) fn advance_display(
        &mut self,
        projection: &display_projection::DisplayProjection,
        maximum_bytes: usize,
        maximum_scalars: usize,
        maximum_metadata: usize,
    ) -> Result<MarkdownAsciiWordStep, MarkdownDocumentBufferError> {
        self.projection
            .as_mut()
            .expect("显示 Word 游标由原显示 gateway 装配")
            .advance(
                projection,
                &self.authority,
                maximum_bytes,
                maximum_scalars,
                maximum_metadata,
            )
    }

    pub(crate) fn authority(&self) -> &crate::MarkdownLayoutTextAuthority {
        &self.authority
    }

    /// Charge whole nodes and individual span records once each; retain leaf descriptions across yields rather than restarting at root.
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
            if self.scope.as_ref().is_some_and(|range| {
                if self.forward {
                    self.position == range.end
                } else {
                    self.position == range.start
                }
            }) {
                self.complete = true;
                break;
            } // At the sealed run's actual edge, do not inspect hidden original characters or a later raw sibling.
            let Some(next) = self.pending.last() else {
                self.complete = true;
                break; // Only exhausting the actual ordered stack proves this direction's original tree endpoint.
            };
            if matches!(next, PendingNode::ScopedLeaf { .. }) {
                if step.bytes == maximum_bytes || step.scalars == maximum_scalars {
                    break;
                }
                let Some(PendingNode::ScopedLeaf {
                    piece,
                    mut utf16,
                    mut byte,
                    mut pending,
                    mut used,
                    mut needed,
                    mut located,
                }) = self.pending.pop()
                else {
                    unreachable!("同一路径的实际 scoped leaf")
                };
                located |= utf16 == self.position && used == 0;
                if !located {
                    // Decode only the mixed raw prefix needed to reach the admitted ASCII run; partial scalars survive a byte-limited yield.
                    let count = (maximum_bytes - step.bytes)
                        .min(maximum_scalars - step.scalars)
                        .min(crate::MARKDOWN_LAYOUT_SOURCE_BYTES)
                        .min(piece.byte_len - byte);
                    let bytes = source.piece_range_bytes(&piece, byte..byte + count)?;
                    step.bytes += bytes.len();
                    let mut failure = None;
                    for &value in &*bytes {
                        if used == 0 {
                            needed = match value {
                                0..=0x7f => 1,
                                0xc2..=0xdf => 2,
                                0xe0..=0xef => 3,
                                0xf0..=0xf4 => 4,
                                _ => {
                                    failure =
                                        Some(MarkdownDocumentBufferError::InvalidUtf8Document);
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
                        utf16 += scalar.len_utf16();
                        used = 0;
                        if utf16 >= self.position {
                            if utf16 != self.position {
                                failure = Some(MarkdownDocumentBufferError::NotUnicodeBoundary);
                            } else {
                                located = true;
                            }
                            break;
                        }
                    }
                    bytes.finish()?;
                    source.source_health()?;
                    if let Some(error) = failure {
                        // Preserve actual lease-return/source failure before reporting a decoded scalar or boundary mismatch.
                        return Err(error);
                    }
                } else {
                    // The sealed equal-length ASCII range permits byte addressing only inside its actual scope, in either direction.
                    let scope = self.scope.as_ref().expect("受控显示原区间已装配");
                    let available = if self.forward {
                        (piece.byte_len - byte).min(scope.end - self.position)
                    } else {
                        byte.min(self.position - scope.start)
                    };
                    let count = (maximum_bytes - step.bytes)
                        .min(maximum_scalars - step.scalars)
                        .min(crate::MARKDOWN_LAYOUT_SOURCE_BYTES)
                        .min(available);
                    if count != 0 {
                        let range = if self.forward {
                            byte..byte + count
                        } else {
                            byte - count..byte
                        };
                        let bytes = source.piece_range_bytes(&piece, range)?;
                        step.bytes += bytes.len();
                        for offset in 0..bytes.len() {
                            let index = if self.forward {
                                offset
                            } else {
                                bytes.len() - 1 - offset
                            };
                            step.scalars += 1;
                            if !bytes[index].is_ascii_alphanumeric() {
                                self.complete = true;
                                break;
                            }
                            if self.forward {
                                self.position += 1;
                                byte += 1;
                            } else {
                                self.position -= 1;
                                byte -= 1;
                            }
                        }
                        bytes.finish()?;
                        source.source_health()?;
                    }
                }
                // Before location, retain UTF-8 continuation; afterward retain only an actual unexhausted interval in this direction.
                if !self.complete
                    && (!located
                        || if self.forward {
                            byte < piece.byte_len
                        } else {
                            byte != 0
                        })
                {
                    self.pending.push(PendingNode::ScopedLeaf {
                        piece,
                        utf16,
                        byte,
                        pending,
                        used,
                        needed,
                        located,
                    });
                }
                continue;
            }
            if matches!(next, PendingNode::Leaf(..)) {
                if step.bytes == maximum_bytes || step.scalars == maximum_scalars {
                    break;
                }
                let Some(PendingNode::Leaf(piece, base)) = self.pending.pop() else {
                    unreachable!("同一无重入栈内已经确认叶描述")
                };
                let local = self.position - base;
                let available = if self.forward {
                    self.scope.as_ref().map_or(piece.byte_len - local, |range| {
                        (piece.byte_len - local).min(range.end - self.position)
                    })
                } else {
                    self.scope
                        .as_ref()
                        .map_or(local, |range| local.min(self.position - range.start))
                };
                let count = (maximum_bytes - step.bytes)
                    .min(maximum_scalars - step.scalars)
                    .min(crate::MARKDOWN_LAYOUT_SOURCE_BYTES)
                    .min(available);
                if count == 0 {
                    continue; // At this leaf's directional endpoint, do not request an empty lease merely to reach its sibling.
                }
                let range = if self.forward {
                    local..local + count
                } else {
                    local - count..local
                };
                let bytes = source.piece_range_bytes(&piece, range)?;
                // Charge the actual logical lease request, not just bytes dereferenced before finding a stopping character.
                // Backing may pin system pages; page-aligned residency is a separate resource fact rather than logical read fuel.
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
                        break; // This actual character stops alnum skipping and remains for the original GCB rule to interpret.
                    }
                    if self.forward {
                        self.position += 1;
                    } else {
                        self.position -= 1;
                    }
                }
                bytes.finish()?; // Return the actual lease first; computed extent cannot mask reclaim I/O failure.
                source.source_health()?; // Include original cell authority; revocation during the last leaf's return must withhold the extent too.
                if stopped {
                    self.complete = true;
                } else if (self.forward && self.position < base + piece.metrics.utf16_len)
                    || (!self.forward && self.position > base)
                {
                    self.pending.push(PendingNode::Leaf(piece, base)); // Continue the same partial leaf without copying its text.
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
                        continue; // Prune the subtree on the other side of the original caret without reading its text.
                    }
                    if self.position < base || self.position > end {
                        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                    }
                    if all_alnum(node.metrics()) {
                        self.position = if self.forward {
                            self.scope.as_ref().map_or(end, |range| end.min(range.end))
                        } else {
                            self.scope
                                .as_ref()
                                .map_or(base, |range| base.max(range.start))
                        };
                        continue; // Consume the strict positive fact from original leaf/join construction, not printability alone as a word proof.
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
                            } // Ordered DFS keeps O(height) sibling nodes, whose Arc identities remain in the original frozen tree.
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
                        MarkdownPieceTreeNode::Leaf(piece) => self.push_leaf(piece.clone(), base),
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
                        continue; // Charge physical-block prefix records individually, allowing budget-one progress through the original directory.
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
                        self.position = if self.forward {
                            self.scope.as_ref().map_or(end, |range| end.min(range.end))
                        } else {
                            self.scope
                                .as_ref()
                                .map_or(base, |range| base.max(range.start))
                        };
                    } else {
                        self.push_leaf(piece, base);
                    } // Consume one actual compact record at a time rather than hiding sixteen expansions in one metadata unit.
                }
                PendingNode::Leaf(..) | PendingNode::ScopedLeaf { .. } => {
                    unreachable!("叶字节由上方独立预算分支消费")
                }
            }
        }
        step.position = self.position;
        step.complete = self.complete;
        Ok(step)
    }

    /// Address original all-ASCII leaves directly; only a mixed raw leaf containing a displayed range needs actual prefix decoding.
    fn push_leaf(&mut self, piece: MarkdownPiece, base: usize) {
        if self.scope.is_some() && !piece.metrics.all_printable_ascii.is_printable() {
            self.pending.push(PendingNode::ScopedLeaf {
                piece,
                utf16: base,
                byte: 0,
                pending: [0; 4],
                used: 0,
                needed: 0,
                located: false,
            });
        } else {
            self.pending.push(PendingNode::Leaf(piece, base));
        }
    }
}
