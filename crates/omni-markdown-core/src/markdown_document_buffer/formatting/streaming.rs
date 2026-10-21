//! Ordered source traversal and fixed-scratch canonical replacement for dense formatting commands.

use super::*;

/// One source piece plus its selected local bytes; this descriptor borrows no append segment.
pub(crate) struct SourcePieceRange {
    pub(crate) piece: MarkdownPiece,
    pub(crate) local: Range<usize>,
}

/// Retain one immutable root path, an AVL-height stack and a finite Span continuation, not a list of all pieces.
pub(crate) struct SourcePieceCursor {
    stack: Vec<(Arc<MarkdownPieceTreeNode>, usize)>,
    range: Range<usize>,
    span: Option<(OriginalSpan, u8, usize)>,
}

impl SourcePieceCursor {
    /// Hold source roots independently from the unpublished replacement, with no borrowed byte slice.
    pub(crate) fn new(root: PieceTreeLink, range: Range<usize>) -> Self {
        let height = root.as_ref().map_or(0, |root| {
            // Production reads the cached root height directly; test-only counters are not required.
            usize::from(root.height())
        });
        let mut stack = Vec::with_capacity(height);
        if let Some(root) = root {
            // Empty documents need no sentinel; a real source starts at its original byte zero.
            stack.push((root, 0));
        }
        Self {
            stack,
            range,
            span: None,
        }
    }

    /// Yield one scalar-aligned leaf overlap, pruning unselected source subtrees in order.
    pub(crate) fn next_piece(&mut self) -> Option<SourcePieceRange> {
        loop {
            if let Some((span, ordinal, start)) = self.span.take() {
                // Expand one validated compact record at a time. Continuation
                // retains no borrowed bytes; callers still read through the original piece API.
                let piece = span.piece(ordinal).expect("原不可变Span小目录已完整验证");
                let end = start + piece.byte_len;
                if ordinal + 1 < span.record_range().end {
                    // Retain only the shared directory and next logical byte
                    // start; reaching its end returns this directory Arc naturally.
                    self.span = Some((span, ordinal + 1, end));
                }
                if self.range.end > start && self.range.start < end {
                    // Yield only an actual intersection. Pruned pieces neither
                    // borrow the source nor change the original output ordering.
                    return Some(SourcePieceRange {
                        local: self.range.start.saturating_sub(start)
                            ..(self.range.end - start).min(piece.byte_len),
                        piece,
                    });
                }
                continue; // Advance only the finite directory; consumers still receive ordinary pieces up to 4 KiB.
            }
            let (node, start) = self.stack.pop()?;
            // Every iteration consumes a stack node; only intersecting branches add finite children.
            let end = start + node.metrics().byte_len;
            if self.range.end <= start || self.range.start >= end {
                // Disjoint metadata never requires touching backing pages or copying source bytes.
                continue;
            }
            match node.as_ref() {
                MarkdownPieceTreeNode::Leaf(piece) => {
                    // Retain metadata only, so the caller can end its source borrow before appending.
                    return Some(SourcePieceRange {
                        piece: piece.clone(),
                        local: self.range.start.saturating_sub(start)
                            ..(self.range.end - start).min(piece.byte_len),
                    });
                }
                MarkdownPieceTreeNode::OriginalSpan(span) => {
                    // One actual directory leaf becomes one continuation; do
                    // not allocate 16 Arc-backed leaves just to traverse its records.
                    self.span = Some((span.clone(), span.record_range().start, start));
                }
                MarkdownPieceTreeNode::Branch { left, right, .. } => {
                    // LIFO right-then-left traversal yields original byte order without collecting leaves.
                    self.stack
                        .push((Arc::clone(right), start + left.metrics().byte_len));
                    self.stack.push((Arc::clone(left), start));
                }
            }
        }
    }
}

