//! Persistent balanced piece-tree storage, coordinate indexes, and streaming traversal.

use super::*;
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};

/// Match the preserved editor's Unicode 17 `General_Category=L|N` word-run membership.
pub(super) fn is_word_character(character: char) -> bool {
    if character.is_ascii() {
        // ASCII L/N membership is exactly A-Z, a-z, and 0-9; avoid table searches on common Markdown bytes.
        return character.is_ascii_alphanumeric();
    }
    // `is_alphanumeric` includes Alphabetic combining marks; only exact letter/number categories match JS.
    matches!(
        character.general_category_group(),
        GeneralCategoryGroup::Letter | GeneralCategoryGroup::Number
    )
}

#[derive(Default)]
/// Session-owned append-only storage; persistent leaves borrow immutable ranges within its segments.
pub(super) struct MarkdownEditSegments {
    pub(super) segments: Vec<Vec<u8>>,
    pub(super) used_bytes: usize,
}

impl MarkdownEditSegments {
    /// Append immutable UTF-8 chunks into bounded session-owned edit segments and return their leaf views.
    pub(super) fn append(
        &mut self,
        text: &str,
        maximum_session_edit_bytes: usize,
    ) -> Result<Vec<Arc<MarkdownPieceTreeNode>>, MarkdownDocumentBufferError> {
        if text.is_empty() {
            // An empty insertion owns no bytes and needs no tree leaves.
            return Ok(Vec::new());
        }
        if self
            .used_bytes
            .checked_add(text.len())
            .is_none_or(|result| {
                // Reject a checked accumulated size that would exceed the lifetime append budget.
                result > maximum_session_edit_bytes
            })
        {
            // Reject the entire insertion before mutating segments when lifetime edit storage would exceed its budget.
            return Err(MarkdownDocumentBufferError::EditBufferLimitExceeded);
        }
        let mut leaves = Vec::with_capacity(text.len().div_ceil(ORIGINAL_PIECE_BYTES));
        let mut consumed = 0;
        while consumed < text.len() {
            // Consume scalar-aligned chunks until every input byte has one immutable leaf view.
            if self.segments.last().is_none_or(|segment| {
                // A full tail segment cannot receive another immutable edit chunk.
                segment.len() == EDIT_SEGMENT_BYTES
            }) {
                // Allocate another fixed-capacity segment only when no writable tail remains.
                self.segments.push(Vec::with_capacity(EDIT_SEGMENT_BYTES));
            }
            let segment_index = self.segments.len() - 1;
            let segment = &mut self.segments[segment_index];
            let available = EDIT_SEGMENT_BYTES - segment.len();
            let requested = ORIGINAL_PIECE_BYTES
                .min(available)
                .min(text.len() - consumed);
            let mut chunk_end = consumed + requested;
            while !text.is_char_boundary(chunk_end) {
                // Retreat the chunk boundary so no leaf splits a multibyte UTF-8 scalar.
                chunk_end -= 1;
            }
            if chunk_end == consumed {
                // A segment tail too small for the next scalar is left unused; a fresh segment guarantees progress.
                self.segments.push(Vec::with_capacity(EDIT_SEGMENT_BYTES));
                continue;
            }
            let segment_start = segment.len();
            let chunk = &text.as_bytes()[consumed..chunk_end];
            segment.extend_from_slice(chunk);
            self.used_bytes = self
                .used_bytes
                .checked_add(chunk.len())
                .ok_or(MarkdownDocumentBufferError::EditBufferLimitExceeded)?;
            let piece = MarkdownPiece {
                source: MarkdownPieceSource::EditSegment(segment_index),
                byte_start: segment_start,
                byte_len: chunk.len(),
                metrics: MarkdownDocumentMetrics::from_text(
                    std::str::from_utf8(chunk).expect("inserted string chunk"),
                ),
            };
            leaves.push(Arc::new(MarkdownPieceTreeNode::Leaf(piece)));
            consumed = chunk_end;
        }
        Ok(leaves)
    }

