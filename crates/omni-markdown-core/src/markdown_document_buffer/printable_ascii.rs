//! Exact printable-ASCII range facts from original aggregates and scalar-validated mixed endpoints.

use super::*;

#[cfg(feature = "paged-shaping-frontier")]
#[path = "printable_ascii/suffix.rs"]
mod suffix;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use suffix::{FrozenPrintableAsciiSuffix, FrozenPrintableAsciiSuffixStep};

pub(super) fn query(
    source: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    range: Range<usize>,
) -> Result<bool, MarkdownDocumentBufferError> {
    // Source failure precedes both coordinate errors and cached aggregate facts.
    // An already-proven subtree does not authorize reading another unhealthy source.
    source.source_health()?;
    let result = (|| {
        let length = root.map_or(0, |node| node.metrics().utf16_len);
        if range.start > range.end || range.end > length {
            // Reject the original extent rather than clamping it: a fact about
            // a shortened range cannot authorize the caller's complete item.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if root.is_some_and(|node| node.metrics().all_printable_ascii.is_printable()) {
            // Every original scalar is one printable ASCII UTF-16 unit, so all
            // in-range integer endpoints, including empty ranges, are valid.
            // Do not reread boundary leaves for each style/item; outer health checks remain.
            return Ok(true);
        }
        if range.is_empty() {
            // An empty range still needs a real scalar endpoint. Decode only its
            // finite hit piece to reject positions inside a surrogate pair.
            byte_offset_for_utf16_in_tree(source, root, range.start, 0)?;
            return Ok(true);
        }
        within(source, root, range, 0)
    })();
    // Even an aggregate-only path checks shared source health before publishing.
    // A failure observed here takes precedence over a candidate boolean or range error.
    source.source_health()?;
    result
}

fn within(
    source: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    range: Range<usize>,
    base: usize,
) -> Result<bool, MarkdownDocumentBufferError> {
    let Some(node) = root else {
        // An absent subtree contributes the conjunction identity; the entry point
        // has already confined the original request to actual document bounds.
        return Ok(true);
    };
    let metrics = node.metrics();
    let end = base + metrics.utf16_len;
    if range.end <= base || range.start >= end {
        // An unrelated subtree supplies neither bytes nor counterexamples to this
        // range; controls outside the selected extent cannot change its result.
        return Ok(true);
    }
    if metrics.all_printable_ascii.is_printable() {
        // A positive original aggregate proves every intersection and its scalar
        // endpoints. Only partial mixed leaves need decoding; unrelated Unicode
        // must not force repeated reads of already-proven ASCII subtrees.
        return Ok(true);
    }
    if range.start <= base && range.end >= end {
        // Complete-node endpoints were established by scalar-aligned construction.
        // Its exact aggregate can reject a huge mixed extent without touching text pages.
        return Ok(metrics.all_printable_ascii.is_printable());
    }
    match node {
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            // Visit both endpoint paths: a false left predicate cannot hide a
            // right surrogate-interior endpoint or an actual source failure.
            let first = within(source, Some(left), range.clone(), base)?;
            let second = within(source, Some(right), range, base + left.metrics().utf16_len)?;
            Ok(first && second)
        }
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Expand at most the existing 16-entry directory. Complete pieces use
            // aggregates; only partial boundary pieces acquire actual byte leases.
            let mut offset = base;
            let mut printable = true;
            for ordinal in span.record_range() {
                let piece = span.piece(ordinal)?;
                let next = offset + piece.metrics.utf16_len;
                printable &= within(
                    source,
                    Some(&MarkdownPieceTreeNode::Leaf(piece)),
                    range.clone(),
                    offset,
                )?;
                offset = next;
            }
            Ok(printable)
        }
        MarkdownPieceTreeNode::Leaf(piece) => source.with_piece_bytes(piece, |bytes| {
            // One real finite piece validates both endpoints in one borrow, rather
            // than translating offsets and borrowing the same text a second time.
            let text = std::str::from_utf8(bytes).expect("原始piece已经验证UTF-8");
            let start = range.start.saturating_sub(base);
            let end = (range.end - base).min(piece.metrics.utf16_len);
            let mut offset = 0;
            let mut start_valid = start == 0;
            let mut end_valid = end == 0;
            let mut printable = true;
            for scalar in text.chars() {
                // Classify only selected original scalars; Unicode or controls
                // elsewhere in this borrowed piece cannot affect the predicate.
                if offset >= start && offset < end {
                    printable &= (' '..='~').contains(&scalar);
                }
                offset += scalar.len_utf16();
                start_valid |= offset == start;
                end_valid |= offset == end;
            }
            if !start_valid || !end_valid {
                // Reject either surrogate-interior endpoint even after a negative
                // character fact; false must not imply that invalid coordinates were validated.
                return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
            }
            Ok(printable)
        }),
    }
}

#[cfg(test)]
#[path = "printable_ascii/tests.rs"]
mod tests;
