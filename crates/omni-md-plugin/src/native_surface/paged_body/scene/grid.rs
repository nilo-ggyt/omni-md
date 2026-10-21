//! Production Scenes retain currently visible cell windows only; structure tickets, display mappings, and CPU paint create no second editing graph.

use super::*;
use crate::native_surface::{
    paged_line_viewport::NativeMarkdownComposedTableCell,
    text_attributes::{
        NATIVE_MARKDOWN_TABLE_BORDER, NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND,
        NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND,
    },
};
use omni_markdown_core::{
    VerifiedMarkdownLayoutTableRow, markdown_document_buffer::VerifiedMarkdownTableCellSource,
};

/// Truly empty display needs neither Composer nor directory FD; unknown nonempty cells cannot become prepared through Empty.
pub(in crate::native_surface::paged_body) enum GridCellContent {
    Empty(VerifiedMarkdownTableCellSource),
    /// The viewport intersects actual top padding while text lies below; retain the original cell start without certifying EOF or completion.
    BeforeText(VerifiedMarkdownTableCellSource),
    /// The actual short cell finished layout and visible space lies below its text; unprepared content cannot enter this state.
    OutsideText(VerifiedMarkdownTableCellSource),
    Composed(NativeMarkdownComposedTableCell),
}

/// Each cell owns an actual window or empty-cell Source ticket; frame includes table padding while text retains its own actual clip.
pub(in crate::native_surface::paged_body) struct GridCellPublication {
    pub frame: NSRect,
    /// Measured cell frame minus original padding, retaining the actual measured prefix without subtracting text scan.first_top.
    pub content_frame: NSRect,
    pub content: GridCellContent,
}

/// An unfinished tall row publishes measured height only; no final bottom border or later body layout appears before complete.
pub(in crate::native_surface::paged_body) struct GridRowPublication {
    pub row: VerifiedMarkdownLayoutTableRow,
    pub frame: NSRect,
    pub complete: bool,
    pub cells: Box<[GridCellPublication]>,
}

/// Hidden syntax and offscreen cells remain in Grid; missing visible rectangles cannot fall through to ordinary font lookup.
/// Only actual cell windows yield Ready; only Outside permits Body geometry. Neither placeholder glyphs nor invented heights exist.
pub(super) enum GridCharacterGeometry {
    Outside,
    Unavailable,
    Ready(NativeMarkdownPagedCharacterRect),
}

impl Scene {
    /// Visit bounded visible cells only; empty, before-text, and after-text cells authorize no link cursor.
    /// Each Composed window uses its actual cell clip; hovering prepares no offscreen text and reads no destination string.
    pub(super) fn visit_grid_link_cursor_rects(&self, visit: &mut dyn FnMut(NSRect)) -> Result<()> {
        self.validate_grids(&|| self.current())?;
        for row in &self.grids {
            for cell in &row.cells {
                if let GridCellContent::Composed(window) = &cell.content {
                    window
                        .visit_link_cursor_rects(&|| self.current(), visit)
                        .map_err(scene_error)?;
                }
            }
        }
        self.validate_grids(&|| self.current())
    }

    /// Return the actual hit cell, never inferring columns from equal widths or visible-array indexes.
    pub(super) fn grid_cell_at(&self, local: NSPoint) -> Option<&GridCellPublication> {
        self.grids
            .iter()
            .filter(|row| inside(local, row.frame))
            .flat_map(|row| row.cells.iter())
            .find(|cell| inside(local, cell.frame))
    }

    /// Cells without text windows still validate original Doc/raw tickets; old frames cannot authorize hits after row deletion.
    fn raw_cell_range(&self, cell: &VerifiedMarkdownTableCellSource) -> Result<Range<usize>> {
        let owner = self.key.request.document.upgrade().ok_or(Error::Revoked)?;
        let document = owner.try_borrow().map_err(|_| Error::Busy)?;
        cell.canonical_range_for(&document)
            .cloned()
            .map_err(|cause| scene_error(ViewportError::Table(cause)))
    }