    #[cfg(test)]
    /// Measure retained edit capacity for storage regression tests, including unused segment tails.
    pub(super) fn allocated_bytes(&self) -> usize {
        self.segments.iter().map(Vec::capacity).sum()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Identifies the document-owned backing allocation for one immutable piece.
pub(super) enum MarkdownPieceSource {
    Original,
    EditSegment(usize),
}

#[derive(Clone, Debug)]
/// A scalar-aligned immutable byte view and its cached local metrics.
pub(super) struct MarkdownPiece {
    pub(super) source: MarkdownPieceSource,
    pub(super) byte_start: usize,
    pub(super) byte_len: usize,
    pub(super) metrics: MarkdownDocumentMetrics,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Associative text aggregates with boundary state for cross-piece CRLF and word reconciliation.
pub(super) struct MarkdownDocumentMetrics {
    pub(super) byte_len: usize,
    pub(super) utf16_len: usize,
    pub(super) character_count: usize,
    pub(super) word_count: usize,
    pub(super) starts_in_word: bool,
    pub(super) ends_in_word: bool,
    pub(super) line_breaks: usize,
    pub(super) starts_with_line_feed: bool,
    pub(super) ends_with_carriage_return: bool,
    pub(super) contains_carriage_return: bool,
    pub(super) contains_crlf: bool,
    /// A raw '<' anywhere can introduce an inherited HTML block outside a local cue envelope.
    pub(super) contains_ascii_less_than: bool,
}

impl MarkdownDocumentMetrics {
    /// Compute associative document aggregates for one scalar-aligned leaf without materializing auxiliary indexes.
    pub(super) fn from_text(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut metrics = Self {
            byte_len: bytes.len(),
            starts_with_line_feed: bytes.first() == Some(&b'\n'),
            ends_with_carriage_return: bytes.last() == Some(&b'\r'),
            starts_in_word: text.chars().next().is_some_and(|character| {
                // A piece beginning with an L/N scalar may continue a word ending in its left neighbor.
                is_word_character(character)
            }),
            ..Self::default()
        };
        let mut previous_was_carriage_return = false;
        for character in text.chars() {
            // Visit each Unicode scalar once; only the previous CR and word state cross iterations.
            // Normalize a CRLF pair to one visible character, including across piece joins below.
            if character != '\n' || !previous_was_carriage_return {
                // A CRLF continuation shares its visible-character count with the preceding CR.
                metrics.character_count += 1;
            }
            // Cache the exact baseline regex runs without allocating tokens or applying locale segmentation.
            let in_word = is_word_character(character);
            if in_word && !metrics.ends_in_word {
                // Punctuation, marks, emoji, controls, and line endings separate successive L/N runs.
                metrics.word_count += 1;
            }
            metrics.ends_in_word = in_word;
            metrics.utf16_len += character.len_utf16();
            // Maintain conservative syntax risk in the existing aggregate pass, including edited pieces.
            metrics.contains_ascii_less_than |= character == '<';
            match character {
                // A CR starts a line break immediately; a later LF may merge with it.
                '\r' => {
                    metrics.line_breaks += 1;
                    metrics.contains_carriage_return = true;
                    previous_was_carriage_return = true;
                }
                // The LF completes the existing CRLF break and records the line-ending form.
                '\n' if previous_was_carriage_return => {
                    metrics.contains_crlf = true;
                    previous_was_carriage_return = false;
                }
                // A standalone LF starts its own line break.
                '\n' => {
                    metrics.line_breaks += 1;
                    previous_was_carriage_return = false;
                }
                // Ordinary text ends the CRLF lookbehind state.
                _ => previous_was_carriage_return = false,
            }
        }
        metrics
    }

    /// Merge adjacent subtree aggregates, reconciling CRLF and word runs split at the join.
    pub(super) fn joined(left: Self, right: Self) -> Self {
        if left.byte_len == 0 {
            // An empty left side contributes no boundary state and acts as the identity.
            return right;
        }
        if right.byte_len == 0 {
            // An empty right side likewise preserves all left aggregate state.
            return left;
        }
        let boundary_is_crlf = left.ends_with_carriage_return && right.starts_with_line_feed;
        Self {
            byte_len: left.byte_len + right.byte_len,
            utf16_len: left.utf16_len + right.utf16_len,
            character_count: left.character_count + right.character_count
                - usize::from(boundary_is_crlf),
            word_count: left.word_count + right.word_count
                - usize::from(left.ends_in_word && right.starts_in_word),
            starts_in_word: left.starts_in_word,
            ends_in_word: right.ends_in_word,
            line_breaks: left.line_breaks + right.line_breaks - usize::from(boundary_is_crlf),
            starts_with_line_feed: left.starts_with_line_feed,
            ends_with_carriage_return: right.ends_with_carriage_return,
            contains_carriage_return: left.contains_carriage_return
                || right.contains_carriage_return,
            contains_crlf: left.contains_crlf || right.contains_crlf || boundary_is_crlf,
            contains_ascii_less_than: left.contains_ascii_less_than
                || right.contains_ascii_less_than,
        }
    }
}

/// An absent root denotes empty text without a synthetic sentinel allocation.
pub(super) type PieceTreeLink = Option<Arc<MarkdownPieceTreeNode>>;

#[derive(Clone, Debug)]
/// Persistent AVL node; Arc ownership lets history share unchanged immutable subtrees.
pub(super) enum MarkdownPieceTreeNode {
    Leaf(MarkdownPiece),
    Branch {
        left: Arc<MarkdownPieceTreeNode>,
        right: Arc<MarkdownPieceTreeNode>,
        metrics: MarkdownDocumentMetrics,
        height: u16,
    },
}

impl MarkdownPieceTreeNode {
    /// Read cached subtree aggregates without scanning document text.
    pub(super) fn metrics(&self) -> MarkdownDocumentMetrics {
        match self {
            // A leaf owns the metrics of its immutable byte slice.
            Self::Leaf(piece) => piece.metrics,
            // A branch stores the already-reconciled aggregate of its children.
            Self::Branch { metrics, .. } => *metrics,
        }
    }

    /// Read the cached AVL height used by path-copying joins and rotations.
    pub(super) fn height(&self) -> u16 {
        match self {
            // A leaf is the nonempty tree height base case.
            Self::Leaf(_) => 1,
            // A branch carries its height so balancing never walks the whole subtree.
            Self::Branch { height, .. } => *height,
        }
    }
}

/// Index the mapped original text in bounded UTF-8 leaves while hashing the initial revision.
pub(super) fn build_original_piece_tree(text: &str) -> (PieceTreeLink, MarkdownDocumentRevision) {
    let mut leaves = Vec::with_capacity(text.len().div_ceil(ORIGINAL_PIECE_BYTES));
    let mut hasher = Sha256::new();
    let mut byte_start = 0;
    while byte_start < text.len() {
        // Build each leaf and hash the same byte sequence in document order.
        let mut byte_end = (byte_start + ORIGINAL_PIECE_BYTES).min(text.len());
        while !text.is_char_boundary(byte_end) {
            // Keep leaf endpoints on scalar boundaries so later slices can be decoded independently.
            byte_end -= 1;
        }
        let slice = &text[byte_start..byte_end];
        hasher.update(slice.as_bytes());
        leaves.push(Arc::new(MarkdownPieceTreeNode::Leaf(MarkdownPiece {
            source: MarkdownPieceSource::Original,
            byte_start,
            byte_len: slice.len(),
            metrics: MarkdownDocumentMetrics::from_text(slice),
        })));
        byte_start = byte_end;
    }
    if text.is_empty() {
        // The empty document still receives the canonical SHA-256 digest for zero bytes.
        hasher.update([]);
    }
    let revision = MarkdownDocumentRevision(hasher.finalize().into());
    (build_balanced_piece_tree(leaves), revision)
}

/// Turn ordered leaves into a balanced persistent root, using None for empty text.
pub(super) fn build_balanced_piece_tree(nodes: Vec<Arc<MarkdownPieceTreeNode>>) -> PieceTreeLink {
    if nodes.is_empty() {
        // An empty leaf collection has no root and allocates no sentinel node.
        return None;
    }
    Some(build_balanced_piece_tree_slice(&nodes))
}

/// Recursively bisect ordered leaves to establish a balanced initial tree.
pub(super) fn build_balanced_piece_tree_slice(
    nodes: &[Arc<MarkdownPieceTreeNode>],
) -> Arc<MarkdownPieceTreeNode> {
    if nodes.len() == 1 {
        // Reuse a singleton leaf by Arc rather than copying its text or metadata.
        return Arc::clone(&nodes[0]);
    }
    let middle = nodes.len() / 2;
    make_piece_tree_branch(
        build_balanced_piece_tree_slice(&nodes[..middle]),
        build_balanced_piece_tree_slice(&nodes[middle..]),
    )
}

/// Create one immutable branch with recomputed metrics and height from its owned children.
pub(super) fn make_piece_tree_branch(
    left: Arc<MarkdownPieceTreeNode>,
    right: Arc<MarkdownPieceTreeNode>,
) -> Arc<MarkdownPieceTreeNode> {
    Arc::new(MarkdownPieceTreeNode::Branch {
        metrics: MarkdownDocumentMetrics::joined(left.metrics(), right.metrics()),
        height: left.height().max(right.height()) + 1,
        left,
        right,
    })
}

/// Concatenate optional trees while preserving byte order and sharing unchanged roots.
pub(super) fn join_piece_trees(left: PieceTreeLink, right: PieceTreeLink) -> PieceTreeLink {
    match (left, right) {
        // An absent prefix leaves the suffix root unchanged.
        (None, right) => right,
        // An absent suffix leaves the prefix root unchanged.
        (left, None) => left,
        // Two nonempty sides require a height-aware persistent join.
        (Some(left), Some(right)) => Some(join_piece_tree_nodes(left, right)),
    }
}

/// Join nonempty AVL trees by descending only the taller spine and rebuilding that path.
pub(super) fn join_piece_tree_nodes(
    left: Arc<MarkdownPieceTreeNode>,
    right: Arc<MarkdownPieceTreeNode>,
) -> Arc<MarkdownPieceTreeNode> {
    let left_height = left.height();
    let right_height = right.height();
    if left_height > right_height + 1 {
        // Descend the left tree's right spine so the joined subtree remains height compatible.
        let MarkdownPieceTreeNode::Branch {
            left: left_left,
            right: left_right,
            ..
        } = left.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        return rebalance_piece_tree(
            Arc::clone(left_left),
            join_piece_tree_nodes(Arc::clone(left_right), right),
        );
    }
    if right_height > left_height + 1 {
        // Descend the right tree's left spine for the symmetric height imbalance.
        let MarkdownPieceTreeNode::Branch {
            left: right_left,
            right: right_right,
            ..
        } = right.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        return rebalance_piece_tree(
            join_piece_tree_nodes(left, Arc::clone(right_left)),
            Arc::clone(right_right),
        );
    }
    make_piece_tree_branch(left, right)
}

/// Restore the AVL height invariant with order-preserving single or double rotations.
pub(super) fn rebalance_piece_tree(
    left: Arc<MarkdownPieceTreeNode>,
    right: Arc<MarkdownPieceTreeNode>,
) -> Arc<MarkdownPieceTreeNode> {
    if left.height() > right.height() + 1 {
        // A left-heavy pair must rotate before becoming a reusable branch.
        let MarkdownPieceTreeNode::Branch {
            left: left_left,
            right: left_right,
            ..
        } = left.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        if left_left.height() >= left_right.height() {
            // An outer-heavy left child needs one right rotation; all unaffected subtrees remain shared.
            return make_piece_tree_branch(
                Arc::clone(left_left),
                make_piece_tree_branch(Arc::clone(left_right), right),
            );
        }
        let MarkdownPieceTreeNode::Branch {
            left: middle_left,
            right: middle_right,
            ..
        } = left_right.as_ref()
        else {
            // The inner child is taller than its leaf sibling, which requires another branch.
            unreachable!("a double rotation requires an inner branch");
        };
        return make_piece_tree_branch(
            make_piece_tree_branch(Arc::clone(left_left), Arc::clone(middle_left)),
            make_piece_tree_branch(Arc::clone(middle_right), right),
        );
    }
    if right.height() > left.height() + 1 {
        // A right-heavy pair uses the symmetric rotation rules.
        let MarkdownPieceTreeNode::Branch {
            left: right_left,
            right: right_right,
            ..
        } = right.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        if right_right.height() >= right_left.height() {
            // An outer-heavy right child needs one left rotation rather than a double rotation.
            return make_piece_tree_branch(
                make_piece_tree_branch(left, Arc::clone(right_left)),
                Arc::clone(right_right),
            );
        }
        let MarkdownPieceTreeNode::Branch {
            left: middle_left,
            right: middle_right,
            ..
        } = right_left.as_ref()
        else {
            // The inner child is taller than its leaf sibling, which requires another branch.
            unreachable!("a double rotation requires an inner branch");
        };
        return make_piece_tree_branch(
            make_piece_tree_branch(left, Arc::clone(middle_left)),
            make_piece_tree_branch(Arc::clone(middle_right), Arc::clone(right_right)),
        );
    }
    make_piece_tree_branch(left, right)
}

/// Split at a scalar-aligned byte position, sharing untouched subtrees for undo history.
pub(super) fn split_piece_tree(
    document: &MarkdownDocumentBuffer,
    root: PieceTreeLink,
    byte_offset: usize,
) -> Result<(PieceTreeLink, PieceTreeLink), MarkdownDocumentBufferError> {
    let Some(root) = root else {
        // Empty storage has only its zero boundary; reject any positive requested extent below.
        return if byte_offset == 0 {
            // Only offset zero exists in an empty document.
            Ok((None, None))
        } else {
            // A nonzero position cannot be represented by an absent root.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        };
    };
    if byte_offset == 0 {
        // A zero split transfers the complete existing tree to the suffix.
        return Ok((None, Some(root)));
    }
    if byte_offset == root.metrics().byte_len {
        // An end split transfers the complete existing tree to the prefix.
        return Ok((Some(root), None));
    }
    if byte_offset > root.metrics().byte_len {
        // Offsets beyond the aggregate byte length are rejected before slicing.
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    }

    match root.as_ref() {
        // Only the intersected leaf is divided; its bytes continue to borrow their original storage.
        MarkdownPieceTreeNode::Leaf(piece) => {
            let bytes = document.piece_bytes(piece);
            if !std::str::from_utf8(bytes)
                .expect("piece bytes remain valid UTF-8")
                .is_char_boundary(byte_offset)
            {
                // Reject a split inside a UTF-8 scalar rather than producing invalid leaf slices.
                return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
            }
            let left_piece = markdown_piece_subrange(piece, bytes, 0..byte_offset);
            let right_piece = markdown_piece_subrange(piece, bytes, byte_offset..bytes.len());
            Ok((
                Some(Arc::new(MarkdownPieceTreeNode::Leaf(left_piece))),
                Some(Arc::new(MarkdownPieceTreeNode::Leaf(right_piece))),
            ))
        }
        // Use subtree byte aggregates to split only the side containing the requested boundary.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            let left_bytes = left.metrics().byte_len;
            if byte_offset < left_bytes {
                // A boundary inside the left child retains the right child as part of the suffix.
                let (prefix, left_suffix) =
                    split_piece_tree(document, Some(Arc::clone(left)), byte_offset)?;
                Ok((
                    prefix,
                    join_piece_trees(left_suffix, Some(Arc::clone(right))),
                ))
            } else if byte_offset == left_bytes {
                // A boundary between children can reuse both roots without allocating leaf fragments.
                Ok((Some(Arc::clone(left)), Some(Arc::clone(right))))
            } else {
                // A boundary inside the right child retains the left child as part of the prefix.
                let (right_prefix, suffix) =
                    split_piece_tree(document, Some(Arc::clone(right)), byte_offset - left_bytes)?;
                Ok((
                    join_piece_trees(Some(Arc::clone(left)), right_prefix),
                    suffix,
                ))
            }
        }
    }
}

/// Create a metadata-only slice of a validated UTF-8 piece and recompute its local aggregates.
pub(super) fn markdown_piece_subrange(
    piece: &MarkdownPiece,
    bytes: &[u8],
    range: Range<usize>,
) -> MarkdownPiece {
    let text = std::str::from_utf8(&bytes[range.clone()]).expect("piece subrange is UTF-8");
    MarkdownPiece {
        source: piece.source,
        byte_start: piece.byte_start + range.start,
        byte_len: range.len(),
        metrics: MarkdownDocumentMetrics::from_text(text),
    }
}

/// Translate a UTF-16 boundary through cached subtree sizes and at most one leaf scan.
pub(super) fn byte_offset_for_utf16_in_tree(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    utf16_offset: usize,
    accumulated_bytes: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    let Some(root) = root else {
        // Empty storage has only its zero boundary; reject any positive requested extent below.
        return if utf16_offset == 0 {
            // The empty subtree has exactly one boundary, at offset zero.
            Ok(accumulated_bytes)
        } else {
            // A positive UTF-16 position cannot exist without a subtree.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        };
    };
    match root {
        // Decode the selected leaf to distinguish scalar boundaries from surrogate interiors.
        MarkdownPieceTreeNode::Leaf(piece) => {
            let text = std::str::from_utf8(document.piece_bytes(piece))
                .expect("piece bytes remain valid UTF-8");
            let mut traversed_utf16 = 0;
            for (byte_index, character) in text.char_indices() {
                // Advance by Unicode scalar while retaining the UTF-16 length consumed before it.
                if traversed_utf16 == utf16_offset {
                    // A completed scalar prefix identifies an exact byte boundary.
                    return Ok(accumulated_bytes + byte_index);
                }
                let next = traversed_utf16 + character.len_utf16();
                if utf16_offset < next {
                    // The requested unit lies inside a surrogate pair and cannot define a text edit boundary.
                    return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
                }
                traversed_utf16 = next;
            }
            if traversed_utf16 == utf16_offset {
                // The position immediately after the final scalar is a valid leaf boundary.
                Ok(accumulated_bytes + text.len())
            } else {
                // Any remaining requested units lie beyond this leaf.
                Err(MarkdownDocumentBufferError::RangeOutsideDocument)
            }
        }
        // Descend using cached UTF-16 lengths while carrying the corresponding byte prefix.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if utf16_offset <= left.metrics().utf16_len {
                // The target boundary belongs to the left subtree, including its end position.
                byte_offset_for_utf16_in_tree(
                    document,
                    Some(left.as_ref()),
                    utf16_offset,
                    accumulated_bytes,
                )
            } else {
                // Subtract the left UTF-16 prefix and add its bytes before entering the right subtree.
                byte_offset_for_utf16_in_tree(
                    document,
                    Some(right.as_ref()),
                    utf16_offset - left.metrics().utf16_len,
                    accumulated_bytes + left.metrics().byte_len,
                )
            }
        }
    }
}

/// Translate a small standalone string boundary with the same surrogate rejection as tree coordinates.
pub(super) fn byte_offset_for_utf16_text(
    text: &str,
    utf16_offset: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    let mut traversed_utf16 = 0;
    for (byte_index, character) in text.char_indices() {
        // Inspect scalar boundaries in order without allocating a UTF-16 copy.
        if traversed_utf16 == utf16_offset {
            // The consumed unit count identifies an exact byte boundary.
            return Ok(byte_index);
        }
        let next = traversed_utf16 + character.len_utf16();
        if utf16_offset < next {
            // A partial surrogate pair cannot be used as a replacement endpoint.
            return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
        }
        traversed_utf16 = next;
    }
    if traversed_utf16 == utf16_offset {
        // The end-of-string boundary is valid after all scalars are consumed.
        Ok(text.len())
    } else {
        // An offset beyond the encoded length is outside the supplied text.
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    }
}

/// Translate a scalar-aligned byte offset into document UTF-16 coordinates.
pub(super) fn utf16_offset_for_byte_in_tree(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    byte_offset: usize,
    accumulated_utf16: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    let Some(root) = root else {
        // Empty storage has only its zero boundary; reject any positive requested extent below.
        return if byte_offset == 0 {
            // The empty subtree contributes no UTF-16 units at its only boundary.
            Ok(accumulated_utf16)
        } else {
            // A positive byte offset requires nonempty storage.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        };
    };
    match root {
        // Decode only the selected leaf to count the UTF-16 prefix.
        MarkdownPieceTreeNode::Leaf(piece) => {
            let text = std::str::from_utf8(document.piece_bytes(piece))
                .expect("piece bytes remain valid UTF-8");
            if !text.is_char_boundary(byte_offset) {
                // A byte inside a multibyte scalar has no valid UTF-16 edit boundary.
                return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
            }
            Ok(accumulated_utf16 + text[..byte_offset].encode_utf16().count())
        }
        // Branch aggregates select a single descent and preserve the accumulated UTF-16 prefix.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if byte_offset <= left.metrics().byte_len {
                // The boundary lies in the left subtree, including the shared child boundary.
                utf16_offset_for_byte_in_tree(
                    document,
                    Some(left.as_ref()),
                    byte_offset,
                    accumulated_utf16,
                )
            } else {
                // The right-side coordinate excludes the left bytes and includes its UTF-16 length.
                utf16_offset_for_byte_in_tree(
                    document,
                    Some(right.as_ref()),
                    byte_offset - left.metrics().byte_len,
                    accumulated_utf16 + left.metrics().utf16_len,
                )
            }
        }
    }
}

/// Read one UTF-16 code unit, including either half of a surrogate pair for NSString semantics.
pub(super) fn utf16_code_unit_in_tree(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    utf16_offset: usize,
) -> Result<u16, MarkdownDocumentBufferError> {
    let root = root.ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
    match root {
        // Only the selected leaf needs scalar decoding; nth may intentionally return a surrogate half.
        MarkdownPieceTreeNode::Leaf(piece) => {
            let text = std::str::from_utf8(document.piece_bytes(piece))
                .expect("piece bytes remain valid UTF-8");
            text.encode_utf16()
                .nth(utf16_offset)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)
        }
        // Cached UTF-16 lengths locate the leaf without flattening the document.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if utf16_offset < left.metrics().utf16_len {
                // A strictly smaller position addresses a unit in the left child.
                utf16_code_unit_in_tree(document, Some(left.as_ref()), utf16_offset)
            } else {
                // The right child receives an offset relative to its own first unit.
                utf16_code_unit_in_tree(
                    document,
                    Some(right.as_ref()),
                    utf16_offset - left.metrics().utf16_len,
                )
            }
        }
    }
}

