//! 已完成 Grid 的独立块记录仍写入原 GlyphStore；固定四记录逐预算提交，不进入文字 Measure。
use super::super::MarkdownComposedLinePartKind;
use super::*;

pub(super) struct TableLineCapture {
    rows: [[u8; 64]; 4],
    written: usize,
    end: usize,
    key: ItemKey,
    facts: MarkdownComposedLineFacts,
}
impl GlyphStore {
    /// 只接原 item producer 的真实 TableRow proof；源覆盖、字体和两个硬端点已经封闭。
    pub(in crate::markdown_line_composer) fn begin_table_line(
        &mut self,
        proof: &StoredItemProof,
    ) -> Result<()> {
        self.guard()?;
        let item = self.directory(proof)?;
        if self.table_line.is_some()
            || item.kind != MarkdownComposedLinePartKind::TableRow
            || item.facts.source_utf16.start != self.line_source_end
            || item.facts.source_utf16.end != self.source_end
            || item.glyph_count != 0
            || item.cluster_count != 0
            || item.x_advance <= 0
            || item.y_advance <= 0
        {
            return Err(Error::InconsistentCoverage);
        }
        let header = self.record_count()?;
        let end = header.checked_add(3).ok_or(Error::Overflow)?;
        let mut rows = [[0; 64]; 4];
        for (at, value) in [
            (0, item.facts.source_utf16.start),
            (8, item.facts.source_utf16.end),
            (16, item.facts.paragraph_utf16.start),
            (24, item.facts.paragraph_utf16.end),
            (32, self.lines),
        ] {
            rows[0][at..at + 8].copy_from_slice(&number(value)?.to_le_bytes());
        }
        self.write_line_skip(&mut rows[0])?;
        rows[0][56] = output::LINE;
        rows[0][57] = 4; // 真实 TableRow 类型，不借 Mandatory/空文字行冒充块。
        rows[1][..8].copy_from_slice(&number(item.facts.source_utf16.start)?.to_le_bytes());
        rows[1][8..16].copy_from_slice(&number(item.facts.source_utf16.end)?.to_le_bytes());
        rows[1][16..24].copy_from_slice(&number(item.glyph_start)?.to_le_bytes());
        rows[1][48..56].copy_from_slice(&item.origin.to_le_bytes());
        rows[1][56] = output::PART;
        rows[2][..8].copy_from_slice(&item.font.generation.to_le_bytes());
        rows[2][8..10].copy_from_slice(&item.font.slot.to_le_bytes());
        rows[2][16..24].copy_from_slice(&item.x_advance.to_le_bytes());
        rows[2][24..32].copy_from_slice(&item.y_advance.to_le_bytes());
        rows[2][32..40].copy_from_slice(&number(self.lines)?.to_le_bytes());
        rows[2][40..48].copy_from_slice(&number(proof.key.record)?.to_le_bytes());
        rows[2][56] = output::PART_META;
        rows[2][59] = MarkdownComposedLinePartKind::TableRow.code();
        rows[2][60..64].copy_from_slice(b"Zyyy");
        rows[3][..8].copy_from_slice(&number(header)?.to_le_bytes());
        rows[3][8..16].copy_from_slice(&1u64.to_le_bytes());
        rows[3][16..24].copy_from_slice(&item.x_advance.to_le_bytes());
        rows[3][24..32].copy_from_slice(&item.y_advance.to_le_bytes());
        rows[3][40..48].copy_from_slice(&number(item.facts.source_utf16.len())?.to_le_bytes());
        rows[3][48..56].copy_from_slice(
            &self
                .line_end
                .map(number)
                .transpose()?
                .unwrap_or(NONE)
                .to_le_bytes(),
        );
        rows[3][56] = output::LINE_END;
        self.table_line = Some(TableLineCapture {
            rows,
            written: 0,
            end,
            key: proof.key,
            facts: MarkdownComposedLineFacts {
                source_utf16: item.facts.source_utf16,
                paragraph_utf16: item.facts.paragraph_utf16,
                parts: 1,
                glyphs: 0,
                ordinal: self.lines,
                x_advance_26_6: item.x_advance,
                y_advance_26_6: item.y_advance,
                kind: MarkdownComposedLineKind::TableRow,
            },
        });
        Ok(())
    }

    /// 部分四记录仍属于当前 writer，只有真实末记录和原 Font 活性均完成才对外发 LineReady。
    pub(in crate::markdown_line_composer) fn step_table_line(
        &mut self,
        records: usize,
    ) -> Result<Option<MarkdownComposedLineFacts>> {
        let result = (|| {
            self.guard()?;
            if records == 0 || records > STEP {
                return Err(Error::InvalidRequest);
            }
            let capture = self.table_line.as_mut().ok_or(Error::InvalidRequest)?;
            let count = records.min(4 - capture.written);
            for _ in 0..count {
                self.index
                    .as_mut()
                    .ok_or(Error::Poisoned)?
                    .append(capture.rows[capture.written])?;
                capture.written += 1;
            }
            if capture.written != 4 {
                return Ok(None);
            }
            let capture = self.table_line.take().ok_or(Error::InconsistentCoverage)?;
            let proof = StoredItemProof {
                store: Arc::downgrade(&self.identity),
                key: capture.key,
                #[cfg(test)]
                job: None,
            };
            self.directory(&proof)?;
            self.publish_line(capture.end, capture.facts.source_utf16.end)?;
            Ok(Some(capture.facts))
        })();
        self.finish(result)
    }
}
