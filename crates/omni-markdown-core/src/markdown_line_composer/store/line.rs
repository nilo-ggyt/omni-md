//! Commit LineEnd from an actual safe slice. This entry accepts the original Job
//! path; the same sealed provider authenticates cross-item composition separately.
#[cfg(test)]
use super::output;
use super::output::OutputSeal;
use super::*;
#[cfg(test)]
use crate::{MarkdownBidiParagraph, MarkdownSafeGlyphSlice, MarkdownShapingJob};

impl GlyphStore {
    /// Consume only the original real safe token, never caller-selected glyphs via
    /// a Source range. L1/L2 changes require actual multiple-part assembly.
    #[cfg(test)]
    pub(super) fn append_safe_line(
        &mut self,
        proof: &StoredItemProof,
        mut slice: MarkdownSafeGlyphSlice,
        job: &mut MarkdownShapingJob,
        bidi: &mut MarkdownBidiParagraph,
        kind: MarkdownComposedLineKind,
    ) -> Result<MarkdownComposedLineFacts> {
        let result = (|| {
            self.guard()?;
            if self.capture.is_some() || !matches!(self.index, Some(Rows::Writing(_))) {
                return Err(Error::InvalidRequest);
            }
            let item = self.directory(proof)?;
            if !proof
                .job
                .as_ref()
                .is_some_and(|original| Weak::ptr_eq(original, &job.index_identity()))
            {
                return Err(Error::SourceMismatch);
            }
            // The original Job identity and token must still match before reading
            // its selected facts. Same coordinates or a different Job cannot renew it.
            slice.require(job)?;
            if !slice
                .cancellation()
                .same_request(&self.cancellation.measure())
            {
                return Err(Error::SourceMismatch);
            }
            let selected = slice.facts();
            let font = self.fonts.resolve(item.font)?;
            let profile = job.profile()?;
            if profile.glyph_count != item.glyph_count
                || slice.source != self.authority
                || !font.font().permits(&slice.font)
                || selected.source_utf16.start != self.line_source_end
                || selected.source_utf16.end > item.facts.source_utf16.end
                || selected.visual_glyphs.end > item.glyph_count
            {
                return Err(Error::InconsistentCoverage);
            }
            bidi.require_layout_source(&self.authority)?;
            if bidi.source_range() != (0..self.source.utf16_len())
                || bidi
                    .paragraph_range_at_utf16(selected.source_utf16.start)?
                    .as_ref()
                    != Some(&item.facts.paragraph_utf16)
            {
                return Err(Error::SourceMismatch);
            }
            let mut runs = 0usize;
            let mut identical = true;
            bidi.visit_visual_runs(selected.source_utf16.clone(), |run| {
                runs += 1;
                identical &= run.source_utf16_start == selected.source_utf16.start
                    && run.source_utf16_end == selected.source_utf16.end
                    && run.level == item.facts.level;
            })?;
            if runs != 1 || !identical {
                return Err(Error::NeedsBidiAssembly);
            } // One item cannot impersonate a complete reordered L1/L2 visual line.
            // Require the actual same-Source migrated grapheme directory;
            // publish_line will recheck its coverage of the complete line endpoint.
            self.grapheme_directory()?;
            let header = self.index.as_ref().ok_or(Error::Poisoned)?.len();
            let end = header.checked_add(3).ok_or(Error::Overflow)?;
            let glyph_start = item
                .glyph_start
                .checked_add(selected.visual_glyphs.start)
                .ok_or(Error::Overflow)?;
            let mut rows = [[0; 64]; 4];
            for (at, value) in [
                (0, selected.source_utf16.start),
                (8, selected.source_utf16.end),
                (16, item.facts.paragraph_utf16.start),
                (24, item.facts.paragraph_utf16.end),
                (32, self.lines),
            ] {
                rows[0][at..at + 8].copy_from_slice(&number(value)?.to_le_bytes());
            }
            self.write_line_skip(&mut rows[0])?;
            rows[0][56] = output::LINE;
            rows[0][57] = match kind {
                MarkdownComposedLineKind::Allowed => 0,
                MarkdownComposedLineKind::Mandatory => 1,
                MarkdownComposedLineKind::Emergency => 2,
                MarkdownComposedLineKind::Overflow => 3,
                MarkdownComposedLineKind::TableRow => return Err(Error::InvalidRequest),
            };
            rows[0][58] = self.paragraph_base_level(bidi, selected.source_utf16.start)?;
            // Original same-Source paragraph base differs from this item's resolved
            // level. A complete Uniform certificate avoids rereading text for it.
            for (at, value) in [
                (0, selected.source_utf16.start),
                (8, selected.source_utf16.end),
                (16, glyph_start),
                (24, selected.visual_glyphs.len()),
            ] {
                rows[1][at..at + 8].copy_from_slice(&number(value)?.to_le_bytes());
            }
            // The one actual visual run starts at this line's origin; glyph 26.6
            // offsets remain exactly those produced by the original shaper.
            rows[1][48..56].copy_from_slice(&item.origin.to_le_bytes());
            rows[1][56] = output::PART;
            rows[2][..8].copy_from_slice(&item.font.generation.to_le_bytes());
            rows[2][8..10].copy_from_slice(&item.font.slot.to_le_bytes());
            rows[2][12..16].copy_from_slice(&item.facts.points.to_le_bytes());
            rows[2][16..24].copy_from_slice(&selected.x_advance_26_6.to_le_bytes());
            rows[2][24..32].copy_from_slice(&selected.y_advance_26_6.to_le_bytes());
            rows[2][32..40].copy_from_slice(&number(self.lines)?.to_le_bytes());
            rows[2][40..48].copy_from_slice(&number(proof.key.record)?.to_le_bytes());
            rows[2][48..56].copy_from_slice(&number(selected.visual_glyphs.start)?.to_le_bytes());
            rows[2][56] = output::PART_META;
            rows[2][57] = item.facts.level;
            rows[2][58] = u8::from(item.facts.direction == MarkdownShapingDirection::RightToLeft);
            rows[2][60..64].copy_from_slice(&item.facts.script);
            rows[3][..8].copy_from_slice(&number(header)?.to_le_bytes());
            rows[3][8..16].copy_from_slice(&1u64.to_le_bytes());
            rows[3][16..24].copy_from_slice(&selected.x_advance_26_6.to_le_bytes());
            rows[3][24..32].copy_from_slice(&selected.y_advance_26_6.to_le_bytes());
            rows[3][32..40].copy_from_slice(&number(selected.visual_glyphs.len())?.to_le_bytes());
            rows[3][40..48].copy_from_slice(&number(selected.source_utf16.len())?.to_le_bytes());
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
            // Four fixed metadata records form the candidate. Recheck original
            // token/Source/bidi after actual writes before publishing LineEnd.
            slice.require(job)?;
            bidi.require_layout_source(&self.authority)?;
            self.guard()?;
            let facts = MarkdownComposedLineFacts {
                source_utf16: selected.source_utf16.clone(),
                paragraph_utf16: item.facts.paragraph_utf16,
                parts: 1,
                glyphs: selected.visual_glyphs.len(),
                ordinal: self.lines,
                x_advance_26_6: selected.x_advance_26_6,
                y_advance_26_6: selected.y_advance_26_6,
                kind,
            };
            self.publish_line(end, selected.source_utf16.end)?;
            Ok(facts)
        })();
        self.finish(result)
    }

