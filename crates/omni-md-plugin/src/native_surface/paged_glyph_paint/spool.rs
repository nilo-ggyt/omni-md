//! Consume the original worker's sealed glyph file read-only on the main thread,
//! without transferring a mutable Job or NativeFont between threads.

use objc2_core_graphics::CGContext;
use omni_markdown_core::{MarkdownShapedGlyph, MarkdownShapedSpool, MarkdownShapedSpoolAuthority};

use super::{
    NATIVE_MARKDOWN_PAGED_GLYPH_BATCH, NativeMarkdownPagedGlyphPaintCause,
    NativeMarkdownPagedGlyphPaintError, NativeMarkdownPagedGlyphPaintReceipt,
    NativeMarkdownPagedGlyphPaintRequest, PreparedBatch, Result, batch_count, validate_native_font,
};

/// Every file read binds the original output nonce. A newer task with the same
/// Source/font cannot authorize a different sealed file in its place.
struct SpoolProof<'a, 'b> {
    output: &'a MarkdownShapedSpoolAuthority,
    request: &'a NativeMarkdownPagedGlyphPaintRequest<'b>,
    native_font_authority: omni_markdown_core::MarkdownLayoutFontAuthority,
    glyph_count: usize,
}

impl SpoolProof<'_, '_> {
    /// Authenticate exact EOF under both original font identities before and
    /// after reentrant frontend checks, without issuing a glyph pread.
    fn current(
        &self,
        spool: &mut MarkdownShapedSpool,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<()> {
        if !current() {
            return Err(NativeMarkdownPagedGlyphPaintCause::Revoked);
        }
        // An empty actual-EOF batch still checks output, Source health,
        // cancellation, and exact font payload. Request/Core-Native font checks
        // prevent a matching PostScript name from substituting another face.
        for font in [self.request.font_authority, &self.native_font_authority] {
            spool.read_batch(
                self.output,
                self.request.source,
                font,
                self.glyph_count..self.glyph_count,
                &mut [],
            )?;
        }
        if !current() {
            return Err(NativeMarkdownPagedGlyphPaintCause::Revoked);
        }
        // current may discover Source reclamation failure synchronously. Recheck
        // original tickets before return, never a newly captured output/Source.
        spool.read_batch(
            self.output,
            self.request.source,
            self.request.font_authority,
            self.glyph_count..self.glyph_count,
            &mut [],
        )?;
        Ok(())
    }
}

/// Draw one complete original worker-sealed batch into the caller's actual
/// context. One finite read_batch preserves visual order and all preflight
/// checks finish before the single draw. Output must retain this spool's task
/// nonce; callers end document/RefCell borrows, retain original frontend
/// authority, and discard candidate pixels on Err. Soft-line/clip/selection/
/// viewport readiness belongs to the existing enclosing owners.
///
/// # Errors
/// Reject excessive ranges, revoked/mismatched authorities, and actual file IO
/// failures; post-draw revocation retains the actual returned-draw fact.
pub fn paint_paged_spool_glyphs(
    spool: &mut MarkdownShapedSpool,
    output: &MarkdownShapedSpoolAuthority,
    request: NativeMarkdownPagedGlyphPaintRequest<'_>,
    context: &CGContext,
    current: &(impl Fn() -> bool + ?Sized),
) -> std::result::Result<NativeMarkdownPagedGlyphPaintReceipt, NativeMarkdownPagedGlyphPaintError> {
    let mut native_draw_returned = false;
    let result = (|| -> Result<NativeMarkdownPagedGlyphPaintReceipt> {
        // Reject an oversized range before any glyph read; neither truncate it
        // nor internally loop over an entire giant shaping run.
        let count = batch_count(&request.glyph_range)?;
        let proof = SpoolProof {
            output,
            request: &request,
            native_font_authority: request.native_font.core_font().authority(),
            glyph_count: spool.glyph_count(),
        };
        proof.current(spool, current)?;
        if request.glyph_range.end > proof.glyph_count {
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidRange);
        }
        if !request.native_font.accepts_shaping_size(spool.points()) {
            return Err(NativeMarkdownPagedGlyphPaintCause::FontSizeMismatch);
        }
        validate_native_font(&request, &mut || proof.current(spool, current))?;
        let mut records = [MarkdownShapedGlyph {
            glyph_id: 0,
            source_utf16_cluster: 0,
            flags: 0,
            x_advance: 0,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
        }; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH];
        // The entire fixed-stack candidate is discarded on read failure. Core
        // Text receives no successful prefix from an incomplete batch.
        let read = spool.read_batch(
            output,
            request.source,
            request.font_authority,
            request.glyph_range.clone(),
            &mut records[..count],
        )?;
        if read != count {
            // A short file result cannot stand in for the complete requested batch.
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidRange);
        }
        proof.current(spool, current)?;
        let mut batch = PreparedBatch::new(&request, spool.source_range());
        for glyph in records[..count].iter().copied() {
            // Rust-only preflight shares the Job path's exact ID/coordinate/
            // flags rules, with neither Native calls nor document text reads.
            batch.push(&request, glyph)?;
        }
        batch.draw(
            &request,
            context,
            &mut || proof.current(spool, current),
            &mut native_draw_returned,
        )
    })();
    result.map_err(|cause| NativeMarkdownPagedGlyphPaintError {
        cause,
        native_draw_returned,
    })
}
