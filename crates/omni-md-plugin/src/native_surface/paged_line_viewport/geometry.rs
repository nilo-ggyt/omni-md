//! Actual grapheme geometry in bounded visible lines, without uniform UTF-16 spacing or whole-line grapheme arrays.

use super::*;
use crate::native_surface::paged_line_geometry::{
    ComposedPartSourceEdge, composed_part_caret, composed_part_extent_edge,
};
use omni_markdown_core::{
    MarkdownComposedLinePartKind, MarkdownGraphemeAnswer, MarkdownGraphemeQuery,
    MarkdownPresentationStyles,
};

/// Each hit queries a fixed maximum of actual boundaries; oversized zero-advance lines need later producer indexing.
const HIT_BOUNDARIES: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeMarkdownComposedViewportHit {
    pub source_utf16: usize,
    /// The actual hit part's start/end side; after Cell syntax removal, equal local carets may map to different canonical endpoints.
    pub source_affinity: omni_markdown_core::MarkdownComposedLineAffinity,
    /// The second candidate retains its own part side, rather than borrow primary affinity to interpret a Bidi boundary.
    pub other_source_affinity: Option<omni_markdown_core::MarkdownComposedLineAffinity>,
    /// Retain another logical candidate for the same actual visual X; this does not authorize an outer layer to invent primary direction.
    pub other_source_utf16: Option<usize>,
    pub line_ordinal: usize,
}

impl NativeMarkdownComposedViewport {
    /// Enumerate actual advance envelopes of current visible labels without target lookup or glyph/text copies.
    /// This layer establishes View coordinates/viewport-Cell clipping; the AppKit installer owns cursor-region sorting/merging.
    pub(in crate::native_surface) fn visit_link_cursor_rects(
        &self,
        current: &dyn Fn() -> bool,
        visit: &mut dyn FnMut(NSRect),
    ) -> Result<()> {
        self.require(current)?;
        for line in self.lines.iter().filter(|line| self.visible(line.frame)) {
            for part in &line.parts {
                let Some(area) = link_part_rect(line, part) else {
                    continue;
                };
                let area = objc2_foundation::NSIntersectionRect(area, self.clip());
                if area.size.width <= 0.0 || area.size.height <= 0.0 {
                    continue; // Offscreen/zero-advance labels cannot expand hover regions to a whole line or neighboring Cell.
                }
                if !rect(area) {
                    return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
                }
                self.require(current)?; // Deliver Copy geometry only; the caller returns Scene after collection before entering Native.
                visit(area);
                self.require(current)?; // Source errors still come from original output; temporary collections cannot authorize a successor Scene.
            }
        }
        self.require(current)
    }

    /// Find uses actual part ranges from visible soft lines; Preview Markdown syntax has no highlight authority,
    /// while Source keeps literal characters. Fold/table placeholders have no ordinary text glyphs, so line envelopes cannot authorize scanning their hidden Source.
    pub(in crate::native_surface) fn visible_find_ranges(
        &self,
        current: &dyn Fn() -> bool,
    ) -> Result<Vec<Range<usize>>> {
        self.require(current)?;
        let preview = self.key.presentation_mode == MarkdownPresentationMode::InlinePreview;
        let mut ranges = Vec::new();
        for line in self.lines.iter().filter(|line| self.visible(line.frame)) {
            for part in &line.parts {
                let facts = part.handle.facts();
                if facts.source_utf16.is_empty()
                    || matches!(
                        facts.kind,
                        MarkdownComposedLinePartKind::FoldedSection
                            | MarkdownComposedLinePartKind::TableRow
                    )
                    || preview
                        && (facts
                            .presentation
                            .contains(MarkdownPresentationStyles::MARKER)
                            || facts
                                .presentation
                                .contains(MarkdownPresentationStyles::HIDDEN_MARKER))
                {
                    continue;
                }
                ranges.push(facts.source_utf16.clone()); // Copy sealed metadata only, without reading, copying, or parsing text.
            }
        }
        self.require(current)?;
        Ok(ranges)
    }