/// Count the line-local ECMAScript whitespace prefix in one ordered pass over immutable pieces.
pub(super) fn scan_heading_whitespace(
    document: &MarkdownDocumentBuffer,
    start_utf16: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    // Validate the first scalar boundary once; every subsequent piece boundary already preserves UTF-8.
    let start = document.byte_offset_for_utf16(start_utf16)?;
    let mut cursor = SourcePieceCursor::new(document.root.clone(), start..document.byte_len());
    let mut units = 0;
    while let Some(source) = cursor.next_piece() {
        // The borrowed source is read-only, and heading planning appends no bytes or native state.
        let stopped = document.with_piece_bytes(&source.piece, |bytes| {
            let bytes = &bytes[source.local];
            let text =
                std::str::from_utf8(bytes).expect("source piece overlaps remain scalar aligned");
            for scalar in text.chars() {
                // JS whitespace contains only BMP scalars; CR/LF and supplementary text stop this line scan.
                let whitespace = u16::try_from(u32::from(scalar)).is_ok_and(is_heading_whitespace);
                if !whitespace {
                    // An ordinary character or preserved line separator ends the existing heading prefix.
                    return Ok(true);
                }
                units += scalar.len_utf16();
            }
            Ok(false)
        })?;
        if stopped {
            return Ok(units);
        }
    }
    Ok(units)
}

/// Fixed output scratch is flushed directly into the document's normal append-only canonical storage.
pub(crate) struct CanonicalOutput {
    bytes: [u8; ORIGINAL_PIECE_BYTES],
    length: usize,
    root: PieceTreeLink,
}

impl CanonicalOutput {
    /// The builder owns no selected text; an empty unpublished root needs no allocation.
    pub(crate) fn new() -> Self {
        Self {
            bytes: [0; ORIGINAL_PIECE_BYTES],
            length: 0,
            root: None,
        }
    }

    /// Append one complete scalar or constant marker, flushing only at valid UTF-8 boundaries.
    pub(crate) fn push(
        &mut self,
        document: &mut MarkdownDocumentBuffer,
        bytes: &[u8],
    ) -> Result<(), MarkdownDocumentBufferError> {
        if self.length + bytes.len() > self.bytes.len() {
            // Markers and scalars each fit the fixed scratch; flush before either could be divided.
            self.flush(document)?;
        }
        // The complete scalar/marker now fits; copy it atomically so scratch always remains valid UTF-8.
        self.bytes[self.length..self.length + bytes.len()].copy_from_slice(bytes);
        self.length += bytes.len();
        Ok(())
    }

    /// Finish a keyboard command's canonical replacement without publishing its document root.
    pub(crate) fn finish(
        mut self,
        document: &mut MarkdownDocumentBuffer,
    ) -> Result<PieceTreeLink, MarkdownDocumentBufferError> {
        // Flush the finite tail before transferring ownership of the complete candidate subtree.
        self.flush(document)?;
        Ok(self.root)
    }

    /// Add immutable chunk leaves to the unpublished root, without retaining a chunk list or body copy.
    fn flush(
        &mut self,
        document: &mut MarkdownDocumentBuffer,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if self.length == 0 {
            // No synthetic empty piece is needed between source chunks or at completion.
            return Ok(());
        }
        // Append this complete scratch chunk to ordinary canonical segments, under their lifetime budget.
        let text = std::str::from_utf8(&self.bytes[..self.length])
            .expect("output flushes only complete scalars");
        let chunk = append_marker(document, text)?;
        // Transfer the old candidate root into a persistent join; no previous source root is published.
        self.root = join_piece_trees(self.root.take(), chunk);
        self.length = 0;
        Ok(())
    }
}

