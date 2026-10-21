//! Draw bounded batches from the one sealed line directory. All Native calls
//! and reentrant frontend checks occur outside the Reader borrow.

use std::cell::RefCell;

use objc2_core_graphics::CGContext;
use omni_markdown_core::{
    MarkdownComposedLinePart, MarkdownComposedLinePartKind, MarkdownComposedLines,
    MarkdownShapedGlyph,
};

use super::{
    NATIVE_MARKDOWN_PAGED_GLYPH_BATCH, NativeMarkdownPagedGlyphPaintCause as Cause,
    NativeMarkdownPagedGlyphPaintError, NativeMarkdownPagedGlyphPaintReceipt,
    NativeMarkdownPagedGlyphPaintRequest, PreparedBatch, Result, batch_count, validate_native_font,
};

/// An empty bounded batch authenticates original output/part/Source/font. Return
/// its borrow before the reentrant current predicate can run again.
fn require(
    output: &RefCell<MarkdownComposedLines>,
    part: &MarkdownComposedLinePart,
    request: &NativeMarkdownPagedGlyphPaintRequest<'_>,
) -> Result<()> {
    let registered = part.font_registration().lease()?;
    if !registered.font().permits(request.font_authority)
        || !registered
            .font()
            .permits(&request.native_font.core_font().authority())
    {
        return Err(Cause::InvalidGlyph);
    } // Equal font names/sizes do not grant the original directory's glyph identity.
    let mut lines = output.try_borrow_mut().map_err(|_| Cause::OutputBusy)?;
    lines.require(request.source)?;
    lines.read_batch(part, 0..0, &mut [])?;
    Ok(())
}

/// One synchronous call draws a finite batch from one actual part, without
/// inferring cross-part source or laying text out. Output is the original
/// composer's sole sealed Reader; part comes from its actual LineEnd. Origin
/// includes the preceding actual visual pen. The caller owns same-ticket clip,
/// decorations, and candidate publication. Err discards the candidate; an actual
/// returned draw remains recorded and grants no right to replay an edit.
///
/// # Errors
/// Reject revoked authority, reentrant borrowing, mismatched Source/font/part,
/// invalid batch/coordinates, and actual IO failure as whole-batch failures.
pub fn paint_composed_glyphs(
    output: &RefCell<MarkdownComposedLines>,
    part: &MarkdownComposedLinePart,
    request: NativeMarkdownPagedGlyphPaintRequest<'_>,
    context: &CGContext,
    current: &(impl Fn() -> bool + ?Sized),
) -> std::result::Result<NativeMarkdownPagedGlyphPaintReceipt, NativeMarkdownPagedGlyphPaintError> {
    let mut native_draw_returned = false;
    let result = (|| -> Result<NativeMarkdownPagedGlyphPaintReceipt> {
        let mut check = || -> Result<()> {
            if !current() {
                return Err(Cause::Revoked);
            }
            require(output, part, &request)?;
            if !current() {
                return Err(Cause::Revoked);
            }
            require(output, part, &request) // A later current call may revoke Source/font; never replace the original ticket.
        };
        check()?;
        if part.facts().kind != MarkdownComposedLinePartKind::Glyphs {
            return Err(Cause::NonGlyphPart); // Tabs/break controls supply layout, never a synthetic .notdef draw.
        }
        let count = batch_count(&request.glyph_range)?;
        if !request
            .native_font
            .accepts_shaping_size(part.facts().points)
        {
            return Err(Cause::FontSizeMismatch);
        }
        validate_native_font(&request, &mut check)?;
        let mut records = [MarkdownShapedGlyph {
            glyph_id: 0,
            source_utf16_cluster: 0,
            flags: 0,
            x_advance: 0,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
        }; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH];
        {
            // Execute Core-only pread here. No current callback, Native getter,
            // or delegate may reenter while this original Reader borrow is live.
            let mut lines = output.try_borrow_mut().map_err(|_| Cause::OutputBusy)?;
            lines.require(request.source)?;
            lines.read_batch(part, request.glyph_range.clone(), &mut records[..count])?;
        }
        check()?; // The complete fixed-stack read still needs original authority before Native sees it.
        let mut batch = PreparedBatch::new(&request, part.facts().source_utf16.clone());
        for glyph in records[..count].iter().copied() {
            batch.push(&request, glyph)?;
        } // Share Job/spool's actual glyph ID, fixed-point offset, and advance validation.
        batch.draw(&request, context, &mut check, &mut native_draw_returned)
    })();
    result.map_err(|cause| NativeMarkdownPagedGlyphPaintError {
        cause,
        native_draw_returned,
    })
}
