//! Query trailing whitespace in an original complete soft line using actual
//! endpoints, one fixed 64-byte fragment, and at most four UTF-8 continuation bytes.

use super::*;
use crate::MarkdownLineComposeBudget;

/// None requires another original work quantum. Some is the UTF-16 endpoint
/// before complete trailing-whitespace GCBs that alignment may omit. A cluster
/// containing non-whitespace Prepend/combining scalars retains its full width;
/// this fact changes neither editing authority nor the sealed complete advance.
pub struct MarkdownComposedLineTrailingWhitespaceStep {
    /// Source bytes delivered into this step's fixed fragment, including bytes
    /// not yet classified. This is not a kernel page-pin or lease-extent measure.
    pub bytes: usize,
    /// Complete scalars decoded this step; a partial scalar remains in the fixed
    /// continuation and is not charged as an already decoded character.
    pub scalars: usize,
    /// Actual fixed GCB directory reads, charged separately from Source bytes.
    pub records: usize,
    /// Metadata work for original folds/GCB records; directory traversal cannot
    /// disappear behind the separately reported record count.
    pub metadata_units: usize,
    /// Publish only after both scalar whitespace and complete-GCB facts finish;
    /// this endpoint is not an independently reusable authority ticket.
    pub answer: Option<usize>,
}

#[derive(Clone, Copy)]
enum Phase {
    Folds,
    End,
    Start,
    Tail,
    Trim,
    Done,
}

/// Bind original Output/Line/Source weak identity without retaining a reader,
/// document, font, or second directory. The finite reverse-scan fragment ends
/// with this query and never grows with line count or document length.
pub struct MarkdownComposedLineTrailingWhitespace {
    // Another layout width for the same Source cannot resume this line. Weak
    // identity does not extend the original output FD's lifetime.
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    // Equal UTF-16 coordinates cannot stand in for this actual issued line
    // record and its complete LineEnd identity.
    header: usize,
    end: usize,
    range: Range<usize>,
    phase: Phase,
    fold_index: usize,
    // Reuse one actual directory continuation for the end byte, start byte, and
    // final complete-grapheme bound, in that original order.
    boundary: graphemes::BoundarySearch,
    first_byte: usize,
    next_byte: usize,
    utf16: usize,
    // remaining indexes unclassified bytes inside the current fixed fragment;
    // it is not a recaptured Source coordinate for a different read.
    bytes: [u8; 64],
    remaining: usize,
    // One scalar may span pieces or four one-byte quanta. Retain only its exact
    // reverse-order UTF-8 bytes until the original leading byte arrives.
    scalar: [u8; 4],
    scalar_len: usize,
    answer: Option<usize>,
    failed: bool,
}

impl MarkdownComposedLines {
    /// Accept a complete line actually issued by this output. Construction reads
    /// no line record and does not relocate endpoints in a giant paragraph.
    pub fn begin_line_trailing_whitespace(
        &self,
        line: &MarkdownComposedLine,
    ) -> Result<MarkdownComposedLineTrailingWhitespace> {
        self.require(&self.authority)?;
        if !Weak::ptr_eq(&line.output, &Arc::downgrade(&self.identity)) {
            return Err(Error::SourceMismatch);
        }
        if line.facts.kind == MarkdownComposedLineKind::TableRow {
            return Err(Error::TableRange);
        } // A Grid row has no ordinary body-internal GCBs; the actual cell Source owns that query.
        let range = line.facts.source_utf16.clone();
        let empty = range.is_empty();
        // An empty paragraph already has a real sealed line. It needs no text/
        // GCB read, but its original Source checks still govern repeated answers.
        let query = MarkdownComposedLineTrailingWhitespace {
            output: Arc::downgrade(&self.identity),
            source: self.authority.clone(),
            header: line.header,
            end: line.end,
            phase: if empty { Phase::Done } else { Phase::Folds },
            fold_index: 0,
            boundary: graphemes::BoundarySearch::offset(range.end),
            first_byte: 0,
            next_byte: 0,
            utf16: range.end,
            bytes: [0; 64],
            remaining: 0,
            scalar: [0; 4],
            scalar_len: 0,
            answer: empty.then_some(range.start),
            range,
            failed: false,
        };
        self.require(&self.authority)?;
        Ok(query)
    }

