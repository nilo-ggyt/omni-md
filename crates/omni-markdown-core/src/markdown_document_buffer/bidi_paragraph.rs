//! Locate actual B-class paragraph endpoints in one frozen tree with bounded scalar and metadata work.

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
    // Immutable nodes and compact records retain only original metrics/path
    // coordinates. Actual body bytes are borrowed only while advancing a leaf.
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
    // One original physical piece and at most one UTF-8 scalar remainder survive
    // a yield. No source lease or paragraph-sized buffer is retained here.
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

/// A completed original scalar boundary, never a lease, copied text, or unvalidated UTF-8 remainder.
struct ScalarPoint {
    piece: MarkdownPiece,
    utf16: usize,
    // The same hit leaf's global UTF-16 end; byte differences cannot supply this
    // remaining UTF-16 extent when scalar widths differ.
    end: usize,
    byte: usize,
    source_byte: usize,
}

/// Retain the original tree path and one scalar remainder under a weak Frozen identity, not a reader owner.
pub(crate) struct FrozenBidiParagraphCursor {
    origin: std::sync::Weak<FrozenText>,
    // A display source must traverse its actual output directory. Its absent
    // canonical root cannot be mistaken for an empty original source.
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
    // Once the actual B paragraph is ready, a forward query validates only its
    // new scalar point; both range and point still belong to this original cursor.
    boundary_only: bool,
    forward: Option<ScalarPoint>,
}

pub(crate) struct FrozenBidiParagraphStep {
    pub(crate) bytes: usize,
    pub(crate) scalars: usize,
    pub(crate) metadata: usize,
    pub(crate) range: Option<Range<usize>>,
    // The same paragraph search retains canonical byte endpoints, so later UBA
    // work need not borrow the boundary leaves again just to translate coordinates.
    pub(crate) byte_range: Option<Range<usize>>,
    pub(crate) content_end: usize,
    pub(crate) maximum_borrow: usize,
}

impl FrozenBidiParagraphCursor {
    pub(super) fn new(snapshot: &MarkdownTextSnapshot, position: usize) -> Self {
        // Establish original identity and metadata only. The snapshot entry point
        // has confined the request; mixed scalar validation is charged on advance.
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(projection) = snapshot.display_projection() {
            // Projection coordinates use their own true visible directory and
            // cannot borrow a canonical source's empty-root completion path.
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
        // One-unit scalars make every integer endpoint valid. Otherwise validate
        // the actual hit scalar before searching the surrounding B-class boundaries.
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

    /// The owner transfers this only after actual Ready; projections have no canonical forward-point proof.
    pub(crate) fn into_forward_range(self) -> Option<Self> {
        #[cfg(feature = "s2-table-window-prototype")]
        if self.projection.is_some() {
            return None;
        }
        Some(self)
    }

    /// Resume scalar validation only for a forward request inside this same Frozen paragraph.
    /// Backward, in-flight, other-paragraph and projection requests require the caller's ordinary cold cursor.
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
            return Ok(false); // Projection output and canonical pieces cannot share this endpoint permission.
        }
        if self.seek != Seek::Complete
            || position <= self.position
            || !(self.start..self.end).contains(&position)
        {
            // An unready or unrelated position cannot inherit the completed
            // paragraph's original point. False asks the owner to use its cold path.
            return Ok(false);
        }
        // Preserve the already-closed paragraph and replace only the requested
        // point. It remains pending until actual scalar validation completes.
        self.position = position;
        self.boundary_only = true;
        if let Some(point) = &self.forward
            && point.utf16 < position
            && position <= point.end
        {
            // Resume only from the original validated point in this leaf. Crossing
            // leaves still pays for the real tree path and hit-leaf Boundary work.
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
            // The new point and closed B endpoints share the same immutable
            // Frozen value; do not rescan previous/next boundary leaves.
            self.pending.clear();
            self.seek = Seek::Complete;
        } else {
            // A cold query has validated only one scalar so far; locate the
            // original B paragraph before handing any completed range outward.
            self.restart(Seek::Previous);
        }
    }

    fn restart(&mut self, seek: Seek) {
        // Return the exhausted path and seed another traversal of the same
        // original root. No current document capture can replace this authority.
        self.pending.clear();
        self.seek = seek;
        if let Some(root) = &self.root {
            self.pending.push(Pending::Node(Arc::clone(root), 0, 0));
        }
    }

