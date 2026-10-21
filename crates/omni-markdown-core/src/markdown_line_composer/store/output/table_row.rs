//! Restore a prior viewport using this output's sealed TableRow column widths,
//! without remeasuring headers or caching widths for the complete document.
use super::*;
impl MarkdownComposedLines {
    /// Require this same read-only output's line handle. Caller-owned finite
    /// scratch accepts the actual column widths, including both cell insets.
    pub fn table_column_widths(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        line: &MarkdownComposedLine,
        output: &mut [i64],
    ) -> Result<usize> {
        let result = (|| {
            self.require(authority)?;
            self.require_line(line)?;
            if line.facts.kind != MarkdownComposedLineKind::TableRow || line.facts.parts != 1 {
                // Only the original single structural table part owns a sealed
                // width directory; an ordinary glyph line grants no row authority.
                return Err(Error::InvalidRequest);
            }
            // Authenticate the actual part before following its original item
            // pointer. Read only that item's fixed three directory records.
            let part = self.part(line, 0)?.ok_or(Error::InconsistentCoverage)?;
            let meta = self.row(line.header.checked_add(2).ok_or(Error::Overflow)?)?;
            let at = usize_at(&meta, 40)?;
            let mut rows = [[0; 64]; 3];
            for (index, row) in rows.iter_mut().enumerate() {
                *row = self.row(at + index)?;
            }
            let item = decode_directory(rows)?;
            if item.kind != super::super::super::MarkdownComposedLinePartKind::TableRow
                || part.facts.kind != item.kind
            {
                // Structural line and original item kinds must agree before
                // their style pointer can be interpreted as column widths.
                return Err(Error::InconsistentCoverage);
            }
            let header = self.row(item.style_start)?;
            let columns = usize_at(&header, 0)?;
            if header[56] != control::TABLE_WIDTHS
                || columns == 0
                || columns > 256
                || columns > output.len()
                || item.style_rows != 1 + columns.div_ceil(7)
            {
                // Admit the entire nonempty finite width group, never a partial
                // caller buffer or a directory with inconsistent record extent.
                return Err(Error::InconsistentCoverage);
            }
            let mut sum = 0i64;
            for first in (0..columns).step_by(7) {
                let row = self.row(item.style_start + 1 + first / 7)?;
                if row[56] != control::TABLE_WIDTHS {
                    // A foreign record kind cannot provide trustworthy widths.
                    return Err(Error::InconsistentCoverage);
                }
                for (index, slot) in output[first..columns.min(first + 7)].iter_mut().enumerate() {
                    let width = i64_at(&row, index * 8);
                    if width <= 0 {
                        // Actual Grid widths are positive; malformed metadata
                        // must not collapse or reverse visible cell geometry.
                        return Err(Error::InconsistentCoverage);
                    }
                    sum = sum.checked_add(width).ok_or(Error::Overflow)?;
                    *slot = width;
                }
            }
            if sum != line.facts.x_advance_26_6 {
                // Column widths must reconstruct this same structural row's
                // sealed horizontal advance, including its original padding.
                return Err(Error::InconsistentCoverage);
            }
            // The associated original font registration must remain live through
            // the completed query, even though no new glyph shaping occurs here.
            self.payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .fonts
                .require_registration(&part.registration)?;
            Ok(columns)
        })();
        self.finish(result)
    }
}
