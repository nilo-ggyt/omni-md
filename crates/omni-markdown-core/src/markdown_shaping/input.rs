//! Borrow bounded original pieces with at most one partial UTF-8 scalar, preserving original global UTF-16 clusters.
use std::ops::Range;

use harfrust::Buffer;

use super::{
    MARKDOWN_LAYOUT_SOURCE_BYTES, MarkdownDocumentBufferError, MarkdownLayoutTextSource,
    MarkdownShapingBudget, MarkdownShapingError as Error, MarkdownShapingJob,
    MarkdownShapingStatus as Status, MarkdownShapingStep, Result,
};

pub(super) struct Input {
    pub byte: usize,
    pub utf16: usize,
    pub scalars: usize,
    pending: [u8; 4],
    used: usize,
    needed: usize,
}

impl Input {
    pub(super) fn new(byte: usize, utf16: usize) -> Self {
        Self {
            byte,
            utf16,
            scalars: 0,
            pending: [0; 4],
            used: 0,
            needed: 0,
        }
    }

    fn push(&mut self, byte: u8, buffer: &mut Buffer) -> Result<bool> {
        if self.used == 0 {
            self.needed = match byte {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(Error::InvalidUtf8),
            }; // Retain the actual scalar width instead of dropping partial UTF-8 and reinterpreting the next page.
        }
        self.pending[self.used] = byte;
        self.used += 1;
        self.byte = self.byte.checked_add(1).ok_or(Error::CoordinateOverflow)?;
        if self.used < self.needed {
            return Ok(false); // Partial input still consumes this step's byte budget; never append half a character.
        }
        let scalar = std::str::from_utf8(&self.pending[..self.used])
            .ok()
            .and_then(|value| value.chars().next())
            .ok_or(Error::InvalidUtf8)?;
        let cluster = u32::try_from(self.utf16).map_err(|_| Error::CoordinateOverflow)?;
        buffer.push(u32::from(scalar), cluster);
        if !buffer.allocation_successful() {
            return Err(Error::ShaperStorage); // A paged-write failure prevents further ingestion or claiming a complete prefix.
        }
        self.utf16 = self
            .utf16
            .checked_add(scalar.len_utf16())
            .ok_or(Error::CoordinateOverflow)?;
        self.scalars = self
            .scalars
            .checked_add(1)
            .ok_or(Error::CoordinateOverflow)?;
        self.used = 0;
        Ok(true)
    }
}

impl MarkdownShapingJob {
    pub(super) fn step_input(
        &mut self,
        budget: MarkdownShapingBudget,
    ) -> Result<MarkdownShapingStep> {
        let cancellation = self.cancellation.clone();
        let payload = self.payload.as_mut().ok_or(Error::Poisoned)?;
        // Each byte can finish at most one scalar; the minimum of byte/scalar remainders and fixed source window bounds every read.
        let count = MARKDOWN_LAYOUT_SOURCE_BYTES
            .min(budget.bytes)
            .min(budget.scalars)
            .min(self.source_bytes.end - self.input.byte);
        let mut failure = None;
        let mut step = MarkdownShapingStep {
            status: Status::Ingesting,
            bytes: 0,
            scalars: 0,
        };
        let visit =
            payload
                .source
                .visit_byte_range(self.input.byte..self.input.byte + count, |piece| {
                    self.source_borrow_max = self.source_borrow_max.max(piece.len());
                    for (index, &byte) in piece.iter().enumerate() {
                        if index.is_multiple_of(128) && cancellation.requested() {
                            failure = Some(Error::Cancelled);
                            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                        } // Observe cancellation within the actual finite input loop; return the lease first so reclaim failure wins.
                        // Count actual dereferenced bytes even when a later failure prevents publication.
                        step.bytes += 1;
                        self.source_bytes_read += 1;
                        match self.input.push(byte, &mut payload.buffer) {
                            Ok(true) => step.scalars += 1,
                            Ok(false) => {} // Retain this same decoder while a scalar spans the current boundary.
                            Err(error) => {
                                failure = Some(error);
                                // The visitor uses a Source error type; retain the actual shaping error locally and restore it after lease return.
                                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                            }
                        }
                    }
                    Ok(())
                });
        let visit = match visit {
            Err(error) if !matches!(error, MarkdownDocumentBufferError::RangeOutsideDocument) => {
                return Err(error.into()); // Actual original-lease return failure precedes the callback's cancellation placeholder.
            }
            result => result,
        };
        if let Some(error) = failure {
            return Err(error);
        }
        visit?;
        if self.input.byte == self.source_bytes.end {
            if self.input.used != 0 || self.input.utf16 != self.source_utf16.end {
                return Err(Error::InvalidUtf8); // Only the actual run end with complete encoding and coordinates may enter shaping.
            }
            self.status = Status::AwaitingShape;
            step.status = self.status;
        }
        if !payload.buffer.paged_checkpoint() || !payload.buffer.allocation_successful() {
            return Err(Error::ShaperStorage); // Actual writeback/reclaim failure in this step permanently retires the job.
        }
        Ok(step)
    }
}

