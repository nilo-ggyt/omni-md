//! 原控制请求的有限记录producer；没有假Job、伪字形、第二正文或按控制数量增长的owner表。
use super::super::controls::ValidatedControl;
use super::super::{
    MarkdownComposedLinePartKind, MarkdownLineInlineControlKind, MarkdownLineTabAlignment,
};
use super::*;
const TAB_STOP: u8 = 14;
const CONTROL_ROWS: usize = 68;

pub(super) struct ControlCapture {
    value: ValidatedControl,
    rows: Box<[[u8; 64]; CONTROL_ROWS]>,
    length: usize,
    written: usize,
    tree: Option<forest::Build>,
    key: ItemKey,
}
impl GlyphStore {
    pub(in crate::markdown_line_composer) fn begin_control(
        &mut self,
        value: ValidatedControl,
    ) -> Result<()> {
        let result = (|| {
            self.guard()?;
            if self.capture.is_some()
                || self.control_capture.is_some()
                || value.facts.source_utf16.start != self.source_end
                || !matches!(self.index, Some(Rows::Writing(_)))
            {
                return Err(Error::InvalidRequest);
            }
            self.grapheme_directory()?;
            value.require(&self.source, &self.authority, &self.fonts)?;
            for end in [value.facts.source_utf16.start, value.facts.source_utf16.end] {
                if !self.is_grapheme(end)? {
                    return Err(Error::InconsistentCoverage);
                }
            }
            let glyph_start = self.record_count()?;
            let cluster_count = usize::from(!value.facts.source_utf16.is_empty());
            let style_start = glyph_start
                .checked_add(cluster_count)
                .ok_or(Error::Overflow)?;
            let kind = if value.request.kind == MarkdownLineInlineControlKind::Tab {
                MarkdownComposedLinePartKind::Tab
            } else {
                MarkdownComposedLinePartKind::ParagraphBreak
            };
            let stops = if kind == MarkdownComposedLinePartKind::Tab {
                value.paragraph.tab_stops.len()
            } else {
                0
            };
            let record = style_start.checked_add(stops).ok_or(Error::Overflow)?;
            let origin = self.origins.checked_add(1).ok_or(Error::Overflow)?;
            let key = ItemKey { record, origin };
            let mut rows = Box::new([[0; 64]; CONTROL_ROWS]);
            let mut length = 0;
            if cluster_count != 0 {
                // 原真实控制范围可包含CRLF两unit，但不是可画的glyph；不制造.notdef或space。
                rows[length] = encode_cluster(&MarkdownGlyphCluster {
                    source_utf16: value.facts.source_utf16.clone(),
                    glyphs: 0..0,
                    missing_glyphs: 0,
                    flags: 0,
                    prefix_x_26_6: 0,
                    prefix_y_26_6: 0,
                    x_advance_26_6: 0,
                    y_advance_26_6: 0,
                })?;
                length += 1;
            }
            for stop in value.paragraph.tab_stops.iter().take(stops) {
                rows[length][..8].copy_from_slice(&stop.location_26_6.to_le_bytes());
                rows[length][8] = match stop.alignment {
                    MarkdownLineTabAlignment::Left => 0,
                    MarkdownLineTabAlignment::Right => 1,
                    MarkdownLineTabAlignment::Center => 2,
                    MarkdownLineTabAlignment::Decimal => 3,
                };
                rows[length][56] = TAB_STOP;
                length += 1;
            }
            let directory = Directory {
                facts: value.facts.clone(),
                font: value.registered.registration().key(),
                glyph_start,
                glyph_count: 0,
                cluster_start: glyph_start,
                cluster_count,
                previous: self.last_item,
                origin,
                style_start,
                style_rows: stops,
                features: stops,
                language_bytes: 0,
                x_advance: 0,
                y_advance: 0,
                kind,
                tab_interval: if kind == MarkdownComposedLinePartKind::Tab {
                    value.paragraph.default_tab_interval_26_6
                } else {
                    None
                },
                frontier_word_end: None, // 控制不是已认证词前缀，不能继承附近字形目录的证书。
            };
            for row in encode_directory(&directory)? {
                rows[length] = row;
                length += 1;
            }
            if length > CONTROL_ROWS {
                return Err(Error::Overflow);
            }
            self.control_capture = Some(ControlCapture {
                value,
                rows,
                length,
                written: 0,
                tree: None,
                key,
            });
            Ok(())
        })();
        self.finish(result)
    }
    pub(in crate::markdown_line_composer) fn step_control(
        &mut self,
        records: usize,
    ) -> Result<Option<StoredItemProof>> {
        let result = (|| {
            self.guard()?;
            if records == 0 || records > STEP {
                return Err(Error::InvalidRequest);
            }
            let capture = self.control_capture.as_mut().ok_or(Error::InvalidRequest)?;
            capture
                .value
                .require(&self.source, &self.authority, &self.fonts)?;
            let file = self.index.as_mut().ok_or(Error::Poisoned)?;
            let mut used = 0;
            while capture.written < capture.length && used < records {
                file.append(capture.rows[capture.written])?;
                capture.written += 1;
                used += 1;
            }
            if capture.written != capture.length {
                return Ok(None);
            }
            if !capture.value.facts.source_utf16.is_empty() {
                if capture.tree.is_none() {
                    capture.tree = Some(forest::Build::new(
                        capture.key,
                        capture.value.facts.source_utf16.clone(),
                    ));
                }
                if used == records
                    || !capture
                        .tree
                        .as_mut()
                        .ok_or(Error::InconsistentCoverage)?
                        .step(file, &mut self.forest, records - used)?
                {
                    return Ok(None);
                }
            } // 真空EOF行没有源字符，不插入会覆盖上一项的伪空树节点。
            capture
                .value
                .require(&self.source, &self.authority, &self.fonts)?;
            let key = capture.key;
            let end = capture.value.facts.source_utf16.end;
            self.last_item = Some(key.record);
            self.origins = key.origin;
            self.items = self.items.checked_add(1).ok_or(Error::Overflow)?;
            self.source_end = end;
            drop(self.control_capture.take());
            let proof = StoredItemProof {
                store: Arc::downgrade(&self.identity),
                key,
                #[cfg(test)]
                job: None,
            };
            self.directory(&proof)?; // 临时响应归还后，真实外部palette仍须持同Font注册，不能偷偷延寿。
            Ok(Some(proof))
        })();
        self.finish(result)
    }
    pub(super) fn tab_style(
        &mut self,
        item: &Directory,
    ) -> Result<super::super::MarkdownLineParagraphStyle> {
        if item.kind != MarkdownComposedLinePartKind::Tab
            || item.style_rows != item.features
            || item.language_bytes != 0
            || item.features > 64
        {
            return Err(Error::InconsistentCoverage);
        }
        let mut stops = Vec::with_capacity(item.features);
        for index in 0..item.features {
            self.guard()?;
            let row = self
                .index
                .as_mut()
                .ok_or(Error::Poisoned)?
                .read(item.style_start.checked_add(index).ok_or(Error::Overflow)?)?;
            if row[56] != TAB_STOP
                || row[9..56]
                    .iter()
                    .chain(row[57..].iter())
                    .any(|byte| *byte != 0)
            {
                return Err(Error::InconsistentCoverage);
            }
            let alignment = match row[8] {
                0 => MarkdownLineTabAlignment::Left,
                1 => MarkdownLineTabAlignment::Right,
                2 => MarkdownLineTabAlignment::Center,
                3 => MarkdownLineTabAlignment::Decimal,
                _ => return Err(Error::InconsistentCoverage),
            };
            stops.push(super::super::MarkdownLineTabStop {
                location_26_6: i64_at(&row, 0),
                alignment,
            });
        }
        let style = super::super::MarkdownLineParagraphStyle {
            tab_stops: stops,
            default_tab_interval_26_6: item.tab_interval,
        };
        style.require()?;
        Ok(style)
    }
}