    /// Preserve actual Source failure for every display window before returning current revocation.
    pub(super) fn validate_grids(&self, current: &dyn Fn() -> bool) -> Result<()> {
        for row in &self.grids {
            for cell in &row.cells {
                match &cell.content {
                    GridCellContent::Empty(source)
                    | GridCellContent::BeforeText(source)
                    | GridCellContent::OutsideText(source) => {
                        self.raw_cell_range(source)?;
                    }
                    GridCellContent::Composed(window) => {
                        window.validate_ready(current).map_err(scene_error)?
                    }
                }
            }
        }
        if !current() {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// Actual cell mapping returns canonical carets; another real BiDi position requires explicit handling, not a guessed primary side.
    pub(super) fn grid_hit(&self, local: NSPoint) -> Result<Option<usize>> {
        let Some(cell) = self.grid_cell_at(local) else {
            return Ok(None);
        };
        match &cell.content {
            GridCellContent::Empty(source) | GridCellContent::BeforeText(source) => {
                Ok(Some(self.raw_cell_range(source)?.start))
            }
            GridCellContent::OutsideText(source) => Ok(Some(self.raw_cell_range(source)?.end)),
            GridCellContent::Composed(window) => {
                let hit = window.hit(local, &|| self.current()).map_err(scene_error)?;
                if hit.other_source_utf16.is_some() {
                    return Err(Error::UnsupportedCommand);
                }
                Ok(Some(hit.source_utf16))
            }
        }
    }

    /// Activation transfers actual row/raw tickets and the same measured content frame; before-text cells supply endpoints pending original reveal.
    /// Hit measured column frames; double-click bias comes from actual display GCB geometry, not equal-width or LTR guesses.
    pub(super) fn grid_pointer_hit(
        &self,
        local: NSPoint,
        word: bool,
    ) -> Result<Option<crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit>> {
        let Some(row) = self.grids.iter().find(|row| inside(local, row.frame)) else {
            return Ok(None);
        };
        let Some(cell) = row.cells.iter().find(|cell| inside(local, cell.frame)) else {
            return Ok(None); // This horizontal window has no actual publication for that column; the row range cannot reconstruct a cell.
        };
        let (source, head_utf16, head_affinity, backward_bias) = match &cell.content {
            GridCellContent::Composed(window) => {
                let (hit, backward_bias) = if word {
                    window
                        .word_hit(local, &|| self.current())
                        .map_err(scene_error)?
                } else {
                    (
                        window.hit(local, &|| self.current()).map_err(scene_error)?,
                        false,
                    )
                };
                if hit.other_source_utf16.is_some() {
                    return Err(Error::Pending); // Two logical positions at one point require the actual side, not an arbitrary editing start.
                }
                (
                    window.canonical_cell_source().clone(),
                    hit.source_utf16,
                    Affinity::from_upstream(
                        hit.source_affinity
                            == omni_markdown_core::MarkdownComposedLineAffinity::Upstream,
                    ),
                    backward_bias,
                )
            }
            GridCellContent::Empty(source) | GridCellContent::BeforeText(source) => {
                // BeforeText has no glyph side; retain its raw start and let the child's own Source resolve word/paragraph selection after Ready.
                (
                    source.clone(),
                    self.raw_cell_range(source)?.start,
                    Affinity::Downstream,
                    false,
                )
            }
            GridCellContent::OutsideText(source) => (
                source.clone(),
                self.raw_cell_range(source)?.end,
                Affinity::Upstream,
                false,
            ),
        };
        // Read word hits and actual raw column coordinates under the same Scene health guard; row start comes from actual publication.
        // This is later-query intent only, not a substitute for Producer validation of latest Source/Row/raw authority.
        let word_target = if word {
            let original = self.key.request.document.upgrade().ok_or(Error::Revoked)?;
            let document = original.try_borrow().map_err(|_| Error::Busy)?;
            let (_, _, column) = source
                .coordinate_for(&document)
                .map_err(|cause| scene_error(ViewportError::Table(cause)))?;
            Some(
                crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget {
                    row_start: row.row.source_range().start,
                    column,
                    affinity: head_affinity,
                },
            )
        } else {
            None
        };
        Ok(Some(
            crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit {
                row: row.row.clone(),
                source,
                content_frame: cell.content_frame,
                head_utf16,
                head_affinity,
                backward_bias,
                word_target,
            },
        ))
    }

    /// Tab/Enter targets consume published cells only; raw tickets from another publication must match each actual fact in the same Doc.
    /// Return actual content frames and visible endpoints without renewing Row authority, reading text, or upgrading offscreen cells to Ready.
    pub(in crate::native_surface::paged_body) fn cell_source_hit(
        &self,
        source: &VerifiedMarkdownTableCellSource,
    ) -> Result<Option<crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit>> {
        self.require()?;
        let (coordinate, range, kind) = {
            let owner = self.key.request.document.upgrade().ok_or(Error::Revoked)?;
            let document = owner.try_borrow().map_err(|_| Error::Busy)?;
            (
                source.coordinate_for(&document),
                source.canonical_range_for(&document).cloned(),
                source.source_for(&document),
            )
        };
        let coordinate = coordinate.map_err(|cause| scene_error(ViewportError::Table(cause)))?;
        let range = range.map_err(|cause| scene_error(ViewportError::Table(cause)))?;
        let kind = kind.map_err(|cause| scene_error(ViewportError::Table(cause)))?;
        for row in &self.grids {
            if row.row.coordinate() != (coordinate.0, coordinate.1) {
                continue;
            }
            for cell in &row.cells {
                let raw = match &cell.content {
                    GridCellContent::Empty(raw)
                    | GridCellContent::BeforeText(raw)
                    | GridCellContent::OutsideText(raw) => raw,
                    GridCellContent::Composed(window) => window.canonical_cell_source(),
                };
                let matches = {
                    // Compare Source facts under the same actual Doc; empty integer ranges alone cannot distinguish Present from implicit missing cells.
                    let owner = self.key.request.document.upgrade().ok_or(Error::Revoked)?;
                    let document = owner.try_borrow().map_err(|_| Error::Busy)?;
                    let coordinate = raw
                        .coordinate_for(&document)
                        .map_err(|cause| scene_error(ViewportError::Table(cause)))?
                        == coordinate;
                    let range = raw
                        .canonical_range_for(&document)
                        .map_err(|cause| scene_error(ViewportError::Table(cause)))?
                        == &range;
                    let kind = raw
                        .source_for(&document)
                        .map_err(|cause| scene_error(ViewportError::Table(cause)))?
                        == kind;
                    coordinate && range && kind
                };
                if !matches {
                    continue;
                }
                let frame = cell.content_frame;
                let viewport = self.key.request.viewport;
                let visible = frame.origin.x.max(viewport.origin.x)
                    < (frame.origin.x + frame.size.width)
                        .min(viewport.origin.x + viewport.size.width)
                    && frame.origin.y.max(viewport.origin.y)
                        < (frame.origin.y + frame.size.height)
                            .min(viewport.origin.y + viewport.size.height);
                if !visible {
                    self.require()?;
                    return Ok(None);
                }
                let head_utf16 = match &cell.content {
                    GridCellContent::Composed(window) => {
                        match window.first_rect(
                            NSRange::new(range.start, 0),
                            Affinity::Downstream,
                            &|| self.current(),
                        ) {
                            Ok(value)
                                if value.actual_range.length == 0
                                    && range.start <= value.actual_range.location
                                    && value.actual_range.location <= range.end =>
                            {
                                value.actual_range.location
                            }
                            Ok(_) | Err(ViewportError::Pending) => {
                                self.require()?; // Original resource failure precedes offscreen/unprepared geometry and cannot become ordinary waiting.
                                return Ok(None);
                            }
                            Err(cause) => return Err(scene_error(cause)),
                        }
                    }
                    GridCellContent::Empty(_) if range.is_empty() => {
                        // An empty cell has no display glyph; its raw empty range proves the sole BOF. Return only the actual frame, not an invented caret.
                        // Child Source still prepares its actual font line; production input and IME cannot treat this frame as character geometry.
                        range.start
                    }
                    _ => {
                        self.require()?;
                        return Ok(None); // Before/Outside endpoints are not in the content window; original SourceTarget continues reveal.
                    }
                };
                self.require()?;
                return Ok(Some(
                    crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit {
                        row: row.row.clone(),
                        source: source.clone(),
                        content_frame: frame,
                        head_utf16,
                        head_affinity: Affinity::Downstream,
                        backward_bias: false,
                        word_target: None,
                    },
                )); // Non-pointer navigation borrows actual endpoints only; without word-click bias it performs no word handoff.
            }
        }
        self.require()?;
        Ok(None)
    }

    /// Find/IME first resolve the actual row domain, then cell coordinates. Only empty carets may project hidden pipe/syntax boundaries.
    /// Nonempty syntax has no display character; offscreen/unprepared cells cannot borrow neighbors. Three states prevent TableRow text fallback.
    pub(super) fn grid_first_rect(
        &self,
        range: NSRange,
        affinity: Affinity,
    ) -> Result<GridCharacterGeometry> {
        // Both entry boundaries already checked Scene health; validate actual raw tickets/windows here without another full-graph scan.
        for row in &self.grids {
            let physical = row.row.source_range();
            let owns = if range.length == 0 && range.location != 0 && affinity.is_upstream() {
                physical.start < range.location && range.location <= physical.end
            } else {
                physical.contains(&range.location)
            };
            if !owns {
                continue; // Actual physical endpoints decide upstream/downstream ownership; the next row start cannot also belong to the previous table row.
            }
            let mut previous: Option<(&GridCellPublication, Range<usize>, usize)> = None;
            let mut boundary = None;
            for cell in &row.cells {
                let raw = match &cell.content {
                    GridCellContent::Empty(raw)
                    | GridCellContent::BeforeText(raw)
                    | GridCellContent::OutsideText(raw) => raw,
                    GridCellContent::Composed(window) => window.canonical_cell_source(),
                };
                let (canonical, column) = {
                    // Borrow Doc for pure Core reads of this structure ticket, returning the lease before Native caret/current callbacks.
                    let owner = self.key.request.document.upgrade().ok_or(Error::Revoked)?;
                    let document = owner.try_borrow().map_err(|_| Error::Busy)?;
                    let canonical = raw
                        .canonical_range_for(&document)
                        .map_err(|cause| scene_error(ViewportError::Table(cause)))?
                        .clone();
                    let (_, _, column) = raw
                        .coordinate_for(&document)
                        .map_err(|cause| scene_error(ViewportError::Table(cause)))?;
                    (canonical, column)
                };
                // Raw end permits an empty caret only; following whitespace/pipe requests cannot be truncated into a fabricated character rectangle.
                if canonical.contains(&range.location)
                    || range.length == 0 && range.location == canonical.end
                {
                    let end = range
                        .location
                        .checked_add(range.length)
                        .ok_or(Error::InvalidRange)?
                        .min(canonical.end);
                    return self.grid_cell_rect(
                        cell,
                        NSRange::new(range.location, end - range.location),
                        affinity,
                    );
                }
                if range.length != 0 {
                    continue; // Original pipes/delimiters are not visible characters; a first-cell character cannot replace that nonempty request.
                }
                if column == 0 && range.location < canonical.start {
                    // Only a certified first column owns leading syntax; the first visible-array cell might actually be column ten.
                    boundary = Some((cell, canonical.start, Affinity::Downstream));
                    break;
                }
                if let Some((before, previous_range, previous_column)) = &previous
                    && previous_column + 1 == column
                    && previous_range.end < range.location
                    && range.location < canonical.start
                {
                    // Only actual adjacent columns certify both sides of a hidden pipe; missing horizontal offscreen columns cannot be skipped to fabricate authority.
                    boundary = Some(if affinity.is_upstream() {
                        (*before, previous_range.end, Affinity::Upstream)
                    } else {
                        (cell, canonical.start, Affinity::Downstream)
                    });
                    break;
                }
                if column + 1 == row.row.columns() && range.location > canonical.end {
                    // Header physical range includes the actual delimiter; trailing syntax projects through this row's actual final-cell end caret.
                    boundary = Some((cell, canonical.end, Affinity::Upstream));
                    break;
                }
                previous = Some((cell, canonical, column));
            }
            let Some((cell, position, side)) = boundary else {
                return Ok(GridCharacterGeometry::Unavailable); // The position belongs to this row but lacks an actual visible cell; preserve the original request.
            };
            return match self.grid_cell_rect(cell, NSRange::new(position, 0), side)? {
                GridCharacterGeometry::Ready(mut value) => {
                    // Project geometry only: canonical meaning remains the original hidden position, without moving Input/Find into cell text.
                    value.actual_range = range;
                    Ok(GridCharacterGeometry::Ready(value))
                }
                unavailable => Ok(unavailable),
            };
        }
        Ok(GridCharacterGeometry::Outside)
    }

    /// Before/Outside/Empty cells have no font line to borrow; an actual window may also lack this visible caret.
    /// Only geometric Pending becomes Unavailable; preserve Source/font/mapping errors instead of swallowing them as an absent caret.
    fn grid_cell_rect(
        &self,
        cell: &GridCellPublication,
        range: NSRange,
        affinity: Affinity,
    ) -> Result<GridCharacterGeometry> {
        let GridCellContent::Composed(window) = &cell.content else {
            return Ok(GridCharacterGeometry::Unavailable);
        };
        match window.first_rect(range, affinity, &|| self.current()) {
            Ok(value) => Ok(GridCharacterGeometry::Ready(value)),
            Err(ViewportError::Pending) => Ok(GridCharacterGeometry::Unavailable),
            Err(cause) => Err(scene_error(cause)),
        }
    }

    /// Activation still follows original mouseUp and target resolution; expose only the actual visible label's canonical range here.
    pub(super) fn grid_link_hit(&self, local: NSPoint) -> Result<Option<(Range<usize>, usize)>> {
        let Some(cell) = self.grid_cell_at(local) else {
            return Ok(None);
        };
        let GridCellContent::Composed(window) = &cell.content else {
            return Ok(None);
        };
        let Some(range) = window
            .link_part_at(local, &|| self.current())
            .map_err(scene_error)?
        else {
            return Ok(None);
        };
        // A real LINK part has one destination; query its certified start without treating its end caret as a character outside the link.
        let position = range.start;
        Ok(Some((range, position)))
    }

    /// Preserve table colors and one-point borders using the actual outer viewport clip, without whole-row/table backing allocation.
    pub(super) fn grid_decorations(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        self.validate_grids(current)?;
        CGContext::save_g_state(Some(context));
        let restore = Restore(context);
        CGContext::clip_to_rect(Some(context), self.key.request.viewport);
        for row in &self.grids {
            self.color(
                context,
                if row.row.coordinate().1 == 0 {
                    NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND
                } else {
                    NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND
                },
            );
            CGContext::fill_rect(Some(context), row.frame);
            for cell in &row.cells {
                if let GridCellContent::Composed(window) = &cell.content {
                    window
                        .paint_code_backgrounds(context, current)
                        .map_err(paint_error)?;
                }
            }
            self.stroke_color(context, NATIVE_MARKDOWN_TABLE_BORDER);
            CGContext::set_line_width(Some(context), 1.0);
            CGContext::begin_path(Some(context));
            let top = row.frame.origin.y + row.frame.size.height;
            let left = row.frame.origin.x;
            let right = left + row.frame.size.width;
            CGContext::move_to_point(Some(context), left, top);
            CGContext::add_line_to_point(Some(context), right, top);
            // An unfinished tall row has only its actual visible prefix; its temporary bottom is not the sealed row end.
            if row.complete {
                CGContext::move_to_point(Some(context), left, row.frame.origin.y);
                CGContext::add_line_to_point(Some(context), right, row.frame.origin.y);
            }
            for x in row
                .cells
                .iter()
                .map(|cell| cell.frame.origin.x)
                .chain(std::iter::once(right))
            {
                CGContext::move_to_point(Some(context), x, row.frame.origin.y);
                CGContext::add_line_to_point(Some(context), x, top);
            }
            CGContext::stroke_path(Some(context));
        }
        drop(restore);
        self.validate_grids(current)
    }

    /// One actual row background performs one rectangle fill under the same outer viewport clip.
    pub(super) fn paint_grid_row_background(
        &self,
        row: usize,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        // Fixed frames read no text; full checks belong to quantum entry/exit. Guard original cancellation here without rescanning every font per cell.
        if !current() {
            return Err(Error::Revoked);
        }
        CGContext::save_g_state(Some(context));
        let restore = Restore(context);
        CGContext::clip_to_rect(Some(context), self.key.request.viewport);
        let row = &self.grids[row];
        self.color(
            context,
            if row.row.coordinate().1 == 0 {
                NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND
            } else {
                NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND
            },
        );
        CGContext::fill_rect(Some(context), row.frame);
        drop(restore);
        if !current() {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// Open the row path after code backgrounds finish; append actual borders across quanta, then stroke once to preserve seam pixels.
    pub(super) fn begin_grid_border(
        &self,
        row: usize,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        if !current() {
            return Err(Error::Revoked);
        }
        let row = &self.grids[row];
        let left = row.frame.origin.x;
        let right = left + row.frame.size.width;
        let top = row.frame.origin.y + row.frame.size.height;
        // The SDK states ClipToRect clears current path; clip before creating this cross-quantum row path.
        // The sole Paint continuation owns this saved state and restores it on completion or cancellation.
        CGContext::save_g_state(Some(context));
        CGContext::clip_to_rect(Some(context), self.key.request.viewport);
        CGContext::begin_path(Some(context));
        CGContext::move_to_point(Some(context), left, top);
        CGContext::add_line_to_point(Some(context), right, top);
        if row.complete {
            CGContext::move_to_point(Some(context), left, row.frame.origin.y);
            CGContext::add_line_to_point(Some(context), right, row.frame.origin.y);
        }
        Ok(()) // Paint records graphics-state ownership before performing the original Source/current post-check.
    }

    /// Each visible cell appends one original column border, without looping over all columns in one metadata record.
    pub(super) fn append_grid_border(
        &self,
        row: usize,
        cell: usize,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        // The path uses one x coordinate from the sealed row frame, with no Source read; step exit still prioritizes actual Source failure.
        if !current() {
            return Err(Error::Revoked);
        }
        let row = &self.grids[row];
        let x = row.cells[cell].frame.origin.x;
        CGContext::move_to_point(Some(context), x, row.frame.origin.y);
        CGContext::add_line_to_point(Some(context), x, row.frame.origin.y + row.frame.size.height);
        if !current() {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// Stroke the original complete-row path once; splitting steps must not duplicate antialiased seam coverage.
    pub(super) fn finish_grid_border(
        &self,
        row: usize,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        if !current() {
            return Err(Error::Revoked);
        }
        let row = &self.grids[row];
        let x = row.frame.origin.x + row.frame.size.width;
        CGContext::move_to_point(Some(context), x, row.frame.origin.y);
        CGContext::add_line_to_point(Some(context), x, row.frame.origin.y + row.frame.size.height);
        self.stroke_color(context, NATIVE_MARKDOWN_TABLE_BORDER);
        CGContext::set_line_width(Some(context), 1.0);
        CGContext::stroke_path(Some(context));
        if !current() {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// Map cross-cell selection through each window's actual display without copying selected source text.
    pub(super) fn grid_selection(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        self.color(context, self.colors.selection);
        for row in &self.grids {
            let source = row.row.source_range();
            let first = self
                .selections
                .partition_point(|selection| selection.end <= source.start);
            for selection in &self.selections[first..] {
                if selection.start >= source.end {
                    break;
                }
                for cell in &row.cells {
                    if let GridCellContent::Composed(window) = &cell.content {
                        window
                            .selection_rects(selection.clone(), current, |frame| {
                                CGContext::fill_rect(Some(context), frame);
                                Ok(())
                            })
                            .map_err(scene_error)?;
                    }
                }
            }
        }
        self.validate_grids(current)
    }
}

/// This synchronous stack restores graphics state on failure/cancellation, protecting later work on the same CPU candidate.
struct Restore<'a>(&'a CGContext);
impl Drop for Restore<'_> {
    fn drop(&mut self) {
        CGContext::restore_g_state(Some(self.0));
    }
}

/// Use text hit-testing's half-open frames; only the actual right/upper neighbor consumes a shared boundary.
fn inside(point: NSPoint, frame: NSRect) -> bool {
    point.x >= frame.origin.x
        && point.x < frame.origin.x + frame.size.width
        && point.y >= frame.origin.y
        && point.y < frame.origin.y + frame.size.height
}
