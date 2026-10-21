//! Consumes input geometry and paint from sealed original windows; no method acquires another Source or advances layout.

use std::{
    cell::{OnceCell, RefCell},
    ops::Range,
    rc::{Rc, Weak},
};

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGColorSpace, CGContext, kCGColorSpaceSRGB};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use omni_markdown_core::{
    MarkdownComposedLines, MarkdownGraphemeAnswer, MarkdownGraphemeQuery, MarkdownTextSelection,
};

use super::producer::scene_error;
use super::{
    NativeMarkdownPagedBody as Owner, NativeMarkdownPagedBodyColors, NativeMarkdownPagedSceneKey,
    NativeMarkdownPagedTextScene, ViewError as Error,
};
use crate::native_surface::{
    gutter::paged::GutterRow,
    paged_input::{NativeMarkdownBidiCaretSide, NativeMarkdownVerticalGoal},
    paged_line_viewport::{
        NativeMarkdownComposedViewport, NativeMarkdownComposedViewportError as ViewportError,
        NativeMarkdownComposedViewportPaintBudget, NativeMarkdownComposedViewportPaintCause,
        NativeMarkdownComposedViewportPaintError, NativeMarkdownComposedViewportPaintOutcome,
        NativeMarkdownComposedViewportPaintSession,
    },
    paged_text_view::{
        NativeMarkdownPagedCharacterRect, NativeMarkdownPagedEditCommand as Edit,
        NativeMarkdownPagedEditPlan, NativeMarkdownPagedHeadAffinity as Affinity,
        NativeMarkdownPagedNavigation as Nav, NativeMarkdownPagedNavigationContext,
        NativeMarkdownPagedNavigationGeometry, NativeMarkdownPagedNavigationTarget,
        NativeMarkdownPagedSceneNonce, NativeMarkdownPagedScenePaintBudget,
        NativeMarkdownPagedScenePaintCompletion, NativeMarkdownPagedScenePaintOutcome,
        NativeMarkdownPagedScenePaintSession, NativeMarkdownPagedScenePaintStep,
        NativeMarkdownPagedScenePaintWork, NativeMarkdownPagedTextViewDelegate,
    },
    paged_viewport::NativeMarkdownPagedViewportGeometry,
};

type Result<T> = std::result::Result<T, Error>;
mod grid;
mod paint;
use grid::GridCharacterGeometry;
pub(super) use grid::{GridCellContent, GridCellPublication, GridRowPublication};
use paint::Paint;

pub(in crate::native_surface) struct Scene {
    key: NativeMarkdownPagedSceneKey,
    _nonce: NativeMarkdownPagedSceneNonce,
    geometry: NativeMarkdownPagedViewportGeometry,
    window: Option<Rc<NativeMarkdownComposedViewport>>,
    output: Option<Rc<RefCell<MarkdownComposedLines>>>,
    grids: Box<[GridRowPublication]>,
    owner: Weak<Owner>,
    ranges: Box<[Range<usize>]>,
    paragraphs: Box<[Range<usize>]>,
    folded_ends: Box<[Option<usize>]>,
    simple_ltr: bool,
    navigation: NativeMarkdownPagedNavigationGeometry,
    colors: NativeMarkdownPagedBodyColors,
    head_affinity: Option<Affinity>,
    first_ordinal: usize,
    /// Bounded ranges from the publication's same Input ticket; the primary range cannot stand in for every selection.
    selections: Box<[Range<usize>]>,
    /// Bounded current-query matches in actual visible Source domains, without bar, query, or Native strong ownership.
    find_highlights: Box<[Range<usize>]>,
    /// Canonical logical line for the original Source single directed selection; neither UBA runs nor soft lines authorize it.
    active_line: Option<Range<usize>>,
    /// Bounded line numbers from the same Source publication; Producer alone intersects actual soft-line geometry with canonical logical lines.
    gutter: Option<Box<[GutterRow]>>,
    // Scene publication creates no Native resource. The first paint captures one named color space, owned until this Scene drops.
    // None records actual creation failure; substituting DeviceRGB would silently paint another color.
    paint_space: OnceCell<Option<CFRetained<CGColorSpace>>>,
}

/// Bounded facts from one completed preparation. Only Body constructs this; bare ranges cannot manufacture Ready.
pub(super) struct Publication {
    pub key: NativeMarkdownPagedSceneKey,
    pub nonce: NativeMarkdownPagedSceneNonce,
    pub geometry: NativeMarkdownPagedViewportGeometry,
    pub window: Option<Rc<NativeMarkdownComposedViewport>>,
    pub output: Option<Rc<RefCell<MarkdownComposedLines>>>,
    pub grids: Box<[GridRowPublication]>,
    pub owner: Weak<Owner>,
    pub ranges: Box<[Range<usize>]>,
    pub paragraphs: Box<[Range<usize>]>,
    pub folded_ends: Box<[Option<usize>]>,
    pub simple_ltr: bool,
    pub width_key: i64,
    pub layout_epoch: u64,
    pub colors: NativeMarkdownPagedBodyColors,
    pub head_affinity: Option<Affinity>,
    pub first_ordinal: usize,
    pub selections: Box<[Range<usize>]>,
    pub find_highlights: Box<[Range<usize>]>,
    pub active_line: Option<Range<usize>>,
    pub gutter: Option<Box<[GutterRow]>>,
}

impl Scene {
    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    /// Expose bounded geometry only under the original frame's live weak identity; the short oracle gains no lasting Scene ownership.
    pub(super) fn probe_line_geometry(
        &self,
    ) -> Result<Vec<crate::native_surface::preview_paragraph::PreviewLineObservation>> {
        self.require()?;
        self.text_window()?
            .probe_line_geometry(&|| self.current())
            .map_err(scene_error)
    }

