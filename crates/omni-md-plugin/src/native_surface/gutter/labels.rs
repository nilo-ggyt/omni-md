//! Finite line numbers retain ten actual digit glyphs, without document strings,
//! CTLine allocations, or a separate glyph cache.

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2_app_kit::{NSFont, NSFontWeightRegular};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGContext;
use objc2_core_text::{CTFont, CTFontOrientation};

use crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError as Error;

/// AppKit supplies its shared monospaced system font. Returning this retained
/// reference does not require a system-cached font object to be destroyed.
pub(super) struct LineNumberFont {
    font: Retained<NSFont>,
    glyphs: [u16; 10],
    advances: [f64; 10],
    ascent: f64,
    descent: f64,
}

impl LineNumberFont {
    /// Preserve the established 14-point monospaced digits. Failed glyph coverage
    /// or metrics reject the label source rather than silently switching fonts.
    pub(super) fn new() -> Result<Self, Error> {
        // SAFETY: AppKit's public regular weight is an immutable process-lifetime scalar.
        let font = NSFont::monospacedSystemFontOfSize_weight(14.0, unsafe { NSFontWeightRegular });
        let core: &CTFont = font.as_ref();
        let mut digits = (*b"0123456789").map(u16::from);
        let mut glyphs = [0u16; 10];
        // SAFETY: Both stack arrays have exactly ten entries. The input contains
        // complete BMP digits, and this synchronous call exclusively borrows output.
        let covered = unsafe {
            core.glyphs_for_characters(
                NonNull::from(&mut digits[0]),
                NonNull::from(&mut glyphs[0]),
                10,
            )
        };
        if !covered || glyphs.contains(&0) {
            // A missing digit cannot be represented as a successful blank label.
            return Err(Error::Raster("行号字体缺少实际数字字形".into()));
        }
        let mut sizes = [CGSize::ZERO; 10];
        // SAFETY: Glyph input and CGSize output each cover ten entries; the actual
        // font and exclusive output buffer stay alive throughout the synchronous call.
        unsafe {
            core.advances_for_glyphs(
                CTFontOrientation::Horizontal,
                NonNull::from(&mut glyphs[0]),
                sizes.as_mut_ptr(),
                10,
            );
        }
        // SAFETY: Read immutable metrics from the same live CTFont, without source
        // document access or an escaping output pointer.
        let (ascent, descent) = unsafe { (core.ascent(), core.descent()) };
        if sizes
            .iter()
            .any(|size| !size.width.is_finite() || size.width <= 0.0)
            || !ascent.is_finite()
            || !descent.is_finite()
            || ascent <= 0.0
            || descent < 0.0
        {
            // Nonfinite or invalid metrics cannot define a safe label bitmap.
            return Err(Error::InvalidGeometry);
        }
        Ok(Self {
            font,
            glyphs,
            advances: sizes.map(|size| size.width),
            ascent,
            descent,
        })
    }

    /// Size the ruler from the canonical line-count digit width, preserving the
    /// existing padding, minimum width, border, and gap to the document.
    pub(super) fn width(&self, lines: usize) -> f64 {
        let mut value = lines;
        let mut digits = 1usize;
        while value >= 10 {
            value /= 10; // Reduce one scalar only; never enumerate document or visible lines.
            digits += 1;
        }
        // The established layout reserves repeated nines of the required length.
        // Its minimum border-box width already contains both horizontal paddings.
        (self.advances[9] * digits as f64 + 5.0 + 3.0).max(20.0) + 1.0 + 10.0
    }

    /// Right-align one logical line number. A 64-bit usize needs at most twenty
    /// decimal digits, so glyphs and positions fit fixed stack arrays.
    pub(super) fn draw(&self, context: &CGContext, number: usize, right: f64, frame: CGRect) {
        let mut value = number;
        let mut ordinals = [0usize; 20];
        let mut start = ordinals.len();
        loop {
            start -= 1;
            ordinals[start] = value % 10;
            value /= 10;
            if value == 0 {
                break; // Preserve the final digit, including a visible zero label.
            }
        }
        let width: f64 = ordinals[start..]
            .iter()
            .map(|index| self.advances[*index])
            .sum();
        let baseline =
            frame.origin.y + (frame.size.height - self.ascent - self.descent) * 0.5 + self.descent;
        let mut x = right - width;
        let mut glyphs = [0u16; 20];
        let mut positions = [CGPoint::ZERO; 20];
        for (target, ordinal) in ordinals[start..].iter().enumerate() {
            glyphs[target] = self.glyphs[*ordinal];
            positions[target] = CGPoint::new(x, baseline);
            x += self.advances[*ordinal]; // Use each actual digit's advance, without averaging widths.
        }
        let core: &CTFont = self.font.as_ref();
        // SAFETY: Count fits both twenty-entry stack arrays, and every glyph came
        // from this actual font. The caller exclusively owns this bitmap context.
        unsafe {
            core.draw_glyphs(
                NonNull::from(&mut glyphs[0]),
                NonNull::from(&mut positions[0]),
                ordinals.len() - start,
                context,
            );
        }
    }
}
