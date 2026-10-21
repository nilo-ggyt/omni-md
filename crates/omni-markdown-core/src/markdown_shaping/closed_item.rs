//! Consume the original coordinator's closed-item ticket without relocating endpoints, using the sole original Job assembly path.

use super::*;
use crate::markdown_line_composer::{ClosedItem, ClosedShapingInput};

struct Payload {
    source: MarkdownLayoutTextSource,
    authority: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFont,
    font_authority: MarkdownLayoutFontAuthority,
    utf16: Range<usize>,
    bytes: Range<usize>,
    options: options::ValidatedOptions,
    cancellation: MarkdownShapingCancellation,
}

/// Move the original Style ticket into this sole continuation; queueing creates no Buffer and reads no adjacent Source.
pub(crate) struct PendingClosedShaping {
    payload: Option<Payload>,
    context: ContextInput,
}

pub(crate) struct ClosedShapingStep {
    pub(crate) bytes: usize,
    pub(crate) scalars: usize,
    pub(crate) job: Option<MarkdownShapingJob>,
}

impl PendingClosedShaping {
    /// Transfer authority when consuming original Style scope; retire the old probe immediately so only this owner continues.
    pub(crate) fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        closed: ClosedItem,
        options: MarkdownShapingOptions,
    ) -> Result<Self> {
        source.health()?;
        if !source.permits(authority) {
            return Err(MarkdownShapingError::SourceMismatch);
        } // Weak Source tickets still require this same frozen payload; coordinates cannot recapture the text.
        if !font.permits(font_authority) {
            return Err(MarkdownShapingError::FontMismatch);
        } // Names, face indices, or equal axes cannot replace the actual Font chosen by original Style authority.
        let ClosedShapingInput {
            source_utf16: utf16,
            source_bytes: bytes,
            context_bytes: context_bounds,
            cancellation,
        } = closed.into_input(authority, font_authority, source.byte_len())?;
        if cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled);
        } // An already cancelled request creates no Buffer or context lease; original source failure was checked first.
        let validated = options::ValidatedOptions::new(options, &font)?;
        let context = ContextInput::new(&bytes, context_bounds);
        Ok(Self {
            payload: Some(Payload {
                source,
                authority: authority.clone(),
                font,
                font_authority: font_authority.clone(),
                utf16,
                bytes,
                options: validated,
                cancellation,
            }),
            context,
        }) // Consume both original GCB tickets while scope lives, without new public coordinates or a replaceable cancellation bit.
    }

    fn guard(&self) -> Result<()> {
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        payload.source.health()?;
        if !payload.source.permits(&payload.authority) {
            return Err(MarkdownShapingError::SourceMismatch);
        }
        if !payload.font.permits(&payload.font_authority) {
            return Err(MarkdownShapingError::FontMismatch);
        }
        if payload.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled);
        } // Even zero-work/source-read turns check original authority; retained coordinates cannot bypass revocation.
        Ok(())
    }

    pub(crate) fn step(&mut self, budget: MarkdownShapingBudget) -> Result<ClosedShapingStep> {
        let result = self.advance(budget);
        if result.is_err() {
            self.payload.take();
        } // Failure returns original Source/Font immediately; an old continuation cannot assemble a newer document instead.
        result
    }

    fn advance(&mut self, budget: MarkdownShapingBudget) -> Result<ClosedShapingStep> {
        self.guard()?;
        if budget.bytes == 0 || budget.scalars == 0 {
            return Err(MarkdownShapingError::InvalidOptions);
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        let (bytes, scalars) = self.context.step(&payload.source, budget)?;
        self.guard()?;
        if !self.context.complete() {
            return Ok(ClosedShapingStep {
                bytes,
                scalars,
                job: None,
            });
        } // Continue the same partial UTF-8 scalar next step; incomplete context creates neither a Job nor glyph FD.
        let prepared = self.context.prepared()?;
        let payload = self.payload.take().ok_or(MarkdownShapingError::Poisoned)?;
        let mut job = MarkdownShapingJob::assemble_with_context(
            payload.source,
            &payload.authority,
            payload.font,
            &payload.font_authority,
            (payload.utf16, payload.bytes, Some(prepared)),
            payload.options,
            payload.cancellation,
        )?;
        job.guard(&payload.authority, &payload.font_authority)?;
        Ok(ClosedShapingStep {
            bytes,
            scalars,
            job: Some(job),
        })
    }
}