    pub(super) fn new(publication: Publication) -> Self {
        let Publication {
            key,
            nonce,
            geometry,
            window,
            output,
            grids,
            owner,
            ranges,
            paragraphs,
            folded_ends,
            simple_ltr,
            width_key,
            layout_epoch,
            colors,
            head_affinity,
            first_ordinal,
            selections,
            find_highlights,
            active_line,
            gutter,
        } = publication;
        let navigation = NativeMarkdownPagedNavigationGeometry {
            source: key.source.clone(),
            width_key,
            layout_epoch,
        };
        Self {
            key,
            _nonce: nonce,
            geometry,
            window,
            output,
            grids,
            owner,
            ranges,
            paragraphs,
            folded_ends,
            simple_ltr,
            navigation,
            colors,
            head_affinity,
            first_ordinal,
            selections,
            find_highlights,
            active_line,
            gutter,
            paint_space: OnceCell::new(),
        }
    }

    /// Validate Source/Ready around actual color-space capture; creation failure remains a paint failure rather than a geometry error.
    fn prepare_paint_space(&self, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        let space = self.paint_space.get_or_init(|| {
            // SAFETY: The public sRGB name is statically valid; this Scene reuses one space rather than allocating for each color or glyph.
            CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        });
        self.validate_ready(current)?;
        if space.is_none() {
            return Err(Error::Raster(
                ViewportError::ColorSpaceUnavailable.to_string(),
            ));
        }
        Ok(())
    }

    /// Palette values are source sRGB components, requiring an explicit input space independently of the bitmap destination space.
    fn color(&self, context: &CGContext, color: [f64; 4]) {
        let space = self
            .paint_space
            .get()
            .and_then(Option::as_deref)
            .expect("Scene 实际绘图先取得命名 sRGB 空间");
        CGContext::set_fill_color_space(Some(context), Some(space));
        // SAFETY: Named sRGB has three color components; this synchronous stack array adds one alpha as required by the public API.
        unsafe { CGContext::set_fill_color(Some(context), color.as_ptr()) };
    }

    /// Borders use the same source color space, avoiding convenience setters that switch input back to DeviceRGB.
    fn stroke_color(&self, context: &CGContext, color: [f64; 4]) {
        let space = self
            .paint_space
            .get()
            .and_then(Option::as_deref)
            .expect("Scene 实际绘图先取得命名 sRGB 空间");
        CGContext::set_stroke_color_space(Some(context), Some(space));
        // SAFETY: The stroke space is three-component sRGB; all four stack components remain valid until this call returns.
        unsafe { CGContext::set_stroke_color(Some(context), color.as_ptr()) };
    }

