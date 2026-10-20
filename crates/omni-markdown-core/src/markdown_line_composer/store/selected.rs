//! 原Store测量选中的整行，按同Source真实L1/L2次序组合，不把每个item结尾当换行。
use super::output;
use super::*;
use crate::markdown_soft_line_measure::provider::{
    GlyphProviderOrigin, SelectedMeasureKind, SelectedMeasurePermit,
};
use crate::{MarkdownBidiParagraph, MarkdownGlyphCluster, MarkdownLineBreakKind};

struct PartPlacement {
    source: Range<usize>,
    level: u8,
    glyphs: Range<usize>,
    x: i64,
    y: i64,
}
struct Totals {
    parts: usize,
    glyphs: usize,
    covered: usize,
    x: i64,
    y: i64,
}
impl GlyphStore {
    /// 为原Line头取得真实base；完整UniformZero已证明每段为0，不能为此回扫段落端点。
    /// General只查原已完成UBA段表；调用方给的是同一真实Line的GCB起点。
    pub(super) fn paragraph_base_level(
        &self,
        bidi: &mut MarkdownBidiParagraph,
        utf16: usize,
    ) -> Result<u8> {
        #[cfg(feature = "paged-shaping-frontier")]
        if bidi.whole_source_uniform_zero(&self.authority)? {
            return Ok(0);
        }
        bidi.paragraph_base_level_at_utf16(utf16)?
            .ok_or(Error::InconsistentCoverage)
    }