/// At most two 20-byte adjacent windows, without another reader, text mirror, or paragraph-sized residual state.
struct ContextInput {
    ranges: [Range<usize>; 2],
    raw: [[u8; 20]; 2],
    used: [usize; 2],
    phase: usize,
    partial: [u8; 4],
    partial_used: usize,
    partial_needed: usize,
    total: usize,
    maximum_borrow: usize,
}

impl ContextInput {
    fn new(bytes: &Range<usize>, bounds: Range<usize>) -> Self {
        Self {
            ranges: [
                bytes.start.saturating_sub(20).max(bounds.start)..bytes.start,
                bytes.end..bytes.end.saturating_add(20).min(bounds.end),
            ],
            raw: [[0; 20]; 2],
            used: [0; 2],
            phase: 0,
            partial: [0; 4],
            partial_used: 0,
            partial_needed: 0,
            total: 0,
            maximum_borrow: 0,
        }
    }

    fn finish_ranges(&mut self) {
        while self.phase < 2 && self.used[self.phase] == self.ranges[self.phase].len() {
            self.phase += 1;
            self.partial_used = 0;
            self.partial_needed = 0;
        } // At most two fixed-window transitions; discard an after-window's UTF-8 tail per decode_context, not as document EOF.
    }

    fn scalar(&mut self, byte: u8) -> Result<usize> {
        if self.partial_used == 0 {
            if self.phase == 0 && self.used[0] < 3 && byte & 0xc0 == 0x80 {
                return Ok(0); // A preceding window may start inside UTF-8; skip at most its actual initial three continuation bytes.
            }
            self.partial_needed = match byte {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(MarkdownShapingError::InvalidUtf8),
            };
        }
        self.partial[self.partial_used] = byte;
        self.partial_used += 1;
        if self.partial_used != self.partial_needed {
            return Ok(0); // Count no scalar before complete decoding; retain residual bytes in this same fixed array.
        }
        std::str::from_utf8(&self.partial[..self.partial_used])
            .map_err(|_| MarkdownShapingError::InvalidUtf8)?;
        self.partial_used = 0;
        Ok(1)
    }

    fn step(
        &mut self,
        source: &MarkdownLayoutTextSource,
        budget: MarkdownShapingBudget,
    ) -> Result<(usize, usize)> {
        self.finish_ranges();
        if self.complete() {
            return Ok((0, 0)); // At actual document edges there may be no context; assemble with zero reads rather than reading beyond EOF.
        }
        let phase = self.phase;
        let at = self.ranges[phase].start + self.used[phase];
        let count = budget
            .bytes
            .min(budget.scalars)
            .min(self.ranges[phase].end - at);
        let mut scalars = 0;
        let mut failure = None;
        source
            .visit_byte_range(at..at + count, |piece| {
                self.maximum_borrow = self.maximum_borrow.max(piece.len());
                for &byte in piece {
                    match self.scalar(byte) {
                        Ok(count) => scalars += count,
                        Err(error) => {
                            failure = Some(error);
                            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                        }
                    }
                    self.raw[phase][self.used[phase]] = byte;
                    self.used[phase] += 1;
                    self.total += 1;
                }
                Ok(())
            })
            .or_else(|error| {
                if matches!(error, MarkdownDocumentBufferError::RangeOutsideDocument)
                    && failure.is_some()
                {
                    Ok(())
                } else {
                    Err(error)
                }
            })?; // Actual original-lease return failure remains a Source error, not hidden by the local decode-error placeholder.
        if let Some(error) = failure {
            return Err(error);
        }
        self.finish_ranges();
        Ok((count, scalars)) // Charge bytes and complete scalars in this read step, without also ingesting center text.
    }

    fn complete(&self) -> bool {
        self.phase == 2
    }

    fn prepared(&self) -> Result<input::PreparedContext> {
        if !self.complete() {
            return Err(MarkdownShapingError::Pending);
        }
        Ok(input::PreparedContext {
            before: input::decode_context(&self.raw[0][..self.used[0]], true)?,
            after: input::decode_context(&self.raw[1][..self.used[1]], false)?,
            bytes: self.total,
            maximum_borrow: self.maximum_borrow,
        }) // Fixed-buffer decoding reuses the original algorithm; assembly neither dereferences Source again nor charges these bytes twice.
    }
}
