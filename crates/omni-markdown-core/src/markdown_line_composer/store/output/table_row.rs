//! 恢复旧视口只读原 TableRow 已封存列宽，不重测表头、不保整篇 Native 列宽缓存。
use super::*;
impl MarkdownComposedLines {
    /// handle 必须属于同一只读输出；固定调用方工作区接最多原 256 列，实际格宽含左右内边距。
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
                return Err(Error::InvalidRequest);
            }
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
                return Err(Error::InconsistentCoverage);
            }
            let mut sum = 0i64;
            for first in (0..columns).step_by(7) {
                let row = self.row(item.style_start + 1 + first / 7)?;
                if row[56] != control::TABLE_WIDTHS {
                    return Err(Error::InconsistentCoverage);
                }
                for (index, slot) in output[first..columns.min(first + 7)].iter_mut().enumerate() {
                    let width = i64_at(&row, index * 8);
                    if width <= 0 {
                        return Err(Error::InconsistentCoverage);
                    }
                    sum = sum.checked_add(width).ok_or(Error::Overflow)?;
                    *slot = width;
                }
            }
            if sum != line.facts.x_advance_26_6 {
                return Err(Error::InconsistentCoverage);
            }
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