/// Copy a bounded UTF-16 range into caller storage, pruning nonintersecting subtrees.
pub(super) fn copy_utf16_tree_range(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    range: Range<usize>,
    accumulated_utf16: usize,
    destination: &mut [u16],
    written: &mut usize,
) -> Result<(), MarkdownDocumentBufferError> {
    let Some(root) = root else {
        // Empty text contributes no bytes or code units and cannot invoke the output callback.
        return Ok(());
    };
    let node_start = accumulated_utf16;
    let node_end = node_start + root.metrics().utf16_len;
    if range.end <= node_start || range.start >= node_end {
        // A disjoint node contributes no units and requires no text decoding.
        return Ok(());
    }

    match root {
        // Clamp the overlap to this leaf and reserve only the corresponding destination slice.
        MarkdownPieceTreeNode::Leaf(piece) => {
            let local_start = range.start.saturating_sub(node_start);
            let local_end = (range.end - node_start).min(piece.metrics.utf16_len);
            let requested_units = local_end.saturating_sub(local_start);
            let destination_end = written
                .checked_add(requested_units)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
            let output = destination
                .get_mut(*written..destination_end)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
            let text = std::str::from_utf8(document.piece_bytes(piece))
                .expect("piece bytes remain valid UTF-8");
            for (slot, code_unit) in output
                .iter_mut()
                .zip(text.encode_utf16().skip(local_start).take(requested_units))
            {
                // Copy exactly the requested code units; partial surrogate ranges are valid NSString reads.
                *slot = code_unit;
            }
            *written = destination_end;
            Ok(())
        }
        // Visit children in document order so the mutable output cursor advances monotonically.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            copy_utf16_tree_range(
                document,
                Some(left.as_ref()),
                range.clone(),
                accumulated_utf16,
                destination,
                written,
            )?;
            copy_utf16_tree_range(
                document,
                Some(right.as_ref()),
                range,
                accumulated_utf16 + left.metrics().utf16_len,
                destination,
                written,
            )
        }
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Locate the byte after one logical line break while merging cross-leaf CRLF pairs.
pub(super) fn find_line_break_end(
    document: &MarkdownDocumentBuffer,
    root: &Arc<MarkdownPieceTreeNode>,
    break_index: usize,
    accumulated_bytes: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    match root.as_ref() {
        // Scan only the selected leaf and distinguish paired from standalone line endings.
        MarkdownPieceTreeNode::Leaf(piece) => {
            let bytes = document.piece_bytes(piece);
            let mut seen = 0;
            let mut index = 0;
            while index < bytes.len() {
                // Each iteration consumes at least one byte and counts only logical line breaks.
                let break_end = match bytes[index] {
                    // A local CRLF is one break whose endpoint follows both bytes.
                    b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                        index += 2;
                        Some(index)
                    }
                    // A standalone CR or LF contributes one one-byte break.
                    b'\r' | b'\n' => {
                        index += 1;
                        Some(index)
                    }
                    // Ordinary UTF-8 bytes cannot be ASCII line-ending bytes and need no decoding here.
                    _ => {
                        index += 1;
                        None
                    }
                };
                if let Some(break_end) = break_end {
                    // Only a recognized line ending advances the logical break index.
                    if seen == break_index {
                        // The requested break has been reached; publish its document-relative endpoint.
                        return Ok(accumulated_bytes + break_end);
                    }
                    seen += 1;
                }
            }
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        }
        // Use subtree counts but reconcile a CRLF split across the child boundary.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            let left_metrics = left.metrics();
            let right_metrics = right.metrics();
            let boundary_is_crlf =
                left_metrics.ends_with_carriage_return && right_metrics.starts_with_line_feed;
            if boundary_is_crlf {
                // The left trailing CR and right leading LF represent a single shared line break.
                let independent_left_breaks = left_metrics.line_breaks - 1;
                if break_index < independent_left_breaks {
                    // Breaks before the shared CRLF remain wholly inside the left child.
                    return find_line_break_end(document, left, break_index, accumulated_bytes);
                }
                if break_index == independent_left_breaks {
                    // The shared CRLF ends one byte into the right child.
                    return Ok(accumulated_bytes + left_metrics.byte_len + 1);
                }
                return find_line_break_end(
                    document,
                    right,
                    break_index - independent_left_breaks,
                    accumulated_bytes + left_metrics.byte_len,
                );
            }
            if break_index < left_metrics.line_breaks {
                // Without a boundary CRLF, ordinary subtree break counts directly select the left side.
                find_line_break_end(document, left, break_index, accumulated_bytes)
            } else {
                // The right side receives the remaining break index after the complete left prefix.
                find_line_break_end(
                    document,
                    right,
                    break_index - left_metrics.line_breaks,
                    accumulated_bytes + left_metrics.byte_len,
                )
            }
        }
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Aggregate a validated byte prefix using cached complete subtrees and one partial leaf.
pub(super) fn metrics_for_tree_prefix(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    byte_count: usize,
) -> Result<MarkdownDocumentMetrics, MarkdownDocumentBufferError> {
    let Some(root) = root else {
        // Empty storage has only its zero boundary; reject any positive requested extent below.
        return if byte_count == 0 {
            // An empty tree has a valid empty prefix with identity metrics.
            Ok(MarkdownDocumentMetrics::default())
        } else {
            // A nonempty prefix cannot be represented without a root.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        };
    };
    if byte_count == 0 {
        // The empty prefix requires no leaf decoding.
        return Ok(MarkdownDocumentMetrics::default());
    }
    if byte_count == root.metrics().byte_len {
        // A complete prefix can reuse all cached aggregates.
        return Ok(root.metrics());
    }
    if byte_count > root.metrics().byte_len {
        // Reject oversized prefixes before arithmetic or slicing.
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    }
    match root {
        // Only a partial leaf requires rescanning its scalar-aligned prefix.
        MarkdownPieceTreeNode::Leaf(piece) => {
            let text = std::str::from_utf8(document.piece_bytes(piece))
                .expect("piece bytes remain valid UTF-8");
            if !text.is_char_boundary(byte_count) {
                // Prefix boundaries cannot divide a UTF-8 scalar.
                return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
            }
            Ok(MarkdownDocumentMetrics::from_text(&text[..byte_count]))
        }
        // Complete child metrics avoid rescanning unaffected text.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if byte_count <= left.metrics().byte_len {
                // A prefix contained in the left child excludes the right child entirely.
                metrics_for_tree_prefix(document, Some(left.as_ref()), byte_count)
            } else {
                // A prefix entering the right child combines the complete left aggregate with a partial right one.
                let right_prefix = metrics_for_tree_prefix(
                    document,
                    Some(right.as_ref()),
                    byte_count - left.metrics().byte_len,
                )?;
                Ok(MarkdownDocumentMetrics::joined(
                    left.metrics(),
                    right_prefix,
                ))
            }
        }
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Visit only byte slices intersecting the requested range in document order.
pub(super) fn visit_piece_tree_range(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    range: Range<usize>,
    accumulated_bytes: usize,
    visit: &mut impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
) -> Result<(), MarkdownDocumentBufferError> {
    let Some(root) = root else {
        // Empty text contributes no bytes or code units and cannot invoke the output callback.
        return Ok(());
    };
    let node_start = accumulated_bytes;
    let node_end = node_start + root.metrics().byte_len;
    if range.end <= node_start || range.start >= node_end {
        // Prune disjoint subtrees before borrowing any backing bytes.
        return Ok(());
    }
    match root {
        // Clamp the overlap to the selected leaf and lend it only for this callback.
        MarkdownPieceTreeNode::Leaf(piece) => {
            let local_start = range.start.saturating_sub(node_start);
            let local_end = (range.end - node_start).min(piece.byte_len);
            visit(&document.piece_bytes(piece)[local_start..local_end])
        }
        // Visit the left range first, propagating callback failures before entering the right side.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            visit_piece_tree_range(
                document,
                Some(left.as_ref()),
                range.clone(),
                node_start,
                visit,
            )?;
            visit_piece_tree_range(
                document,
                Some(right.as_ref()),
                range,
                node_start + left.metrics().byte_len,
                visit,
            )
        }
    }
}

