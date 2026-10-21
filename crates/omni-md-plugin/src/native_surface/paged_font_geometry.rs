//! Layout borrows actual metrics and GDEF carets from the original font, without CTLine, copied text,
//! or a second font snapshot.

use omni_markdown_core::MarkdownLayoutFontAuthority;

use super::paged_fonts::NativeMarkdownPagedFont;

/// OpenType GDEF LigCaretList uses a 16-bit count; this bounds font metadata without truncating user
/// text.
const MAX_LIGATURE_CARETS: usize = u16::MAX as usize;

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownPagedFontGeometryError {
    #[error("原字体几何请求已经失权")]
    Revoked,
    #[error("字体度量不属于原字体实例")]
    FontMismatch,
    #[error("字形不属于原字体")]
    InvalidGlyph,
    #[error("原字体变换尚未由分页字形坐标建模")]
    UnsupportedMatrix,
    #[error("原字体返回非有限或无效的度量")]
    InvalidMetrics,
    #[error("原字体连字插入点数量或坐标无效")]
    InvalidCarets,
    #[error("无法分配有限字体插入点缓冲")]
    Allocation,
}
type Result<T> = std::result::Result<T, NativeMarkdownPagedFontGeometryError>;

/// All values use points at the actual CTFont size; line composition combines real run ascent/descent
/// instead of imposing fixed line height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeMarkdownPagedFontMetricValues {
    pub ascent: f64,
    pub descent: f64,
    pub leading: f64,
    pub cap_height: f64,
    pub x_height: f64,
    pub underline_position: f64,
    pub underline_thickness: f64,
}

/// Weak identity extends no font/mapping lifetime; a new capture with the same name or size cannot
/// consume these old metrics.
pub struct NativeMarkdownPagedFontMetrics {
    font: MarkdownLayoutFontAuthority,
    values: NativeMarkdownPagedFontMetricValues,
}
impl NativeMarkdownPagedFontMetrics {
    /// Borrow captured metrics only for their exact original font payload.
    pub fn values_for(
        &self,
        font: &NativeMarkdownPagedFont,
    ) -> Result<NativeMarkdownPagedFontMetricValues> {
        if !font.core_font().permits(&self.font) {
            return Err(NativeMarkdownPagedFontGeometryError::FontMismatch);
        } // Equal numbers cannot replace original capture identity; neither reread Native nor renew authority using another font.
        Ok(self.values)
    }
}

/// Actual complete GDEF carets for one glyph; absence of GDEF does not mean the ligature has no
/// interior graphemes.
/// Actual line geometry owns text/grapheme boundaries and editing without GDEF; these values grant no
/// document range authority.
pub struct NativeMarkdownPagedLigatureCarets {
    font: MarkdownLayoutFontAuthority,
    glyph: u16,
    positions: Vec<f64>,
}
impl NativeMarkdownPagedLigatureCarets {
    /// Borrow GDEF positions only for the original font and glyph identity.
    pub fn positions_for(&self, font: &NativeMarkdownPagedFont, glyph: u32) -> Result<&[f64]> {
        if !font.core_font().permits(&self.font) {
            return Err(NativeMarkdownPagedFontGeometryError::FontMismatch);
        } // A same-name capture after the old font releases cannot restore its weak ticket.
        if glyph != u32::from(self.glyph) {
            return Err(NativeMarkdownPagedFontGeometryError::InvalidGlyph);
        } // One glyph's GDEF positions cannot be applied to another glyph in the same font.
        Ok(&self.positions)
    }
}

/// Validate original authority around each synchronous Native call; revocation discards this candidate
/// without recapture or retry.
fn step<T>(current: &dyn Fn() -> bool, operation: impl FnOnce() -> T) -> Result<T> {
    if !current() {
        return Err(NativeMarkdownPagedFontGeometryError::Revoked);
    }
    let value = operation();
    if !current() {
        return Err(NativeMarkdownPagedFontGeometryError::Revoked);
    }
    Ok(value)
}

/// Shaper output has no arbitrary font matrix applied; reject unmodeled transforms consistently with
/// glyph painting.
fn require_matrix(font: &NativeMarkdownPagedFont, current: &dyn Fn() -> bool) -> Result<()> {
    // SAFETY: The original main-thread font owner covers this synchronous getter, which returns a Copy
    // matrix without external pointers.
    let matrix = step(current, || unsafe { font.native_font().matrix() })?;
    if matrix.a != 1.0
        || matrix.b != 0.0
        || matrix.c != 0.0
        || matrix.d != 1.0
        || matrix.tx != 0.0
        || matrix.ty != 0.0
    {
        return Err(NativeMarkdownPagedFontGeometryError::UnsupportedMatrix);
    } // Transformed Native metrics cannot be combined with untransformed 26.6 glyph coordinates.
    Ok(())
}