    /// Transfer immutable output only after at least one actual LineEnd. Its
    /// source_range cannot describe an uncovered suffix as completed layout.
    pub(in crate::markdown_line_composer) fn into_output(
        mut self,
    ) -> Result<MarkdownComposedLines> {
        self.guard()?;
        if self.lines == 0 || self.capture.is_some() || self.control_capture.is_some() {
            return Err(Error::InvalidRequest);
        }
        // Transfer the sole real I/O owner with the grapheme facts captured at the
        // latest complete LineEnd, not metadata prefetched beyond that publication.
        let graphemes = self.published_grapheme_directory()?;
        let reader = match self.index.take().ok_or(Error::Poisoned)? {
            Rows::Writing(index) => index.into_reader()?,
            #[cfg(test)]
            Rows::Reading(reader) => reader,
        };
        MarkdownComposedLines::from_sealed(OutputSeal {
            source: self.source,
            authority: self.authority,
            fonts: self.fonts,
            cancellation: self.cancellation,
            reader,
            #[cfg(feature = "paged-shaping-frontier")]
            lineage: Arc::downgrade(&self.identity),
            identity: self.identity,
            last: self.line_end,
            count: self.lines,
            range: 0..self.line_source_end,
            graphemes,
            folds: self.folds,
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            tables: self.tables.clone(),
        })
    }
}