/// Stream the live document into a writer without allocating a document-sized String.
pub(super) fn write_piece_tree(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    destination: &mut impl Write,
) -> Result<(), MarkdownDocumentBufferError> {
    let Some(root) = root else {
        // Empty text contributes no bytes or code units and cannot invoke the output callback.
        return Ok(());
    };
    match root {
        // Write an immutable backing slice directly; I/O failure aborts the caller's save transaction.
        MarkdownPieceTreeNode::Leaf(piece) => destination
            .write_all(document.piece_bytes(piece))
            .map_err(|_| {
                // A writer failure propagates without changing the authoritative document tree.
                MarkdownDocumentBufferError::WriteUnavailable
            }),
        // Preserve text order and stop at the first failed child write.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            write_piece_tree(document, Some(left.as_ref()), destination)?;
            write_piece_tree(document, Some(right.as_ref()), destination)
        }
    }
}

/// Feed live bytes into a revision hash without copying or changing tree ownership.
pub(super) fn hash_piece_tree(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    hasher: &mut Sha256,
) {
    let Some(root) = root else {
        // An absent tree contributes the empty byte sequence to the caller's ongoing hash.
        return;
    };
    match root {
        // Hash the leaf's exact stored bytes, preserving the document's original line endings.
        MarkdownPieceTreeNode::Leaf(piece) => hasher.update(document.piece_bytes(piece)),
        // Hash children in text order so tree shape never affects the revision.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            hash_piece_tree(document, Some(left.as_ref()), hasher);
            hash_piece_tree(document, Some(right.as_ref()), hasher);
        }
    }
}

