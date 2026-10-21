//! Prove printable ASCII on one frozen range using a tree path and one UTF-8 remainder, not copied text.

use super::*;

enum Pending {
    // Nodes and compact span records retain immutable metrics and physical piece
    // coordinates only. Leaf progress carries at most one incomplete scalar.
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

/// Original facts through the requested endpoint; a weak Frozen identity authorizes the retained path.
pub(crate) struct FrozenPrintableAsciiSuffix {
    origin: std::sync::Weak<FrozenText>,
    // A display source has its own deleted-marker/style directory; its absent
    // canonical root cannot be treated as an empty all-ASCII document.
    #[cfg(feature = "s2-table-window-prototype")]
    projection: Option<Box<display_projection::algorithms::ProjectionPrintableAsciiSuffix>>,
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
    // Only a true answer authorizes an envelope for the complete range. Positive
    // partial nodes may widen it conservatively; this is not an exact character set.
    pub(crate) minimum: u8,
    pub(crate) maximum: u8,
}

impl FrozenPrintableAsciiSuffix {
    pub(in super::super) fn new(snapshot: &MarkdownTextSnapshot, range: Range<usize>) -> Self {
        // Construction retains the original identity and metadata path, without
        // borrowing a boundary leaf or allocating a second source/text owner.
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(projection) = snapshot.display_projection() {
            // Keep projection traversal on the same original display source; a
            // missing canonical tree does not authorize the ordinary empty path.
            return Self {
                origin: Arc::downgrade(&snapshot.text),
                projection: Some(Box::new(
                    display_projection::algorithms::ProjectionPrintableAsciiSuffix::new(
                        projection,
                        range.clone(),
                    ),
                )),
                pending: Vec::new(),
                range,
                rejected: false,
                answer: None,
                minimum: 0,
                maximum: 0,
            };
        }
        Self {
            origin: Arc::downgrade(&snapshot.text),
            #[cfg(feature = "s2-table-window-prototype")]
            projection: None,
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
        } // Empty nodes contribute no character; zero is not evidence of a selected NUL.
    }

    /// Charge every directory expansion and resume mixed leaves across one-byte quanta.
    /// Actual leases end within this step; the snapshot entry point checks source health around it.
    pub(in super::super) fn advance(
        &mut self,
        snapshot: &MarkdownTextSnapshot,
        maximum_bytes: usize,
        maximum_scalars: usize,
        maximum_metadata: usize,
    ) -> Result<FrozenPrintableAsciiSuffixStep, MarkdownDocumentBufferError> {
        if !std::sync::Weak::ptr_eq(&self.origin, &Arc::downgrade(&snapshot.text)) {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        } // Equal text from another Frozen value cannot replace the original resource authority.
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
        let mut step = FrozenPrintableAsciiSuffixStep {
            // Report only this quantum's actual work while retaining the original
            // cumulative candidate and envelope on the cursor itself.
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
                break; // Only exhaustion of the actual intersecting path completes the range proof.
            };
            if matches!(next, Pending::Leaf { .. }) {
                if step.bytes == maximum_bytes || step.scalars == maximum_scalars {
                    // A quota boundary leaves the original continuation pending;
                    // it cannot manufacture EOF or a positive answer.
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
                    break; // Both endpoints are original scalar/node boundaries; do not borrow a successor.
                }
                // Bound the requested slice by remaining byte/scalar work, the
                // logical 4-KiB lease limit, the actual piece and requested endpoint.
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
                    // Keep at most one UTF-8 scalar across quanta. Invalid leading
                    // bytes and invalid completed encodings remain typed source errors.
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
                    } // A surrogate-interior endpoint is invalid even after an earlier counterexample.
                    if utf16 >= self.range.start {
                        if (' '..='~').contains(&scalar) {
                            self.include(scalar as u8, scalar as u8);
                        } else {
                            self.rejected = true;
                        }
                    } // A negative fact cannot skip final endpoint validation; complete nodes still use aggregates.

                    utf16 = end;
                    used = 0;
                    if utf16 == self.range.end {
                        self.answer = Some(!self.rejected);
                        break;
                    }
                }
                // Return the actual lease before publishing a scalar/error or
                // preserving continuation; reclaim/source failure outranks the candidate.
                bytes.finish()?;
                snapshot.text.source_health()?;
                if let Some(error) = failure {
                    return Err(error);
                }
                if self.answer.is_none() && byte != piece.byte_len {
                    // Resume this same piece and scalar remainder after yielding;
                    // never restart at its prefix or retain borrowed body bytes.
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
                // Directory work has its own budget; unvisited nodes remain in
                // original order instead of being implicitly certified by the quota.
                break;
            }
            step.metadata += 1;
            match self.pending.pop().expect("原待访问树节点") {
                Pending::Node(node, base) => {
                    let metrics = node.metrics();
                    let end = base + metrics.utf16_len;
                    if end <= self.range.start || base >= self.range.end {
                        continue; // Original node bounds are scalar-aligned; unrelated nodes require no text lease.
                    }
                    if metrics.all_printable_ascii.is_printable() {
                        self.include(
                            metrics.unicode_properties.printable_ascii_min,
                            metrics.unicode_properties.printable_ascii_max,
                        );
                        continue; // A conservative envelope may widen this intersection, but never omit unread characters.
                    }
                    if self.range.start <= base && end <= self.range.end {
                        self.rejected = true;
                        continue; // A complete mixed node proves false; pending partial endpoints still need validation.
                    }
                    match node.as_ref() {
                        MarkdownPieceTreeNode::Branch { left, right, .. } => {
                            // Push right before left so the stack visits original
                            // source order without creating a flat piece list.
                            self.pending.push(Pending::Node(
                                Arc::clone(right),
                                base + left.metrics().utf16_len,
                            ));
                            self.pending.push(Pending::Node(Arc::clone(left), base));
                        }
                        MarkdownPieceTreeNode::OriginalSpan(span) => {
                            // The physical compact directory may start before its
                            // selected logical records; charge that prefix explicitly.
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
                        continue; // Each physical-prefix record consumes metadata; one unit cannot hide 16 reads.
                    }
                    let metrics = span.word_record_metrics(ordinal)?;
                    if ordinal + 1 < span.record_range().end {
                        // Preserve the exact next record coordinates before
                        // classifying this intersection; no byte lease survives here.
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
                        continue; // The original compact envelope covers this intersection without a text read.
                    }
                    if self.range.start <= base && base + metrics.utf16_len <= self.range.end {
                        self.rejected = true;
                        continue;
                    }
                    self.pending.push(Pending::Leaf {
                        // Only a partial mixed record needs scalar decoding. Its
                        // physical piece coordinates still refer to the original source.
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
        } // A completed answer releases the exhausted path instead of keeping an extra tree cache.
        step.answer = self.answer;
        step.minimum = self.minimum;
        step.maximum = self.maximum;
        Ok(step)
    }
}
