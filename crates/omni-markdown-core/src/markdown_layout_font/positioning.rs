//! Bind precise positioning scale and design-axis equivalence to actual point size, preserving the general font's original quantization contract.
use read_fonts::{
    TableProvider,
    types::{F2Dot14, Fixed, Tag},
};

use super::{MarkdownLayoutFont, MarkdownLayoutFontError as Error, Result};

pub(super) fn validate_points(points: f64) -> Result<()> {
    // Validate the original double value against the representable 26.6 range; never saturate invalid input into a usable size.
    let scale = (points * 64.0).round();
    if !points.is_finite() || points <= 0.0 || scale < 1.0 || scale > f64::from(i32::MAX) {
        return Err(Error::InvalidPointSize);
    }
    Ok(())
}

/// Use at most 2^14 internal precision, reducing the factor for larger sizes to retain the original valid size range.
/// Nominal advances, GPOS, kerx, and trak accumulate on one fine scale before final glyph-output quantization.
pub(crate) fn scale(font: &MarkdownLayoutFont, points: f32, legacy: i32) -> Result<(i32, i32)> {
    let Some(bound) = font.shaping_point_size() else {
        return Ok((legacy, 1)); // The general Font path remains bit-for-bit aligned with the original HarfBuzz oracle.
    };
    if (bound as f32).to_bits() != points.to_bits() {
        return Err(Error::InvalidPointSize); // Another size for the same face cannot acquire this instance's positioning authority.
    }
    bound_scale(bound)
}

/// Bound size and platform pure-value advances share one internal scale; do not round them separately and patch differences while drawing.
pub(super) fn bound_scale(bound: f64) -> Result<(i32, i32)> {
    let mut divisor = 1_i32 << 14;
    loop {
        let scaled = (bound * 64.0 * f64::from(divisor)).round();
        if (1.0..=f64::from(i32::MAX)).contains(&scaled) {
            return Ok((scaled as i32, divisor));
        }
        if divisor == 1 {
            return Err(Error::InvalidPointSize); // Do not saturate into a different valid scale.
        }
        divisor /= 2;
    }
}

pub(super) fn verify_design_coordinates(
    font: &MarkdownLayoutFont,
    coordinates: &[([u8; 4], f64)],
) -> Result<()> {
    if coordinates.len() > 32 {
        return Err(Error::VariationBudgetExceeded);
    }
    for (index, (tag, value)) in coordinates.iter().enumerate() {
        // Only actual unique finite axes can enter normalization; source identity is already bound to this Font instance.
        if !value.is_finite() || coordinates[..index].iter().any(|(prior, _)| prior == tag) {
            return Err(Error::InvalidAxisValue);
        }
        if !font.metadata().axes.iter().any(|axis| axis.tag == *tag) {
            return Err(Error::UnknownAxis);
        }
    }
    if font.metadata().axes.is_empty() {
        return Ok(()); // Static faces accept only an empty axis set; every foreign tag was rejected above.
    }
    let fvar = font
        .font()
        .tables()
        .fvar()
        .map_err(|_| Error::InvalidVariationTable)?;
    let axes = fvar.axes().map_err(|_| Error::InvalidVariationTable)?;
    if axes.len() > 32 {
        return Err(Error::VariationBudgetExceeded);
    }
    for (tag, value) in coordinates {
        let axis = axes
            .iter()
            .find(|axis| axis.axis_tag().to_be_bytes() == *tag)
            .ok_or(Error::UnknownAxis)?;
        if *value < axis.min_value().to_f64() || *value > axis.max_value().to_f64() {
            return Err(Error::InvalidAxisValue); // fvar's normal clamping must not hide out-of-range original Native values.
        }
    }
    let avar = if font.font().tables().avar_data().is_some() {
        Some(
            font.font()
                .tables()
                .avar()
                .map_err(|_| Error::InvalidVariationTable)?,
        )
    } else {
        None // Only actual table absence means no remapping; a corrupt present table cannot be treated as missing.
    };
    let mut expected = [F2Dot14::ZERO; 32];
    // Normalize original f64 coordinates through actual fvar/avar, then compare exact quantized positions rather than an epsilon in design space.
    fvar.user_to_normalized(
        avar.as_ref(),
        coordinates
            .iter()
            .map(|(tag, value)| (Tag::new(tag), Fixed::from_f64(*value))),
        &mut expected[..axes.len()],
    );
    let actual = font.font().normalized_coords();
    for (index, expected) in expected[..axes.len()].iter().enumerate() {
        let actual = actual.get(index).copied().unwrap_or(F2Dot14::ZERO);
        if actual != *expected {
            return Err(Error::DesignCoordinatesMismatch);
        }
    }
    Ok(())
}
