//! 当前item的有限独占读取借用；目录只核一次，每次glyph/cluster仍核同源、字体和取消。
//!
//! 借用没有原job或Font强owner，也不缓存正文。它只在一次受限Measure调用期间存在，
//! 借用结束后上层重新从同Store与同opaque Proof取得资格，不能把metadata当永久权限。

use super::*;
use crate::MarkdownLayoutFontAuthority;
use crate::MarkdownSoftLineMeasureError;
use crate::markdown_soft_line_measure::provider::{self, GlyphProvider};

/// 只能由原Store验证私有Proof后构造；外部crate没有实现provider或伪造origin的入口。
pub(in crate::markdown_line_composer) struct StoredGlyphBorrow<'a> {
    store: &'a mut GlyphStore,
    directory: Directory,
    pen: i64,
    // 仅本次Measure借用保留一条已认证的64B记录和实际ordinal；不跨借用保存源或字体权限。
    cluster_hint: Option<(usize, [u8; 64])>,
}

impl provider::sealed::Sealed for StoredGlyphBorrow<'_> {}

impl GlyphProvider for StoredGlyphBorrow<'_> {
    /// 本次原目录已由 borrow/require 核对；只回读独占 kind，不产生新的源或范围权限。
    fn folded_section(&self) -> Option<Range<usize>> {
        (self.directory.kind == super::super::MarkdownComposedLinePartKind::FoldedSection)
            .then(|| self.directory.facts.source_utf16.clone())
    }

    /// 仅原 ValidatedControl 写成的真实零字形换段可终止前行；TableRow 与普通 glyph 不获此能力。
    fn ends_paragraph_control(&self) -> bool {
        self.directory.kind == super::super::MarkdownComposedLinePartKind::ParagraphBreak
            && self.directory.glyph_count == 0
            && !self.directory.facts.source_utf16.is_empty()
    }

    fn origin(&self) -> provider::GlyphProviderOrigin {
        let (store, origin) = StoredGlyphBorrow::origin(self);
        provider::GlyphProviderOrigin::Stored { store, origin }
    }
    fn require(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> std::result::Result<provider::GlyphProviderFacts, MarkdownSoftLineMeasureError> {
        let result = (|| {
            self.store.guard()?;
            if source != self.authority()
                || !self.source().permits(source)
                || !self
                    .store
                    .fonts
                    .resolve(self.directory.font)?
                    .font()
                    .permits(font)
            {
                return Err(Error::SourceMismatch);
            }
            // 目录已在本借用建立时核过覆盖；这里只核本次真实来源和字体权限。
            // 返回前由原finish统一复核健康，实际cluster读取仍各自检查范围和完整记录。
            let (store, origin) = self.origin();
            Ok(provider::GlyphProviderFacts {
                origin: provider::GlyphProviderOrigin::Stored { store, origin },
                source_utf16: self.source_range(),
                glyphs: self.glyph_count(),
                generation: self.generation(),
                direction: self.direction(),
            })
        })();
        self.store.finish(result).map_err(measure_error)
    }
    fn cluster(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        offset: usize,
    ) -> std::result::Result<MarkdownGlyphClusterAnswer, MarkdownSoftLineMeasureError> {
        GlyphProvider::require(self, source, font)?;
        let mut answer = StoredGlyphBorrow::cluster(self, offset).map_err(measure_error)?;
        if self.directory.kind == super::super::MarkdownComposedLinePartKind::Tab
            && let Some(value) = &mut answer
        {
            let placement = self
                .store
                .tab_style(&self.directory)
                .and_then(|style| style.tab(self.pen))
                .map_err(measure_error)?;
            let super::super::MarkdownLineTabPlacement::Left { advance_26_6 } = placement else {
                return Err(MarkdownSoftLineMeasureError::InvalidRequest);
            };
            value.x_advance_26_6 = advance_26_6;
            GlyphProvider::require(self, source, font)?; // Tab另读原style/placement，须在这次额外IO后核原权限。
        }
        // 普通cluster已由内层require_current及finish核过真实Font、取消和Source首错；
        // 此后仅移动本次结果，没有Native、回调或另一借用，不重复完整catalog/来源扫描。
        Ok(MarkdownGlyphClusterAnswer::Known(answer))
    }
    #[cfg(feature = "paged-shaping-frontier")]
    fn frontier_word_end(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> std::result::Result<Option<usize>, MarkdownSoftLineMeasureError> {
        GlyphProvider::require(self, source, font)?;
        let end = self.directory.frontier_word_end;
        GlyphProvider::require(self, source, font)?;
        Ok(end) // 只回读原producer写入同origin的事实，不重建已Drop Job或扩大当前source_range。
    }
    #[cfg(feature = "paged-shaping-frontier")]
    fn bidi_shaping_end(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> std::result::Result<Option<usize>, MarkdownSoftLineMeasureError> {
        GlyphProvider::require(self, source, font)?;
        let end = self.directory.shaping_seams[1].map(|_| self.directory.facts.source_utf16.end);
        GlyphProvider::require(self, source, font)?;
        Ok(end) // 私有目录只由原 Item 的 sealed seam 写入；公开整数不能补签该事实。
    }
    fn committed_line_start(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        selected: &provider::SelectedMeasureSelection,
    ) -> std::result::Result<provider::CommittedLineStart, MarkdownSoftLineMeasureError> {
        GlyphProvider::require(self, source, font)?;
        let committed = self.committed_line(&selected.kind).map_err(measure_error)?;
        if committed.source_utf16 != selected.source_utf16
            || committed.x_advance_26_6 != selected.x_advance_26_6
            || selected.boundary_flags & harfrust::GlyphFlags::UNSAFE_TO_BREAK.to_bits() != 0
        {
            return self
                .store
                .finish(Err(Error::InconsistentCoverage))
                .map_err(measure_error);
        }
        GlyphProvider::require(self, source, font)?;
        Ok(committed) // 只有原latest LineEnd实际写完，且宽度/选中事实仍相同，才签本次行首。
    }
    fn require_committed_line_start(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        committed: &provider::CommittedLineStart,
    ) -> std::result::Result<(), MarkdownSoftLineMeasureError> {
        GlyphProvider::require(self, source, font)?;
        let actual = self
            .committed_line(&committed.kind)
            .map_err(measure_error)?;
        if actual.source_utf16 != committed.source_utf16
            || actual.generation != committed.generation
            || actual.record != committed.record
            || actual.ordinal != committed.ordinal
            || actual.kind != committed.kind
            || actual.x_advance_26_6 != committed.x_advance_26_6
        {
            return self
                .store
                .finish(Err(Error::InvalidRequest))
                .map_err(measure_error);
        } // 旧line票不能在更晚LineEnd之后重开原行；同源/同坐标也不接受另一个Store。
        GlyphProvider::require(self, source, font)?;
        Ok(())
    }
}

fn measure_error(error: Error) -> MarkdownSoftLineMeasureError {
    match error {
        Error::Source(error) => MarkdownSoftLineMeasureError::Source(error),
        Error::SourceMismatch => MarkdownSoftLineMeasureError::SourceMismatch,
        Error::InvalidRequest => MarkdownSoftLineMeasureError::InvalidRequest,
        Error::InconsistentCoverage => MarkdownSoftLineMeasureError::InconsistentCoverage,
        Error::Overflow => MarkdownSoftLineMeasureError::Overflow,
        Error::Cancelled => MarkdownSoftLineMeasureError::Cancelled,
        Error::Poisoned => MarkdownSoftLineMeasureError::Poisoned,
        Error::Shaping(error) => MarkdownSoftLineMeasureError::Shaping(error),
        Error::Cluster(error) => MarkdownSoftLineMeasureError::Cluster(error),
        Error::Break(error) => MarkdownSoftLineMeasureError::Break(error),
        // 字体过期、实际文件IO与catalog失败保留完整首因，不压成假空cluster或普通Pending。
        other => MarkdownSoftLineMeasureError::Stored(Box::new(other)),
    }
}

impl GlyphStore {
    pub(in crate::markdown_line_composer) fn borrow_at(
        &mut self,
        proof: &StoredItemProof,
        pen: i64,
    ) -> Result<StoredGlyphBorrow<'_>> {
        let mut borrowed = self.borrow(proof)?;
        borrowed.pen = pen;
        Ok(borrowed)
    }
    pub(in crate::markdown_line_composer) fn tab_placement(
        &mut self,
        proof: &StoredItemProof,
        pen: i64,
    ) -> Result<Option<super::super::MarkdownLineTabPlacement>> {
        let result = (|| {
            let directory = self.directory(proof)?;
            if directory.kind != super::super::MarkdownComposedLinePartKind::Tab {
                return Ok(None);
            }
            Ok(Some(self.tab_style(&directory)?.tab(pen)?))
        })();
        self.finish(result)
    }
    pub(in crate::markdown_line_composer) fn borrow(
        &mut self,
        proof: &StoredItemProof,
    ) -> Result<StoredGlyphBorrow<'_>> {
        let result = self.directory(proof);
        let directory = self.finish(result)?;
        Ok(StoredGlyphBorrow {
            store: self,
            directory,
            pen: 0,
            cluster_hint: None,
        })
    }
}

