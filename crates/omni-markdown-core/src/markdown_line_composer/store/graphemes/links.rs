//! Fixed skip links for migrated grapheme records. Text and the original grapheme
//! algorithm remain under the same Break Job.

use super::*;

pub(super) struct Ancestors([Option<(usize, usize)>; usize::BITS as usize]);

impl Ancestors {
    pub(super) fn new() -> Self {
        Self([None; usize::BITS as usize])
    }

    /// As with lines, the lowest-set-bit target still owns its popcount slot before
    /// this publication. Grapheme records may interleave with glyphs/items, so a
    /// record pointer cannot be derived from start plus ordinal.
    pub(super) fn encode(
        &self,
        value: MarkdownGraphemeBoundary,
        ordinal: usize,
        previous: Option<usize>,
    ) -> Result<[u8; 64]> {
        ordinal.checked_add(1).ok_or(Error::Overflow)?;
        let skip = if ordinal == 0 {
            if previous.is_some() {
                return Err(Error::InconsistentCoverage);
            }
            None
        } else {
            let target = ordinal & (ordinal - 1);
            let (record, actual) =
                self.0[target.count_ones() as usize].ok_or(Error::InconsistentCoverage)?;
            if actual != target || previous.is_none_or(|previous| record > previous) {
                return Err(Error::InconsistentCoverage);
            }
            Some((record, actual))
        };
        // Seal only existing predecessor/skip pointers into this fixed candidate.
        // The ancestor table is updated separately, after the actual append succeeds.
        let mut row = [0; 64];
        for (at, value) in [(0, value.byte), (8, value.utf16), (40, ordinal)] {
            row[at..at + 8].copy_from_slice(&number(value)?.to_le_bytes());
        }
        row[16..24].copy_from_slice(
            &previous
                .map(number)
                .transpose()?
                .unwrap_or(NONE)
                .to_le_bytes(),
        );
        row[24..32].copy_from_slice(
            &skip
                .map(|value| number(value.0))
                .transpose()?
                .unwrap_or(NONE)
                .to_le_bytes(),
        );
        row[32..40].copy_from_slice(
            &skip
                .map(|value| number(value.1))
                .transpose()?
                .unwrap_or(NONE)
                .to_le_bytes(),
        );
        row[56] = GRAPHEME;
        Ok(row)
    }

    /// Update a slot only after successful append; a failed partial record cannot
    /// become an authenticated ancestor for subsequent records.
    pub(super) fn publish(&mut self, ordinal: usize, record: usize) -> Result<()> {
        let slot = ordinal.count_ones() as usize;
        *self.0.get_mut(slot).ok_or(Error::Overflow)? = Some((record, ordinal));
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) struct BoundaryRecord {
    pub value: MarkdownGraphemeBoundary,
    pub ordinal: usize,
    pub previous: Option<usize>,
    pub skip: Option<(usize, usize)>,
}

/// Decode this actual 64-byte read without following links or reading text. Every
/// pointer must reference a strictly earlier original record.
pub(super) fn decode(
    row: [u8; 64],
    record: usize,
    count: usize,
    bytes: usize,
    utf16: usize,
) -> Result<BoundaryRecord> {
    if row[56] != GRAPHEME
        || row[48..56]
            .iter()
            .chain(row[57..].iter())
            .any(|byte| *byte != 0)
    {
        return Err(Error::InconsistentCoverage);
    }
    let value = MarkdownGraphemeBoundary {
        byte: usize_at(&row, 0)?,
        utf16: usize_at(&row, 8)?,
    };
    let ordinal = usize_at(&row, 40)?;
    let previous = match u64_at(&row, 16) {
        NONE => None,
        _ => Some(usize_at(&row, 16)?),
    };
    // Check both the exact lowest-set-bit rank and pointer order. An adjacent skip
    // must be the same actual predecessor, not merely a plausible earlier number.
    let skip = match (u64_at(&row, 24), u64_at(&row, 32)) {
        (NONE, NONE) if ordinal == 0 => None,
        (pointer, rank) if pointer != NONE && rank != NONE && ordinal != 0 => {
            let pointer = usize_at(&row, 24)?;
            let rank = usize_at(&row, 32)?;
            if rank != (ordinal & (ordinal - 1))
                || pointer >= record
                || previous.is_none_or(|previous| pointer > previous)
                || (rank.checked_add(1) == Some(ordinal) && Some(pointer) != previous)
            {
                return Err(Error::InconsistentCoverage);
            }
            Some((pointer, rank))
        }
        _ => return Err(Error::InconsistentCoverage),
    };
    // The first actual record is Source start; later records advance both coordinate
    // axes. Directory bounds cannot manufacture a missing start/predecessor proof.
    if ordinal >= count
        || value.byte > bytes
        || value.utf16 > utf16
        || previous.is_some_and(|previous| previous >= record)
        || (ordinal == 0) != previous.is_none()
        || (ordinal == 0 && value != (MarkdownGraphemeBoundary { byte: 0, utf16: 0 }))
        || (ordinal != 0 && (value.byte == 0 || value.utf16 == 0))
    {
        return Err(Error::InconsistentCoverage);
    }
    Ok(BoundaryRecord {
        value,
        ordinal,
        previous,
        skip,
    })
}