    /// Authenticate endpoints in bounded original GCB-directory steps and copy
    /// at most one fixed Source fragment at a time. A scalar can span one-byte
    /// quanta. The original finish boundary preserves Source failure priority
    /// and ends this query's read responsibility on terminal failure.
    pub fn step_line_trailing_whitespace(
        &mut self,
        line: &MarkdownComposedLine,
        query: &mut MarkdownComposedLineTrailingWhitespace,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownComposedLineTrailingWhitespaceStep> {
        let result = self.advance_line_trailing_whitespace(line, query, budget);
        let result = self.finish(result);
        if result.is_err() {
            query.failed = true;
        }
        result
    }

    /// Advance only this original line/query under independent byte, scalar,
    /// record, and metadata budgets; exhaustion never manufactures a boundary.
    fn advance_line_trailing_whitespace(
        &mut self,
        line: &MarkdownComposedLine,
        query: &mut MarkdownComposedLineTrailingWhitespace,
        budget: MarkdownLineComposeBudget,
    ) -> Result<MarkdownComposedLineTrailingWhitespaceStep> {
        self.require(&query.source)?;
        if query.failed {
            return Err(Error::Poisoned);
        }
        if !Weak::ptr_eq(&query.output, &Arc::downgrade(&self.identity))
            || !Weak::ptr_eq(&line.output, &query.output)
            || line.header != query.header
            || line.end != query.end
            || line.facts.source_utf16 != query.range
        {
            return Err(Error::SourceMismatch);
        } // Equal coordinates, another width, or a newer prefix cannot rebind existing byte progress.
        if budget.bytes == 0
            || budget.scalars == 0
            || budget.records == 0
            || budget.metadata_units == 0
        {
            // Every actual work dimension needs a positive caller allowance.
            return Err(Error::InvalidRequest);
        }
        let budget = MarkdownLineComposeBudget {
            bytes: budget.bytes.min(4096),
            scalars: budget.scalars.min(256),
            records: budget.records.min(256),
            metadata_units: budget.metadata_units.min(256),
        };
        // Limits bound actual progress only; exhaustion is neither line/scalar
        // end nor permission to trim a partially established whitespace cluster.
        let mut step = MarkdownComposedLineTrailingWhitespaceStep {
            bytes: 0,
            scalars: 0,
            records: 0,
            metadata_units: 0,
            answer: query.answer,
        };
        loop {
            match query.phase {
                Phase::Done => break,
                Phase::Folds => {
                    let folds = self
                        .payload
                        .as_ref()
                        .ok_or(Error::Poisoned)?
                        .folds
                        .as_ref()
                        .map(|folds| folds.spans());
                    let Some(fold) = folds.and_then(|folds| folds.get(query.fold_index)) else {
                        // No remaining fold can intersect this original line.
                        query.phase = Phase::End;
                        continue;
                    };
                    if step.metadata_units == budget.metadata_units {
                        // Resume the same finite fold cursor in the next quantum.
                        break;
                    }
                    step.metadata_units += 1;
                    query.fold_index += 1;
                    if fold.start.utf16 < query.range.end && query.range.start < fold.end.utf16 {
                        return Err(Error::FoldedRange(fold.start.utf16..fold.end.utf16));
                    } // This output has no hidden-internal GCB authority for an alignment scan.
                    if fold.start.utf16 >= query.range.end {
                        query.phase = Phase::End;
                    }
                }
                Phase::End | Phase::Start | Phase::Trim => {
                    // The same actual directory record consumes record and
                    // metadata allowances; stop when either dimension is spent.
                    let limit = (budget.records - step.records)
                        .min(budget.metadata_units - step.metadata_units);
                    if limit == 0 {
                        break;
                    }
                    let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
                    let actual = query.boundary.step(
                        self.graphemes,
                        source.byte_len(),
                        source.utf16_len(),
                        limit,
                        |record| self.row(record),
                    )?;
                    step.records += actual.records;
                    step.metadata_units += actual.records;
                    let Some((_, Some(boundary))) = actual.answer else {
                        if actual.answer.is_some() {
                            // A completed search without the required endpoint
                            // is corrupt coverage, not ordinary backpressure.
                            return Err(Error::InconsistentCoverage);
                        }
                        break;
                    };
                    if matches!(query.phase, Phase::Trim) {
                        if boundary.utf16 < query.utf16 || boundary.utf16 > query.range.end {
                            return Err(Error::InconsistentCoverage);
                        }
                        query.answer = Some(boundary.utf16);
                        query.phase = Phase::Done;
                        continue;
                    } // Preserve a whole GCB when non-whitespace content shares its trailing spaces.
                    let end = matches!(query.phase, Phase::End);
                    if boundary.utf16
                        != if end {
                            query.range.end
                        } else {
                            query.range.start
                        }
                    {
                        return Err(Error::InconsistentCoverage);
                    } // A lower-bound successor cannot supply the requested line endpoint's byte coordinate.
                    if end {
                        // Locate the actual line start after recording its end;
                        // no full-line byte slice is requested between them.
                        query.next_byte = boundary.byte;
                        query.boundary = graphemes::BoundarySearch::offset(query.range.start);
                        query.phase = Phase::Start;
                    } else {
                        // Both exact endpoints now bound the reverse byte scan.
                        query.first_byte = boundary.byte;
                        query.phase = Phase::Tail;
                    }
                }
                Phase::Tail => {
                    let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
                    query.scan_tail(source, budget, &mut step)?;
                    if matches!(query.phase, Phase::Tail) {
                        break;
                    }
                }
            }
        }
        step.answer = query.answer;
        Ok(step)
    }
}

impl MarkdownComposedLineTrailingWhitespace {
    /// The query owns its finite bytes; Source exists only during each original
    /// borrow. Resume within the actual line start/end, retaining one fragment
    /// and partial scalar when exhausted. The first non-whitespace returns to
    /// the original directory to establish a complete-GCB trimming boundary.
    fn scan_tail(
        &mut self,
        source: &MarkdownLayoutTextSource,
        budget: MarkdownLineComposeBudget,
        step: &mut MarkdownComposedLineTrailingWhitespaceStep,
    ) -> Result<()> {
        while step.scalars < budget.scalars {
            if self.remaining == 0 {
                if self.next_byte == self.first_byte {
                    // Only exact byte/UTF-16 start with no pending half-scalar
                    // establishes that the whole line is whitespace.
                    if self.scalar_len != 0 || self.utf16 != self.range.start {
                        return Err(Error::InconsistentCoverage);
                    }
                    self.answer = Some(self.range.start);
                    self.phase = Phase::Done;
                    return Ok(());
                }
                if !self.read_fragment(source, budget.bytes, step)? {
                    // Preserve this query's original continuation until another
                    // byte quantum arrives; do not recapture text or endpoints.
                    return Ok(());
                }
            }
            let Some(character) = self.receive_scalar()? else {
                // A reverse UTF-8 continuation byte is not a decoded character.
                continue;
            };
            step.scalars += 1;
            if character.is_whitespace() {
                // Move only after a complete scalar, keeping UTF-16 units
                // distinct from the number of bytes read into the fragment.
                self.utf16 = self
                    .utf16
                    .checked_sub(character.len_utf16())
                    .filter(|value| *value >= self.range.start)
                    .ok_or(Error::InconsistentCoverage)?;
                continue;
            }
            if self.utf16 == self.range.end {
                // No whitespace was skipped; the original complete line endpoint
                // already supplies the exact unchanged alignment bound.
                self.answer = Some(self.utf16);
                self.phase = Phase::Done;
            } else {
                // Find the first complete GCB at/after the non-whitespace tail;
                // scalar adjacency alone cannot split a combining cluster.
                self.boundary = graphemes::BoundarySearch::offset(self.utf16);
                self.phase = Phase::Trim;
            }
            return Ok(()); // L1 reset controls are not Unicode whitespace and cannot be consumed as such.
        }
        Ok(())
    }