    fn relevant(&self, metrics: MarkdownDocumentMetrics, base: usize) -> bool {
        // Scalar validation follows only the containing subtree. Paragraph search
        // follows only actual B-containing subtrees on the requested side.
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

    /// Charge each actual node/directory visit; continue scalar remainders across one-byte quanta.
    /// Each real lease ends inside this step; the snapshot entry point owns surrounding health checks.
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
        } // Equal text in another Frozen value cannot replace the actual resource owner.
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
                continue; // Only exhaustion of the original ordered path establishes BOF/EOF, never a quota limit.
            };
            if matches!(next, Pending::Leaf(_)) {
                if step.bytes == maximum_bytes || step.scalars == maximum_scalars {
                    // Leave the original leaf continuation pending; a budget
                    // boundary cannot be published as a paragraph endpoint.
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
                    continue; // An original leaf start is already scalar-aligned and needs no text read.
                }
                let mut count = (maximum_bytes - step.bytes)
                    .min(maximum_scalars - step.scalars)
                    .min(crate::MARKDOWN_LAYOUT_SOURCE_BYTES)
                    .min(leaf.piece.byte_len - leaf.byte);
                if self.seek == Seek::Boundary && self.boundary_only {
                    // Each UTF-16 unit needs at most three UTF-8 bytes. Limit the
                    // requested slice near this point instead of repeatedly borrowing
                    // a full quantum for small GCBs; a surrogate interior still needs full scalar validation.
                    count = count.min(
                        (self.position - leaf.utf16).min(crate::MARKDOWN_LAYOUT_SOURCE_BYTES) * 3,
                    );
                }
                let bytes = snapshot
                    .text
                    .piece_range_bytes(&leaf.piece, leaf.byte..leaf.byte + count)?;
                step.bytes += bytes.len(); // Charge the requested leased slice, including any bytes beyond the first B hit.
                step.maximum_borrow = step.maximum_borrow.max(bytes.len());
                for &byte in &*bytes {
                    // Decode at most one retained scalar across quanta. Malformed
                    // starts/completions remain source errors, not artificial boundaries.
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
                            // A lease can include bytes beyond the target. Save
                            // only that target's validated byte endpoint, never
                            // relabel prefetch length or another scalar's end as this UTF-16 position.
                            let byte = if self.position == leaf.utf16 {
                                leaf.byte - leaf.used
                            } else {
                                leaf.byte
                            };
                            found = Some((valid, byte));
                        }
                    } else if UnicodeBidiData.bidi_class(scalar) == BidiClass::B {
                        // Previous remembers the last original separator before
                        // the request; Next closes at the first separator at/after it.
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
                // End the real pin before publishing endpoints or decode errors.
                // Reclaim/source failure takes precedence over the computed candidate.
                bytes.finish()?;
                snapshot.text.source_health()?;
                if let Some(error) = failure {
                    return Err(error);
                }
                if let Some((valid, byte)) = found {
                    if !valid {
                        return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
                    }
                    // Only after actual lease return and original-source health
                    // checks may this decoded point become a retained continuation.
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
                    self.pending.push(Pending::Leaf(leaf)); // Resume the same original leaf without copied body bytes.
                }
                continue;
            }
            if step.metadata == maximum_metadata {
                // Directory work yields independently of byte/scalar work. The
                // untouched path is not an implicit completed paragraph.
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
                            // Stack order follows the actual search direction;
                            // byte and UTF-16 bases advance with their own metrics.
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
                            // Begin at the correct selected compact record and
                            // explicitly charge any physical-directory prefix needed to locate it.
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
                        continue; // Every physical-prefix record is charged; one metadata unit cannot hide 16 visits.
                    }
                    let metrics = span.word_record_metrics(ordinal)?;
                    // Reverse traversal starts from this record's end; forward
                    // traversal starts from its beginning. Keep physical/source byte bases distinct.
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
                        // Retain only the next original compact-record cursor,
                        // without expanding the directory into another piece list.
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
                        // Decode only a genuinely relevant original record. The
                        // constructed leaf retains coordinates, not a live byte reader.
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
            self.pending.clear(); // Ready retains the original root/point/paragraph facts, not the exhausted traversal path.
            step.range = Some(self.start..self.end);
            step.byte_range = Some(self.start_byte..self.end_byte);
            step.content_end = self.content_end;
        }
        Ok(step)
    }
}