    /// Only an actual visible link-label advance envelope admits target lookup; style bits cannot construct URLs or expand to trailing line whitespace.
    pub fn link_part_at(
        &self,
        local: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<Range<usize>>> {
        self.require(current)?;
        if !inside(local, self.clip()) {
            return Ok(None);
        }
        for line in &self.lines {
            if !self.visible(line.frame) {
                continue;
            }
            for part in &line.parts {
                let facts = part.handle.facts();
                let Some(area) = link_part_rect(line, part) else {
                    continue;
                };
                if inside(local, area) {
                    self.require(current)?;
                    return Ok(Some(facts.source_utf16.clone()));
                }
            }
        }
        Ok(None)
    }

    /// IME/visibility/painting consume the same selection slot's actual Bidi side; nonempty ranges still locate their first grapheme.
    pub fn first_rect_with_caret_side(
        &self,
        range: NSRange,
        affinity: crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity,
        side: Option<crate::native_surface::paged_input::NativeMarkdownBidiCaretSide>,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedCharacterRect> {
        if range.length != 0 {
            return self.first_rect_with_affinity(range, affinity, current);
        } // Caret side does not alter character rectangles; it explicitly chooses between two carets at the same position.
        let caret = self.caret_geometry_with_side(range.location, affinity, side, current)?;
        if !self.visible(caret.line_frame) {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        self.require(current)?;
        Ok(NativeMarkdownPagedCharacterRect {
            rect: caret.primary,
            actual_range: range,
        })
    }

    /// Return the actual typographic frame of the first scalar's grapheme; remain Pending without a visible line covering that first position.
    ///
    /// # Errors
    /// Reject offscreen/unknown ranges, revoked Source/font/Scene, and non-grapheme carets rather than substitute later visible text.
    pub fn first_rect(
        &self,
        range: NSRange,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedCharacterRect> {
        self.first_rect_with_affinity(
            range,
            crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity::Downstream,
            current,
        )
    }

    /// IME/navigation uses original selection wrap affinity, not the following line in place of the user's actual upstream caret.
    /// Nonempty ranges locate the first scalar's grapheme; original outer selection evidence must resolve two Bidi positions.
    ///
    /// # Errors
    /// As with first_rect; an empty range with two visual positions remains Pending without guessing primary caret.
    pub fn first_rect_with_affinity(
        &self,
        range: NSRange,
        affinity: crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedCharacterRect> {
        self.require(current)?;
        range
            .location
            .checked_add(range.length)
            .filter(|end| *end <= self.utf16_len())
            .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?;
        if range.length == 0 {
            let caret = self.caret_geometry(range.location, affinity, current)?;
            if !self.visible(caret.line_frame) || caret.secondary.is_some() {
                return Err(NativeMarkdownComposedViewportError::Pending);
            } // Use the prepared side only; wrap affinity grants no Bidi primary/secondary choice.
            self.require(current)?;
            return Ok(NativeMarkdownPagedCharacterRect {
                rect: caret.primary,
                actual_range: range,
            });
        }
        let (line, _) = self.part_for_position(range.location)?;
        if !self.visible(line.frame) {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        // Expand nonempty requests to their first scalar's complete grapheme; neither later lines nor UTF-16 interiors are the first glyph position.
        let start = self.boundary(range.location, false)?;
        let next = self.next_boundary(start)?;
        if next > line.handle.facts().source_utf16.end || next <= start {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        let actual = start..next;
        let mut rect: Option<NSRect> = None;
        let mut covered = 0usize;
        for part in &line.parts {
            let source = &part.handle.facts().source_utf16;
            let overlap = actual.start.max(source.start)..actual.end.min(source.end);
            if overlap.start >= overlap.end {
                continue;
            }
            // One grapheme may span actual UBA/font items; union their actual envelopes within the bounded original line only.
            // Internal seams are paint sections only; actual_range retains the complete grapheme instead of giving IME a partial character frame.
            let next = self.part_rect(line, part, overlap.clone(), current)?;
            covered = covered
                .checked_add(overlap.len())
                .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?;
            rect = Some(if let Some(previous) = rect {
                let left = previous.origin.x.min(next.origin.x);
                let bottom = previous.origin.y.min(next.origin.y);
                let right =
                    (previous.origin.x + previous.size.width).max(next.origin.x + next.size.width);
                let top = (previous.origin.y + previous.size.height)
                    .max(next.origin.y + next.size.height);
                NSRect::new(
                    NSPoint::new(left, bottom),
                    NSSize::new(right - left, top - bottom),
                )
            } else {
                next
            });
        }
        if covered != actual.len() {
            return Err(NativeMarkdownComposedViewportError::Pending);
        } // An already displayed fragment cannot certify a full grapheme that is not completely prepared.
        let rect = rect.ok_or(NativeMarkdownComposedViewportError::Pending)?;
        if !self.visible(rect) && rect.size.width != 0.0 {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        self.require(current)?;
        Ok(NativeMarkdownPagedCharacterRect {
            rect,
            actual_range: NSRange::new(actual.start, actual.end - actual.start),
        })
    }

    /// Actual glyph/adjacent-grapheme caret direction decides word-hit bias, shared by Body and read-only Cells.
    /// Return a hit rather than a word range; the same-Source UBA producer still classifies Chinese, punctuation, and Bidi word boundaries.
    pub(in crate::native_surface) fn word_hit(
        &self,
        local: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<(NativeMarkdownComposedViewportHit, bool)> {
        let hit = self.hit(local, current)?;
        if hit.other_source_utf16.is_some() {
            return Err(NativeMarkdownComposedViewportError::Pending);
        } // Two logical candidates at one visual position cannot be resolved from UTF-16 ordering or assumed LTR.
        let line = self
            .lines
            .iter()
            .find(|line| line.handle.facts().ordinal == hit.line_ordinal)
            .ok_or(NativeMarkdownComposedViewportError::InvalidGeometry)?;
        let range = &line.handle.facts().source_utf16;
        let affinity =
            crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity::from_upstream(
                hit.source_affinity == omni_markdown_core::MarkdownComposedLineAffinity::Upstream,
            );
        let caret = self.caret_geometry(hit.source_utf16, affinity, current)?;
        if caret.line_ordinal != hit.line_ordinal || caret.secondary.is_some() {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        for next in [true, false] {
            let position = {
                // Read the original sealed grapheme directory and end the reader borrow before Native geometry/current callbacks.
                let mut output = self
                    .output
                    .try_borrow_mut()
                    .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
                match output.grapheme_query(
                    hit.source_utf16,
                    if next {
                        MarkdownGraphemeQuery::Next
                    } else {
                        MarkdownGraphemeQuery::Previous
                    },
                )? {
                    MarkdownGraphemeAnswer::Known(Some(boundary)) => Some(boundary.utf16),
                    _ => None,
                }
            };
            self.require(current)?;
            let Some(position) =
                position.filter(|position| range.contains(position) || *position == range.end)
            else {
                continue; // Offwindow/hard-break/missing neighbors grant no direction; the other actual same-run neighbor may still supply it.
            };
            let adjacent = self.caret_geometry(
                position,
                crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity::from_upstream(next),
                current,
            )?;
            if adjacent.line_ordinal != hit.line_ordinal || adjacent.secondary.is_some() {
                continue;
            }
            let forward_x = if next {
                adjacent.primary.origin.x - caret.primary.origin.x
            } else {
                caret.primary.origin.x - adjacent.primary.origin.x
            };
            if forward_x == 0.0 {
                continue; // Zero advance proves no direction; UTF-16 adjacency cannot substitute for actual visual displacement.
            }
            let backward_bias = if forward_x > 0.0 {
                local.x < caret.primary.origin.x
            } else {
                local.x > caret.primary.origin.x
            };
            self.require(current)?;
            return Ok((hit, backward_bias));
        }
        self.require(current)?;
        if range.is_empty() {
            return Ok((hit, false)); // An actual empty line has no word; UBA returns its empty range without borrowing neighboring text.
        }
        Err(NativeMarkdownComposedViewportError::Pending)
    }

    /// Project drag endpoints through bounded prepared-line geometry; text hit-testing below retains actual grapheme/Bidi candidates.
    ///
    /// # Errors
    /// Invalid coordinates/original authority fail; without a prepared eligible line return no projected point.
    /// Project only onto genuinely visible sealed lines, retaining physical pointer position for scrolling rather than guessing offscreen text.
    pub(in crate::native_surface) fn project_drag_point(
        &self,
        local: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<NSPoint>> {
        self.require(current)?;
        if !point(local) {
            return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
        }
        let clip = self.clip();
        let mut nearest = None;
        let mut distance = f64::INFINITY;
        for line in &self.lines {
            if line.handle.facts().kind == omni_markdown_core::MarkdownComposedLineKind::TableRow
                || line.parts.is_empty()
                || line.parts.iter().all(|part| {
                    matches!(
                        part.handle.facts().kind,
                        MarkdownComposedLinePartKind::FoldedSection
                            | MarkdownComposedLinePartKind::TableRow
                    )
                })
            {
                continue; // Actual Grid owns structural lines; hidden folds supply neither visible drag targets nor invented line heights.
            }
            if self.key.presentation_mode == MarkdownPresentationMode::InlinePreview {
                let mut hidden_text = false;
                let mut visible_text = false;
                for part in &line.parts {
                    let facts = part.handle.facts();
                    if matches!(
                        facts.kind,
                        MarkdownComposedLinePartKind::Glyphs | MarkdownComposedLinePartKind::Tab
                    ) {
                        if facts
                            .presentation
                            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
                        {
                            hidden_text = true;
                        } else {
                            visible_text = true;
                        }
                    }
                }
                if hidden_text && !visible_text {
                    continue; // A line containing only tiny hidden syntax is not a drag target; actual empty-line ParagraphBreak remains eligible.
                }
            }
            let bottom = line.frame.origin.y.max(clip.origin.y);
            let top = (line.frame.origin.y + line.frame.size.height)
                .min(clip.origin.y + clip.size.height);
            if top <= bottom {
                continue;
            }
            // Use hit-testing's half-open right/top edges; next_down picks the nearest representable point inside the same actual frame.
            let projected = NSPoint::new(
                local
                    .x
                    .clamp(clip.origin.x, (clip.origin.x + clip.size.width).next_down()),
                local.y.clamp(bottom, top.next_down()),
            );
            let delta = (local.x - projected.x).hypot(local.y - projected.y);
            if delta < distance {
                nearest = Some(projected);
                distance = delta;
            }
        }
        self.require(current)?;
        Ok(nearest) // Visit existing bounded line metadata only, without Source/GCB reads or another directory.
    }

    pub fn hit(
        &self,
        local: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedViewportHit> {
        self.require(current)?;
        if !point(local) || !inside(local, self.clip()) {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        let line = self
            .lines
            .iter()
            .find(|line| {
                local.y >= line.frame.origin.y
                    && local.y < line.frame.origin.y + line.frame.size.height
            })
            .ok_or(NativeMarkdownComposedViewportError::Pending)?;
        if line.handle.facts().kind == omni_markdown_core::MarkdownComposedLineKind::TableRow {
            return Err(NativeMarkdownComposedViewportError::Pending);
        }
        // Preflight complete line work using directory ranks; insufficient budget cannot publish the nearest position from a scanned prefix.
        let mut count = 0usize;
        {
            for part in &line.parts {
                let Some(range) = self.part_grapheme_range(part)? else {
                    continue; // There may be no editable boundary between shaping seams; zero graphemes do not imply missing Source.
                };
                let mut reader = self
                    .output
                    .try_borrow_mut()
                    .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
                let start = reader
                    .grapheme_ordinal(*range.start())?
                    .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?;
                let end = reader
                    .grapheme_ordinal(*range.end())?
                    .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?;
                count = count
                    .checked_add(
                        end.checked_sub(start)
                            .and_then(|value| value.checked_add(1))
                            .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?,
                    )
                    .ok_or(NativeMarkdownComposedViewportError::Pending)?;
                if count > HIT_BOUNDARIES {
                    return Err(NativeMarkdownComposedViewportError::Pending);
                }
            }
        }
        let mut answer: Option<NativeMarkdownComposedViewportHit> = None;
        let mut distance = f64::INFINITY;
        // Compare only after an actual caret produced answer; equal distances at a character midpoint are not coincident visual positions.
        // This coordinate belongs to one bounded scan, without Source ownership, successor Input, or a cross-call geometry cache.
        let mut primary_caret_x = 0.0;
        let mut primary_inside_part = false;
        // Compare actual bounded grapheme carets without assuming monotone ligature GDEF or interpolating by Source byte length.
        for part in &line.parts {
            let Some(range) = self.part_grapheme_range(part)? else {
                continue;
            };
            let mut position = *range.start();
            loop {
                let caret = composed_part_caret(
                    &self.output,
                    &part.handle,
                    &self.palette[part.font],
                    position,
                    line.baseline,
                    &|| self.require(current).is_ok(),
                );
                self.require(current)?; // Original Source failure takes priority over boolean revocation from geometry callbacks.
                let caret = caret?;
                let affinity = if position == part.handle.facts().source_utf16.end {
                    omni_markdown_core::MarkdownComposedLineAffinity::Upstream
                } else {
                    omni_markdown_core::MarkdownComposedLineAffinity::Downstream
                }; // Use actual original part boundaries, not text direction/average advance to guess hidden-syntax Source affinity.
                let facts = part.handle.facts();
                let part_start = line.baseline.x + facts.origin_x_26_6 as f64 / 64.0;
                let part_end = part_start + facts.x_advance_26_6 as f64 / 64.0;
                let inside_part =
                    local.x >= part_start.min(part_end) && local.x < part_start.max(part_end);
                let delta = (caret.baseline.x - local.x).abs();
                if delta < distance {
                    // A closer visual position replaces the entire candidate so obsolete Bidi ambiguity cannot contaminate it.
                    distance = delta;
                    primary_caret_x = caret.baseline.x;
                    primary_inside_part = inside_part;
                    answer = Some(NativeMarkdownComposedViewportHit {
                        source_utf16: position,
                        source_affinity: affinity,
                        other_source_affinity: None,
                        other_source_utf16: None,
                        line_ordinal: line.handle.facts().ordinal,
                    });
                } else if delta == distance
                    && let Some(answer) = &mut answer
                    && answer.source_utf16 == position
                    && inside_part
                    && !primary_inside_part
                {
                    // For two sides of one source caret, prefer the part advance span containing the actual hit and update visual position together.
                    // If primary moves from another equally distant X, old secondary belongs to that old X and cannot imply coincidence at the new position.
                    // An affinity change at the same X retains its actual other logical candidate, including hidden-syntax edge facts.
                    if primary_caret_x != caret.baseline.x {
                        answer.other_source_affinity = None;
                        answer.other_source_utf16 = None;
                    }
                    answer.source_affinity = affinity;
                    primary_caret_x = caret.baseline.x;
                    primary_inside_part = true;
                } else if delta == distance
                    && primary_caret_x == caret.baseline.x
                    && let Some(answer) = &mut answer
                    && answer.source_utf16 != position
                {
                    // Only distinct actual endpoints sharing X create logical ambiguity; equal-distance different-X ties retain the stable primary,
                    // without width interpolation, invented direction, or rejecting valid character midpoints in the outer layer.
                    if answer
                        .other_source_utf16
                        .is_some_and(|other| other != position)
                    {
                        return Err(NativeMarkdownComposedViewportError::Pending);
                    } // Several coincident zero-advance graphemes must not lose intermediate candidates; wait for complete affinity production.
                    answer.other_source_affinity = Some(affinity);
                    answer.other_source_utf16 = Some(position); // Retain Bidi-boundary ambiguity without claiming a selection anchor/head.
                }
                if position == *range.end()
                    || part.handle.facts().kind == MarkdownComposedLinePartKind::ParagraphBreak
                    || part.handle.facts().kind == MarkdownComposedLinePartKind::FoldedSection
                {
                    break;
                } // ParagraphBreak starts at this line's final caret and ends on the next visual line; zero-advance CRLF cannot invent Bidi ambiguity.
                // Other parts visit original grapheme endpoints and retain same-X candidates without guessing across font/direction directories.
                let next = self.next_boundary(position)?;
                if next <= position || next > *range.end() {
                    return Err(NativeMarkdownComposedViewportError::InvalidRange);
                }
                position = next;
            }
        }
        self.require(current)?;
        answer.ok_or(NativeMarkdownComposedViewportError::Pending)
    }

    /// Stream visible rectangles for a logical selection through original visual parts, without a complete selection/grapheme/text Vec.
    /// Visitors may draw into an unpublished candidate; any error discards that complete candidate, never publishes a successful prefix.
    ///
    /// # Errors
    /// Reject the operation for bounds, Source/font/Scene, or visitor failure; offscreen portions produce no rectangles.
    pub fn selection_rects(
        &self,
        selection: Range<usize>,
        current: &dyn Fn() -> bool,
        mut visitor: impl FnMut(NSRect) -> Result<()>,
    ) -> Result<usize> {
        self.require(current)?;
        if selection.start > selection.end || selection.end > self.utf16_len() {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        let mut count = 0usize;
        // Each prepared part supplies at most one span; the outer caller bounds whole-frame work across finite multiple ranges.
        for line in &self.lines {
            if !self.visible(line.frame) {
                continue;
            }
            for part in &line.parts {
                let facts = part.handle.facts();
                if matches!(
                    facts.kind,
                    MarkdownComposedLinePartKind::FoldedSection
                        | MarkdownComposedLinePartKind::TableRow
                ) {
                    continue; // Hidden regions have no visible glyph/selection rectangle; their UTF-16 length cannot authorize scanning internal graphemes.
                }
                let start = selection.start.max(facts.source_utf16.start);
                let end = selection.end.min(facts.source_utf16.end);
                if start >= end {
                    continue;
                } // This visual part has no selected range; rounding cannot manufacture additional highlight.
                // Preserve complete original graphemes, then intersect actual paint items; shaping seams affect envelopes without splitting selection.
                let actual = self.boundary(start, false)?.max(facts.source_utf16.start)
                    ..self.boundary(end, true)?.min(facts.source_utf16.end);
                let rect = self.part_rect(line, part, actual, current)?;
                let Some(rect) = clip_selection(rect, self.clip()) else {
                    continue;
                };
                self.require(current)?;
                visitor(rect)?; // Geometry returned its reader borrow before the outer Native paint/callback.
                self.require(current)?;
                count += 1;
            }
        }
        Ok(count)
    }

    fn part_for_position(&self, offset: usize) -> Result<(&Line, &Part)> {
        for line in &self.lines {
            for part in &line.parts {
                let range = &part.handle.facts().source_utf16;
                if range.contains(&offset)
                    && !matches!(
                        part.handle.facts().kind,
                        MarkdownComposedLinePartKind::FoldedSection
                            | MarkdownComposedLinePartKind::TableRow
                    )
                {
                    return Ok((line, part));
                }
            }
        }
        Err(NativeMarkdownComposedViewportError::Pending) // This path locates a nonempty first scalar only; caret lookup separately uses actual wrap affinity.
    }

    pub(super) fn boundary(&self, offset: usize, following: bool) -> Result<usize> {
        let mut reader = self
            .output
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        if reader.grapheme_ordinal(offset)?.is_some() {
            return Ok(offset);
        } // Keep an existing original boundary rather than expand an already aligned range to a neighboring grapheme.
        let query = if following {
            MarkdownGraphemeQuery::Next
        } else {
            MarkdownGraphemeQuery::Previous
        };
        match reader.grapheme_query(offset, query)? {
            MarkdownGraphemeAnswer::Known(Some(boundary)) => Ok(boundary.utf16),
            _ => Err(NativeMarkdownComposedViewportError::InvalidRange), // Unknown/missing grapheme boundaries cannot be guessed from adjacent UTF-16 positions.
        }
    }
    fn next_boundary(&self, offset: usize) -> Result<usize> {
        let mut reader = self
            .output
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        match reader.grapheme_query(offset, MarkdownGraphemeQuery::Next)? {
            MarkdownGraphemeAnswer::Known(Some(boundary)) => Ok(boundary.utf16),
            _ => Err(NativeMarkdownComposedViewportError::InvalidRange), // Actual EOF or a missing directory cannot manufacture a next grapheme.
        }
    }

    /// Use the original directory for first/last grapheme boundaries inside this part, without invented boundaries or a per-grapheme cache.
    fn part_grapheme_range(&self, part: &Part) -> Result<Option<std::ops::RangeInclusive<usize>>> {
        let range = &part.handle.facts().source_utf16;
        let first = self.boundary(range.start, true)?;
        let last = self.boundary(range.end, false)?;
        Ok((first <= last).then_some(first..=last))
    }
    pub(super) fn part_rect(
        &self,
        line: &Line,
        part: &Part,
        selection: Range<usize>,
        current: &dyn Fn() -> bool,
    ) -> Result<NSRect> {
        let actual = &part.handle.facts().source_utf16;
        if selection.start < actual.start || selection.end > actual.end {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        let guarded = || self.require(current).is_ok(); // Native GDEF receives a boolean guard; the outer layer still preserves actual Source error priority.
        let position = |at| {
            let edge = if at == actual.start {
                Some(ComposedPartSourceEdge::Start)
            } else if at == actual.end {
                Some(ComposedPartSourceEdge::End)
            } else {
                None
            };
            if let Some(edge) = edge {
                composed_part_extent_edge(
                    &self.output,
                    &part.handle,
                    &self.palette[part.font],
                    edge,
                    line.baseline,
                    &guarded,
                )
            } else {
                // Interior positions follow the public caret contract using original GCB/GDEF, without scalar interpolation.
                composed_part_caret(
                    &self.output,
                    &part.handle,
                    &self.palette[part.font],
                    at,
                    line.baseline,
                    &guarded,
                )
                .map(|caret| caret.baseline)
            }
        };
        let start = position(selection.start);
        self.require(current)?;
        let start = start?;
        let end = position(selection.end);
        self.require(current)?;
        let end = end?;
        // Use min/max actual visual X for RTL endpoints and measured line height, never width inferred from UTF-16 length.
        let rect = NSRect::new(
            NSPoint::new(start.x.min(end.x), line.baseline.y - line.descent),
            NSSize::new((end.x - start.x).abs(), line.ascent + line.descent),
        );
        if !point(rect.origin)
            || !rect.size.width.is_finite()
            || !rect.size.height.is_finite()
            || rect.size.height <= 0.0
        {
            return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
        }
        Ok(rect)
    }
}

/// Click/hover share the actual LINK-label advance envelope; exposed syntax/zero advances gain no extra activation area.
/// Negative advance swaps left/right only, retaining actual line font height without UTF-16 counts or average glyph widths.
fn link_part_rect(line: &Line, part: &Part) -> Option<NSRect> {
    let facts = part.handle.facts();
    let styles = facts.presentation;
    if !styles.contains(MarkdownPresentationStyles::LINK)
        || styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        || styles.contains(MarkdownPresentationStyles::MARKER)
    {
        return None;
    }
    let start = line.baseline.x + facts.origin_x_26_6 as f64 / 64.0;
    let end = start + facts.x_advance_26_6 as f64 / 64.0;
    Some(NSRect::new(
        NSPoint::new(start.min(end), line.baseline.y - line.descent),
        NSSize::new((end - start).abs(), line.ascent + line.descent),
    ))
}

fn inside(point: NSPoint, clip: NSRect) -> bool {
    point.x >= clip.origin.x
        && point.x < clip.origin.x + clip.size.width
        && point.y >= clip.origin.y
        && point.y < clip.origin.y + clip.size.height
}

pub(super) fn clip_selection(rect: NSRect, clip: NSRect) -> Option<NSRect> {
    let left = rect.origin.x.max(clip.origin.x);
    let bottom = rect.origin.y.max(clip.origin.y);
    let right = (rect.origin.x + rect.size.width).min(clip.origin.x + clip.size.width);
    let top = (rect.origin.y + rect.size.height).min(clip.origin.y + clip.size.height);
    if top <= bottom || right < left || (rect.size.width > 0.0 && right == left) {
        return None;
    }
    Some(NSRect::new(
        NSPoint::new(left, bottom),
        NSSize::new(right - left, top - bottom),
    ))
}
