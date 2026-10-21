//! Draw one finite batch of actual glyphs from the same Ready job, without
//! constructing text, paragraphs, soft lines, or Native views.

use std::{ops::Range, ptr::NonNull};

use objc2_core_foundation::{CGAffineTransform, CGPoint};
use objc2_core_graphics::CGContext;
use omni_markdown_core::{
    MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority, MarkdownShapedGlyph,
    MarkdownShapedSpoolError, MarkdownShapingError, MarkdownShapingJob, MarkdownShapingStatus,
};
use thiserror::Error;

use super::paged_fonts::NativeMarkdownPagedFont;

mod spool;
pub use spool::paint_paged_spool_glyphs;
#[cfg(feature = "native-paged-production")]
mod composed;
#[cfg(feature = "native-paged-production")]
pub use composed::paint_composed_glyphs;

/// Fixed stack allowance for one synchronous draw. Longer visible output keeps
/// its original authority at the scheduling owner and uses explicit batches.
pub const NATIVE_MARKDOWN_PAGED_GLYPH_BATCH: usize = 256;
const MAX_EXACT_26_6: i64 = (1i64 << 53) - 1;
const MAX_POINT_MAGNITUDE: f64 = (1u64 << 46) as f64;

#[derive(Clone, Copy, Debug)]
/// Baseline uses the caller's current CGContext Y-up coordinates; pen advances
/// follow the shaper's actual visual order, including RTL output.
pub struct NativeMarkdownPagedGlyphOrigin {
    pub baseline: CGPoint,
    pub pen_x_26_6: i64,
    pub pen_y_26_6: i64,
}

/// Borrow references only for this synchronous call. Both authorities must come
/// from the original owners that created this same shaping job.
pub struct NativeMarkdownPagedGlyphPaintRequest<'a> {
    pub source: &'a MarkdownLayoutTextAuthority,
    pub font_authority: &'a MarkdownLayoutFontAuthority,
    pub native_font: &'a NativeMarkdownPagedFont,
    pub glyph_range: Range<usize>,
    pub origin: NativeMarkdownPagedGlyphOrigin,
}

