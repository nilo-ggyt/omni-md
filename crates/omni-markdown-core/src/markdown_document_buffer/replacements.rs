//! Stream multiple replacements against the immutable old canonical root. The
//! owning transaction publishes the entire candidate once or rolls it all back.

use super::changes::{MarkdownObservedEditError, MarkdownTextChangeObserver};
use super::command_transaction::{PreparedCommand, commit_prepared_command_observed};
use super::formatting::streaming::SourcePieceCursor;
use super::*;
use std::borrow::Cow;

#[derive(Debug)]
/// One replacement in original UTF-16 coordinates. Consume all its parts before
/// requesting the next replacement, without retaining a full match list.
pub struct MarkdownReplacement<P> {
    pub range_utf16: Range<usize>,
    pub parts: P,
}

#[derive(Debug, Eq, PartialEq)]
/// A literal or a range of the old canonical root. Cow may borrow the caller's
/// template for this call; Core retains no such borrow. Large source spans share
/// pieces, while short spans coalesce into normal canonical chunks. Owned strings
/// can be passed with `.into()`.
pub enum MarkdownReplacementPart<'a> {
    Literal(Cow<'a, str>),
    SourceRange(Range<usize>),
}

impl MarkdownDocumentBuffer {
    /// Replace ordered, non-overlapping original ranges in one atomic history entry.
    ///
    /// Both iterator levels must read plain data without native callbacks or
    /// reentering this document. The caller checks the matched generation before
    /// obtaining its mutable borrow. Input, boundary, budget, and final-selection
    /// failures preserve text, redo, selection, and generation. An empty iterator
    /// validates the requested selection and returns None without adopting it.
    /// Large SourceRanges share old subtrees; a 4 KiB scratch coalesces small
    /// fragments so dense matches do not each allocate a separate leaf.
    pub fn apply_replacements<'a, I, P>(
        &mut self,
        replacements: I,
        before: MarkdownTextSelection,
        after: MarkdownTextSelection,
    ) -> Result<Option<MarkdownHistoryTextEdit>, MarkdownDocumentBufferError>
    where
        I: IntoIterator<Item = Result<MarkdownReplacement<P>, MarkdownDocumentBufferError>>,
        P: IntoIterator<Item = Result<MarkdownReplacementPart<'a>, MarkdownDocumentBufferError>>,
    {
        self.apply_replacements_observed(replacements, before, after, changes::NoopObserver)
            .map(|(edit, ())| edit)
            .map_err(changes::legacy_error)
    }

    /// Observe each actual change after consuming that replacement's parts. The
    /// observer may fail, but its candidate output is published only with the text.
    pub fn apply_replacements_observed<'a, I, P, O>(
        &mut self,
        replacements: I,
        before: MarkdownTextSelection,
        after: MarkdownTextSelection,
        observer: O,
    ) -> changes::MarkdownObservedReplacementsResult<O>
    where
        I: IntoIterator<Item = Result<MarkdownReplacement<P>, MarkdownDocumentBufferError>>,
        P: IntoIterator<Item = Result<MarkdownReplacementPart<'a>, MarkdownDocumentBufferError>>,
        O: MarkdownTextChangeObserver,
    {
        self.ensure_text_input_idle()?;
        self.validate_selection(before)?;
        let mut replacements = replacements.into_iter();
        let Some(first) = replacements.next() else {
            // Empty input neither adopts `after` nor clears redo. The observer
            // may derive output from the actual unchanged head; failure publishes
            // no derived result and does not manufacture an empty text command.
            self.validate_selection(after)?;
            let output = observer
                .finish(
                    changes::MarkdownPreparedText {
                        document: self,
                        root: &self.root,
                    },
                    self.selection,
                )
                .map_err(MarkdownObservedEditError::Observer)?;
            return Ok((None, output));
        };
        commit_prepared_command_observed(self, before, observer, |document, observer| {
            let original = document.root.clone();
            prepare_replacements(
                document,
                original,
                std::iter::once(first).chain(replacements),
                after,
                observer,
            )
        })
        .map(|(edit, output)| (Some(edit), output))
    }
}

/// Consume original coordinates monotonically. Keep each leaf's decoded position
/// so dense match boundaries do not repeatedly scan from the start of that leaf.
struct OrderedBoundaries {
    cursor: SourcePieceCursor,
    piece: Option<MarkdownPiece>,
    local_byte: usize,
    remaining_units: usize,
    utf16: usize,
    byte: usize,
}

impl OrderedBoundaries {
    fn new(root: PieceTreeLink, byte_len: usize) -> Self {
        Self {
            cursor: SourcePieceCursor::new(root, 0..byte_len),
            piece: None,
            local_byte: 0,
            remaining_units: 0,
            utf16: 0,
            byte: 0,
        }
    }