impl GlyphStore {
    /// EOF空段只消费原空控制producer的私有Proof；不能用任意0长Range制造空行。
    pub(in crate::markdown_line_composer) fn append_empty_line(
        &mut self,
        proof: &StoredItemProof,
    ) -> Result<MarkdownComposedLineFacts> {
        let result = (|| {
            self.guard()?;
            let item = self.directory(proof)?;
            if self.capture.is_some()
                || self.control_capture.is_some()
                || item.kind != MarkdownComposedLinePartKind::ParagraphBreak
                || !item.facts.source_utf16.is_empty()
                || item.facts.source_utf16.start != self.source.utf16_len()
                || self.line_source_end != self.source.utf16_len()
                || item.facts.paragraph_utf16.end != item.facts.source_utf16.end
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
            rows[0][57] = 1;
            // 只有ValidatedControl::empty可生成本Proof；其level已由原段策略/LS的
            // 实际UBA base认证，不从非空文字part推断空行方向。
            rows[0][58] = item.facts.level;
            rows[1][..8].copy_from_slice(&number(item.facts.source_utf16.start)?.to_le_bytes());
            rows[1][8..16].copy_from_slice(&number(item.facts.source_utf16.end)?.to_le_bytes());
            rows[1][16..24].copy_from_slice(&number(item.glyph_start)?.to_le_bytes());
            rows[1][48..56].copy_from_slice(&item.origin.to_le_bytes());
            rows[1][56] = output::PART;
            rows[2][..8].copy_from_slice(&item.font.generation.to_le_bytes());
            rows[2][8..10].copy_from_slice(&item.font.slot.to_le_bytes());
            rows[2][12..16].copy_from_slice(&item.facts.points.to_le_bytes());
            rows[2][32..40].copy_from_slice(&number(self.lines)?.to_le_bytes());
            rows[2][40..48].copy_from_slice(&number(proof.key.record)?.to_le_bytes());
            rows[2][56] = output::PART_META;
            rows[2][57] = item.facts.level;
            rows[2][58] = u8::from(item.facts.direction == MarkdownShapingDirection::RightToLeft);
            rows[2][59] = MarkdownComposedLinePartKind::ParagraphBreak.code();
            rows[2][60..64].copy_from_slice(&item.facts.script);
            rows[3][..8].copy_from_slice(&number(header)?.to_le_bytes());
            rows[3][8..16].copy_from_slice(&1u64.to_le_bytes());
            rows[3][48..56].copy_from_slice(
                &self
                    .line_end
                    .map(number)
                    .transpose()?
                    .unwrap_or(NONE)
                    .to_le_bytes(),
            );
            rows[3][56] = output::LINE_END;
            for row in rows {
                self.index.as_mut().ok_or(Error::Poisoned)?.append(row)?;
            }
            self.directory(proof)?;
            self.grapheme_directory()?;
            let facts = MarkdownComposedLineFacts {
                source_utf16: item.facts.source_utf16,
                paragraph_utf16: item.facts.paragraph_utf16,
                parts: 1,
                glyphs: 0,
                ordinal: self.lines,
                x_advance_26_6: 0,
                y_advance_26_6: 0,
                kind: MarkdownComposedLineKind::Mandatory,
            };
            self.publish_line(end, facts.source_utf16.end)?;
            Ok(facts)
        })();
        self.finish(result)
    }
}
