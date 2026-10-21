//! Read actual caret sides from sealed lines; wrap affinity cannot substitute for a second Bidi visual position.

use super::*;
use crate::native_surface::{
    paged_input::NativeMarkdownBidiCaretSide, paged_line_geometry::composed_part_caret,
    paged_text_view::NativeMarkdownPagedHeadAffinity,
};

/// Bounded scalar facts retain no Native object, text, or new index; different rectangles require explicit ambiguity resolution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeMarkdownComposedCaretGeometry {
    pub source_utf16: usize,
    pub line_ordinal: usize,
    pub line_frame: NSRect,
    pub affinity: NativeMarkdownPagedHeadAffinity,
    pub primary: NSRect,
    pub secondary: Option<NSRect>,
}

impl NativeMarkdownComposedViewport {
    /// Read the actual grapheme's line frame/visual caret through prepared bounded Line/Part metadata only.
    ///
    /// Upstream chooses the line ending at this position; Downstream chooses the line starting there. Actual BOF/EOF
    /// has only its existing document side; a cache-window edge has no such privilege. A Bidi boundary may still have two rectangles,
    /// and wrap affinity cannot silently discard the second. Prepared overscan lines may also return; screen/IME consumers
    /// must separately enforce their original viewport visibility contract.
    ///
    /// # Errors
    /// Reject invalid original Source/font/Scene, non-grapheme positions, an unprepared side, or more than two distinct caret positions.
    pub fn caret_geometry(
        &self,
        source_utf16: usize,
        affinity: NativeMarkdownPagedHeadAffinity,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedCaretGeometry> {
        self.caret_geometry_impl(source_utf16, affinity, None, current)
    }

    /// The actual selection Bidi side chooses primary caret; absent a side, share visual navigation's lowest actual level rule.
    /// A mismatched side remains Pending; the same old input intent cannot authorize a different part.
    pub fn caret_geometry_with_side(
        &self,
        source_utf16: usize,
        affinity: NativeMarkdownPagedHeadAffinity,
        side: Option<NativeMarkdownBidiCaretSide>,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedCaretGeometry> {
        self.caret_geometry_impl(source_utf16, affinity, Some(side), current)
    }

    /// Scene routing consumes prepared line kind/wrap affinity only, without inferring structural blocks from characters/range length.
    /// Identify tables in Body overscan without a visible Grid window; do not synthesize font-zero text geometry for them.
    pub(in crate::native_surface) fn table_row_at(
        &self,
        position: usize,
        affinity: NativeMarkdownPagedHeadAffinity,
        current: &dyn Fn() -> bool,
    ) -> Result<bool> {
        self.require(current)?;
        let index = self.caret_line_index(position, affinity)?;
        let table = self.lines[index].handle.facts().kind
            == omni_markdown_core::MarkdownComposedLineKind::TableRow;
        self.require(current)?;
        Ok(table)
    }

    fn caret_geometry_impl(
        &self,
        source_utf16: usize,
        affinity: NativeMarkdownPagedHeadAffinity,
        side: Option<Option<NativeMarkdownBidiCaretSide>>,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownComposedCaretGeometry> {
        self.require(current)?;
        let index = self.caret_line_index(source_utf16, affinity)?;
        self.require(current)?; // The reader borrow ended; actual Source failure still precedes missing-page/revocation results.
        let line = &self.lines[index];
        if line.handle.facts().kind == omni_markdown_core::MarkdownComposedLineKind::TableRow {
            return Err(NativeMarkdownComposedViewportError::Pending);
        } // Structural blocks have no text point size/caret; outer same-Source Grid supplies Cell geometry without relaxing font guards.
        let selected = side
            .map(|side| self.caret_part_index(line, source_utf16, side))
            .transpose()?;
        let (mut primary, mut secondary) = (None, None);
        // The window bounds part count; inspect actual font/direction endpoints containing this grapheme, not document text.
        for (index, part) in line.parts.iter().enumerate() {
            if selected.is_some_and(|selected| selected != index) {
                continue; // Caret/IME consumes its actual Bidi side; the unselected public query retains both positions.
            }
            let range = &part.handle.facts().source_utf16;
            if source_utf16 < range.start || source_utf16 > range.end {
                continue;
            } // A visual part on this line without the source position cannot participate in caret choice.
            let caret = composed_part_caret(
                &self.output,
                &part.handle,
                &self.palette[part.font],
                source_utf16,
                line.baseline,
                &|| self.require(current).is_ok(),
            );
            self.require(current)?; // A boolean guard cannot erase the original Source first failure.
            let caret = caret?;
            let rect = NSRect::new(
                NSPoint::new(caret.baseline.x, caret.baseline.y - line.descent),
                NSSize::new(0.0, line.ascent + line.descent),
            );
            if !point(rect.origin) || !rect.size.height.is_finite() || rect.size.height <= 0.0 {
                return Err(NativeMarkdownComposedViewportError::InvalidGeometry);
            } // Invalid coordinates/empty font height cannot be replaced by a fixed-height successful caret.
            if primary.is_none() {
                primary = Some(rect);
            } else if primary != Some(rect) && secondary.is_none() {
                secondary = Some(rect);
            } else if primary != Some(rect) && secondary != Some(rect) {
                return Err(NativeMarkdownComposedViewportError::Pending);
            } // Retain Bidi ambiguity; merge identical font-boundary rectangles only, never arbitrarily keep two of several candidates.
        }
        self.require(current)?;
        Ok(NativeMarkdownComposedCaretGeometry {
            source_utf16,
            line_ordinal: line.handle.facts().ordinal,
            line_frame: line.frame,
            affinity,
            primary: primary.ok_or(NativeMarkdownComposedViewportError::InvalidRange)?,
            secondary,
        })
    }
}