    fn byte_for_utf16(
        &mut self,
        document: &MarkdownDocumentBuffer,
        target: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        if target < self.utf16 {
            // The outer loop checks ordering too. Reject retreat here rather
            // than misinterpret an earlier absolute coordinate as a local offset.
            return Err(MarkdownDocumentBufferError::ReplacementRangesNotOrdered);
        }
        while self.utf16 < target {
            if self.piece.is_none() {
                // Retain only the leaf descriptor. No byte lease can survive
                // across a later append that may change edit-segment storage.
                let source = self
                    .cursor
                    .next_piece()
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                self.remaining_units = source.piece.metrics.utf16_len;
                self.local_byte = 0;
                self.piece = Some(source.piece);
            }
            let piece = self.piece.as_ref().expect("the cursor installed one leaf");
            if target - self.utf16 >= self.remaining_units {
                // Skip the complete remaining leaf by its metrics. Original
                // pages with no requested endpoint need not be read at all.
                self.utf16 += self.remaining_units;
                self.byte += piece.byte_len - self.local_byte;
                self.piece = None;
                continue;
            }
            document.with_piece_bytes(piece, |bytes| {
                let text = std::str::from_utf8(&bytes[self.local_byte..])
                    .expect("canonical leaf boundaries remain UTF-8 aligned");
                for scalar in text.chars() {
                    let units = scalar.len_utf16();
                    if self.utf16 + units > target {
                        // A position inside a surrogate pair is not a replacement
                        // boundary; the owning transaction rolls back the candidate.
                        return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
                    }
                    self.utf16 += units;
                    self.byte += scalar.len_utf8();
                    self.local_byte += scalar.len_utf8();
                    self.remaining_units -= units;
                    if self.utf16 == target {
                        // Resume the next boundary here instead of scanning the
                        // same 4 KiB leaf again for every single-character match.
                        break;
                    }
                }
                Ok(())
            })?;
        }
        Ok(self.byte)
    }
}

/// One output scratch coalesces literals and short source spans. Large spans share
/// metadata; short copies use a separate fixed temporary to end the source borrow
/// before appending to edit storage.
struct ReplacementOutput {
    root: PieceTreeLink,
    scratch: [u8; ORIGINAL_PIECE_BYTES],
    length: usize,
    byte_len: usize,
}

impl ReplacementOutput {
    fn new() -> Self {
        Self {
            root: None,
            scratch: [0; ORIGINAL_PIECE_BYTES],
            length: 0,
            byte_len: 0,
        }
    }

    fn grow(
        &mut self,
        additional: usize,
        maximum: usize,
    ) -> Result<(), MarkdownDocumentBufferError> {
        // Account for the complete emitted prefix before copying or sharing it.
        // Later replacements cannot shrink a prefix already emitted to this root.
        self.byte_len = self
            .byte_len
            .checked_add(additional)
            .filter(|length| *length <= maximum)
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        Ok(())
    }

    fn literal(
        &mut self,
        document: &mut MarkdownDocumentBuffer,
        mut text: &str,
    ) -> Result<(), MarkdownDocumentBufferError> {
        // Output grows monotonically, so reject an oversized literal before any
        // allocation or copy, even if later original ranges will be deleted.
        self.grow(text.len(), document.maximum_document_bytes)?;
        while !text.is_empty() {
            let mut end = text.len().min(self.scratch.len() - self.length);
            while !text.is_char_boundary(end) {
                // Retreat at most the UTF-8 tail of one scalar. Every appended
                // canonical leaf must end on a complete encoded character.
                end -= 1;
            }
            if end == 0 {
                // Flush when the next scalar does not fit. An empty scratch can
                // hold even a four-byte scalar, so this cannot stall indefinitely.
                self.flush(document)?;
                continue;
            }
            self.scratch[self.length..self.length + end].copy_from_slice(&text.as_bytes()[..end]);
            self.length += end;
            text = &text[end..];
            if self.length == self.scratch.len() {
                // Append canonical chunks rather than allocating one leaf per
                // tiny replacement in a dense set of matches.
                self.flush(document)?;
            }
        }
        Ok(())
    }

    fn source(
        &mut self,
        document: &mut MarkdownDocumentBuffer,
        original: &PieceTreeLink,
        range: Range<usize>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if range.is_empty() {
            // Empty source spans require neither a leaf nor an append and consume
            // none of the output scratch capacity.
            return Ok(());
        }
        if range.len() >= ORIGINAL_PIECE_BYTES {
            self.grow(range.len(), document.maximum_document_bytes)?;
            self.flush(document)?;
            // Path-copy only the two boundaries. The selected middle subtree and
            // all underlying original/edit bytes remain shared with the old root.
            let (_, suffix) = split_piece_tree(document, original.clone(), range.start)?;
            let (selected, _) = split_piece_tree(document, suffix, range.len())?;
            self.root = join_piece_trees(self.root.take(), selected);
            return Ok(());
        }
        let mut scratch = [0_u8; ORIGINAL_PIECE_BYTES];
        let mut copied = 0;
        visit_piece_tree_range(document, original.as_deref(), range, 0, &mut |bytes| {
            // Complete the read-only source borrow before literal's flush may
            // grow the edit-segment Vec; the temporary has a fixed 4 KiB bound.
            scratch[copied..copied + bytes.len()].copy_from_slice(bytes);
            copied += bytes.len();
            Ok(())
        })?;
        let text = std::str::from_utf8(&scratch[..copied])
            .expect("validated source spans contain whole Unicode scalars");
        self.literal(document, text)
    }