#[derive(Debug, Error)]
pub enum NativeMarkdownPagedGlyphPaintCause {
    #[error("原字形绘制权限已经撤销")]
    Revoked,
    #[error("单次字形范围必须非空且不超过256项")]
    BatchLimit,
    #[error("字形范围不属于该Ready任务")]
    InvalidRange,
    #[error("字形任务尚未Ready")]
    Pending,
    #[error("原生字体尺寸与该任务不一致")]
    FontSizeMismatch,
    #[error("原生字体含本任务尚未建模的变换")]
    FontMatrixUnsupported,
    #[error("字形编号不能由同一个原生字体绘制")]
    InvalidGlyph,
    #[error("字形坐标溢出、非有限或超出26.6可精确表示范围")]
    InvalidCoordinate,
    #[error("无法读取原Ready字形：{0}")]
    Shaping(#[from] MarkdownShapingError),
    #[error("无法读取原sealed字形：{0}")]
    Spool(#[from] MarkdownShapedSpoolError),
    #[cfg(feature = "native-paged-production")]
    #[error("原分页行输出正在被使用，不能重入读取")]
    OutputBusy,
    #[cfg(feature = "native-paged-production")]
    #[error("无法读取原分页行输出：{0}")]
    Composed(#[from] omni_markdown_core::MarkdownLineComposeError),
    #[cfg(feature = "native-paged-production")]
    #[error("这个原part是布局控制，不能当作字形批次绘制")]
    NonGlyphPart,
}

#[derive(Debug, Error)]
#[error("{cause}；原生绘制已返回={native_draw_returned}")]
/// Revocation after draw remains failure. The caller must discard candidate
/// pixels rather than claim that a returned Native draw left its context untouched.
pub struct NativeMarkdownPagedGlyphPaintError {
    pub cause: NativeMarkdownPagedGlyphPaintCause,
    pub native_draw_returned: bool,
}

#[derive(Debug)]
/// Prove only that this batch actually drew and kept its original ticket through
/// completion. Ink, clip, soft-line layout, and viewport readiness remain separate.
pub struct NativeMarkdownPagedGlyphPaintReceipt {
    pub glyph_range: Range<usize>,
    pub source_utf16_range: Range<usize>,
    pub first_source_cluster: usize,
    pub last_source_cluster: usize,
    pub flags_or: u32,
    pub missing_glyphs: usize,
    pub final_pen_x_26_6: i64,
    pub final_pen_y_26_6: i64,
}

type Result<T> = std::result::Result<T, NativeMarkdownPagedGlyphPaintCause>;

struct Proof<'a, 'b> {
    request: &'a NativeMarkdownPagedGlyphPaintRequest<'b>,
    native_font_authority: MarkdownLayoutFontAuthority,
    glyph_count: usize,
}

impl Proof<'_, '_> {
    /// Recheck the original frontend and both actual font identities without
    /// fetching glyph bytes; real EOF still executes the job's health boundary.
    fn current(
        &self,
        job: &mut MarkdownShapingJob,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<()> {
        if !current() {
            return Err(NativeMarkdownPagedGlyphPaintCause::Revoked);
        }
        // Query exact EOF for the request font and actual Native font. Equal
        // names cannot substitute a different face or bypass original health.
        if job
            .glyph(
                self.request.source,
                self.request.font_authority,
                self.glyph_count,
            )?
            .is_some()
            || job
                .glyph(
                    self.request.source,
                    &self.native_font_authority,
                    self.glyph_count,
                )?
                .is_some()
        {
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidRange);
        }
        if !current() {
            return Err(NativeMarkdownPagedGlyphPaintCause::Revoked);
        }
        // The last current callback may discover a Source reclamation failure.
        // Recheck this same original identity without recapturing successor input.
        if job
            .glyph(
                self.request.source,
                self.request.font_authority,
                self.glyph_count,
            )?
            .is_some()
        {
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidRange);
        }
        Ok(())
    }
}

/// Return exactly this call's saved context frame on success or failure, leaving
/// no extra graphics-state stack entry after rejection.
struct ContextRestore<'a> {
    context: &'a CGContext,
    text_matrix: CGAffineTransform,
    text_position: CGPoint,
}

impl Drop for ContextRestore<'_> {
    fn drop(&mut self) {
        CGContext::restore_g_state(Some(self.context));
        // Core Text may change the text matrix. Restore it and text position
        // explicitly instead of assuming GState includes them. Cleanup neither
        // redraws nor acquires geometry or publishes a revoked candidate.
        CGContext::set_text_matrix(Some(self.context), self.text_matrix);
        CGContext::set_text_position(
            Some(self.context),
            self.text_position.x,
            self.text_position.y,
        );
    }
}

/// Convert only exactly representable finite 26.6 coordinates into the caller's
/// user space; excessive values must reject before any pixel drawing occurs.
fn axis_position(baseline: f64, pen: i64, offset: i32) -> Result<f64> {
    let positioned = pen
        .checked_add(i64::from(offset))
        .filter(|value| value.unsigned_abs() <= MAX_EXACT_26_6 as u64)
        .ok_or(NativeMarkdownPagedGlyphPaintCause::InvalidCoordinate)?;
    if !baseline.is_finite() || baseline.abs() >= MAX_POINT_MAGNITUDE {
        return Err(NativeMarkdownPagedGlyphPaintCause::InvalidCoordinate);
    }
    let result = baseline + positioned as f64 / 64.0;
    if !result.is_finite() || result.abs() >= MAX_POINT_MAGNITUDE {
        return Err(NativeMarkdownPagedGlyphPaintCause::InvalidCoordinate);
    }
    Ok(result)
}

/// Preserve exact checked fixed-point accumulation for subsequent glyphs.
fn advance(pen: i64, amount: i32) -> Result<i64> {
    pen.checked_add(i64::from(amount))
        .filter(|value| value.unsigned_abs() <= MAX_EXACT_26_6 as u64)
        .ok_or(NativeMarkdownPagedGlyphPaintCause::InvalidCoordinate)
}

/// Job/spool/composed sources share finite batch math and synchronous drawing
/// only; each original owner still authenticates its own Source/font/output.
struct PreparedBatch {
    glyphs: [u16; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH],
    positions: [CGPoint; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH],
    source: Range<usize>,
    count: usize,
    pen_x: i64,
    pen_y: i64,
    first_cluster: Option<usize>,
    last_cluster: usize,
    flags_or: u32,
    missing: usize,
}

