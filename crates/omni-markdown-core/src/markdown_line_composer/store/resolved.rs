//! 原selector签包的唯一Store转换；临时child真正归还后才发布同一File中的LineEnd。
use super::*;
use crate::markdown_soft_line_measure::provider::{
    GlyphProviderOrigin, MeasuredLineScope, ResolutionMeasurePermit,
};
use crate::markdown_soft_line_resolve::stored::MarkdownStoredResolvedPrefix;
use crate::{
    MarkdownBidiParagraph, MarkdownEmergencyShapedPrefixBoundary, MarkdownSoftLineResolvedKind,
};

enum Stage {
    Index,
    Glyphs,
    Clusters,
    Directory,
    Release,
    Line,
}

/// 与Store分开持有临时child，避免在原provider独占借用期间再次可变借用同一File。
/// 每批只取原签包的Copy记录，再归还provider借用，最后写入唯一输出页。
pub(in crate::markdown_line_composer) struct ResolvedCapture {
    prefix: Option<MarkdownStoredResolvedPrefix>,
    clusters: Option<super::super::worker_resources::ClusterResourceOwner>,
    boundary: MarkdownEmergencyShapedPrefixBoundary,
    line: MeasuredLineScope,
    original: ItemKey,
    directory: Directory,
    stage: Stage,
    consumed: usize,
    cursor: usize,
    record: usize,
    header: usize,
    y: i64,
    base_level: u8,
}
impl ResolvedCapture {
    pub(in crate::markdown_line_composer) fn new(
        store: &mut GlyphStore,
        original: &StoredItemProof,
        permit: &ResolutionMeasurePermit<'_>,
        mut prefix: MarkdownStoredResolvedPrefix,
        bidi: &mut MarkdownBidiParagraph,
        pen: i64,
    ) -> Result<Self> {
        store.guard()?;
        if store.capture.is_some() || store.control_capture.is_some() {
            return Err(Error::InvalidRequest);
        }
        let item = store.directory(original)?;
        let resources = store.resources.clone();
        let (facts, clusters) = {
            let mut provider = store.borrow_at(original, pen)?;
            let facts = prefix.facts(permit, &mut provider)?;
            let created = prefix.cluster_index(permit, &mut provider);
            if created.is_err()
                && let Some(resources) = &resources
            {
                resources.incomplete();
            }
            let clusters = super::super::worker_resources::ClusterResourceOwner::new(
                created?,
                resources.as_ref(),
            );
            (facts, clusters)
        };
        let GlyphProviderOrigin::Stored {
            store: owner,
            origin,
        } = facts.origin
        else {
            return Err(Error::SourceMismatch);
        };
        let range = facts.boundary.source_utf16.clone();
        if !Weak::ptr_eq(&owner, &Arc::downgrade(&store.identity))
            || origin != original.key.origin
            || facts.source != store.authority
            || !store.fonts.resolve(item.font)?.font().permits(&facts.font)
            || !facts
                .line
                .cancellation
                .same_request(&store.cancellation.measure())
            || range.start != store.line_source_end
            || range.is_empty()
            || range.start < item.facts.source_utf16.start
            || range.end > item.facts.source_utf16.end
            || item.kind != super::super::MarkdownComposedLinePartKind::Glyphs
        {
            return Err(Error::SourceMismatch);
        }
        // 本入口只转换当前真实单item前缀。若L1/L2形成多个run，须继续真实跨run组合，
        // 不能把child方向或glyph数组反转成一条假视觉行。
        bidi.require_layout_source(&store.authority)?;
        if bidi.source_range() != (0..store.source.utf16_len()) {
            return Err(Error::SourceMismatch);
        }
        let base_level = store.paragraph_base_level(bidi, range.start)?;
        let mut runs = 0usize;
        let mut same = true;
        bidi.visit_visual_runs(range.clone(), |run| {
            runs += 1;
            same &= run.source_utf16_start == range.start
                && run.source_utf16_end == range.end
                && run.level == item.facts.level;
        })?;
        if runs != 1 || !same {
            return Err(Error::NeedsBidiAssembly);
        }
        if !store.is_grapheme(range.start)? || !store.is_grapheme(range.end)? {
            return Err(Error::InconsistentCoverage);
        }
        // 输出另有真实child origin，但不插入原item检索树，不覆盖原item来源或余文。
        let start = store.index.as_ref().ok_or(Error::Poisoned)?.len();
        let mut item_facts = item.facts;
        item_facts.source_utf16 = range.clone();
        item_facts.beginning_of_text = range.start == item_facts.paragraph_utf16.start;
        item_facts.end_of_text = range.end == item_facts.paragraph_utf16.end;
        let directory = Directory {
            shaping_seams: [None; 2], // 重排范围两端来自原行选择的 GCB，不能沿普通 item 扩大分段权限。
            paragraph_inset_26_6: item.paragraph_inset_26_6,
            facts: item_facts,
            presentation: item.presentation, // child沿原样式整形，折行不能丢失隐藏或装饰身份。
            font: item.font,
            glyph_start: start,
            glyph_count: facts.boundary.glyphs,
            cluster_start: 0,
            cluster_count: 0,
            previous: None,
            origin: store.origins.checked_add(1).ok_or(Error::Overflow)?,
            style_start: 0,
            style_rows: 0,
            features: 0,
            language_bytes: 0,
            x_advance: 0,
            y_advance: 0,
            kind: super::super::MarkdownComposedLinePartKind::Glyphs,
            tab_interval: None,
            frontier_word_end: None,
        };
        store.guard()?;
        Ok(Self {
            prefix: Some(prefix),
            clusters: Some(clusters),
            boundary: facts.boundary,
            line: facts.line,
            original: original.key,
            directory,
            stage: Stage::Index,
            consumed: 0,
            cursor: range.start,
            record: 0,
            header: 0,
            y: 0,
            base_level,
        })
    }

