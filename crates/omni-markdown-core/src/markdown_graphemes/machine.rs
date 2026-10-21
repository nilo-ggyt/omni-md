//! Fixed state retains only the suffix needed by Unicode rules. Piece, quantum, and viewport
//! edges do not become paragraph starts or ends.
use super::{
    MarkdownGraphemeBoundary as Boundary, MarkdownGraphemeError as Error,
    MarkdownGraphemeStatus as Status, MarkdownGraphemeStep as Step, Result, store::BoundaryIndex,
    unicode_data,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Properties {
    gcb: u8,
    incb: u8,
    ep: bool,
}
fn property(table: &[(u32, u32, u8)], scalar: u32) -> u8 {
    // Generated ranges are sorted and disjoint. An uncovered scalar uses that property's
    // zero/default value without consulting the host Rust version's Unicode tables.
    let index = table.partition_point(|row| row.0 <= scalar);
    if index == 0 || scalar > table[index - 1].1 {
        0
    } else {
        table[index - 1].2
    }
}
fn properties(scalar: char) -> Properties {
    let scalar = scalar as u32;
    Properties {
        gcb: property(unicode_data::GCB, scalar),
        incb: property(unicode_data::INCB, scalar),
        ep: property(unicode_data::EP, scalar) != 0,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct State {
    previous: Option<u8>,
    ri_odd: bool,
    ep_extend_suffix: bool,
    zwj_after_ep: bool,
    linker_extend_suffix: bool,
}
impl State {
    fn break_before(self, current: Properties) -> bool {
        let Some(previous) = self.previous else {
            return true;
        };
        // GB3 precedes control-character breaks: CR/LF remains one pair across chunks.
        if previous == 1 && current.gcb == 2 {
            return false;
        }
        // Apply GB4/GB5 before extension, Indic, and emoji rules.
        if matches!(previous, 1..=3) || matches!(current.gcb, 1..=3) {
            return true;
        }
        // GB6..8 preserve Hangul composition order; LV/LVT come from the original GCB data.
        if previous == 9 && matches!(current.gcb, 9 | 10 | 12 | 13) {
            return false;
        }
        if matches!(previous, 10 | 12) && matches!(current.gcb, 10 | 11) {
            return false;
        }
        if matches!(previous, 11 | 13) && current.gcb == 11 {
            return false;
        }
        // GB9/9a/9b decide this boundary only; state advancement still processes each scalar.
        if matches!(current.gcb, 4 | 5 | 8) || previous == 7 {
            return false;
        }
        // Unicode 18 GB9c uses Linker Extend* × Consonant without requiring a preceding Consonant.
        if current.incb == 1 && self.linker_extend_suffix {
            return false;
        }
        // GB11 requires GCB Extend and actual EP properties; other InCB Extend values are not emoji context.
        if previous == 5 && current.ep && self.zwj_after_ep {
            return false;
        }
        // GB12/13 retain only RI-run parity, with no unbounded RI list or chunk-based reset.
        if previous == 6 && current.gcb == 6 && self.ri_odd {
            return false;
        }
        true
    }
    fn advance(&mut self, current: Properties) {
        // Derive all suffix facts from the previous state before replacing the preceding
        // property, so scalar boundaries do not sever RI, emoji, or Indic context.
        self.ri_odd = current.gcb == 6 && (self.previous != Some(6) || !self.ri_odd);
        self.zwj_after_ep = current.gcb == 5 && self.ep_extend_suffix;
        self.ep_extend_suffix = current.ep || (current.gcb == 4 && self.ep_extend_suffix);
        self.linker_extend_suffix =
            current.incb == 3 || (current.incb == 2 && self.linker_extend_suffix);
        self.previous = Some(current.gcb);
    }
}

/// The script stage reuses these rules and fixed suffix state without another source or boundary index.
#[cfg(feature = "paged-scripts")]
#[derive(Default)]
pub(crate) struct ScalarCursor(State);
#[cfg(feature = "paged-scripts")]
impl ScalarCursor {
    pub(crate) fn push(&mut self, scalar: char) -> bool {
        let current = properties(scalar);
        let boundary = self.0.break_before(current);
        self.0.advance(current);
        boundary // Classify with the old suffix before advancing the same state; chunks and quanta do not reset it.
    }
}

#[derive(Default)]
pub(super) struct Machine {
    state: State,
    pub(super) byte: usize,
    pub(super) utf16: usize,
    pending: [u8; 4],
    pub(super) used: usize,
    needed: usize,
    scalar_start: usize,
    pub(super) frontier: Option<Boundary>,
    last_boundary: Option<Boundary>,
    pub(super) ready: bool,
}

impl Machine {
    /// An original row ticket proves its initial GB1/GB4 boundary at BOF or after a complete
    /// line ending. Unread row interiors produce no grapheme records.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn begin_table(
        &mut self,
        span: crate::markdown_layout_source::table_rows::TableSpan,
        index: &mut BoundaryIndex,
        step: &mut Step,
        records: usize,
    ) -> Result<bool> {
        if self
            .last_boundary
            .is_none_or(|last| last.byte < span.start.byte)
        {
            if step.records == records {
                return Ok(false);
            }
            index.push(span.start)?;
            self.last_boundary = Some(span.start);
            step.records += 1;
        }
        // Preserve any already committed short prefix. Skip the remaining opaque body;
        // the actual trailing CR/LF will reenter this same DFA and establish its control suffix.
        self.byte = span.tail.byte;
        self.utf16 = span.tail.utf16;
        self.used = 0;
        self.needed = 0;
        self.pending = [0; 4];
        self.state = State::default();
        Ok(true)
    }

    /// Commit the GB4 endpoint only after consuming actual trailing CR/LF; an unterminated
    /// row ending at real Source EOF instead receives GB2 through finish.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn finish_table(
        &mut self,
        span: crate::markdown_layout_source::table_rows::TableSpan,
        index: &mut BoundaryIndex,
        step: &mut Step,
    ) -> Result<()> {
        if self.byte != span.end.byte
            || self.utf16 != span.end.utf16
            || self.used != 0
            || !span.eof && !matches!(self.state.previous, Some(1 | 2))
        {
            return Err(Error::CorruptMetadata);
        }
        if span.eof {
            return self.finish(index, step);
        }
        if self.last_boundary != Some(span.end) {
            index.push(span.end)?;
            self.last_boundary = Some(span.end);
            step.records += 1;
        }
        self.frontier = Some(span.end);
        Ok(())
    }
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn last_committed_boundary(&self) -> Option<Boundary> {
        self.last_boundary
    }

    pub(super) fn push(
        &mut self,
        byte: u8,
        index: &mut BoundaryIndex,
        step: &mut Step,
    ) -> Result<()> {
        self.push_scalar_byte(byte, index, step, false)
    }

    /// Actual trailing CR/LF restores rule state without publishing hidden interior boundaries or frontier.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn push_fold_tail(
        &mut self,
        byte: u8,
        index: &mut BoundaryIndex,
        step: &mut Step,
    ) -> Result<()> {
        self.push_scalar_byte(byte, index, step, true)
    }

    /// One decoder and rule state serve the original source. opaque_tail accepts only real
    /// control bytes at a previously proven hidden region's trailing edge.
    fn push_scalar_byte(
        &mut self,
        byte: u8,
        index: &mut BoundaryIndex,
        step: &mut Step,
        opaque_tail: bool,
    ) -> Result<()> {
        let position = self.byte;
        // This byte was actually dereferenced from a piece. Charge it even when decoding
        // fails below and permanently revokes the caller's job.
        self.byte = self.byte.checked_add(1).ok_or(Error::CoordinateOverflow)?;
        step.bytes = step.bytes.checked_add(1).ok_or(Error::CoordinateOverflow)?;
        if self.used == 0 {
            self.scalar_start = position;
            // Retain only one scalar's leading-length state. The completed from_utf8 check
            // below rejects overlong encoding, surrogate values, and out-of-range scalars.
            self.needed = match byte {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(Error::InvalidUtf8),
            };
        }
        self.pending[self.used] = byte;
        self.used += 1;
        if self.used != self.needed {
            // A borrow ending mid-scalar retains its bytes; the next step resumes the same
            // global position rather than declaring a new start of text.
            return Ok(());
        }
        let scalar = std::str::from_utf8(&self.pending[..self.used])
            .ok()
            .and_then(|value| value.chars().next())
            .ok_or(Error::InvalidUtf8)?;
        if opaque_tail && !matches!(scalar, '\r' | '\n') {
            return Err(Error::CorruptMetadata);
        } // Only actual hard controls can sever arbitrary RI/emoji/Indic suffixes; hidden scalars cannot reset them.
        let boundary = Boundary {
            byte: self.scalar_start,
            utf16: self.utf16,
        };
        let value = properties(scalar);
        if !opaque_tail && self.state.break_before(value) && self.last_boundary != Some(boundary) {
            // Append only rule-confirmed boundaries. An I/O failure returns before updating
            // the committed endpoint or pretending that the record was published.
            index.push(boundary)?;
            self.last_boundary = Some(boundary);
            step.records = step
                .records
                .checked_add(1)
                .ok_or(Error::CoordinateOverflow)?;
        }
        self.state.advance(value);
        if !opaque_tail {
            self.frontier = Some(boundary);
        }
        self.utf16 = self
            .utf16
            .checked_add(scalar.len_utf16())
            .ok_or(Error::CoordinateOverflow)?;
        self.used = 0;
        step.scalars = step
            .scalars
            .checked_add(1)
            .ok_or(Error::CoordinateOverflow)?;
        Ok(())
    }

    /// Jump only after consuming the complete opening delimiter; skipped coordinates incur
    /// no fictional byte reads, decoded scalars, or boundary records.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn jump_fold(
        &mut self,
        from: Boundary,
        to: Boundary,
        visible_start: Boundary,
    ) -> Result<()> {
        if self.used != 0
            || self.byte != from.byte
            || self.utf16 != from.utf16
            || to.byte < from.byte
            || to.utf16 < from.utf16
            || self.last_boundary != Some(visible_start)
            || !matches!(self.state.previous, Some(1 | 2))
        {
            return Err(Error::CorruptMetadata);
        }
        self.byte = to.byte;
        self.utf16 = to.utf16;
        self.frontier = Some(visible_start);
        // Preserve the actual opening control suffix, then consume only actual closing
        // controls. GB4/5 isolate hidden context; unterminated real EOF is handled by finish/GB2.
        Ok(())
    }

    pub(super) fn finish(&mut self, index: &mut BoundaryIndex, step: &mut Step) -> Result<()> {
        let end = Boundary {
            byte: self.byte,
            utf16: self.utf16,
        };
        if self.last_boundary != Some(end) {
            // Empty input still has one 0..0 boundary. The final record consumes this step's record quantum.
            index.push(end)?;
            self.last_boundary = Some(end);
            step.records = step
                .records
                .checked_add(1)
                .ok_or(Error::CoordinateOverflow)?;
        }
        // Seal the actual index before granting complete EOF authority. A sealing failure
        // propagates so the caller permanently revokes the job.
        index.seal()?;
        self.frontier = Some(end);
        self.ready = true;
        step.status = Status::Ready;
        Ok(())
    }
}