/// Stream a dense affected interval into new canonical pieces, preserving exact CRLF/CR/LF bytes.
pub(super) fn prepare_dense_line_prefix<O: changes::MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    first: usize,
    last: usize,
    prefix: &'static str,
    observer: &mut O,
) -> Result<PieceTreeLink, changes::MarkdownObservedEditError<O::Error>> {
    // The aggregate edit ends at the final line's start; its existing body remains an original suffix.
    let start = document.byte_offset_for_line(first)?;
    let end = document.byte_offset_for_line(last)?;
    let marker_bytes = (last - first + 1)
        .checked_mul(prefix.len())
        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
    let replacement_bytes = (end - start)
        .checked_add(marker_bytes)
        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
    if document
        .edits
        .used_bytes
        .checked_add(replacement_bytes)
        .is_none_or(|bytes| {
            // Dense output is a real canonical replacement, so its complete bytes consume the edit budget.
            bytes > document.maximum_document_bytes
        })
    {
        // Preflight the entire append before touching segments, root, history, or either selection endpoint.
        return Err(MarkdownDocumentBufferError::EditBufferLimitExceeded.into());
    }

    // A metadata-only cursor keeps old edit-backed source pieces valid without borrowing their segments.
    let mut cursor = SourcePieceCursor::new(document.root.clone(), start..end);
    let mut source_bytes = [0_u8; ORIGINAL_PIECE_BYTES];
    let mut output = CanonicalOutput::new();
    let mut source_units = document.utf16_offset_for_byte(start)?;
    let prefix_units = prefix.encode_utf16().count();
    changes::emit(document, observer, source_units..source_units, prefix_units)?;
    output.push(document, prefix.as_bytes())?;
    let mut pending_carriage_return = false;
    while let Some(source) = cursor.next_piece() {
        // Copy at most one bounded leaf before any append can grow the edit-segment vector. No
        // borrowed source slice crosses a mutable append, including when source is an earlier edit.
        let length = document.with_piece_bytes(&source.piece, |bytes| {
            let bytes = &bytes[source.local];
            source_bytes[..bytes.len()].copy_from_slice(bytes);
            Ok(bytes.len())
        })?;
        let text = std::str::from_utf8(&source_bytes[..length])
            .expect("source overlaps remain scalar aligned");
        for scalar in text.chars() {
            // Delaying a CR's marker by one scalar keeps a split CRLF pair one logical line separator.
            if pending_carriage_return {
                if scalar == '\n' {
                    // The LF completes the prior CR; insert exactly one prefix after the complete pair.
                    output.push(document, b"\n")?;
                    source_units += 1;
                    changes::emit(document, observer, source_units..source_units, prefix_units)?;
                    output.push(document, prefix.as_bytes())?;
                    pending_carriage_return = false;
                    continue;
                }
                // The standalone CR already advanced original coordinates. Emit
                // this line's actual insertion before consuming its next scalar.
                changes::emit(document, observer, source_units..source_units, prefix_units)?;
                output.push(document, prefix.as_bytes())?;
                pending_carriage_return = false;
            }
            // Measure UTF-16 during the same actual source traversal, without
            // another line index or a second decode of the selected body.
            source_units += scalar.len_utf16();
            match scalar {
                '\r' => {
                    // Preserve the CR byte now and wait for the following scalar before inserting a prefix.
                    output.push(document, b"\r")?;
                    pending_carriage_return = true;
                }
                '\n' => {
                    // A standalone LF completes one original line. Its trailing
                    // empty line still has its own actual insertion coordinate.
                    output.push(document, b"\n")?;
                    changes::emit(document, observer, source_units..source_units, prefix_units)?;
                    output.push(document, prefix.as_bytes())?;
                }
                _ => {
                    // Whole scalar encoding keeps every fixed-scratch flush and canonical piece valid UTF-8.
                    let mut encoded = [0_u8; 4];
                    output.push(document, scalar.encode_utf8(&mut encoded).as_bytes())?;
                }
            }
        }
    }
    if pending_carriage_return {
        // The empty line after a selected interval's final standalone CR still
        // owns an insertion anchored in the original source coordinates.
        changes::emit(document, observer, source_units..source_units, prefix_units)?;
        output.push(document, prefix.as_bytes())?;
    }
    output.flush(document)?;
    // Replace the source interval once; the final line body and all outside pieces remain shared.
    Ok(replace_tree_range(
        document,
        document.root.clone(),
        start..end,
        output.root,
    )?)
}
