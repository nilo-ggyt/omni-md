//! Locate frozen text-checking paragraphs using terminator aggregates, without reading whole paragraphs.

use super::*;

/// Retain an aggregate-proven position or the actual BMP terminator found while decoding a leaf.
struct Terminator {
    offset: usize,
    unit: Option<u16>,
}

/// One immutable-tree range algorithm; the frozen snapshot API owns this module's public query boundary.
pub(super) fn paragraph_range(
    source: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    request: Range<usize>,
) -> Result<Range<usize>, MarkdownDocumentBufferError> {
    // This entry observation rejects an already-failed original source before
    // using immutable aggregate facts. It does not claim that unrelated later
    // operations cannot fail after this query has obtained its result.
    source.source_health()?;
    let length = root.map_or(0, |node| node.metrics().utf16_len);
    if request.start > request.end || request.end > length {
        // Native callers own any permitted clamping. Reject an invalid supplied
        // extent here before touching piece bytes or inventing a replacement range.
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    }
    // The starting paragraph follows the last actual terminator strictly before
    // the request. A caret on the LF of CRLF keeps the existing LF-only semantics.
    let start = previous_terminator(source, root, request.start, 0)?.map_or(0, |offset| offset + 1);
    let anchor = if request.is_empty() {
        request.end // An empty caret observes its position; EOF does not create another code unit.
    } else {
        request.end - 1 // A nonempty request selects its final unit, not the following paragraph boundary.
    };
    let Some(terminator) = next_terminator(source, root, anchor, 0)? else {
        // Without a successor terminator, true EOF closes the final paragraph.
        // A huge unterminated paragraph reaches this result through zero aggregates.
        return Ok(start..length);
    };
    let mut end = terminator.offset + 1;
    let unit = match terminator.unit {
        Some(unit) => unit, // The hit leaf already supplied this character; do not borrow it again.
        None => utf16_code_unit_in_tree(source, root, terminator.offset)?,
    };
    // CRLF may cross original/edited piece boundaries. Only a real CR with an
    // existing successor needs one more unit read to include its original LF.
    if unit == u16::from(b'\r')
        && end < length
        && utf16_code_unit_in_tree(source, root, end)? == u16::from(b'\n')
    {
        end += 1;
    }
    Ok(start..end)
}

/// Find the last terminator strictly before the request, pruning both ends with original aggregates.
fn previous_terminator(
    source: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    before: usize,
    base: usize,
) -> Result<Option<usize>, MarkdownDocumentBufferError> {
    let Some(root) = root else {
        return Ok(None);
    };
    let metrics = root.metrics();
    if metrics.text_checking_terminators == 0 || before <= base {
        // Empty or terminator-free subtrees cannot contribute an earlier boundary.
        // Do not read their text merely to reconfirm the exact zero aggregate.
        return Ok(None);
    }
    let first = base
        + metrics
            .first_text_checking_terminator
            .expect("a nonempty terminator aggregate has a first offset")
            .get()
        - 1;
    let last = base
        + metrics
            .last_text_checking_terminator
            .expect("a nonempty terminator aggregate has a last offset")
            .get()
        - 1;
    if last < before {
        return Ok(Some(last)); // The entire subtree's last original endpoint already supplies the answer.
    }
    if first >= before {
        return Ok(None); // All terminators lie at/after the request; no text lease is needed on this path.
    }
    match root {
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            // Search the nearer right subtree first, visiting the left only after
            // its exact aggregates exclude an answer; AVL height bounds descent.
            match previous_terminator(source, Some(right), before, base + left.metrics().utf16_len)?
            {
                Some(offset) => Ok(Some(offset)),
                None => previous_terminator(source, Some(left), before, base),
            }
        }
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Visit compact records backward with aggregate pruning. Only a
            // partial hit piece containing the requested position may need decoding.
            let mut end = base + span.metrics.utf16_len;
            for ordinal in span.record_range().rev() {
                let piece = span.piece(ordinal)?;
                end -= piece.metrics.utf16_len;
                if let Some(offset) = previous_terminator(
                    source,
                    Some(&MarkdownPieceTreeNode::Leaf(piece)),
                    before,
                    end,
                )? {
                    return Ok(Some(offset));
                }
            }
            Ok(None)
        }
        MarkdownPieceTreeNode::Leaf(piece) => source.with_piece_bytes(piece, |bytes| {
            // The original first/last facts prove a hit in this one finite leaf;
            // its actual text lease is bounded by the logical 4-KiB piece size.
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            let mut offset = base;
            let mut found = None;
            for character in text.chars() {
                if offset >= before {
                    break; // Observation may address an interior surrogate unit; this query grants no edit boundary.
                }
                if is_terminator(character) {
                    found = Some(offset);
                }
                offset += character.len_utf16();
            }
            Ok(Some(found.expect(
                "the immutable first terminator precedes the request",
            )))
        }),
    }
}

