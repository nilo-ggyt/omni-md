//! Complete horizontal advance snapshot for the actual platform Font. Override only horizontal
//! metrics, leaving cmap, GSUB, and the original positioning algorithm unchanged.

use harfrust::{Direction, Font, FontFuncs, GlyphId, ShaperFont};

use super::{MarkdownLayoutFontError as Error, Result, positioning};

/// The original Font payload uniquely owns values for its complete fixed glyph domain, without
/// Source/Native strong references, closures, or a global cache.
pub(super) struct HorizontalAdvances {
    scale: i32,
    values: Box<[i32]>,
}

impl HorizontalAdvances {
    /// Platform data arrives in original glyph order; parsed maxp remains the count authority, so
    /// callers cannot shorten the table. Convert double-precision point advances to the same fine
    /// scale as GPOS/trak here; the existing final output still quantizes public 26.6 values once.
    pub(super) fn new(font: &Font, points: f64, glyphs: u32, values: Box<[f64]>) -> Result<Self> {
        if glyphs == 0 || glyphs > 65_536 || values.len() != glyphs as usize {
            return Err(Error::InvalidHorizontalAdvances);
        }
        let (scale, divisor) = positioning::bound_scale(points)?;
        // Native nominal advances already include this point-size/axis instance's default tracking.
        // Match the HarfBuzz CoreText bridge: subtract what this original Font would add at nominal
        // lookup, then let original positioning add it per GCB. Keep kern/trak enabled rather than
        // compensating by glyph/font name. Encode point_size as the same f32 used for shaping while
        // binding scale to the original f64 size, avoiding subtraction of independently rounded terms.
        let tracking = ShaperFont::new(font)
            .with_scale(scale)
            .default_tracking_advance(Direction::LeftToRight, Some(points as f32));
        let mut converted = Vec::new();
        converted
            .try_reserve_exact(values.len())
            .map_err(|_| Error::HorizontalAdvanceAllocation)?;
        for value in values {
            let raw = (value * 64.0 * f64::from(divisor)).round();
            if !value.is_finite() || raw < f64::from(i32::MIN) || raw > f64::from(i32::MAX) {
                return Err(Error::InvalidHorizontalAdvances);
            } // Reject the entire candidate; never keep a partial table or fill missing values from hmtx/zero.
            // Preserve actual negative values. The original frontier uses this same callback and
            // tracking to prove early publication; overflow must reject the table rather than
            // saturating and changing Native nominal metrics.
            converted.push(
                (raw as i32)
                    .checked_sub(tracking)
                    .ok_or(Error::InvalidHorizontalAdvances)?,
            );
        }
        Ok(Self {
            scale,
            values: converted.into_boxed_slice(),
        })
    }

    /// The snapshot belongs to its actual bound point-size scale; the same face at another size
    /// requires a separately captured advance table.
    pub(super) fn require_scale(&self, scale: i32) -> Result<()> {
        if scale != self.scale {
            return Err(Error::InvalidPointSize);
        }
        Ok(())
    }
}

impl FontFuncs for HorizontalAdvances {
    /// HarfRust queries within the original parsed Font's glyph domain; invalid glyphs have no
    /// snapshot index authority. Override only this metric and retain upstream default batch calls,
    /// without additional mutable state.
    fn glyph_h_advance(&self, _font: &ShaperFont<'_, '_>, glyph: GlyphId) -> i32 {
        self.values[glyph.to_u32() as usize]
    }
}