/// Capture actual metrics at the original size/axes without reading text or certifying a soft line or
/// visible region.
pub fn capture_paged_font_metrics(
    font: &NativeMarkdownPagedFont,
    current: &dyn Fn() -> bool,
) -> Result<NativeMarkdownPagedFontMetrics> {
    require_matrix(font, current)?;
    // SAFETY: Read the original main-thread CTFont only, validating each getter separately so reentry
    // cannot mix old/new facts.
    let ascent = step(current, || unsafe { font.native_font().ascent() })?;
    // SAFETY: The caller holds the original font and the getter returns a Copy scalar in points.
    let descent = step(current, || unsafe { font.native_font().descent() })?;
    // SAFETY: No buffer or pointer is passed; the main-thread font stays alive throughout the
    // synchronous call.
    let leading = step(current, || unsafe { font.native_font().leading() })?;
    // SAFETY: Capture verified exact axes/size; this getter does not mutate the font.
    let cap_height = step(current, || unsafe { font.native_font().cap_height() })?;
    // SAFETY: This is the same original CTFont and its getter owns no external storage.
    let x_height = step(current, || unsafe { font.native_font().x_height() })?;
    // SAFETY: Underline position may be negative; the getter returns only a scalar in original-font
    // points.
    let underline_position = step(current, || unsafe {
        font.native_font().underline_position()
    })?;
    // SAFETY: The original strong font claim covers the final Native read; its value retains no
    // resource.
    let underline_thickness = step(current, || unsafe {
        font.native_font().underline_thickness()
    })?;
    let all = [
        ascent,
        descent,
        leading,
        cap_height,
        x_height,
        underline_position,
        underline_thickness,
    ];
    if all.iter().any(|value| !value.is_finite())
        || ascent < 0.0
        || descent < 0.0
        || ascent + descent <= 0.0
        || !(ascent + descent).is_finite()
        || cap_height < 0.0
        || x_height < 0.0
        || underline_thickness < 0.0
    {
        return Err(NativeMarkdownPagedFontGeometryError::InvalidMetrics);
    } // Preserve the actual signs of leading/underline position; never silently clamp invalid metrics into an invented normal line height.
    step(current, || NativeMarkdownPagedFontMetrics {
        font: font.core_font().authority(),
        values: NativeMarkdownPagedFontMetricValues {
            ascent,
            descent,
            leading,
            cap_height,
            x_height,
            underline_position,
            underline_thickness,
        },
    })
}

/// Read a real glyph's complete GDEF list, with at most 512 KiB of temporary metadata independently of
/// document length.
/// The caller separately validates its spool/grapheme facts; this font primitive cannot derive
/// arbitrary text carets.
pub fn capture_paged_ligature_carets(
    font: &NativeMarkdownPagedFont,
    glyph: u32,
    current: &dyn Fn() -> bool,
) -> Result<NativeMarkdownPagedLigatureCarets> {
    require_matrix(font, current)?;
    let glyph = u16::try_from(glyph)
        .ok()
        .filter(|value| u32::from(*value) < font.core_font().metadata().glyph_count)
        .ok_or(NativeMarkdownPagedFontGeometryError::InvalidGlyph)?;
    // SAFETY: Font and glyph were verified; the public API permits null with zero capacity to query the
    // actual count.
    let count = step(current, || unsafe {
        font.native_font()
            .ligature_caret_positions(glyph, std::ptr::null_mut(), 0)
    })?;
    let count = usize::try_from(count)
        .ok()
        .filter(|count| *count <= MAX_LIGATURE_CARETS)
        .ok_or(NativeMarkdownPagedFontGeometryError::InvalidCarets)?;
    let mut positions = Vec::new();
    if count != 0 {
        positions
            .try_reserve_exact(count)
            .map_err(|_| NativeMarkdownPagedFontGeometryError::Allocation)?;
        positions.resize(count, f64::NAN); // Unwritten slots remain NaN so partial Native writes cannot masquerade as valid zero-position carets.
        // SAFETY: The vector has count initialized exclusive f64 slots and does not move during the
        // call; font/glyph identity stays original.
        let actual = step(current, || unsafe {
            font.native_font()
                .ligature_caret_positions(glyph, positions.as_mut_ptr(), count as _)
        })?;
        if usize::try_from(actual).ok() != Some(count)
            || positions.iter().any(|value| !value.is_finite())
        {
            return Err(NativeMarkdownPagedFontGeometryError::InvalidCarets);
        } // A changed count or partial write rejects publication; truncating the list cannot repair this capture.
    } // Actual GDEF absence stays empty; later line geometry chooses its explicit caret policy from real graphemes.
    step(current, || NativeMarkdownPagedLigatureCarets {
        font: font.core_font().authority(),
        glyph,
        positions,
    })
}