/// The upstream shaper consumes at most five adjacent scalars; use a fixed 20-byte stack window instead of a paragraph String.
pub(super) fn install_context(
    source: &MarkdownLayoutTextSource,
    range: Range<usize>,
    buffer: &mut Buffer,
) -> Result<(usize, usize)> {
    Ok(prepare_context(source, range, None)?.install(buffer))
}

/// Reuse the context decoder with an original Bidi-proved reachable range, without borrowing neighbors across its level boundary.
/// The general public Job omits those bounds and retains the original at-most-20-byte context on each side.
pub(super) fn prepare_context(
    source: &MarkdownLayoutTextSource,
    range: Range<usize>,
    bounds: Option<Range<usize>>,
) -> Result<PreparedContext> {
    let bounds = bounds.unwrap_or(0..source.byte_len());
    if bounds.start > range.start || bounds.end < range.end || bounds.end > source.byte_len() {
        return Err(Error::InvalidRange);
    }
    let before = range.start.saturating_sub(20).max(bounds.start)..range.start;
    let after = range.end..range.end.saturating_add(20).min(bounds.end);
    let mut read = 0;
    let mut maximum = 0;
    let mut decoded = [([0_u32; 5], 0); 2];
    for (range, preceding) in [(before, true), (after, false)] {
        let mut bytes = [0u8; 20];
        let mut used = 0;
        source.visit_byte_range(range, |piece| {
            // The 20-byte request bounds cumulative source callbacks, keeping actual copies within the stack array.
            bytes[used..used + piece.len()].copy_from_slice(piece);
            used += piece.len();
            maximum = maximum.max(piece.len());
            Ok(())
        })?;
        read += used;
        decoded[usize::from(!preceding)] = decode_context(&bytes[..used], preceding)?;
    }
    Ok(PreparedContext {
        before: decoded[0],
        after: decoded[1],
        bytes: read,
        maximum_borrow: maximum,
    }) // Synchronous entry, trials, and prepaid closed items share one decode/reversal contract without another text parser.
}

/// Decode at most 20 already borrowed bytes, preserving original adjacency, UTF-8 edge handling, and five-scalar order across input methods.
pub(super) fn decode_context(bytes: &[u8], preceding: bool) -> Result<([u32; 5], usize)> {
    let mut start = 0;
    if preceding {
        // A fixed preceding window can begin inside UTF-8; skip only leading continuations while the actual run start remains scalar-aligned.
        while start < bytes.len() && bytes[start] & 0xc0 == 0x80 {
            start += 1;
        }
    }
    let text = match std::str::from_utf8(&bytes[start..]) {
        Ok(text) => text,
        Err(error) if !preceding && error.error_len().is_none() => {
            // An after-window may end within UTF-8; deliver only the preceding actual complete neighboring scalars.
            std::str::from_utf8(&bytes[start..start + error.valid_up_to()])
                .map_err(|_| Error::InvalidUtf8)?
        }
        Err(_) => return Err(Error::InvalidUtf8),
    };
    let mut context = [0u32; 5];
    let mut count = 0;
    if preceding {
        // Shaper pre-context starts with the nearest predecessor, not the source's forward logical order.
        for scalar in text.chars().rev().take(5) {
            context[count] = u32::from(scalar);
            count += 1;
        }
    } else {
        for scalar in text.chars().take(5) {
            context[count] = u32::from(scalar);
            count += 1;
        }
    }
    Ok((context, count))
}

pub(super) struct PreparedContext {
    pub(super) before: ([u32; 5], usize),
    pub(super) after: ([u32; 5], usize),
    pub(super) bytes: usize,
    pub(super) maximum_borrow: usize,
}

impl PreparedContext {
    pub(super) fn install(&self, buffer: &mut Buffer) -> (usize, usize) {
        buffer.set_pre_context_codepoints(&self.before.0[..self.before.1]);
        buffer.set_post_context_codepoints(&self.after.0[..self.after.1]);
        (self.bytes, self.maximum_borrow) // Install values already read under original budget without borrowing Source again during assembly.
    }
}
