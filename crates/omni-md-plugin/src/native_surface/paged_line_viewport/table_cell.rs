//! Read-only Cells consume actual display Source and map public positions back to canonical; the sole original Cell session still owns editing.

use super::*;
use crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGContext;
use omni_markdown_core::MarkdownComposedLineAffinity;

/// The inner window exposes no display-local input and retains no document, Body, editor, or additional font catalog.
pub struct NativeMarkdownComposedTableCell {
    window: Rc<NativeMarkdownComposedViewport>,
}

impl NativeMarkdownComposedTableCell {
    /// Hover consumes actual display-window geometry without canonical mapping or copied link targets.
    /// window clips to content_frame/outer viewport intersection; hand cursors cannot extend into Cell padding/neighbors.
    pub(in crate::native_surface) fn visit_link_cursor_rects(
        &self,
        current: &dyn Fn() -> bool,
        visit: &mut dyn FnMut(NSRect),
    ) -> Result<()> {
        self.window.visit_link_cursor_rects(current, visit)
    }

    /// Read-only drag projects through actual display lines; the same display mapping below still produces canonical hits.
    pub(in crate::native_surface) fn project_drag_point(
        &self,
        local: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<NSPoint>> {
        self.window.project_drag_point(local, current)
    }

    /// The original table solver determines actual column width; same-row content_frame clips only to the outer viewport intersection.
    ///
    /// # Errors
    /// Reject invalid original publication/Scene, font, Source, or actual line output; offscreen Cells create no placeholder window.
    pub fn prepare(
        output: Rc<RefCell<MarkdownComposedLines>>,
        key: NativeMarkdownPagedSceneKey,
        display: Rc<MarkdownTableCellDisplaySource>,
        palette: Vec<Rc<NativeMarkdownLineFont>>,
        handles: impl IntoIterator<Item = impl Into<NativeMarkdownMeasuredLine>>,
        content_frame: NSRect,
        current: &dyn Fn() -> bool,
    ) -> Result<Self> {
        // Display Cells belong only to actual Body Preview; active drafts/Source mode cannot borrow another coordinate domain to consume them.
        if !key.request.target.is_body(&key.request.document)
            || key.presentation_mode != MarkdownPresentationMode::InlinePreview
        {
            return Err(NativeMarkdownComposedViewportError::InvalidCoverage);
        }
        if !rect(content_frame) {
            return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
        }
        let viewport = key.request.viewport;
        let left = viewport.origin.x.max(content_frame.origin.x);
        let bottom = viewport.origin.y.max(content_frame.origin.y);
        let right = (viewport.origin.x + viewport.size.width)
            .min(content_frame.origin.x + content_frame.size.width);
        let top = (viewport.origin.y + viewport.size.height)
            .min(content_frame.origin.y + content_frame.size.height);
        if right <= left || top <= bottom {
            return Err(NativeMarkdownComposedViewportError::Pending);
        } // An actually offscreen Cell stays in its original spool, without full-Cell pixels or an empty Ready claim.
        let alignment = {
            // Check alignment against original document/root and return the borrow before Native getters; an old Cell cannot renew authority.
            let original = key
                .request
                .document
                .upgrade()
                .ok_or(NativeMarkdownComposedViewportError::Revoked)?;
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
            display
                .canonical_raw_cell()
                .alignment_for(&document)
                .map_err(NativeMarkdownComposedViewportError::Table)?
        };
        let domain = LayoutDomain::TableCell {
            display,
            alignment,
            width: content_frame.size.width,
            clip: NSRect::new(
                NSPoint::new(left, bottom),
                NSSize::new(right - left, top - bottom),
            ),
            identity: Rc::new(()),
        };
        let first_top = NSPoint::new(
            content_frame.origin.x,
            content_frame.origin.y + content_frame.size.height,
        );
        let window = NativeMarkdownComposedViewport::prepare_domain(
            output, key, domain, palette, handles, first_top, current,
        )?;
        Ok(Self {
            window: Rc::new(window),
        })
    }

    /// Borrow the same sealed display Source without reparsing source text, creating another display copy, or reconstructing fonts.
    fn display(&self) -> &MarkdownTableCellDisplaySource {
        match &self.window.domain {
            LayoutDomain::TableCell { display, .. } => display,
            LayoutDomain::Body => {
                unreachable!("only the sealed table-cell constructor builds this owner")
            }
        }
    }

    /// Return both actual canonical hit candidates without silently discarding a Bidi side during mapping.
    pub fn hit(
        &self,
        point: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedViewportHit> {
        let hit = self.window.hit(point, current)?;
        self.canonical_hit(hit, current)
    }

    /// Word bias follows actual display-window grapheme geometry then maps to original raw Cell, without reading/sanitizing the entire cell.
    pub(in crate::native_surface) fn word_hit(
        &self,
        point: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<(NativeMarkdownComposedViewportHit, bool)> {
        let (hit, backward_bias) = self.window.word_hit(point, current)?;
        Ok((self.canonical_hit(hit, current)?, backward_bias))
    }

    /// Ordinary/word hits share this actual display-to-canonical map; primary/secondary candidates retain their own part affinities.
    fn canonical_hit(
        &self,
        hit: NativeMarkdownComposedViewportHit,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedViewportHit> {
        let source_utf16 = self.map_to_canonical(hit.source_utf16, hit.source_affinity)?;
        let other_source_utf16 = hit
            .other_source_utf16
            .zip(hit.other_source_affinity)
            .map(|(value, affinity)| self.map_to_canonical(value, affinity))
            .transpose()?;
        self.window.require(current)?;
        Ok(NativeMarkdownComposedViewportHit {
            source_utf16,
            source_affinity: hit.source_affinity,
            other_source_affinity: hit.other_source_affinity,
            other_source_utf16,
            line_ordinal: hit.line_ordinal,
        })
    }

    /// Original publication still resolves actual link targets; display style supplies only the canonical range of the visible label envelope.
    pub fn link_part_at(
        &self,
        point: NSPoint,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<Range<usize>>> {
        let range = self.window.link_part_at(point, current)?;
        let mapped = range
            .map(|range| -> Result<Range<usize>> {
                Ok(
                    self.map_to_canonical(range.start, MarkdownComposedLineAffinity::Downstream)?
                        ..self
                            .map_to_canonical(range.end, MarkdownComposedLineAffinity::Upstream)?,
                )
            })
            .transpose()?;
        self.window.require(current)?;
        Ok(mapped)
    }

    /// Find/IME uses original canonical positions/explicit wrap affinity, mapping hidden markers through the same on-disk directory.
    pub fn first_rect(
        &self,
        range: NSRange,
        affinity: NativeMarkdownPagedHeadAffinity,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedCharacterRect> {
        self.window.require(current)?;
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownComposedViewportError::InvalidRange)?;
        let start = self.map_to_display(range.location, affinity)?;
        let end = self.map_to_display(end, affinity)?;
        if end < start {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        let displayed_range = NSRange::new(start, end - start);
        let value = self
            .window
            .first_rect_with_affinity(displayed_range, affinity, current)?;
        if range.length == 0 && value.actual_range == displayed_range {
            // Both sides of hidden emphasis/code markers may map to one actual display caret. The original canonical request already
            // maps exactly to that empty range; if the lower layer returns precisely it, retain the original endpoint. Mapping back would cross hidden markers
            // according to affinity, moving a completed word endpoint beyond closing syntax and incorrectly preventing visibility certification.
            self.window.require(current)?;
            return Ok(NativeMarkdownPagedCharacterRect {
                rect: value.rect,
                actual_range: range,
            });
        } // Nonempty/different returned ranges still map back normally; hidden characters/adjacent glyphs cannot stand in for the original request.
        let start = self.map_to_canonical(
            value.actual_range.location,
            if value.actual_range.length == 0 && affinity.is_upstream() {
                MarkdownComposedLineAffinity::Upstream
            } else {
                MarkdownComposedLineAffinity::Downstream
            },
        )?;
        let end = self.map_to_canonical(
            value.actual_range.location + value.actual_range.length,
            if value.actual_range.length == 0 && !affinity.is_upstream() {
                MarkdownComposedLineAffinity::Downstream
            } else {
                MarkdownComposedLineAffinity::Upstream
            },
        )?;
        self.window.require(current)?;
        Ok(NativeMarkdownPagedCharacterRect {
            rect: value.rect,
            actual_range: NSRange::new(start, end - start),
        })
    }

    /// Actual click activation borrows the same original Cell authority; Core owns row/column/range/root facts without another coordinate copy.
    pub(in crate::native_surface) fn canonical_cell_source(
        &self,
    ) -> &omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableCellSource {
        self.display().canonical_raw_cell()
    }

    /// Mode changes take source anchors from actual visible glyph parts, not hidden table delimiters or offscreen line starts.
    pub(in crate::native_surface) fn first_visible_source(
        &self,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<usize>> {
        self.window.require(current)?;
        let position = self
            .window
            .lines
            .iter()
            .filter(|line| self.window.visible(line.frame))
            .flat_map(|line| &line.parts)
            .find(|part| {
                part.handle.facts().kind == omni_markdown_core::MarkdownComposedLinePartKind::Glyphs
                    && !part.handle.facts().source_utf16.is_empty()
            })
            .map(|part| part.handle.facts().source_utf16.start);
        let canonical = position
            .map(|position| {
                self.map_to_canonical(position, MarkdownComposedLineAffinity::Downstream)
            })
            .transpose()?;
        self.window.require(current)?;
        Ok(canonical)
    }

    /// Live Find reads sealed intersecting soft lines only; a large Cell's offscreen prefix cannot be rescanned through its full raw range.
    pub(in crate::native_surface) fn visible_canonical_range(
        &self,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<Range<usize>>> {
        self.window.require(current)?;
        let mapped =
            self.visible_display_range()
                .map(|range| -> Result<Range<usize>> {
                    Ok(self
                        .map_to_canonical(range.start, MarkdownComposedLineAffinity::Downstream)?
                        ..self
                            .map_to_canonical(range.end, MarkdownComposedLineAffinity::Upstream)?)
                })
                .transpose()?;
        self.window.require(current)?;
        Ok(mapped.filter(|range| !range.is_empty()))
    }

    /// Find consumes actual display-directory run fragments; the canonical envelope orders them but cannot restore hidden syntax to matching.
    /// Use the whole viewport's remaining input budget and return a borrowed iterator, not a collection of all large-Cell Source records.
    pub(in crate::native_surface) fn visible_find_ranges(
        &self,
        maximum_units: usize,
        current: &dyn Fn() -> bool,
    ) -> Result<Option<omni_markdown_core::MarkdownTableCellVisibleSourceRanges<'_>>> {
        self.window.require(current)?;
        let Some(range) = self.visible_display_range() else {
            return Ok(None);
        };
        let original = self
            .window
            .key
            .request
            .document
            .upgrade()
            .ok_or(NativeMarkdownComposedViewportError::Revoked)?;
        let document = original
            .try_borrow()
            .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
        let ranges = self
            .display()
            .visible_canonical_ranges_for(&document, range, maximum_units)
            .map_err(NativeMarkdownComposedViewportError::Table)?;
        drop(document);
        self.window.require(current)?;
        Ok(Some(ranges))
    }

    /// Actual visible soft lines share a contiguous display domain; inspect sealed line metadata without reading source bytes.
    fn visible_display_range(&self) -> Option<Range<usize>> {
        let mut range: Option<Range<usize>> = None;
        for line in self
            .window
            .lines
            .iter()
            .filter(|line| self.window.visible(line.frame))
        {
            let actual = line.handle.facts().source_utf16.clone();
            if actual.is_empty() {
                continue;
            }
            if let Some(range) = &mut range {
                range.end = actual.end;
            } else {
                range = Some(actual);
            }
        }
        range
    }

    /// Read original canonical Cell range only from its sealed structural authority, without holding borrows across Native/caller callbacks.
    pub(in crate::native_surface) fn canonical_range(
        &self,
        current: &dyn Fn() -> bool,
    ) -> Result<Range<usize>> {
        self.window.require(current)?;
        let range = {
            let owner = self
                .window
                .key
                .request
                .document
                .upgrade()
                .ok_or(NativeMarkdownComposedViewportError::Revoked)?;
            let document = owner
                .try_borrow()
                .map_err(|_| NativeMarkdownComposedViewportError::Busy)?;
            self.canonical_cell_source()
                .canonical_range_for(&document)
                .map_err(NativeMarkdownComposedViewportError::Table)?
                .clone()
        };
        self.window.require(current)?;
        Ok(range)
    }

    /// Intersect Body selection with actual raw Cell before mapping into display, without passing whole-document ranges into local input.
    pub fn selection_rects(
        &self,
        selection: Range<usize>,
        current: &dyn Fn() -> bool,
        visitor: impl FnMut(NSRect) -> Result<()>,
    ) -> Result<usize> {
        self.window.require(current)?;
        if selection.start > selection.end {
            return Err(NativeMarkdownComposedViewportError::InvalidRange);
        }
        let canonical = self.canonical_range(current)?;
        let start = selection.start.max(canonical.start);
        let end = selection.end.min(canonical.end);
        if start >= end {
            return Ok(0);
        }
        let start = self.map_to_display(start, NativeMarkdownPagedHeadAffinity::Downstream)?;
        let end = self.map_to_display(end, NativeMarkdownPagedHeadAffinity::Upstream)?;
        self.window.selection_rects(start..end, current, visitor)
    }

    /// Check actual derived-directory health before outer revocation; another Cell's cancellation cannot hide this Cell's I/O failure.
    pub(in crate::native_surface) fn source_health(&self) -> Result<()> {
        self.display()
            .source()
            .byte_offset_for_utf16(0)
            .map(|_| ())
            .map_err(|error| MarkdownLineComposeError::Source(error).into())
    }

    /// Pixel reuse still validates original output, bounded fonts, and publication without relayout or extra resource capture.
    pub fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<()> {
        self.window.validate_ready(current)
    }

    /// Create one actual Cell's code-background cursor without Native calls or another context/Source owner.
    pub(in crate::native_surface) fn code_background_cursor(
        &self,
    ) -> NativeMarkdownComposedViewportDecorationCursor {
        NativeMarkdownComposedViewportDecorationCursor::backgrounds()
    }

    /// Take endpoints from the original Cell; across quanta retain integers/original Scene range references, not selected text.
    pub(in crate::native_surface) fn selection_cursor(
        &self,
        selections: &[Range<usize>],
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedViewportDecorationCursor> {
        Ok(NativeMarkdownComposedViewportDecorationCursor::selections(
            self.canonical_range(current)?,
            selections,
        ))
    }

    /// Background/selection consumes the outer step's remaining metadata, never a fresh full budget per Cell.
    pub(in crate::native_surface) fn advance_decorations(
        &self,
        cursor: &mut NativeMarkdownComposedViewportDecorationCursor,
        context: &CGContext,
        selections: &[Range<usize>],
        selection_color: [f64; 4],
        budget: usize,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportDecorationStep,
        NativeMarkdownComposedViewportPaintError,
    > {
        self.window.advance_decorations(
            cursor,
            context,
            selections,
            selection_color,
            budget,
            current,
        )
    }

    /// The same original window paints code backgrounds only within the actual visible Cell clip.
    pub fn paint_code_backgrounds(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<(), NativeMarkdownComposedViewportPaintError> {
        self.window.paint_code_backgrounds(context, current)
    }

    /// Explicit synchronous comparison retains its total glyph bound; production uses resumable CPU sessions without truncating visible output.
    pub(in crate::native_surface) fn paint(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportPaintReceipt,
        NativeMarkdownComposedViewportPaintError,
    > {
        self.window.paint(context, current)
    }

    /// Use the original bounded paint cursor and additionally bind its receipt to this Cell window, not completed outer Body paint.
    pub fn begin_paint(
        &self,
        context: CFRetained<CGContext>,
        current: &dyn Fn() -> bool,
    ) -> std::result::Result<
        NativeMarkdownComposedViewportPaintSession,
        NativeMarkdownComposedViewportPaintError,
    > {
        self.window.clone().begin_paint(context, current)
    }

    /// Another Cell/Body receipt sharing outer key/context cannot claim completion for this Cell.
    #[must_use]
    pub fn permits_completion(
        &self,
        completion: &NativeMarkdownComposedViewportPaintCompletion,
        context: &CGContext,
    ) -> bool {
        completion.permits_cell(&self.window, context)
    }

    /// The sole row solver accumulates heights from actual font/wrapping results, not character-count estimates.
    #[must_use]
    pub fn line_frame(&self, ordinal: usize) -> Option<NSRect> {
        self.window.line_frame(ordinal)
    }

    /// Preserve actual directory read failures as Source errors, not ordinary offscreen/revocation results.
    fn map_to_canonical(
        &self,
        position: usize,
        affinity: MarkdownComposedLineAffinity,
    ) -> Result<usize> {
        self.display()
            .display_to_canonical(position, affinity)
            .map_err(|error| MarkdownLineComposeError::Source(error).into())
    }

    /// Out-of-range positions do not belong to this Cell; surrogate splits/directory faults still return explicitly from original display Source.
    fn map_to_display(
        &self,
        position: usize,
        affinity: NativeMarkdownPagedHeadAffinity,
    ) -> Result<usize> {
        let affinity = if affinity.is_upstream() {
            MarkdownComposedLineAffinity::Upstream
        } else {
            MarkdownComposedLineAffinity::Downstream
        };
        self.display()
            .canonical_to_display(position, affinity)
            .map_err(|error| {
                NativeMarkdownComposedViewportError::Composed(MarkdownLineComposeError::Source(
                    error,
                ))
            })?
            .ok_or(NativeMarkdownComposedViewportError::InvalidRange)
    }
}