    /// Mode capture examines bounded sealed-window candidates only; an offscreen caret cannot substitute for a visible glyph.
    pub(super) fn mode_source_candidates(&self) -> impl Iterator<Item = Result<usize>> + '_ {
        let head = self.key.request.input.primary_range.location;
        let visible = self.window.as_ref().is_some_and(|window| {
            let source = window.source_range();
            source.start <= head && head <= source.end
        }) || self.grids.iter().any(|row| {
            let range = row.row.source_range();
            range.start <= head && head <= range.end
        });
        std::iter::once(head)
            .filter(move |_| visible)
            .map(Ok)
            .chain(self.ranges.iter().map(|range| Ok(range.start)))
            .chain(
                self.grids
                    .iter()
                    .flat_map(|row| row.cells.iter())
                    .filter_map(|cell| {
                        let GridCellContent::Composed(window) = &cell.content else {
                            return None;
                        };
                        match window.first_visible_source(&|| self.current()) {
                            Ok(value) => value.map(Ok),
                            Err(cause) => Some(Err(scene_error(cause))),
                        }
                    }),
            )
    }

    /// Gutter rows borrow this Scene's complete Source/Input/font/geometry authority, without new Ready state or text reads.
    pub(in crate::native_surface) fn gutter_rows(&self) -> Result<&[GutterRow]> {
        self.require()?;
        self.gutter.as_deref().ok_or(Error::Revoked)
    }

    /// Capture the actual caret line and target column using Native font geometry from this bounded window alone.
    pub(super) fn vertical_seed(
        &self,
        head: usize,
        context: &NativeMarkdownPagedNavigationContext,
        keep_goal: bool,
    ) -> Result<super::navigation::VerticalSeed> {
        self.require()?;
        if !context.geometry.same(&self.navigation) {
            return Err(Error::Revoked);
        }
        let caret = self
            .text_window()?
            .caret_geometry_with_side(
                head,
                context.head_affinity.unwrap_or(Affinity::Downstream),
                context.bidi_side,
                &|| self.current(),
            )
            .map_err(scene_error)?;
        if caret.secondary.is_some() {
            return Err(Error::UnsupportedCommand);
        }
        let bottom = self.key.request.virtual_viewport.document_origin.y
            + self.key.request.viewport.size.height
            - caret.line_frame.origin.y;
        let top = bottom - caret.line_frame.size.height;
        let old_goal = context
            .vertical_goal
            .filter(|goal| self.navigation.permits_goal(*goal));
        let x = old_goal.map_or(caret.primary.origin.x, |goal| goal.container_x());
        let goal = if keep_goal {
            old_goal.or_else(|| {
                NativeMarkdownVerticalGoal::new(
                    caret.primary.origin.x - caret.line_frame.origin.x,
                    caret.primary.origin.x,
                    self.navigation.width_key,
                    self.navigation.layout_epoch,
                )
            })
        } else {
            None
        }; // Page retains its existing current-column behavior instead of establishing a continuous Up/Down goal.
        self.require()?;
        Ok(super::navigation::VerticalSeed {
            ordinal: caret.line_ordinal,
            top,
            bottom,
            caret_y: self.key.request.virtual_viewport.document_origin.y
                + self.key.request.viewport.size.height
                - caret.primary.origin.y
                - caret.primary.size.height * 0.5,
            x,
            goal,
        })
    }

    /// Hit only the Producer-measured line actually scrolled into this window; never clamp a missing target to a window edge.
    pub(super) fn vertical_line_target(
        &self,
        ordinal: usize,
        x: f64,
        before: MarkdownTextSelection,
        extend: bool,
        goal: Option<NativeMarkdownVerticalGoal>,
    ) -> Result<NativeMarkdownPagedNavigationTarget> {
        self.require()?;
        let index = ordinal
            .checked_sub(self.first_ordinal)
            .filter(|index| *index < self.ranges.len())
            .ok_or(Error::Pending)?;
        let frame = self
            .text_window()?
            .line_frame(index)
            .ok_or(Error::InvalidGeometry)?;
        let viewport = self.key.request.viewport;
        let bottom = frame.origin.y.max(viewport.origin.y);
        let top =
            (frame.origin.y + frame.size.height).min(viewport.origin.y + viewport.size.height);
        if bottom >= top {
            return Err(Error::Pending);
        }
        let hit = self
            .text_window()?
            .hit(
                NSPoint::new(
                    x.clamp(
                        viewport.origin.x,
                        (viewport.origin.x + viewport.size.width).next_down(),
                    ),
                    bottom + (top - bottom) * 0.5,
                ),
                &|| self.current(),
            )
            .map_err(scene_error)?;
        if hit.line_ordinal != ordinal || hit.other_source_utf16.is_some() {
            return Err(Error::UnsupportedCommand);
        }
        let affinity = if hit.source_utf16 == self.ranges[index].end {
            Affinity::Upstream
        } else {
            Affinity::Downstream
        };
        self.require()?;
        Ok(NativeMarkdownPagedNavigationTarget {
            selection: MarkdownTextSelection {
                anchor_utf16: if extend {
                    before.anchor_utf16
                } else {
                    hit.source_utf16
                },
                head_utf16: hit.source_utf16,
            },
            head_affinity: Some(affinity),
            vertical_goal: goal,
            bidi_side: None,
        }) // Mixed Shift motions continue the same ticket's working anchor, not the uncommitted initial canonical anchor.
    }

    fn current(&self) -> bool {
        self.geometry.is_current()
            && self
                .owner
                .upgrade()
                .is_some_and(|owner| owner.scene_current(&self.key))
    }

    /// Borrow the original Input's sole selection slot; the same key/serial and scope must survive both sides of the operation.
    fn bidi_side(&self) -> Result<Option<NativeMarkdownBidiCaretSide>> {
        self.require()?;
        let owner = self.owner.upgrade().ok_or(Error::Revoked)?;
        let current = |epoch| owner.current(epoch) && self.current();
        let permit = owner.input.capture(owner.configuration.epoch, &current)?;
        if owner.input.state()? != self.key.request.input {
            return Err(Error::Revoked);
        }
        let side = owner.input.navigation_bidi_side(&permit, &current)?;
        self.require()?;
        Ok(side)
    }

    /// A Grid-only prefix has no text lines; text geometry stays Pending rather than inventing an empty text window.
    fn text_window(&self) -> Result<&Rc<NativeMarkdownComposedViewport>> {
        self.window.as_ref().ok_or(Error::Pending)
    }

    fn require(&self) -> Result<()> {
        self.validate_ready(&|| self.current())
    }

    fn selection(&self) -> Result<MarkdownTextSelection> {
        // A primary range alone does not define a directed multi-selection. Keep
        // the original Input's explicit direction requirement at this consumer.
        self.key
            .request
            .input
            .directed_selection()
            .ok_or(Error::InvalidRange)
    }

    fn line(&self, position: usize, affinity: Affinity) -> Result<usize> {
        // Soft-wrap seams belong to the requested side. Actual document EOF may
        // use the final range end, but a cached prefix end cannot gain EOF authority.
        self.ranges
            .iter()
            .position(|range| {
                if position == self.key.request.input.utf16_len {
                    position == range.end
                } else if affinity.is_upstream() && position != 0 {
                    range.start < position && position <= range.end
                } else {
                    range.start <= position && position < range.end
                }
            })
            .ok_or(Error::InvalidRange)
    }

    /// Canonical line coverage includes the full opaque range; within-line actions alone use its certified glyph/control visible end.
    fn visible_line_range(&self, line: usize) -> Range<usize> {
        let range = &self.ranges[line];
        range.start..self.folded_ends[line].unwrap_or(range.end)
    }

    fn boundary(&self, position: usize, next: bool) -> Result<usize> {
        // Query the original GCB directory under a short Core borrow. Release it
        // before require enters reentrant Native geometry validation again.
        self.require()?;
        let answer = {
            let mut output = self
                .output
                .as_ref()
                .ok_or(Error::Pending)?
                .try_borrow_mut()
                .map_err(|_| Error::Busy)?;
            output
                .grapheme_query(
                    position,
                    if next {
                        MarkdownGraphemeQuery::Next
                    } else {
                        MarkdownGraphemeQuery::Previous
                    },
                )
                .map_err(|cause| scene_error(cause.into()))?
        };
        self.require()?;
        match answer {
            MarkdownGraphemeAnswer::Known(Some(boundary)) => Ok(boundary.utf16),
            MarkdownGraphemeAnswer::Known(None) if !next && position == 0 => Ok(0),
            MarkdownGraphemeAnswer::Known(None)
                if next && position == self.key.request.input.utf16_len =>
            {
                Ok(position)
            }
            // Cache EOF is not document EOF. Return Pending only while actual preparation runs; otherwise the range is unavailable.
            _ => Err(self.missing_geometry()),
        }
    }

    fn missing_geometry(&self) -> Error {
        // Pending promises only existing in-flight preparation, not a synthetic
        // line endpoint or an unconditional retry when no producer can supply it.
        if self
            .owner
            .upgrade()
            .is_some_and(|owner| owner.is_preparing())
        {
            Error::Pending
        } else {
            Error::InvalidRange
        }
    }

    fn target(
        &self,
        head: usize,
        extend: bool,
        affinity: Option<Affinity>,
        goal: Option<NativeMarkdownVerticalGoal>,
    ) -> Result<NativeMarkdownPagedNavigationTarget> {
        self.require()?;
        let before = self.selection()?;
        let selection = MarkdownTextSelection {
            anchor_utf16: if extend { before.anchor_utf16 } else { head },
            head_utf16: head,
        };
        Ok(NativeMarkdownPagedNavigationTarget {
            selection,
            head_affinity: affinity,
            vertical_goal: goal,
            bidi_side: None,
        })
    }

    /// The synchronous comparison preserves canvas, Grid backgrounds/borders, text decorations, Grid selection, then glyph order.
    fn decorations(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.fill_canvas(context, current)?;
        self.grid_decorations(context, &|| current() && self.current())?;
        self.text_decorations(context, current)?;
        self.grid_selection(context, &|| current() && self.current())?;
        self.color(context, self.colors.text);
        self.validate_ready(current)
    }

    /// Fill the fixed viewport once; the original Paint continuation visits Grid work rather than scanning cells during construction.
    fn fill_canvas(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        self.color(context, self.colors.background);
        CGContext::fill_rect(Some(context), self.key.request.viewport);
        self.validate_ready(current)
    }

    /// Preserve the existing single-window decoration path and pixel order, including the original Source behavior without Grid.
    fn text_decorations(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        if let Some(window) = &self.window {
            window
                .paint_code_backgrounds(context, &|| current() && self.current())
                .map_err(paint_error)?;
        } // Code backgrounds precede the active line and selections in both synchronous and CPU candidate paths.
        if let Some(active) = &self.active_line {
            self.color(context, self.colors.active_line);
            for (index, range) in self.ranges.iter().enumerate() {
                let selected = if active.is_empty() {
                    range.is_empty() && range.start == active.start
                } else {
                    range.start < active.end && active.start < range.end
                };
                if !selected {
                    continue; // Another logical line or actual trailing EOF line cannot borrow the preceding wrapped line's range for highlighting.
                }
                let frame = self
                    .text_window()?
                    .line_frame(index)
                    .ok_or(Error::InvalidGeometry)?;
                let viewport = self.key.request.viewport;
                let bottom = frame.origin.y.max(viewport.origin.y);
                let top = (frame.origin.y + frame.size.height)
                    .min(viewport.origin.y + viewport.size.height);
                if top <= bottom {
                    continue; // Measured lines outside the visible intersection have no pixels in this candidate.
                }
                // The original window already spans viewport width; clip only the actual vertical intersection without estimating line height.
                CGContext::fill_rect(
                    Some(context),
                    NSRect::new(
                        NSPoint::new(frame.origin.x, bottom),
                        NSSize::new(frame.size.width, top - bottom),
                    ),
                );
            }
            self.validate_ready(current)?;
        } // The active line precedes selections/text, preserving feedback without reading or recomputing offscreen layout.
        // Find backgrounds follow code/active-line paint and precede user selection, matching the stepped path.
        // Keep the synchronous window traversal independent and complete; omitted Find feedback cannot define expected pixels.
        self.find_decorations(context, current)?;
        if let Some(window) = &self.window {
            let prepared = window.source_range();
            // Input certified ordered disjoint ranges; binary search skips ranges before the viewport without reading their text.
            // Borrow publication's integer metadata only; offscreen matches neither advance layout nor materialize their text envelope.
            let first = self
                .selections
                .partition_point(|selection| selection.end <= prepared.start);
            self.color(context, self.colors.selection);
            for selection in &self.selections[first..] {
                if selection.start >= prepared.end {
                    break; // Subsequent ranges also lie beyond this prepared window and have no pixels in this frame.
                }
                if selection.is_empty() {
                    continue; // Empty ranges are caret positions, not additional selection backgrounds.
                }
                // All ranges share the sealed window and candidate canvas; any actual geometry or authority error fails the whole frame.
                window
                    .selection_rects(selection.clone(), current, |rect| {
                        CGContext::fill_rect(Some(context), rect);
                        Ok(())
                    })
                    .map_err(scene_error)?;
            }
        }
        self.color(context, self.colors.text);
        self.validate_ready(current)
    }

    /// Paint sealed visible matches only, mapping/clipping each rectangle through the original window without another Find/layout result.
    /// Body/cells consume their own Sources; translucent paint covers each actual rectangle once without duplicate edge clipping.
    fn find_decorations(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        if self.find_highlights.is_empty() {
            return Ok(()); // Without actual matches there is no drawing effect or Grid traversal.
        }
        self.color(context, [1.0, 0.75, 0.12, 0.25]);
        if let Some(window) = &self.window {
            let prepared = window.source_range();
            let first = self
                .find_highlights
                .partition_point(|range| range.end <= prepared.start);
            for range in &self.find_highlights[first..] {
                if range.start >= prepared.end {
                    break;
                }
                window
                    .selection_rects(range.clone(), current, |frame| {
                        CGContext::fill_rect(Some(context), frame);
                        Ok(())
                    })
                    .map_err(scene_error)?;
            }
        }
        // Actual cell mapping clips its own intersection; hidden Markdown has no area and offscreen matches advance no Source.
        // Match asynchronous Body Find, Grid Find, Body selection, then Grid selection coverage order.
        for row in &self.grids {
            let source = row.row.source_range();
            let first = self
                .find_highlights
                .partition_point(|range| range.end <= source.start);
            for range in &self.find_highlights[first..] {
                if range.start >= source.end {
                    break;
                }
                for cell in &row.cells {
                    if let GridCellContent::Composed(window) = &cell.content {
                        window
                            .selection_rects(range.clone(), current, |frame| {
                                CGContext::fill_rect(Some(context), frame);
                                Ok(())
                            })
                            .map_err(scene_error)?;
                    }
                }
            }
        }
        self.validate_ready(current)
    }

    fn caret(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.validate_ready(current)?;
        let input = self.key.request.input;
        // A visible caret belongs to one actual empty selection. Nonempty or
        // multiple ranges keep their selection rendering without an invented caret.
        if input.selection_range_count != 1 || input.primary_range.length != 0 {
            return Ok(());
        }
        let selection = self.selection()?;
        let affinity = self.head_affinity.unwrap_or(Affinity::Downstream);
        match self.grid_first_rect(NSRange::new(selection.head_utf16, 0), affinity)? {
            GridCharacterGeometry::Ready(value) => {
                let mut rect = value.rect;
                rect.size.width = 1.0;
                self.color(context, self.colors.caret);
                CGContext::fill_rect(Some(context), rect);
                return self.validate_ready(current);
            }
            GridCharacterGeometry::Unavailable => return self.validate_ready(current),
            GridCharacterGeometry::Outside => {}
        } // Hidden syntax still belongs to Grid; absent visible cells never fall back to zero-size TableRow structure parts.

        let Some(window) = &self.window else {
            return self.validate_ready(current);
        };
        let prepared = window.source_range();
        if selection.head_utf16 < prepared.start || selection.head_utf16 > prepared.end {
            return Ok(()); // An offscreen caret has no pixels here and triggers no other paragraph reads or layout.
        }
        if window
            .table_row_at(selection.head_utf16, affinity, &|| {
                current() && self.current()
            })
            .map_err(scene_error)?
        {
            return self.validate_ready(current); // Offscreen structural rows in Body overscan also have no ordinary text caret.
        }
        let geometry = self
            .text_window()?
            .caret_geometry_with_side(
                selection.head_utf16,
                self.head_affinity.unwrap_or(Affinity::Downstream),
                self.bidi_side()?,
                &|| current() && self.current(),
            )
            .map_err(scene_error)?;
        if geometry.secondary.is_some() {
            return Err(Error::UnsupportedCommand);
        }
        let mut rect = geometry.primary;
        rect.size.width = 1.0;
        self.color(context, self.colors.caret);
        CGContext::fill_rect(Some(context), rect);
        self.validate_ready(current)
    }
}