/// Return the root aggregate or the empty-document identity in constant time.
pub(super) fn piece_tree_metrics(root: &PieceTreeLink) -> MarkdownDocumentMetrics {
    root.as_ref()
        .map_or(MarkdownDocumentMetrics::default(), |root| {
            // Use the cached nonempty root aggregate without rescanning source bytes.
            root.metrics()
        })
}

#[cfg(test)]
/// Expose the cached tree height for balance regression assertions.
pub(super) fn piece_tree_height(root: &PieceTreeLink) -> u16 {
    root.as_ref().map_or(0, |root| {
        // Use the cached root height for structural regression measurements.
        root.height()
    })
}

#[cfg(test)]
/// Count retained leaves recursively for structural sharing regression tests.
pub(super) fn piece_tree_leaf_count(root: Option<&MarkdownPieceTreeNode>) -> usize {
    let Some(root) = root else {
        // Empty documents own no leaf allocations.
        return 0;
    };
    match root {
        // A leaf contributes one immutable piece regardless of byte length.
        MarkdownPieceTreeNode::Leaf(_) => 1,
        // Count both disjoint child trees to measure the full persistent shape.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            piece_tree_leaf_count(Some(left.as_ref())) + piece_tree_leaf_count(Some(right.as_ref()))
        }
    }
}

/// Compare persistent root identity to recognize an unchanged saved document.
pub(super) fn piece_tree_links_share_root(left: &PieceTreeLink, right: &PieceTreeLink) -> bool {
    match (left, right) {
        // Two absent roots represent the same empty tree.
        (None, None) => true,
        // Pointer equality recognizes the exact saved root without rescanning text.
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        // An empty and nonempty root cannot represent an unchanged tree.
        _ => false,
    }
}