    fn flush(
        &mut self,
        document: &mut MarkdownDocumentBuffer,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if self.length == 0 {
            // SourceRange-only commands create no new text bytes, including when
            // a replacement reuses a capture covering the entire original document.
            return Ok(());
        }
        let text = std::str::from_utf8(&self.scratch[..self.length])
            .expect("scratch ends at a complete Unicode scalar");
        let leaves = document
            .edits
            .append(text, document.maximum_document_bytes)?;
        self.root = join_piece_trees(self.root.take(), build_balanced_piece_tree(leaves));
        self.length = 0;
        Ok(())
    }
}

/// Build one candidate root from original coordinates. The shared command owner
/// holds the append checkpoint and journal transaction, so every early error here
/// discards candidate structure before reclaiming its newly appended storage.
fn prepare_replacements<'a, I, P, O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    original: PieceTreeLink,
    replacements: I,
    after: MarkdownTextSelection,
    observer: &mut O,
) -> Result<PreparedCommand, MarkdownObservedEditError<O::Error>>
where
    I: IntoIterator<Item = Result<MarkdownReplacement<P>, MarkdownDocumentBufferError>>,
    P: IntoIterator<Item = Result<MarkdownReplacementPart<'a>, MarkdownDocumentBufferError>>,
{
    let original_bytes = document.byte_len();
    let original_units = document.utf16_len();
    let mut boundaries = OrderedBoundaries::new(original.clone(), original_bytes);
    let mut output = ReplacementOutput::new();
    let mut previous: Option<Range<usize>> = None;
    let mut first_start = 0;
    let mut consumed_bytes = 0;
    for replacement in replacements {
        let MarkdownReplacement { range_utf16, parts } = replacement?;
        if range_utf16.start > range_utf16.end || range_utf16.end > original_units {
            // Validate the full range before computing lengths; unsigned
            // arithmetic must not hide reversed or out-of-document coordinates.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument.into());
        }
        if let Some(previous) = &previous {
            if range_utf16.start < previous.end
                || (previous.is_empty()
                    && range_utf16.is_empty()
                    && previous.start == range_utf16.start)
            {
                // Touching ranges are valid. Reject overlaps, retreat, and two
                // empty insertions at one position, whose order is ambiguous here.
                return Err(MarkdownDocumentBufferError::ReplacementRangesNotOrdered.into());
            }
        } else {
            // This function receives a nonempty outer iterator. Its first item
            // anchors the single native notification envelope for the whole command.
            first_start = range_utf16.start;
        }
        let start = boundaries.byte_for_utf16(document, range_utf16.start)?;
        let end = boundaries.byte_for_utf16(document, range_utf16.end)?;
        output.source(document, &original, consumed_bytes..start)?;
        let mut inserted = 0_usize;
        for part in parts {
            match part? {
                MarkdownReplacementPart::Literal(text) => {
                    // Borrowed templates live only during this call; owned parts
                    // drop after consumption. Neither is retained by history/root.
                    inserted = inserted
                        .checked_add(text.encode_utf16().count())
                        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
                    output.literal(document, &text)?;
                }
                MarkdownReplacementPart::SourceRange(range) => {
                    if range.start > range.end || range.end > original_units {
                        // Captures, prefixes, and suffixes all address the old
                        // snapshot; none can refer to the candidate being emitted.
                        return Err(MarkdownDocumentBufferError::RangeOutsideDocument.into());
                    }
                    inserted = inserted
                        .checked_add(range.len())
                        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
                    let start = document.byte_offset_for_utf16(range.start)?;
                    let end = document.byte_offset_for_utf16(range.end)?;
                    output.source(document, &original, start..end)?;
                }
            }
        }
        // Emit as soon as one replacement's actual length is known. Input or
        // observer failure rolls back every append through the same transaction.
        changes::emit(document, observer, range_utf16.clone(), inserted)?;
        consumed_bytes = end;
        previous = Some(range_utf16);
    }
    let last = previous.expect("the public entry passes at least one replacement");
    output.source(document, &original, consumed_bytes..original_bytes)?;
    output.flush(document)?;
    // Preserve the untouched prefix/suffix when deriving the one replacement
    // envelope. The final selection is validated against this complete candidate
    // by the owning command before history or canonical text can be published.
    let result_units = piece_tree_metrics(&output.root).utf16_len;
    let outside_units = first_start
        .checked_add(original_units - last.end)
        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
    let inserted_units = result_units
        .checked_sub(outside_units)
        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
    let metrics =
        MarkdownReplacementMetrics::new(first_start, last.end - first_start, inserted_units)?;
    Ok(PreparedCommand {
        root: output.root,
        after,
        metrics,
    })
}

#[cfg(test)]
mod tests;