impl PreparedBatch {
    /// Own fixed glyph/position arrays for this actual source range and pen.
    fn new(request: &NativeMarkdownPagedGlyphPaintRequest<'_>, source: Range<usize>) -> Self {
        Self {
            glyphs: [0; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH],
            positions: [CGPoint { x: 0.0, y: 0.0 }; NATIVE_MARKDOWN_PAGED_GLYPH_BATCH],
            source,
            count: 0,
            pen_x: request.origin.pen_x_26_6,
            pen_y: request.origin.pen_y_26_6,
            first_cluster: None,
            last_cluster: 0,
            flags_or: 0,
            missing: 0,
        }
    }

    /// Validate one actual glyph and exact position before adding it to the
    /// local candidate. No Native draw occurs while a batch is incomplete.
    fn push(
        &mut self,
        request: &NativeMarkdownPagedGlyphPaintRequest<'_>,
        glyph: MarkdownShapedGlyph,
    ) -> Result<()> {
        // An admitted source range still needs glyph IDs from this actual
        // Native face; another font's same-numbered glyph is not interchangeable.
        if self.count == NATIVE_MARKDOWN_PAGED_GLYPH_BATCH
            || !self.source.contains(&glyph.source_utf16_cluster)
            || glyph.glyph_id >= request.native_font.core_font().metadata().glyph_count
        {
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidGlyph);
        }
        self.glyphs[self.count] = u16::try_from(glyph.glyph_id)
            .map_err(|_| NativeMarkdownPagedGlyphPaintCause::InvalidGlyph)?;
        self.positions[self.count] = CGPoint {
            x: axis_position(request.origin.baseline.x, self.pen_x, glyph.x_offset)?,
            y: axis_position(request.origin.baseline.y, self.pen_y, glyph.y_offset)?,
        };
        self.pen_x = advance(self.pen_x, glyph.x_advance)?;
        self.pen_y = advance(self.pen_y, glyph.y_advance)?;
        self.first_cluster.get_or_insert(glyph.source_utf16_cluster);
        self.last_cluster = glyph.source_utf16_cluster;
        self.flags_or |= glyph.flags; // Preserve unsafe-break facts without issuing new line-break authority.
        self.missing += usize::from(glyph.glyph_id == 0); // Draw .notdef too; never silently omit missing text.
        self.count += 1;
        Ok(())
    }

    /// Save the caller's actual context state and perform one complete Native
    /// draw, checking original authority around getters, drawing, and restoration.
    fn draw(
        mut self,
        request: &NativeMarkdownPagedGlyphPaintRequest<'_>,
        context: &CGContext,
        check: &mut impl FnMut() -> Result<()>,
        native_draw_returned: &mut bool,
    ) -> Result<NativeMarkdownPagedGlyphPaintReceipt> {
        let first_source_cluster = self
            .first_cluster
            .ok_or(NativeMarkdownPagedGlyphPaintCause::InvalidRange)?;
        let text_matrix = native_checked(check, || CGContext::text_matrix(Some(context)))?;
        let text_position = native_checked(check, || CGContext::text_position(Some(context)))?;
        check()?;
        CGContext::save_g_state(Some(context));
        let restore = ContextRestore {
            context,
            text_matrix,
            text_position,
        };
        check()?;
        // SAFETY: Both arrays contain count validated elements, with nonzero
        // count inside the fixed cap. The original CTFont/context outlive the
        // synchronous call, original authority brackets it, and no document/
        // RefCell borrow or partial batch crosses into Native drawing.
        unsafe {
            request.native_font.native_font().draw_glyphs(
                NonNull::from(&mut self.glyphs[0]),
                NonNull::from(&mut self.positions[0]),
                self.count,
                context,
            );
        }
        *native_draw_returned = true;
        check()?;
        drop(restore);
        check()?;
        Ok(NativeMarkdownPagedGlyphPaintReceipt {
            glyph_range: request.glyph_range.clone(),
            source_utf16_range: self.source,
            first_source_cluster,
            last_source_cluster: self.last_cluster,
            flags_or: self.flags_or,
            missing_glyphs: self.missing,
            final_pen_x_26_6: self.pen_x,
            final_pen_y_26_6: self.pen_y,
        })
    }
}