impl NativeMarkdownPagedTextScene for Scene {
    fn key(&self) -> &NativeMarkdownPagedSceneKey {
        &self.key
    }
    /// Body and read-only cells share this Scene; expose current sealed label rectangles without a URL registry or persistent geometry table.
    /// The visitor collects Copy rectangles only. Callers release the Scene before Native entry; original Source failure is still checked before return.
    fn visit_link_cursor_rects(&self, visit: &mut dyn FnMut(NSRect)) -> Result<()> {
        self.require()?;
        if let Some(window) = &self.window {
            window
                .visit_link_cursor_rects(&|| self.current(), visit)
                .map_err(scene_error)?;
        }
        self.visit_grid_link_cursor_rects(visit)?;
        self.require()
    }
    /// The sealed LINK part and actual GCB side authorize the hit only, without synchronous semantic work or marker-as-label substitution.
    fn link_hit(&self, local: NSPoint) -> Result<Option<(Range<usize>, usize)>> {
        self.require()?;
        if self.grid_cell_at(local).is_some() {
            return self.grid_link_hit(local);
        }
        if self.window.is_none() {
            return Ok(None);
        }
        let Some(part) = self
            .text_window()?
            .link_part_at(local, &|| self.current())
            .map_err(scene_error)?
        else {
            return Ok(None); // Ordinary text needs no extra Source query; trailing whitespace and exposed markers do not intercept the click.
        };
        let (head, backward, _) = self.word_hit(local)?;
        let index = if backward {
            self.boundary(head, false)?
        } else {
            head
        };
        if !part.contains(&index) {
            return Ok(None); // The nearest caret can lie outside the label; activation requires the original GCB inside the actually hit part.
        }
        self.require()?;
        Ok(Some((part, index)))
    }
    /// Transfer original mouse-up intent to the sole Body; Scene retains no successor closure or pointer-stack layout/external opening.
    fn follow_link(&self, index: usize, intent: Weak<()>) -> Result<()> {
        self.require()?;
        let owner = self.owner.upgrade().ok_or(Error::Revoked)?;
        owner.follow_link(&self.key, index, intent)
    }
    /// Check this Scene's Source/font/output health before returning the fragment to its same weak Body.
    /// Do not look up another body in Workspace globals or synchronously scan the document during the mouse callback.
    fn follow_heading(&self, fragment: &str, intent: Weak<()>) -> Result<()> {
        self.require()?;
        let owner = self.owner.upgrade().ok_or(Error::Revoked)?;
        owner.follow_heading(&self.key, intent, fragment)
    }
    /// Certify the visible link part and actual GCB side before obtaining a bounded target from the same Source/selection/publication.
    fn link_target(&self, local: NSPoint) -> Result<Option<String>> {
        let Some((_, index)) = self.link_hit(local)? else {
            return Ok(None);
        };
        let owner = self.owner.upgrade().ok_or(Error::Revoked)?;
        let link = {
            let producer = owner.producer.borrow();
            producer
                .as_ref()
                .ok_or(Error::Pending)?
                .link_at(&owner, &self.key, index)?
        }; // Doc/Producer borrows end before subsequent Native URL construction; no Core resource borrow crosses it.
        self.require()?;
        // This pointer URL-query boundary has returned Doc/Producer borrows and verified the same key. The bounded target's contiguous
        // representation goes only to the later Native URL call, never another CPU semantic cache or renewed old-Source authority.
        Ok(link.map(|link| link.destination.into_string()))
    }
    fn cpu_frame_published(&self, paints: usize) {
        // CPU ownership already published a complete frame; queue only this original key's weak notification, without external callbacks on the paint stack.
        if let Some(owner) = self.owner.upgrade() {
            owner.queue_cpu_publication(&self.key, paints);
        }
    }
    fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<()> {
        // Check every actual derived Source first; ordinary Body revocation cannot replace another cell directory's first failure with cancellation.
        for row in &self.grids {
            for cell in &row.cells {
                if let GridCellContent::Composed(window) = &cell.content {
                    window.source_health().map_err(scene_error)?;
                }
            }
        }
        if let Some(window) = &self.window {
            window
                .validate_ready(&|| current() && self.current())
                .map_err(scene_error)?;
        }
        self.validate_grids(&|| current() && self.current())?;
        if self.window.is_none() && self.grids.is_empty() {
            return Err(Error::Pending);
        }
        Ok(())
    }
    fn first_rect(&self, range: NSRange) -> Result<NativeMarkdownPagedCharacterRect> {
        self.require()?; // This includes Grid with no visible cell; geometric Pending cannot hide the original Source error.
        let affinity =
            if range.length == 0 && range.location == self.key.request.input.selection.head_utf16 {
                self.head_affinity.unwrap_or(Affinity::Downstream)
            } else {
                Affinity::Downstream
            };
        match self.grid_first_rect(range, affinity)? {
            GridCharacterGeometry::Ready(rect) => return Ok(rect),
            GridCharacterGeometry::Unavailable => {
                self.require()?; // Preserve Source failure even when geometry is unavailable; an asynchronous read failure is not Pending.
                return Err(Error::Pending);
            }
            GridCharacterGeometry::Outside => {}
        }
        let geometry_error = |cause| match cause {
            ViewportError::Pending => Error::Pending,
            other => scene_error(other),
        }; // Line-domain and character queries share this public boundary; offscreen or unprepared geometry retains Pending semantics.
        let window = self.text_window()?;
        if range.length == 0
            && window
                .table_row_at(range.location, affinity, &|| self.current())
                .map_err(geometry_error)?
        {
            return Err(Error::Pending); // Structural rows outside visible Grid cannot use directory font zero to synthesize character rectangles.
        }
        window
            .first_rect_with_caret_side(range, affinity, self.bidi_side()?, &|| self.current())
            .map_err(geometry_error)
    }
    fn hit(&self, local: NSPoint) -> Result<usize> {
        self.require()?;
        if let Some(position) = self.grid_hit(local)? {
            return Ok(position);
        }
        let hit = self
            .text_window()?
            .hit(local, &|| self.current())
            .map_err(scene_error)?;
        if hit.other_source_utf16.is_some() {
            return Err(Error::UnsupportedCommand);
        }
        Ok(hit.source_utf16)
    }
    fn drag_selection_point(&self, local: NSPoint) -> Result<NSPoint> {
        self.require()?;
        let mut nearest = None;
        let mut distance = f64::INFINITY;
        let mut consider = |point: NSPoint| {
            let delta = (local.x - point.x).hypot(local.y - point.y);
            if delta < distance {
                nearest = Some(point);
                distance = delta;
            }
        };
        if let Some(window) = &self.window
            && let Some(point) = window
                .project_drag_point(local, &|| self.current())
                .map_err(scene_error)?
        {
            consider(point);
        }
        for row in &self.grids {
            for cell in &row.cells {
                if let GridCellContent::Composed(window) = &cell.content {
                    if let Some(point) = window
                        .project_drag_point(local, &|| self.current())
                        .map_err(scene_error)?
                    {
                        consider(point);
                    }
                } else {
                    // Empty cells and before/after-text padding use their actual cell frames for endpoints, never equal-width guesses from the row.
                    let clip = self.key.request.viewport;
                    let left = cell.frame.origin.x.max(clip.origin.x);
                    let right = (cell.frame.origin.x + cell.frame.size.width)
                        .min(clip.origin.x + clip.size.width);
                    let bottom = cell.frame.origin.y.max(clip.origin.y);
                    let top = (cell.frame.origin.y + cell.frame.size.height)
                        .min(clip.origin.y + clip.size.height);
                    if right > left && top > bottom {
                        consider(NSPoint::new(
                            local.x.clamp(left, right.next_down()),
                            local.y.clamp(bottom, top.next_down()),
                        ));
                    }
                }
            }
        }
        self.require()?;
        nearest.ok_or(Error::Pending)
    }
    fn cell_hit(
        &self,
        local: NSPoint,
        word: bool,
    ) -> Result<Option<super::super::paged_text_view::NativeMarkdownPagedCellHit>> {
        self.require()?;
        let hit = self.grid_pointer_hit(local, word)?;
        self.require()?;
        Ok(hit)
    }

