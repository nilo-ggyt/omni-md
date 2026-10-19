//! 原LineEnd的固定祖先槽；只写同一文件的两字段，不建立逐行内存索引。

use super::*;

#[derive(Clone, Copy)]
pub(super) struct LineAncestor {
    ordinal: usize,
    end: usize,
}

impl GlyphStore {
    /// 清除ordinal最低有效位就是本行的skip目标。目标与本行之间的ordinal有更多1位，
    /// 因此按popcount分槽时不会覆盖这个目标；发布计数先checked，最多使用usize::BITS槽。
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

    /// 只有真实末记录和原许可全部完成后调用。槽更新与原LineEnd发布同一步，不从
    /// 半写入行或失败重试中保留跳链；旧只读前缀仍只见自身已封口的记录。
    pub(super) fn publish_line(&mut self, end: usize, source_end: usize) -> Result<()> {
        // 目录必须已经落在本次真实LineEnd之前，且覆盖本行端点。这里只复制固定
        // 事实，不把后继writer当前目录借给旧reader，也不以行前缀末尾声称源EOF。
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