impl StoredGlyphBorrow<'_> {
    fn committed_line(
        &mut self,
        kind: &provider::SelectedMeasureKind,
    ) -> Result<provider::CommittedLineStart> {
        let result = self.committed_line_inner(kind);
        self.store.finish(result) // 新票校验失败也永久撤半候选；析构中的原Source首错仍先行。
    }
    fn committed_line_inner(
        &mut self,
        kind: &provider::SelectedMeasureKind,
    ) -> Result<provider::CommittedLineStart> {
        self.require_current()?;
        let record = self.store.line_end.ok_or(Error::InvalidRequest)?;
        let ordinal = self
            .store
            .lines
            .checked_sub(1)
            .ok_or(Error::InvalidRequest)?;
        let file = self.store.index.as_mut().ok_or(Error::Poisoned)?;
        let tail = file.read(record)?;
        let header = usize_at(&tail, 0)?;
        let parts = usize_at(&tail, 8)?;
        if tail[56] != output::LINE_END
            || tail[57..].iter().any(|byte| *byte != 0)
            || parts == 0
            || header
                .checked_add(1)
                .and_then(|start| parts.checked_mul(2).and_then(|n| start.checked_add(n)))
                != Some(record)
        {
            return Err(Error::InconsistentCoverage);
        }
        let head = file.read(header)?;
        let code = match kind {
            provider::SelectedMeasureKind::Natural(boundary) => {
                u8::from(boundary.kind == crate::MarkdownLineBreakKind::Mandatory)
            }
            #[cfg(feature = "paged-shaping-frontier")]
            provider::SelectedMeasureKind::Emergency {
                overflowing_grapheme,
            } => {
                if *overflowing_grapheme {
                    3
                } else {
                    2
                }
            }
        };
        let range = usize_at(&head, 0)?..usize_at(&head, 8)?;
        if head[56] != output::LINE
            || head[57] != code
            || head[58] > 1
            || head[59..].iter().any(|byte| *byte != 0)
            || usize_at(&head, 32)? != ordinal
            || range.start >= range.end
            || range.end != self.store.line_source_end
            || range.end > self.store.source.utf16_len()
            || usize_at(&tail, 40)? != range.len()
        {
            return Err(Error::InconsistentCoverage);
        }
        match kind {
            provider::SelectedMeasureKind::Natural(boundary) if boundary.utf16 != range.end => {
                return Err(Error::InconsistentCoverage);
            }
            _ => {}
        }
        self.require_current()?;
        Ok(provider::CommittedLineStart {
            source_utf16: range,
            generation: self.generation(),
            record,
            ordinal,
            kind: kind.clone(),
            x_advance_26_6: i64_at(&tail, 16),
        })
    }

    pub(super) fn require_current(&self) -> Result<()> {
        self.store.guard()?;
        self.store.fonts.resolve(self.directory.font)?;
        Ok(()) // 字体解析只借固定弱槽；每次实际记录读取前后及最终finish仍核原Source。
    }

    /// 该origin只供封闭provider比较；数字单独不能恢复Store、Source、Font或Ready资格。
    pub(super) fn origin(&self) -> (Weak<()>, u64) {
        (Arc::downgrade(&self.store.identity), self.directory.origin)
    }
    pub(super) fn source(&self) -> &MarkdownLayoutTextSource {
        &self.store.source
    }
    pub(super) fn authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.store.authority
    }
    pub(super) fn source_range(&self) -> Range<usize> {
        self.directory.facts.source_utf16.clone()
    }
    pub(super) fn direction(&self) -> MarkdownShapingDirection {
        self.directory.facts.direction
    }
    pub(super) fn glyph_count(&self) -> usize {
        self.directory.glyph_count
    }
    pub(super) fn generation(&self) -> u64 {
        self.store.source.generation()
    }
    pub(super) fn cluster(&mut self, offset: usize) -> Result<Option<MarkdownGlyphCluster>> {
        let result = (|| {
            self.require_current()?;
            let source = &self.directory.facts.source_utf16;
            if offset < source.start || offset > source.end {
                return Err(Error::InvalidRequest);
            }
            if offset == source.end {
                return Ok(None);
            }
            if let Some((ordinal, row)) = self.cluster_hint {
                let value = decode_cluster(row)?;
                if value.source_utf16.contains(&offset) {
                    // 内部UTF16或断点flags探针重查同一原cluster时只复用完整记录，不截取或重算glyph。
                    self.require_current()?;
                    return Ok(Some(value));
                }
                if offset == value.source_utf16.end {
                    let next = ordinal.checked_add(1).ok_or(Error::Overflow)?;
                    if next < self.directory.cluster_count {
                        // Store按逻辑源顺序写入真实目录；只允许已知末端领下一ordinal，RTL不推视觉序。
                        self.require_current()?;
                        let (row, following) = self.cluster_record(next)?;
                        if following.source_utf16.start != value.source_utf16.end
                            || !following.source_utf16.contains(&offset)
                        {
                            return Err(Error::InconsistentCoverage);
                        }
                        self.require_current()?;
                        self.cluster_hint = Some((next, row));
                        return Ok(Some(following));
                    }
                }
            }
            let (mut low, mut high) = (0, self.directory.cluster_count);
            while low < high {
                self.require_current()?; // 最多usize::BITS次，每次实际record读取前核同票，不整项线性扫描。
                let middle = low + (high - low) / 2;
                let (row, value) = self.cluster_record(middle)?;
                if offset < value.source_utf16.start {
                    high = middle;
                } else if offset >= value.source_utf16.end {
                    low = middle + 1;
                } else {
                    self.require_current()?;
                    self.cluster_hint = Some((middle, row));
                    return Ok(Some(value));
                }
            }
            Err(Error::InconsistentCoverage)
        })();
        self.store.finish(result)
    }

    /// 读取同一已认证目录中的实际记录；调用者只传二分中点或已知相邻ordinal。
    fn cluster_record(&mut self, ordinal: usize) -> Result<([u8; 64], MarkdownGlyphCluster)> {
        let record = self
            .directory
            .cluster_start
            .checked_add(ordinal)
            .ok_or(Error::Overflow)?;
        let row = self
            .store
            .index
            .as_mut()
            .ok_or(Error::Poisoned)?
            .read(record)?;
        let value = decode_cluster(row)?;
        if value.source_utf16.start < self.directory.facts.source_utf16.start
            || value.source_utf16.end > self.directory.facts.source_utf16.end
            || value.glyphs.end > self.directory.glyph_count
        {
            return Err(Error::InconsistentCoverage); // IO/格式/覆盖失败交原finish撤文件，不能保留半条hint。
        }
        Ok((row, value))
    }
}