    /// Copy only this line's remaining actual bytes. Charge prefetched bytes in
    /// the current step even when classification resumes later. A failed read
    /// publishes no fragment; finish owns original Source/cancellation priority
    /// and release of the one output reader.
    fn read_fragment(
        &mut self,
        source: &MarkdownLayoutTextSource,
        byte_budget: usize,
        step: &mut MarkdownComposedLineTrailingWhitespaceStep,
    ) -> Result<bool> {
        let count = (byte_budget - step.bytes)
            .min(self.bytes.len())
            .min(self.next_byte - self.first_byte);
        if count == 0 {
            // An exhausted byte budget grants no additional Source read.
            return Ok(false);
        }
        let start = self.next_byte - count;
        let mut copied = 0;
        source.visit_byte_range(start..self.next_byte, |bytes| {
            self.bytes[copied..copied + bytes.len()].copy_from_slice(bytes);
            copied += bytes.len();
            Ok(())
        })?;
        step.bytes += copied;
        if copied != count {
            return Err(Error::InconsistentCoverage);
        }
        self.remaining = count;
        self.next_byte = start;
        Ok(true)
    }

    /// Reverse fragments may start inside UTF-8. Decode only after the actual
    /// leading byte arrives, then return the fixed continuation. Partial bytes
    /// move no UTF-16 coordinate and cannot stand in for whitespace or a boundary.
    fn receive_scalar(&mut self) -> Result<Option<char>> {
        if self.scalar_len == self.scalar.len() {
            // Another byte after four unresolved bytes is invalid UTF-8;
            // widening a buffer would hide a Source integrity failure.
            return Err(crate::MarkdownDocumentBufferError::InvalidUtf8Document.into());
        }
        self.remaining -= 1;
        let byte = self.bytes[self.remaining];
        self.scalar[self.scalar_len] = byte;
        self.scalar_len += 1;
        if byte & 0xc0 == 0x80 {
            // Keep the reverse continuation until its actual leading byte arrives.
            return Ok(None);
        }
        self.scalar[..self.scalar_len].reverse();
        let scalar = std::str::from_utf8(&self.scalar[..self.scalar_len])
            .map_err(|_| crate::MarkdownDocumentBufferError::InvalidUtf8Document)?;
        let character = scalar.chars().next().ok_or(Error::InconsistentCoverage)?;
        if character.len_utf8() != self.scalar_len {
            // Accept exactly one complete original scalar, never a multi-scalar
            // fragment or a guessed replacement character.
            return Err(Error::InconsistentCoverage);
        }
        self.scalar_len = 0;
        Ok(Some(character))
    }
}
