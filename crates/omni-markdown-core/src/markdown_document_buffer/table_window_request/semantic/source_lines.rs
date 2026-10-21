//! Copy bounded actual window lines across split UTF-8 frames, never omitted intervals or the whole table.

use super::*;
use crate::markdown_viewport_presentation::table_window::{
    TABLE_WINDOW_MAX_BYTES, TABLE_WINDOW_MAX_UTF16,
};

#[derive(Clone, Debug)]
pub(super) struct LineSpec {
    pub bytes: Range<usize>,
    pub utf16: Range<usize>,
    pub source_line: usize,
    pub ordinal: usize,
    pub prefix: usize,
    pub delimiter: bool,
}

pub(super) struct OwnedLine {
    pub spec: LineSpec,
    pub text: String,
}
impl OwnedLine {
    pub fn borrowed(&self) -> MarkdownTableSourceLine<'_> {
        MarkdownTableSourceLine {
            text: &self.text,
            byte_start: self.spec.bytes.start,
            utf16_start: self.spec.utf16.start,
            source_line: self.spec.source_line,
            row_ordinal: self.spec.ordinal,
            container_prefix_bytes: self.spec.prefix,
        }
    }
}

pub(super) struct LineCopy {
    // One current line and at most three incomplete UTF-8 bytes survive a step.
    // The complete admitted batch remains finite and contains only ticket members.
    lines: Vec<OwnedLine>,
    index: usize,
    copied_current: usize,
    pending: [u8; 4],
    pending_len: usize,
}
impl LineCopy {
    pub fn new(ticket: &MarkdownTableStructuralWindow) -> Result<Self, Error> {
        // Header/delimiter context and body facts carry their own actual byte,
        // UTF-16, physical-line and ordinal coordinates; none are inferred from window position.
        let h = ticket.header;
        let mut specs = Vec::with_capacity(ticket.body.len() + 2);
        specs.push(LineSpec {
            bytes: h.header_byte_start..h.header_byte_end,
            utf16: ticket.header_utf16.clone(),
            source_line: h.header_source_line,
            ordinal: 0,
            prefix: h.header_prefix_bytes,
            delimiter: false,
        });
        specs.push(LineSpec {
            bytes: h.separator_byte_start..h.separator_byte_end,
            utf16: ticket.delimiter_utf16.clone(),
            source_line: h.separator_source_line,
            ordinal: 0,
            prefix: h.separator_prefix_bytes,
            delimiter: true,
        });
        for row in &ticket.body {
            // Preserve verified member coordinates including original EOL/prefix,
            // rather than deriving an ordinal from a window-local base.
            specs.push(LineSpec {
                bytes: row.byte_start..row.byte_end,
                utf16: row.utf16_start..row.utf16_end,
                source_line: row.source_line,
                ordinal: row.row_ordinal,
                prefix: row.container_prefix_bytes,
                delimiter: false,
            });
        }
        let mut bytes = 0usize;
        let mut units = 0usize;
        for spec in &specs {
            bytes = bytes
                .checked_add(
                    spec.bytes
                        .end
                        .checked_sub(spec.bytes.start)
                        .ok_or(Error::InvalidSource)?,
                )
                .ok_or(Error::SourceLimit)?;
            units = units
                .checked_add(
                    spec.utf16
                        .end
                        .checked_sub(spec.utf16.start)
                        .ok_or(Error::InvalidSource)?,
                )
                .ok_or(Error::SourceLimit)?;
            if bytes > TABLE_WINDOW_MAX_BYTES
                || units > TABLE_WINDOW_MAX_UTF16
                || spec.prefix > spec.bytes.len()
            {
                // Admit the entire batch's actual text before any body String
                // allocation; offscreen header context consumes the same source cap.
                return Err(Error::SourceLimit);
            }
        }
        Ok(Self {
            lines: specs
                .into_iter()
                .map(|spec| OwnedLine {
                    spec,
                    text: String::new(),
                })
                .collect(),
            index: 0,
            copied_current: 0,
            pending: [0; 4],
            pending_len: 0,
        })
    }

    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(SemanticWork, bool), Error> {
        if self.index == self.lines.len() {
            return Ok((SemanticWork::default(), true));
        }
        let tree = tree_height(document);
        // from_utf8 can recheck a valid prefix before a split final scalar, and
        // carry can revalidate up to four bytes. Reserve read/copy/validation work
        // plus fixed control, rather than charging distinct source bytes alone.
        let available = budget.checked_sub(tree + 96).ok_or(Error::SourceLimit)? / 4;
        if available == 0 {
            return Err(Error::SourceLimit);
        }
        let line = &mut self.lines[self.index];
        let next = line.spec.bytes.start + self.copied_current;
        let wanted = available.min(line.spec.bytes.end - next);
        let mut work = SemanticWork {
            source_tree_steps: tree,
            structural_transitions: 64,
            ..SemanticWork::default()
        };
        if line.text.capacity() == 0 {
            // Construction admitted the total batch payload. Reserve only this
            // actual current line now, not every row of the original table.
            line.text.reserve_exact(line.spec.bytes.len());
            work.allocated_source_capacity = line.text.capacity();
        }
        let frame = document
            .table_source_frame(next, wanted.max(1))
            .map_err(|_| Error::InvalidSource)?;
        // This boundary maps source acquisition/reclaim errors to InvalidSource;
        // it does not expose the original OS errno as a separate source result.
        let bytes = &frame.bytes[..frame.bytes.len().min(wanted)];
        if wanted > 0 && bytes.is_empty() {
            return Err(Error::InvalidSource);
        }
        work.source_bytes = bytes.len();
        let before = line.text.len();
        let mut at = 0;
        while self.pending_len > 0 && at < bytes.len() {
            // Carry holds at most three incomplete bytes. Append immediately
            // after completing one real scalar, without retaining an arbitrary text tail.
            self.pending[self.pending_len] = bytes[at];
            self.pending_len += 1;
            at += 1;
            work.utf8_validation_bytes += self.pending_len;
            match std::str::from_utf8(&self.pending[..self.pending_len]) {
                Ok(text) => {
                    line.text.push_str(text);
                    self.pending_len = 0;
                }
                Err(error) if error.error_len().is_none() && self.pending_len < 4 => {}
                Err(_) => return Err(Error::InvalidSource),
            }
        }
        if at < bytes.len() {
            work.utf8_validation_bytes += bytes.len() - at;
            match std::str::from_utf8(&bytes[at..]) {
                Ok(text) => line.text.push_str(text),
                Err(error) if error.error_len().is_none() => {
                    // Only a valid incomplete scalar at the frame end may resume;
                    // malformed UTF-8 is an error, not a request for more input.
                    let prefix_end = at + error.valid_up_to();
                    work.utf8_validation_bytes += prefix_end - at;
                    line.text.push_str(
                        std::str::from_utf8(&bytes[at..prefix_end])
                            .map_err(|_| Error::InvalidSource)?,
                    );
                    let tail = &bytes[prefix_end..];
                    if tail.len() > 3 {
                        return Err(Error::InvalidSource);
                    }
                    self.pending[..tail.len()].copy_from_slice(tail);
                    self.pending_len = tail.len();
                }
                Err(_) => return Err(Error::InvalidSource),
            }
        }
        work.copied_source_bytes = line.text.len() - before;
        self.copied_current += bytes.len();
        if self.copied_current == line.spec.bytes.len() {
            if self.pending_len != 0 || line.text.len() != line.spec.bytes.len() {
                return Err(Error::InvalidSource);
            }
            // Advance only after the actual complete line is copied. Frame end
            // is neither EOL nor permission to insert a synthetic newline.
            self.index += 1;
            self.copied_current = 0;
        }
        // Finish the actual lease before exposing this successful step. The
        // parent drops all partially copied strings if any source/validation stage fails.
        frame.finish().map_err(|_| Error::InvalidSource)?;
        Ok((work, self.index == self.lines.len()))
    }

    pub fn into_lines(self) -> Vec<OwnedLine> {
        self.lines
    }
}