    pub(in crate::markdown_line_composer) fn append_selected_line(
        &mut self,
        current: &StoredItemProof,
        permit: &SelectedMeasurePermit<'_>,
        bidi: &mut MarkdownBidiParagraph,
        pen: i64,
    ) -> Result<MarkdownComposedLineFacts> {
        let result = (|| {
            self.guard()?;
            if self.capture.is_some()
                || self.control_capture.is_some()
                || !matches!(self.index, Some(Rows::Writing(_)))
            {
                return Err(Error::InvalidRequest);
            }
            let original = {
                let mut provider = self.borrow_at(current, pen)?;
                permit.require(&mut provider)?
            };
            let GlyphProviderOrigin::Stored { store, origin } = &original.current.origin else {
                return Err(Error::SourceMismatch);
            };
            if !Weak::ptr_eq(store, &Arc::downgrade(&self.identity))
                || *origin != current.key.origin
                || original.source != self.authority
                || original.current.generation != self.source.generation()
                || !original
                    .line
                    .cancellation
                    .same_request(&self.cancellation.measure())
                || original.line.owner.upgrade().is_none()
                || !original
                    .line
                    .alive
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(Error::SourceMismatch);
            }
            let anchor = self.directory(current)?;
            if !self
                .fonts
                .resolve(anchor.font)?
                .font()
                .permits(&original.font)
            {
                return Err(Error::SourceMismatch);
            }
            let range = original.selected.source_utf16.clone();
            if range.start != self.line_source_end
                || range.is_empty()
                || range.end > self.source_end
                || original.selected.boundary_flags
                    & harfrust::GlyphFlags::UNSAFE_TO_BREAK.to_bits()
                    != 0
            {
                return Err(Error::InconsistentCoverage);
            }
            if !self.is_grapheme_range(&range)? {
                return Err(Error::InconsistentCoverage);
            }
            #[cfg(feature = "paged-shaping-frontier")]
            if matches!(
                original.selected.kind,
                SelectedMeasureKind::Emergency { .. }
            ) && range.end < self.source.utf16_len()
            {
                // 紧急行尾可能恰为旧目录末端；必须读取原后继目录的真实cluster，而不是让
                // 单part内的“小于item末端”门漏过跨prefix连接，也不能只信复制出的flags字段。
                let after = self
                    .item_at(range.end)?
                    .ok_or(Error::InconsistentCoverage)?;
                let cluster = self
                    .cluster_in_directory(&after.directory, range.end)?
                    .map(|(_, cluster)| cluster)
                    .ok_or(Error::InconsistentCoverage)?;
                if cluster.source_utf16.start != range.end
                    || cluster.flags & harfrust::GlyphFlags::UNSAFE_TO_BREAK.to_bits() != 0
                {
                    return Err(Error::NeedsBidiAssembly);
                }
            }
            bidi.require_layout_source(&self.authority)?;
            if bidi.source_range() != (0..self.source.utf16_len()) {
                return Err(Error::SourceMismatch);
            }
            let LocatedItem {
                proof: last_proof,
                directory: last_item,
            } = self
                .item_at(range.end.checked_sub(1).ok_or(Error::Overflow)?)?
                .ok_or(Error::InconsistentCoverage)?;
            let hard_break = matches!(
                last_item.kind,
                super::super::MarkdownComposedLinePartKind::ParagraphBreak
                    | super::super::MarkdownComposedLinePartKind::FoldedSection
            )
            .then_some((last_proof, last_item));
            let paragraph = if hard_break.is_some() {
                self.source
                    .snapshot_for_bidi()
                    .text_checking_paragraph_range(range.clone())?
            } else {
                bidi.paragraph_range_at_utf16(range.start)?
                    .ok_or(Error::InconsistentCoverage)?
            };
            if range.end > paragraph.end {
                return Err(Error::InconsistentCoverage);
            }
            let visual_range = range.start
                ..hard_break
                    .as_ref()
                    .map_or(range.end, |(_, item)| item.facts.source_utf16.start);
            let kind = match &original.selected.kind {
                SelectedMeasureKind::Natural(boundary) => match boundary.kind {
                    MarkdownLineBreakKind::Allowed => MarkdownComposedLineKind::Allowed,
                    MarkdownLineBreakKind::Mandatory => MarkdownComposedLineKind::Mandatory,
                },
                #[cfg(feature = "paged-shaping-frontier")]
                SelectedMeasureKind::Emergency {
                    overflowing_grapheme,
                } => {
                    if *overflowing_grapheme {
                        MarkdownComposedLineKind::Overflow
                    } else {
                        MarkdownComposedLineKind::Emergency
                    }
                } // 原实际GCB超宽保持Overflow；紧急选择从不伪装成UAX Allowed/Mandatory。
            };
            let header = self.index.as_ref().ok_or(Error::Poisoned)?.len();
            let mut head = [0; 64];
            // 原同源UBA段给出实际base；不能用L1/L2首part的resolved level代替。
            let base_level = self.paragraph_base_level(bidi, range.start)?;
            for (at, value) in [
                (0, range.start),
                (8, range.end),
                (16, paragraph.start),
                (24, paragraph.end),
                (32, self.lines),
            ] {
                head[at..at + 8].copy_from_slice(&number(value)?.to_le_bytes());
            }
            self.write_line_skip(&mut head)?;
            head[56] = output::LINE;
            head[57] = match kind {
                MarkdownComposedLineKind::Allowed => 0,
                MarkdownComposedLineKind::Mandatory => 1,
                MarkdownComposedLineKind::Emergency => 2,
                MarkdownComposedLineKind::Overflow => 3,
                MarkdownComposedLineKind::TableRow => return Err(Error::InvalidRequest),
            };
            head[58] = base_level; // 原64B头的保留位；不另建段落索引或扩大记录。
            self.index.as_mut().ok_or(Error::Poisoned)?.append(head)?;
            let mut totals = Totals {
                parts: 0,
                glyphs: 0,
                covered: 0,
                x: 0,
                y: 0,
            };
            let mut failure = None;
            // UBA回调只得到候选，不发布Native状态。任何后续错误都撤整个尚未签出的File。
            let visual = bidi.visit_visual_runs(visual_range.clone(), |run| {
                if failure.is_some() {
                    return;
                }
                let result = (|| {
                    let visual = run.source_utf16_start..run.source_utf16_end;
                    if visual.is_empty() || visual.start < range.start || visual.end > range.end {
                        return Err(Error::InconsistentCoverage);
                    }
                    let backwards = run.level & 1 != 0;
                    let mut at = if backwards { visual.end } else { visual.start };
                    while if backwards {
                        at > visual.start
                    } else {
                        at < visual.end
                    } {
                        self.guard()?;
                        let point = if backwards {
                            at.checked_sub(1).ok_or(Error::Overflow)?
                        } else {
                            at
                        };
                        // 同一 forest 定位已完整核过这份目录；本回调不再读取相同三条记录。
                        let LocatedItem {
                            proof,
                            directory: item,
                        } = self.item_at(point)?.ok_or(Error::InconsistentCoverage)?;
                        let segment = visual.start.max(item.facts.source_utf16.start)
                            ..visual.end.min(item.facts.source_utf16.end);
                        if (backwards && segment.end != at) || (!backwards && segment.start != at) {
                            return Err(Error::InconsistentCoverage);
                        }
                        if run.level & 1 != item.facts.level & 1 {
                            return Err(Error::NeedsBidiAssembly);
                        } // L1改变整形方向时须真实重形，不能反转glyph或强制方向。
                        self.append_selected_part(
                            &proof,
                            &item,
                            segment.clone(),
                            run.level,
                            &range,
                            &mut totals,
                        )?;
                        at = if backwards {
                            segment.start
                        } else {
                            segment.end
                        };
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    failure = Some(error);
                }
            });
            self.source.health()?;
            visual?;
            if let Some(error) = failure {
                return Err(error);
            }
            if let Some((proof, item)) = hard_break {
                self.append_selected_part(
                    &proof,
                    &item,
                    item.facts.source_utf16.clone(),
                    item.facts.level,
                    &range,
                    &mut totals,
                )?;
            }
            if totals.covered != range.len()
                || totals.parts == 0
                || totals.x != original.selected.x_advance_26_6
            {
                return Err(Error::InconsistentCoverage);
            }
            // 同一次内部选择仍有效才签LineEnd；公开Choice或相同范围的新Measure不能替代它。
            {
                let mut provider = self.borrow_at(current, pen)?;
                let final_facts = permit.require(&mut provider)?;
                if final_facts.selected != original.selected {
                    return Err(Error::InconsistentCoverage);
                }
            }
            bidi.require_layout_source(&self.authority)?;
            let end = self.index.as_ref().ok_or(Error::Poisoned)?.len();
            if header.checked_add(1).and_then(|value| {
                totals
                    .parts
                    .checked_mul(2)
                    .and_then(|parts| value.checked_add(parts))
            }) != Some(end)
            {
                return Err(Error::InconsistentCoverage);
            }
            let mut tail = [0; 64];
            for (at, value) in [
                (0, header),
                (8, totals.parts),
                (32, totals.glyphs),
                (40, totals.covered),
            ] {
                tail[at..at + 8].copy_from_slice(&number(value)?.to_le_bytes());
            }
            tail[16..24].copy_from_slice(&totals.x.to_le_bytes());
            tail[24..32].copy_from_slice(&totals.y.to_le_bytes());
            tail[48..56].copy_from_slice(
                &self
                    .line_end
                    .map(number)
                    .transpose()?
                    .unwrap_or(NONE)
                    .to_le_bytes(),
            );
            tail[56] = output::LINE_END;
            self.index.as_mut().ok_or(Error::Poisoned)?.append(tail)?;
            {
                let mut provider = self.borrow_at(current, pen)?;
                permit.require(&mut provider)?;
            }
            let facts = MarkdownComposedLineFacts {
                source_utf16: range.clone(),
                paragraph_utf16: paragraph,
                parts: totals.parts,
                glyphs: totals.glyphs,
                ordinal: self.lines,
                x_advance_26_6: totals.x,
                y_advance_26_6: totals.y,
                kind,
            };
            self.publish_line(end, range.end)?;
            Ok(facts)
        })();
        self.finish(result)
    }

    /// 整行两端仍是 GCB；内部视觉分段还可消费原 item 同来源的 UBA 标量 seam。
    fn append_selected_part(
        &mut self,
        proof: &StoredItemProof,
        item: &Directory,
        source: Range<usize>,
        level: u8,
        line: &Range<usize>,
        totals: &mut Totals,
    ) -> Result<()> {
        self.guard()?;
        // 非 GCB 只允许原 Store 签入的 item 本端，不能让任意视觉范围绕过字素校验。
        // 原相邻 resolved levels 已在捕获与目录解码复核；这里没有重写 level 或拆分字形。
        let check_start = source.start != line.start
            && (source.start != item.facts.source_utf16.start || item.shaping_seams[0].is_none());
        let check_end = source.end != line.end
            && (source.end != item.facts.source_utf16.end || item.shaping_seams[1].is_none());
        let boundaries = match (check_start, check_end) {
            (true, true) => self.is_grapheme_range(&source)?,
            (true, false) => self.is_grapheme(source.start)?,
            (false, true) => self.is_grapheme(source.end)?,
            (false, false) => true,
        };
        if !boundaries {
            return Err(Error::InconsistentCoverage);
        }
        if item.kind != super::super::MarkdownComposedLinePartKind::Glyphs {
            if source != item.facts.source_utf16 {
                return Err(Error::InconsistentCoverage);
            }
            let x = if item.kind == super::super::MarkdownComposedLinePartKind::Tab {
                let super::super::MarkdownLineTabPlacement::Left { advance_26_6 } =
                    self.tab_style(item)?.tab(totals.x)?
                else {
                    return Err(Error::InvalidRequest);
                };
                advance_26_6
            } else {
                0
            };
            return self.write_selected_part(
                proof,
                item,
                PartPlacement {
                    source,
                    level,
                    glyphs: 0..0,
                    x,
                    y: 0,
                },
                totals,
            );
        }
        // item已在当前视觉段中由原opaque proof真实读取并验证；三个端点共用这份目录，
        // 每次cluster仍读取自己的真实64B记录并核活Source/字体，不能据目录猜字形或安全flags。
        let (first_ordinal, first) = self
            .cluster_in_directory(item, source.start)?
            .ok_or(Error::InconsistentCoverage)?;
        let last_offset = source.end.checked_sub(1).ok_or(Error::Overflow)?;
        let (last_ordinal, last) = if first.source_utf16.contains(&last_offset) {
            // 同一实际 cluster 已证明覆盖两端；复用本次读取的 flags/glyphs/advance，
            // 不按 UTF16 长度猜一个 cluster，也不跨 Source 或后继调用保留这份值。
            (first_ordinal, first.clone())
        } else {
            self.cluster_in_directory(item, last_offset)?
                .ok_or(Error::InconsistentCoverage)?
        };
        if first.source_utf16.start != source.start || last.source_utf16.end != source.end {
            return Err(Error::NeedsBidiAssembly);
        }
        let unsafe_flag = harfrust::GlyphFlags::UNSAFE_TO_BREAK.to_bits();
        if source.start == line.start
            && source.start > item.facts.source_utf16.start
            && first.flags & unsafe_flag != 0
        {
            return Err(Error::NeedsBidiAssembly);
        }
        if source.end == line.end && source.end < item.facts.source_utf16.end {
            // 上一查询已经定位真实逻辑序号；行尾安全性只需读取紧邻的一个原记录，
            // 不再次二分同一目录，也不按UTF16差值猜glyph或将RTL视觉序当源顺序。
            self.guard()?;
            self.fonts.resolve(item.font)?;
            let next = self.cluster_record_in_directory(
                item,
                last_ordinal.checked_add(1).ok_or(Error::Overflow)?,
            )?;
            self.guard()?;
            if next.source_utf16.start != source.end {
                return Err(Error::InconsistentCoverage); // 逻辑相邻记录出现覆盖缺口属于索引损坏。
            }
            if next.flags & unsafe_flag != 0 {
                return Err(Error::NeedsBidiAssembly);
            }
        }
        let glyphs =
            first.glyphs.start.min(last.glyphs.start)..first.glyphs.end.max(last.glyphs.end);
        let advance =
            |first: &MarkdownGlyphCluster, last: &MarkdownGlyphCluster| -> Result<(i64, i64)> {
                Ok((
                    last.prefix_x_26_6
                        .checked_add(last.x_advance_26_6)
                        .and_then(|end| end.checked_sub(first.prefix_x_26_6))
                        .ok_or(Error::Overflow)?,
                    last.prefix_y_26_6
                        .checked_add(last.y_advance_26_6)
                        .and_then(|end| end.checked_sub(first.prefix_y_26_6))
                        .ok_or(Error::Overflow)?,
                ))
            };
        let (x, y) = advance(&first, &last)?;
        self.write_selected_part(
            proof,
            item,
            PartPlacement {
                source,
                level,
                glyphs,
                x,
                y,
            },
            totals,
        )
    }
    fn write_selected_part(
        &mut self,
        proof: &StoredItemProof,
        item: &Directory,
        placement: PartPlacement,
        totals: &mut Totals,
    ) -> Result<()> {
        let PartPlacement {
            source,
            level,
            glyphs,
            x,
            y,
        } = placement;
        let glyph_start = item
            .glyph_start
            .checked_add(glyphs.start)
            .ok_or(Error::Overflow)?;
        let mut rows = [[0; 64]; 2];
        for (at, value) in [
            (0, source.start),
            (8, source.end),
            (16, glyph_start),
            (24, glyphs.len()),
        ] {
            rows[0][at..at + 8].copy_from_slice(&number(value)?.to_le_bytes());
        }
        rows[0][32..40].copy_from_slice(&totals.x.to_le_bytes());
        rows[0][40..48].copy_from_slice(&totals.y.to_le_bytes());
        rows[0][48..56].copy_from_slice(&item.origin.to_le_bytes());
        rows[0][56] = output::PART;
        rows[1][..8].copy_from_slice(&item.font.generation.to_le_bytes());
        rows[1][8..10].copy_from_slice(&item.font.slot.to_le_bytes());
        rows[1][12..16].copy_from_slice(&item.facts.points.to_le_bytes());
        rows[1][16..24].copy_from_slice(&x.to_le_bytes());
        rows[1][24..32].copy_from_slice(&y.to_le_bytes());
        rows[1][32..40].copy_from_slice(&number(self.lines)?.to_le_bytes());
        rows[1][40..48].copy_from_slice(&number(proof.key.record)?.to_le_bytes());
        rows[1][48..56].copy_from_slice(&number(glyphs.start)?.to_le_bytes());
        rows[1][56] = output::PART_META;
        rows[1][57] = level;
        rows[1][58] = u8::from(level & 1 != 0);
        rows[1][59] = item.kind.code();
        rows[1][60..64].copy_from_slice(&item.facts.script);
        for row in rows {
            self.index.as_mut().ok_or(Error::Poisoned)?.append(row)?;
        }
        totals.parts = totals.parts.checked_add(1).ok_or(Error::Overflow)?;
        totals.glyphs = totals
            .glyphs
            .checked_add(glyphs.len())
            .ok_or(Error::Overflow)?;
        totals.covered = totals
            .covered
            .checked_add(source.len())
            .ok_or(Error::Overflow)?;
        totals.x = totals.x.checked_add(x).ok_or(Error::Overflow)?;
        totals.y = totals.y.checked_add(y).ok_or(Error::Overflow)?;
        Ok(())
    }
}