    fn require(
        &mut self,
        store: &mut GlyphStore,
        original: &StoredItemProof,
        permit: &ResolutionMeasurePermit<'_>,
        pen: i64,
    ) -> Result<()> {
        store.guard()?;
        if original.key != self.original
            || Some(self.directory.origin) != store.origins.checked_add(1)
        {
            return Err(Error::SourceMismatch);
        }
        let mut provider = store.borrow_at(original, pen)?;
        let facts = permit.facts(&mut provider)?;
        if !Weak::ptr_eq(&self.line.owner, &facts.line.owner)
            || !Arc::ptr_eq(&self.line.alive, &facts.line.alive)
            || !self
                .line
                .cancellation
                .same_request(&facts.line.cancellation)
            || !self.line.alive.load(std::sync::atomic::Ordering::Acquire)
            || self.line.owner.strong_count() == 0
        {
            return Err(Error::SourceMismatch);
        }
        if let Some(prefix) = self.prefix.as_mut() {
            prefix.require(permit, &mut provider)?;
        } // child归还后的提交阶段只用本私有转换保留的同scope，不重建已释放child身份。
        Ok(())
    }

    /// 每回合最多消费256条索引/字形/目录记录，Ready只在实际LineEnd最终复核后返回。
    pub(in crate::markdown_line_composer) fn step(
        &mut self,
        store: &mut GlyphStore,
        original: &StoredItemProof,
        permit: &ResolutionMeasurePermit<'_>,
        pen: i64,
        records: usize,
    ) -> Result<Option<MarkdownComposedLineFacts>> {
        let result = self.advance(store, original, permit, pen, records);
        store.finish(result)
    }
    fn advance(
        &mut self,
        store: &mut GlyphStore,
        original: &StoredItemProof,
        permit: &ResolutionMeasurePermit<'_>,
        pen: i64,
        records: usize,
    ) -> Result<Option<MarkdownComposedLineFacts>> {
        self.require(store, original, permit, pen)?;
        if records == 0 {
            return Err(Error::InvalidRequest);
        }
        let limit = records.min(STEP);
        match self.stage {
            Stage::Index => {
                // 先让实际child完成有界cluster索引；Building不是可消费的行范围。
                let mut provider = store.borrow_at(original, pen)?;
                let progress = self.prefix.as_mut().ok_or(Error::Poisoned)?.step_clusters(
                    permit,
                    &mut provider,
                    self.clusters.as_mut().ok_or(Error::Poisoned)?,
                    limit,
                )?;
                if progress.status == MarkdownGlyphClusterStatus::Ready {
                    self.stage = Stage::Glyphs;
                }
            }
            Stage::Glyphs => {
                // 每次只取一个原签包glyph；provider借用结束后才写Store，避免别名和跨IO借用。
                for _ in 0..limit {
                    let value = {
                        let mut provider = store.borrow_at(original, pen)?;
                        self.prefix.as_mut().ok_or(Error::Poisoned)?.glyph(
                            permit,
                            &mut provider,
                            self.consumed,
                        )?
                    };
                    if self.consumed == self.directory.glyph_count {
                        if value.is_some() {
                            return Err(Error::InconsistentCoverage);
                        }
                        self.directory.cluster_start =
                            store.index.as_ref().ok_or(Error::Poisoned)?.len();
                        self.stage = Stage::Clusters;
                        break;
                    }
                    store
                        .index
                        .as_mut()
                        .ok_or(Error::Poisoned)?
                        .append(encode_glyph(value.ok_or(Error::InconsistentCoverage)?)?)?;
                    self.consumed = self.consumed.checked_add(1).ok_or(Error::Overflow)?;
                }
            }
            Stage::Clusters => {
                // 原逻辑cluster连续覆盖整个真实前缀，字形范围仍保child原视觉顺序。
                for _ in 0..limit {
                    let value = {
                        let mut provider = store.borrow_at(original, pen)?;
                        self.prefix
                            .as_mut()
                            .ok_or(Error::Poisoned)?
                            .containing_cluster(
                                permit,
                                &mut provider,
                                self.clusters.as_mut().ok_or(Error::Poisoned)?,
                                self.cursor,
                            )?
                    };
                    if self.cursor == self.boundary.source_utf16.end {
                        if value != MarkdownGlyphClusterAnswer::Known(None)
                            || self.directory.x_advance != self.boundary.x_advance_26_6
                        {
                            return Err(Error::InconsistentCoverage);
                        }
                        self.directory.y_advance = self.y;
                        self.directory.style_start =
                            store.index.as_ref().ok_or(Error::Poisoned)?.len();
                        self.record = self.directory.style_start;
                        self.consumed = 0;
                        self.stage = Stage::Directory;
                        break;
                    }
                    let MarkdownGlyphClusterAnswer::Known(Some(value)) = value else {
                        return Err(Error::InconsistentCoverage);
                    };
                    if value.source_utf16.start != self.cursor
                        || value.source_utf16.end > self.boundary.source_utf16.end
                        || value.glyphs.end > self.directory.glyph_count
                    {
                        return Err(Error::InconsistentCoverage);
                    }
                    store
                        .index
                        .as_mut()
                        .ok_or(Error::Poisoned)?
                        .append(encode_cluster(&value)?)?;
                    self.cursor = value.source_utf16.end;
                    self.directory.cluster_count = self
                        .directory
                        .cluster_count
                        .checked_add(1)
                        .ok_or(Error::Overflow)?;
                    self.directory.x_advance = self
                        .directory
                        .x_advance
                        .checked_add(value.x_advance_26_6)
                        .ok_or(Error::Overflow)?;
                    self.y = self
                        .y
                        .checked_add(value.y_advance_26_6)
                        .ok_or(Error::Overflow)?;
                }
            }
            Stage::Directory => {
                // 目录只是私有候选，写完三条也尚未取得可发布的LineEnd。
                for row in encode_directory(&self.directory)?
                    .into_iter()
                    .skip(self.consumed)
                    .take(limit)
                {
                    store.index.as_mut().ok_or(Error::Poisoned)?.append(row)?;
                    self.consumed += 1;
                }
                if self.consumed == 3 {
                    self.stage = Stage::Release;
                }
            }
            Stage::Release => {
                // 这里先完成真实child和临时cluster FD归还；即使元数据已写入，负面收据也
                // 不允许签LineEnd。索引Drop后只读固定observer，不保活已释放资源。
                let index = self.clusters.take().ok_or(Error::Poisoned)?;
                let observer = index.resource_observer();
                drop(index);
                let facts = observer.snapshot();
                let closed = self.prefix.take().ok_or(Error::Poisoned)?.close();
                closed?;
                if facts.file_open || facts.mapped_bytes != 0 || facts.cleanup_failed {
                    return Err(Error::Poisoned);
                }
                self.header = store.index.as_ref().ok_or(Error::Poisoned)?.len();
                self.consumed = 0;
                self.stage = Stage::Line;
            }
            Stage::Line => {
                // child已真实归还后，以原scope继续签四条固定行记录；最后一次复核才移动公开末端。
                let rows = self.line_rows(store)?;
                for row in rows.into_iter().skip(self.consumed).take(limit) {
                    store.index.as_mut().ok_or(Error::Poisoned)?.append(row)?;
                    self.consumed += 1;
                }
                if self.consumed == 4 {
                    self.require(store, original, permit, pen)?;
                    let facts = self.line_facts(store.lines, store.source.utf16_len());
                    store.publish_line(
                        self.header.checked_add(3).ok_or(Error::Overflow)?,
                        self.boundary.source_utf16.end,
                    )?;
                    store.origins = self.directory.origin;
                    return Ok(Some(facts));
                }
            }
        }
        self.require(store, original, permit, pen)?;
        Ok(None)
    }
    fn line_facts(&self, ordinal: usize, source_length: usize) -> MarkdownComposedLineFacts {
        MarkdownComposedLineFacts {
            source_utf16: self.boundary.source_utf16.clone(),
            paragraph_utf16: self.directory.facts.paragraph_utf16.clone(),
            parts: 1,
            glyphs: self.directory.glyph_count,
            ordinal,
            x_advance_26_6: self.boundary.x_advance_26_6,
            y_advance_26_6: self.y,
            kind: match self.boundary.kind {
                MarkdownSoftLineResolvedKind::Natural
                    if self.boundary.source_utf16.end == source_length =>
                {
                    // 真正文档末端仍是原UAX14强制机会；紧急续行的最后有限余量不能
                    // 因经过重排便降为普通可选断点，字形分片末端也不具有这个资格。
                    MarkdownComposedLineKind::Mandatory
                }
                MarkdownSoftLineResolvedKind::Natural => MarkdownComposedLineKind::Allowed,
                MarkdownSoftLineResolvedKind::Emergency => MarkdownComposedLineKind::Emergency,
                MarkdownSoftLineResolvedKind::OverflowingGrapheme => {
                    MarkdownComposedLineKind::Overflow
                }
            },
        }
    }
    fn line_rows(&self, store: &GlyphStore) -> Result<[[u8; 64]; 4]> {
        let facts = self.line_facts(store.lines, store.source.utf16_len());
        let mut rows = [[0; 64]; 4];
        for (at, value) in [
            (0, facts.source_utf16.start),
            (8, facts.source_utf16.end),
            (16, facts.paragraph_utf16.start),
            (24, facts.paragraph_utf16.end),
            (32, facts.ordinal),
        ] {
            rows[0][at..at + 8].copy_from_slice(&number(value)?.to_le_bytes());
        }
        store.write_line_skip(&mut rows[0])?;
        rows[0][56] = output::LINE;
        rows[0][57] = match facts.kind {
            MarkdownComposedLineKind::Allowed => 0,
            MarkdownComposedLineKind::Mandatory => 1,
            MarkdownComposedLineKind::Emergency => 2,
            MarkdownComposedLineKind::Overflow => 3,
            MarkdownComposedLineKind::TableRow => return Err(Error::InvalidRequest),
        };
        rows[0][58] = self.base_level; // 原同源准备阶段捕获，不能由child方向重签原段base。
        for (at, value) in [
            (0, facts.source_utf16.start),
            (8, facts.source_utf16.end),
            (16, self.directory.glyph_start),
            (24, facts.glyphs),
        ] {
            rows[1][at..at + 8].copy_from_slice(&number(value)?.to_le_bytes());
        }
        rows[1][48..56].copy_from_slice(&self.directory.origin.to_le_bytes());
        rows[1][56] = output::PART;
        rows[2][..8].copy_from_slice(&self.directory.font.generation.to_le_bytes());
        rows[2][8..10].copy_from_slice(&self.directory.font.slot.to_le_bytes());
        rows[2][12..16].copy_from_slice(&self.directory.facts.points.to_le_bytes());
        rows[2][16..24].copy_from_slice(&facts.x_advance_26_6.to_le_bytes());
        rows[2][24..32].copy_from_slice(&facts.y_advance_26_6.to_le_bytes());
        rows[2][32..40].copy_from_slice(&number(facts.ordinal)?.to_le_bytes());
        rows[2][40..48].copy_from_slice(&number(self.record)?.to_le_bytes());
        rows[2][56] = output::PART_META;
        rows[2][57] = self.directory.facts.level;
        rows[2][58] =
            u8::from(self.directory.facts.direction == MarkdownShapingDirection::RightToLeft);
        rows[2][59] = self.directory.kind.code();
        rows[2][60..64].copy_from_slice(&self.directory.facts.script);
        for (at, value) in [
            (0, self.header),
            (8, facts.parts),
            (32, facts.glyphs),
            (40, facts.source_utf16.len()),
        ] {
            rows[3][at..at + 8].copy_from_slice(&number(value)?.to_le_bytes());
        }
        rows[3][16..24].copy_from_slice(&facts.x_advance_26_6.to_le_bytes());
        rows[3][24..32].copy_from_slice(&facts.y_advance_26_6.to_le_bytes());
        rows[3][48..56].copy_from_slice(
            &store
                .line_end
                .map(number)
                .transpose()?
                .unwrap_or(NONE)
                .to_le_bytes(),
        );
        rows[3][56] = output::LINE_END;
        Ok(rows)
    }
}
