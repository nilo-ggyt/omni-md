//! Bounded exclusive reads of the current item. Authenticate its directory once;
//! each glyph/cluster read still checks original Source, font, and cancellation.
//!
//! This borrow owns no Job/font payload or text cache. It lives for one bounded
//! Measure call; later calls reacquire authority from the same Store and opaque
//! proof instead of treating immutable metadata as permanent permission.

use super::*;
use crate::MarkdownLayoutFontAuthority;
use crate::MarkdownSoftLineMeasureError;
use crate::markdown_soft_line_measure::provider::{self, GlyphProvider};

/// Constructed only after the original Store verifies its private proof. External
/// crates cannot implement this sealed provider or manufacture its origin.
pub(in crate::markdown_line_composer) struct StoredGlyphBorrow<'a> {
    store: &'a mut GlyphStore,
    directory: Directory,
    pen: i64,
    // Retain one authenticated 64-byte cluster and its real ordinal for this
    // Measure borrow only, without carrying Source/font authority across calls.
    cluster_hint: Option<(usize, [u8; 64])>,
}

impl provider::sealed::Sealed for StoredGlyphBorrow<'_> {}

impl GlyphProvider for StoredGlyphBorrow<'_> {
    /// The original directory was authenticated by borrow/require. Read its kind
    /// only, without deriving new Source or range authority.
    fn folded_section(&self) -> Option<Range<usize>> {
        (self.directory.kind == super::super::MarkdownComposedLinePartKind::FoldedSection)
            .then(|| self.directory.facts.source_utf16.clone())
    }

    /// Only an actual zero-glyph paragraph break from ValidatedControl can end a
    /// preceding text line. Tables and ordinary glyphs do not gain that capability.
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
            // Coverage was authenticated when this borrow began. Check the same
            // actual Source/font now; finish rechecks health before return and
            // individual cluster reads still validate their full records/ranges.
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
            GlyphProvider::require(self, source, font)?; // Recheck after actual tab-style I/O.
        }
        // Ordinary clusters already passed require_current/finish for actual font,
        // cancellation, and Source first failure. Moving this result involves no
        // Native call, callback, or other borrow requiring another full scan.
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
        Ok(end) // Same-origin producer fact only; no retired Job or expanded range.
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
        Ok(end) // Only the original item's sealed seam populated this directory fact.
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
        Ok(committed) // The latest actual LineEnd must retain its selection and width facts.
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
        } // An old line ticket cannot reopen the line after a later LineEnd, and
        // identical Source/coordinates in a different Store grant no authority.
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
        // Preserve font expiry, actual file I/O, and catalog causes instead of
        // returning fabricated empty clusters or ordinary Pending.
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
        self.store.finish(result) // A failed ticket also revokes the partial candidate.
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
        // Read the actual latest LineEnd and its exact head; a retained numeric
        // line range cannot independently certify complete committed output.
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
        Ok(()) // Font lookup borrows a fixed weak slot; actual reads/finish still check Source.
    }

    /// Only the sealed provider compares this origin. Its number alone cannot
    /// recover Store, Source, font, or Ready authority.
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
                    // A same-cluster UTF-16/flags query reuses the whole original
                    // record, without slicing it or recomputing glyphs.
                    self.require_current()?;
                    return Ok(Some(value));
                }
                if offset == value.source_utf16.end {
                    let next = ordinal.checked_add(1).ok_or(Error::Overflow)?;
                    if next < self.directory.cluster_count {
                        // Store records follow logical Source order. Only the
                        // known endpoint admits next ordinal; RTL is not inferred.
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
                self.require_current()?; // Bounded binary search; every read keeps original authority.
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

    /// Read an actual record in this same authenticated directory. Callers supply
    /// only a binary-search midpoint or a known adjacent ordinal.
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
            return Err(Error::InconsistentCoverage); // Invalid coverage cannot leave a usable hint.
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
            let Some(located) = self.store.item_at(end)? else {
                return Ok(None);
            };
            // Lookup authenticated the full directory through the same Store's
            // original proof. Reuse it only within this one provider borrow.
            let next = located.directory;
            if next.kind != super::super::MarkdownComposedLinePartKind::Glyphs
                || next.facts.source_utf16.start != end
                || next.cluster_count == 0
            {
                return Ok(None); // Incomplete/control coverage grants no successor glyph proof.
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
            } // Validate this one real first cluster; corruption is not safe continuation.
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
        // Only the original resolver's real candidate reaches this sealed method.
        // Check same-Source/font/complete-directory authority before creating its Job.
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
            // Intersect original paragraph flags with this actual range. Soft wraps
            // cannot invent BOT/EOT; original adjacent Source context is still used.
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
        // If authority expired during construction, release the real candidate;
        // creating a new Job cannot renew the original request.
        GlyphProvider::require(self, source, font)?;
        Ok(job)
    }
}
