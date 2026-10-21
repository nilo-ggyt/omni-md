//! Fixed ancestor slots for original LineEnds. Two fields in the same file encode
//! skip links without an in-memory entry per line.

use super::*;

#[derive(Clone, Copy)]
pub(super) struct LineAncestor {
    ordinal: usize,
    end: usize,
}

impl GlyphStore {
    /// Clearing the ordinal's lowest set bit selects this line's skip target.
    /// Intermediate ordinals have more set bits, so popcount slots preserve that
    /// target. Check the next publication count before using at most usize::BITS slots.
    pub(super) fn write_line_skip(&self, row: &mut [u8; 64]) -> Result<()> {
        self.lines.checked_add(1).ok_or(Error::Overflow)?;
        let previous = if self.lines == 0 {
            if self.line_end.is_some() {
                return Err(Error::InconsistentCoverage);
            }
            None
        } else {
            let target = self.lines & (self.lines - 1);
            let ancestor = self.line_ancestors[target.count_ones() as usize]
                .ok_or(Error::InconsistentCoverage)?;
            if ancestor.ordinal != target || self.line_end.is_none_or(|last| ancestor.end > last) {
                return Err(Error::InconsistentCoverage);
            }
            Some(ancestor)
        };
        row[40..48].copy_from_slice(
            &previous
                .map(|value| number(value.end))
                .transpose()?
                .unwrap_or(NONE)
                .to_le_bytes(),
        );
        row[48..56].copy_from_slice(
            &previous
                .map(|value| number(value.ordinal))
                .transpose()?
                .unwrap_or(NONE)
                .to_le_bytes(),
        );
        Ok(())
    }

    /// Publish only after the actual tail record and original permit checks finish.
    /// Update slots with LineEnd publication; no partial line or failed retry leaves
    /// a live skip link. Old readers still see only their own sealed prefix.
    pub(super) fn publish_line(&mut self, end: usize, source_end: usize) -> Result<()> {
        // Grapheme metadata must precede this actual LineEnd and cover its endpoint.
        // Copy fixed facts only: a newer writer directory cannot extend an old
        // reader, and a line-prefix endpoint cannot claim whole-Source EOF.
        let graphemes = self.grapheme_directory()?;
        if graphemes.last >= end || graphemes.covered.utf16 < source_end {
            return Err(Error::InconsistentCoverage);
        }
        let count = self.lines.checked_add(1).ok_or(Error::Overflow)?;
        let slot = self.lines.count_ones() as usize;
        if slot >= self.line_ancestors.len()
            || end >= self.index.as_ref().ok_or(Error::Poisoned)?.len()
            || self.line_end.is_some_and(|previous| previous >= end)
            || source_end < self.line_source_end
            || source_end > self.source.utf16_len()
        {
            return Err(Error::InconsistentCoverage);
        }
        self.line_ancestors[slot] = Some(LineAncestor {
            ordinal: self.lines,
            end,
        });
        self.line_end = Some(end);
        self.line_source_end = source_end;
        self.lines = count;
        self.published_graphemes = Some(graphemes);
        Ok(())
    }
}