impl provider::ResolutionProvider for StoredGlyphBorrow<'_> {
    fn shaping_cancellation(&self) -> crate::MarkdownShapingCancellation {
        self.store.cancellation.shaping()
    }
    fn following_item_flags(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> std::result::Result<Option<u32>, crate::MarkdownSoftLineResolveError> {
        GlyphProvider::require(self, source, font)?;
        let result = (|| {
            let end = self.directory.facts.source_utf16.end;
            let Some(proof) = self.store.item_at(end)? else {
                return Ok(None);
            };
            let next = self.store.directory(&proof)?; // 同Store真实proof同时核原Source、弱Font注册及完整目录。
            if next.kind != super::super::MarkdownComposedLinePartKind::Glyphs
                || next.facts.source_utf16.start != end
                || next.cluster_count == 0
            {
                return Ok(None); // 未完成、控制或缺口没有后继glyph见证，仍交原NextItem能力出口。
            }
            let row = self
                .store
                .index
                .as_mut()
                .ok_or(Error::Poisoned)?
                .read(next.cluster_start)?;
            let cluster = decode_cluster(row)?;
            if cluster.source_utf16.start != end
                || cluster.source_utf16.end > next.facts.source_utf16.end
                || cluster.glyphs.end > next.glyph_count
            {
                return Err(Error::InconsistentCoverage);
            } // 错目录/记录不得被当作“安全后继”；只消费同一真实首cluster，不扫描整项。
            Ok((cluster.missing_glyphs == 0).then_some(cluster.flags))
        })();
        let value = self.store.finish(result).map_err(measure_error)?;
        GlyphProvider::require(self, source, font)?;
        Ok(value)
    }
    fn reshape_subrange(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        range: Range<usize>,
    ) -> std::result::Result<crate::MarkdownShapingJob, crate::MarkdownSoftLineResolveError> {
        use crate::{MarkdownShapingJob, MarkdownShapingOptions, MarkdownSoftLineResolveError};
        // 该sealed方法只由原Resolver的真实候选调用；先核同源/字体/完整目录，再派生实际子任务。
        GlyphProvider::require(self, source, font)?;
        if self.directory.kind != super::super::MarkdownComposedLinePartKind::Glyphs
            || range.is_empty()
            || range.start < self.directory.facts.source_utf16.start
            || range.end > self.directory.facts.source_utf16.end
        {
            return Err(MarkdownSoftLineResolveError::InvalidRequest);
        }
        let registered = self
            .store
            .fonts
            .resolve(self.directory.font)
            .map_err(measure_error)?;
        let proof = StoredItemProof {
            store: Arc::downgrade(&self.store.identity),
            key: ItemKey {
                record: self
                    .directory
                    .style_start
                    .checked_add(self.directory.style_rows)
                    .ok_or(MarkdownSoftLineResolveError::Overflow)?,
                origin: self.directory.origin,
            },
            #[cfg(test)]
            job: None,
        };
        let style = self.store.style(&proof).map_err(measure_error)?;
        let options = MarkdownShapingOptions {
            direction: self.directory.facts.direction,
            script: self.directory.facts.script,
            language: style.language,
            points: self.directory.facts.points,
            // 沿原真实段落旗标相与；软换行不伪造新的BOT/EOT，仍安装同Source邻接上下文。
            beginning_of_text: self.directory.facts.beginning_of_text
                && range.start == self.directory.facts.source_utf16.start,
            end_of_text: self.directory.facts.end_of_text
                && range.end == self.directory.facts.source_utf16.end,
            features: style.features,
        };
        let job = MarkdownShapingJob::new_with_cancellation(
            self.store.source.clone(),
            source,
            registered.font().clone(),
            font,
            range,
            options,
            self.store.cancellation.shaping(),
        )?;
        GlyphProvider::require(self, source, font)?; // 创建期间失权就归还真实候选，不能以新Job补签原请求。
        Ok(job)
    }
}