/// Reject the complete excessive/empty range instead of truncating visible text.
fn batch_count(range: &Range<usize>) -> Result<usize> {
    range
        .end
        .checked_sub(range.start)
        .filter(|count| (1..=NATIVE_MARKDOWN_PAGED_GLYPH_BATCH).contains(count))
        .ok_or(NativeMarkdownPagedGlyphPaintCause::BatchLimit)
}

/// Reentrant Native operations retain their original check on both sides.
fn native_checked<T>(
    check: &mut impl FnMut() -> Result<()>,
    operation: impl FnOnce() -> T,
) -> Result<T> {
    check()?;
    let value = operation();
    check()?;
    Ok(value)
}

/// Accept only the actual untransformed font model used by the original shaper;
/// unsupported transforms cannot be approximated through coordinate arithmetic.
fn validate_native_font(
    request: &NativeMarkdownPagedGlyphPaintRequest<'_>,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    // SAFETY: The actual main-thread NativeFont outlives this getter. Read only
    // its immutable original CTFont's public transform matrix.
    let matrix = native_checked(check, || unsafe {
        request.native_font.native_font().matrix()
    })?;
    if matrix
        != (CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        })
    {
        return Err(NativeMarkdownPagedGlyphPaintCause::FontMatrixUnsupported);
    }
    Ok(())
}

/// Send one original Ready-job batch to actual Core Text. The caller owns the
/// context, clip, and callback lifetime. Interpret baseline in current Y-up user
/// coordinates and divide original fixed-point offsets/advances by 64; never
/// reverse already visual-order RTL glyphs. End document/RefCell borrows first
/// and discard candidate pixels on Err. Excessive ranges reject as a whole;
/// this call creates no surface/layout or selection/viewport readiness proof.
///
/// # Errors
/// Reject Pending, revoked Source/font/frontend, invalid range/coordinates, and
/// IO failure. Revocation after actual draw preserves its returned-draw fact.
pub fn paint_paged_glyphs(
    job: &mut MarkdownShapingJob,
    request: NativeMarkdownPagedGlyphPaintRequest<'_>,
    context: &CGContext,
    current: &(impl Fn() -> bool + ?Sized),
) -> std::result::Result<NativeMarkdownPagedGlyphPaintReceipt, NativeMarkdownPagedGlyphPaintError> {
    let mut native_draw_returned = false;
    let result = (|| -> Result<NativeMarkdownPagedGlyphPaintReceipt> {
        batch_count(&request.glyph_range)?;
        if !current() {
            return Err(NativeMarkdownPagedGlyphPaintCause::Revoked);
        }
        if job.status() != MarkdownShapingStatus::Ready {
            // Pending shaping cannot be converted into partial visible glyphs.
            return Err(NativeMarkdownPagedGlyphPaintCause::Pending);
        }
        let profile = job.profile()?;
        if request.glyph_range.end > profile.glyph_count {
            return Err(NativeMarkdownPagedGlyphPaintCause::InvalidRange);
        }
        if !request.native_font.accepts_shaping_size(job.points()) {
            // Native size/optical geometry must match this exact original job.
            return Err(NativeMarkdownPagedGlyphPaintCause::FontSizeMismatch);
        }
        let proof = Proof {
            native_font_authority: request.native_font.core_font().authority(),
            request: &request,
            glyph_count: profile.glyph_count,
        };
        proof.current(job, current)?;
        validate_native_font(&request, &mut || proof.current(job, current))?;
        let source = job.source_range();
        let mut batch = PreparedBatch::new(&request, source);
        for index in request.glyph_range.clone() {
            // Complete the entire finite candidate before the single draw;
            // failed IO or coordinates cannot leave a partially drawn batch.
            proof.current(job, current)?;
            let glyph = job
                .glyph(request.source, request.font_authority, index)?
                .ok_or(NativeMarkdownPagedGlyphPaintCause::InvalidRange)?;
            batch.push(&request, glyph)?;
        }
        batch.draw(
            &request,
            context,
            &mut || proof.current(job, current),
            &mut native_draw_returned,
        )
    })();
    result.map_err(|cause| NativeMarkdownPagedGlyphPaintError {
        cause,
        native_draw_returned,
    })
}