    fn paragraph_selection(
        &self,
        position: usize,
        affinity: Affinity,
    ) -> Result<MarkdownTextSelection> {
        self.require()?;
        let line = self.line(position, affinity)?;
        let range = &self.paragraphs[line];
        self.require()?;
        Ok(MarkdownTextSelection {
            anchor_utf16: range.start,
            head_utf16: range.end,
        })
    }

    fn word_hit(
        &self,
        local: NSPoint,
    ) -> Result<(
        usize,
        bool,
        Option<crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget>,
    )> {
        self.require()?;
        if let Some(hit) = self.grid_pointer_hit(local, true)? {
            self.require()?;
            return Ok((hit.head_utf16, hit.backward_bias, hit.word_target));
        }
        let (hit, backward_bias) = self
            .text_window()?
            .word_hit(local, &|| self.current())
            .map_err(scene_error)?;
        self.require()?;
        Ok((hit.source_utf16, backward_bias, None))
    }

    fn mouse_selection(
        &self,
        local: NSPoint,
        clicks: usize,
        anchor: Option<usize>,
        extend: bool,
    ) -> Result<(MarkdownTextSelection, Affinity)> {
        self.require()?;
        let (position, affinity) = if let Some(hit) = self.grid_pointer_hit(local, false)? {
            (hit.head_utf16, hit.head_affinity)
        } else {
            let hit = self
                .text_window()?
                .hit(local, &|| self.current())
                .map_err(scene_error)?;
            if hit.other_source_utf16.is_some() {
                return Err(Error::Pending);
            }
            (
                hit.source_utf16,
                Affinity::from_upstream(
                    hit.source_affinity
                        == omni_markdown_core::MarkdownComposedLineAffinity::Upstream,
                ),
            )
        }; // Deliver the actual hit side with selection; cross-cell commits cannot replace soft-wrap upstream with fixed Downstream.
        let before = self.selection()?;
        let selection = match clicks {
            0 | 1 => MarkdownTextSelection {
                anchor_utf16: anchor.unwrap_or(if extend {
                    before.anchor_utf16
                } else {
                    position
                }),
                head_utf16: position,
            },
            3 => self.paragraph_selection(position, affinity)?,
            // Double-click selection requires the actual word producer; ASCII-space guessing cannot define Chinese or punctuation groups.
            _ => return Err(Error::UnsupportedCommand),
        };
        self.require()?;
        Ok((
            selection,
            if clicks == 3 {
                Affinity::Upstream
            } else {
                affinity
            },
        ))
    }
    fn navigation_geometry(&self) -> Result<NativeMarkdownPagedNavigationGeometry> {
        self.require()?;
        Ok(self.navigation.clone())
    }
    fn navigate(
        &self,
        action: Nav,
        extend: bool,
        context: &NativeMarkdownPagedNavigationContext,
    ) -> Result<NativeMarkdownPagedNavigationTarget> {
        self.require()?;
        if !context.geometry.same(&self.navigation) {
            return Err(Error::Revoked);
        }
        let before = self.selection()?;
        let head = before.head_utf16;
        let affinity = context.head_affinity.unwrap_or(Affinity::Downstream);
        let ordered = before.anchor_utf16.min(head)..before.anchor_utf16.max(head);
        let collapse = |backward: bool| if backward { ordered.start } else { ordered.end };
        match action {
            Nav::Left | Nav::Right if extend || ordered.is_empty() => {
                let visual = self.text_window()?.visual_neighbor(
                    head,
                    affinity,
                    context.bidi_side,
                    action == Nav::Right,
                    &|| self.current(),
                );
                self.require()?;
                let visual = match visual {
                    Ok(visual) => visual,
                    Err(crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError::Pending) => return Err(Error::Pending),
                    Err(cause) => return Err(scene_error(cause)),
                }; // A missing actual neighbor remains original Producer work, never BOF/EOF or an invented neighboring offset.
                Ok(NativeMarkdownPagedNavigationTarget {
                    selection: MarkdownTextSelection {
                        anchor_utf16: if extend {
                            before.anchor_utf16
                        } else {
                            visual.utf16
                        },
                        head_utf16: visual.utf16,
                    },
                    head_affinity: Some(visual.affinity),
                    vertical_goal: None,
                    bidi_side: Some(visual.side),
                })
            }
            Nav::Backward | Nav::Forward | Nav::Left | Nav::Right => {
                let backward = match action {
                    Nav::Left | Nav::Right => {
                        // The existing CodeMirror nonempty selection collapses once; actual paragraph base decides the visual left/right endpoint,
                        // rather than the first part, minimum level, or a mixed-direction flag guessing RTL paragraph direction.
                        let base = match self.text_window()?.paragraph_base_level_at(head, affinity, &|| self.current()) {
                            Ok(base) => base,
                            Err(crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError::Pending) => return Err(Error::Pending),
                            Err(cause) => return Err(scene_error(cause)),
                        };
                        (action == Nav::Left) == (base == 0)
                    }
                    _ => action == Nav::Backward, // Preserve existing standard logical Backward/Forward semantics.
                };
                let target = if !extend && !ordered.is_empty() {
                    collapse(backward)
                } else {
                    self.boundary(head, !backward)?
                };
                self.target(
                    target,
                    extend,
                    Some(if backward {
                        Affinity::Upstream
                    } else {
                        Affinity::Downstream
                    }),
                    None,
                )
            }
            Nav::LineStart | Nav::LineEnd | Nav::LineLeft | Nav::LineRight => {
                // Logical line endpoints use original Source coverage. Visual
                // left/right endpoints require the already-certified simple LTR case.
                if matches!(action, Nav::LineLeft | Nav::LineRight) && !self.simple_ltr {
                    return Err(Error::UnsupportedCommand);
                }
                let line = self.line(head, affinity)?;
                let start = matches!(action, Nav::LineStart | Nav::LineLeft);
                let range = self.visible_line_range(line);
                self.target(
                    if start { range.start } else { range.end },
                    extend,
                    Some(if start {
                        Affinity::Downstream
                    } else {
                        Affinity::Upstream
                    }),
                    None,
                )
            }
            Nav::ParagraphStart | Nav::ParagraphEnd | Nav::SelectParagraph | Nav::SelectLine => {
                // Paragraph selection keeps canonical paragraph coverage; selecting
                // a visible line uses its certified folded endpoint instead.
                let line = self.line(head, affinity)?;
                let range = if action == Nav::SelectLine {
                    self.visible_line_range(line)
                } else {
                    self.paragraphs[line].clone()
                };
                if matches!(action, Nav::SelectParagraph | Nav::SelectLine) {
                    self.require()?;
                    Ok(NativeMarkdownPagedNavigationTarget {
                        selection: MarkdownTextSelection {
                            anchor_utf16: range.start,
                            head_utf16: range.end,
                        },
                        head_affinity: Some(Affinity::Upstream),
                        vertical_goal: None,
                        bidi_side: None,
                    })
                } else {
                    self.target(
                        if action == Nav::ParagraphStart {
                            range.start
                        } else {
                            range.end
                        },
                        extend,
                        Some(if action == Nav::ParagraphStart {
                            Affinity::Downstream
                        } else {
                            Affinity::Upstream
                        }),
                        None,
                    )
                }
            }
            Nav::DocumentStart | Nav::DocumentEnd => self.target(
                if action == Nav::DocumentStart {
                    0
                } else {
                    self.key.request.input.utf16_len
                },
                extend,
                Some(if action == Nav::DocumentStart {
                    Affinity::Downstream
                } else {
                    Affinity::Upstream
                }),
                None,
            ),
            Nav::Up | Nav::Down | Nav::PageUp | Nav::PageDown => {
                // Derive vertical targets from the original caret and actual line
                // frames. A missing neighbor remains preparation work, not an
                // estimated offset or a clamp to the prepared-window boundary.
                let caret = self
                    .text_window()?
                    .caret_geometry(head, affinity, &|| self.current())
                    .map_err(scene_error)?;
                if caret.secondary.is_some() {
                    return Err(Error::UnsupportedCommand);
                }
                let old_goal = context
                    .vertical_goal
                    .filter(|goal| self.navigation.permits_goal(*goal));
                let x = old_goal.map_or(caret.primary.origin.x, |goal| goal.container_x());
                let line = self.line(head, affinity)?;
                let backwards = matches!(action, Nav::Up | Nav::PageUp);
                let target_y = if matches!(action, Nav::PageUp | Nav::PageDown) {
                    caret.primary.origin.y
                        + if backwards {
                            self.key.request.viewport.size.height
                        } else {
                            -self.key.request.viewport.size.height
                        }
                } else {
                    let target_line = if backwards {
                        line.checked_sub(1)
                    } else {
                        line.checked_add(1)
                    }
                    .filter(|line| *line < self.ranges.len())
                    .ok_or_else(|| self.missing_geometry())?;
                    let frame = self
                        .text_window()?
                        .line_frame(target_line)
                        .ok_or(Error::InvalidGeometry)?;
                    frame.origin.y + frame.size.height * 0.5
                };
                let hit = self
                    .text_window()?
                    .hit(NSPoint::new(x, target_y), &|| self.current())
                    .map_err(scene_error)?;
                if hit.other_source_utf16.is_some() {
                    return Err(Error::UnsupportedCommand);
                }
                let goal = if matches!(action, Nav::Up | Nav::Down) {
                    old_goal.or_else(|| {
                        NativeMarkdownVerticalGoal::new(
                            caret.primary.origin.x,
                            caret.primary.origin.x,
                            self.navigation.width_key,
                            self.navigation.layout_epoch,
                        )
                    })
                } else {
                    None
                };
                let hit_index = hit
                    .line_ordinal
                    .checked_sub(self.first_ordinal)
                    .filter(|index| *index < self.ranges.len())
                    .ok_or(Error::InvalidGeometry)?;
                self.target(
                    hit.source_utf16,
                    extend,
                    Some(if hit.source_utf16 == self.ranges[hit_index].end {
                        Affinity::Upstream
                    } else {
                        Affinity::Downstream
                    }),
                    goal,
                )
            }
            Nav::WordBackward
            | Nav::WordForward
            | Nav::WordLeft
            | Nav::WordRight
            | Nav::SelectWord => Err(Error::UnsupportedCommand),
        }
    }
    fn edit_plan(&self, action: Edit) -> Result<NativeMarkdownPagedEditPlan> {
        self.require()?;
        let range = self
            .key
            .request
            .input
            .marked
            .unwrap_or(self.key.request.input.primary_range);
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(Error::InvalidRange)?;
        if end > self.key.request.input.utf16_len {
            return Err(Error::InvalidRange);
        }
        let mut replacement = range;
        // Plans describe one actual Input operation. Source/GCB ranges remain
        // canonical; the few inserted control bytes never copy existing text.
        let inserted = match action {
            Edit::InsertNewline => "\n".to_owned(),
            Edit::InsertTab => "\t".to_owned(),
            Edit::DeleteBackward | Edit::DeleteForward => {
                if range.length == 0 {
                    let backward = action == Edit::DeleteBackward;
                    let target = self.boundary(range.location, !backward)?;
                    let start = target.min(range.location);
                    replacement = NSRange::new(start, target.max(range.location) - start);
                }
                String::new()
            }
            Edit::DeleteToLineStart | Edit::DeleteToLineEnd => {
                let line = self.line(range.location, Affinity::Downstream)?;
                let target = if action == Edit::DeleteToLineStart {
                    self.ranges[line].start
                } else {
                    self.visible_line_range(line).end
                };
                let start = range.location.min(target);
                replacement = NSRange::new(start, end.max(target) - start);
                String::new()
            }
            // Word deletion requires its actual producer, and Backtab its indentation command; neither becomes ordinary byte deletion.
            Edit::DeleteWordBackward | Edit::DeleteWordForward | Edit::InsertBacktab => {
                return Err(Error::UnsupportedCommand);
            }
        };
        self.require()?;
        Ok(NativeMarkdownPagedEditPlan {
            replacement,
            inserted,
        })
    }
    fn begin_cpu_paint(
        self: Rc<Self>,
        context: CFRetained<CGContext>,
        current: &dyn Fn() -> bool,
    ) -> Result<Box<dyn NativeMarkdownPagedScenePaintSession>> {
        // Construction fills only the fixed canvas for Body/Grid; all decorations resume under the shared quantum instead of scanning the whole window.
        // Synchronous paint remains an independent complete pixel comparison, not part of asynchronous candidate advancement.
        self.prepare_paint_space(current)?;
        self.fill_canvas(&context, &|| current() && self.current())?;
        Ok(Box::new(Paint::new(self, context)))
    }
    fn paint(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()> {
        self.prepare_paint_space(current)?;
        self.decorations(context, current)?;
        if let Some(window) = &self.window {
            let receipt = window
                .paint(context, &|| current() && self.current())
                .map_err(paint_error)?;
            if receipt.missing_glyphs != 0 {
                return Err(Error::UnsupportedCommand);
            }
        }
        for row in &self.grids {
            for cell in &row.cells {
                if let GridCellContent::Composed(window) = &cell.content {
                    let receipt = window
                        .paint(context, &|| current() && self.current())
                        .map_err(paint_error)?;
                    if receipt.missing_glyphs != 0 {
                        return Err(Error::UnsupportedCommand);
                    }
                }
            }
        }
        // Independent synchronous paint uses composition appearance from the same complete Input, without extra glyphs or successor marked authority.
        // It shares production CPU's per-part decoration implementation and the same range mapping, font metrics, and revocation rules.
        if let Some(marked) = self.key.request.input.marked {
            use crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportDecorationCursor;
            let marked = marked.location..marked.location + marked.length;
            let ranges = std::slice::from_ref(&marked);
            let permit = || current() && self.current();
            if let Some(window) = &self.window {
                let mut cursor = NativeMarkdownComposedViewportDecorationCursor::selections(
                    window.source_range(),
                    ranges,
                )
                .marked_text();
                while !window
                    .advance_decorations(
                        &mut cursor,
                        context,
                        ranges,
                        self.colors.text,
                        NativeMarkdownComposedViewportPaintBudget::default().metadata_records,
                        &permit,
                    )
                    .map_err(paint_error)?
                    .complete
                {}
            }
            // Cell windows first resolve their actual canonical domain; Body endpoints are not display endpoints.
            // The synchronous entry visits bounded prepared windows, without entering unprepared lines or starting additional layout.
            for row in &self.grids {
                for cell in &row.cells {
                    if let GridCellContent::Composed(window) = &cell.content {
                        let mut cursor = window
                            .selection_cursor(ranges, &permit)
                            .map_err(scene_error)?
                            .marked_text();
                        while !window
                            .advance_decorations(
                                &mut cursor,
                                context,
                                ranges,
                                self.colors.text,
                                NativeMarkdownComposedViewportPaintBudget::default()
                                    .metadata_records,
                                &permit,
                            )
                            .map_err(paint_error)?
                            .complete
                        {}
                    }
                }
            }
        }
        self.caret(context, current)
    }
}

fn paint_error(cause: NativeMarkdownComposedViewportPaintError) -> Error {
    match cause.cause {
        NativeMarkdownComposedViewportPaintCause::Window(cause) => scene_error(cause),
        other => Error::Raster(other.to_string()),
    }
}