/// Find the first terminator at/after the request, decoding only its partial hit leaf when needed.
fn next_terminator(
    source: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    at: usize,
    base: usize,
) -> Result<Option<Terminator>, MarkdownDocumentBufferError> {
    let Some(root) = root else {
        return Ok(None);
    };
    let metrics = root.metrics();
    if metrics.text_checking_terminators == 0 || at >= base + metrics.utf16_len {
        // EOF and terminator-free subtrees have no candidate; neither original
        // mmap nor edited text pages need to be touched on this path.
        return Ok(None);
    }
    let first = base
        + metrics
            .first_text_checking_terminator
            .expect("a nonempty terminator aggregate has a first offset")
            .get()
        - 1;
    let last = base
        + metrics
            .last_text_checking_terminator
            .expect("a nonempty terminator aggregate has a last offset")
            .get()
        - 1;
    if first >= at {
        return Ok(Some(Terminator {
            offset: first,
            unit: None,
        })); // The same immutable root proves the offset; read its character only for a needed CRLF decision.
    }
    if last < at {
        return Ok(None); // All actual terminators precede the target; prune without borrowing a leaf.
    }
    match root {
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            // Search left first and visit right only after the original left
            // aggregate/path excludes a hit; do not scan intervening body text.
            match next_terminator(source, Some(left), at, base)? {
                Some(terminator) => Ok(Some(terminator)),
                None => next_terminator(source, Some(right), at, base + left.metrics().utf16_len),
            }
        }
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Compact records supply validated coordinates for forward search,
            // without retaining or copying any complete paragraph.
            let mut offset = base;
            for ordinal in span.record_range() {
                let piece = span.piece(ordinal)?;
                let next = offset + piece.metrics.utf16_len;
                if let Some(found) = next_terminator(
                    source,
                    Some(&MarkdownPieceTreeNode::Leaf(piece)),
                    at,
                    offset,
                )? {
                    return Ok(Some(found));
                }
                offset = next;
            }
            Ok(None)
        }
        MarkdownPieceTreeNode::Leaf(piece) => source.with_piece_bytes(piece, |bytes| {
            // Original first/last facts guarantee this leaf contains the hit;
            // no neighboring leaf must be decoded to locate the same endpoint.
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            let mut offset = base;
            for character in text.chars() {
                if offset >= at && is_terminator(character) {
                    return Ok(Some(Terminator {
                        offset,
                        unit: Some(character as u16),
                    })); // All three terminators are BMP; retain the actual unit to avoid a repeat lookup.
                }
                offset += character.len_utf16();
            }
            unreachable!("the immutable last terminator follows the request")
        }),
    }
}

fn is_terminator(character: char) -> bool {
    // This text-checking contract excludes NEL and LS even though other paragraph
    // or bidi algorithms may classify them differently.
    matches!(character, '\r' | '\n' | '\u{2029}')
}

#[cfg(test)]
#[path = "text_checking_range/tests.rs"]
mod tests;
