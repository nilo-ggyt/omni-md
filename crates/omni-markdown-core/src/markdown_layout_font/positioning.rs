//! 绑定实际字号的内部定位尺度与设计轴等价证明；不改变通用字体的既有量化合同。
use read_fonts::{
    TableProvider,
    types::{F2Dot14, Fixed, Tag},
};

use super::{MarkdownLayoutFont, MarkdownLayoutFontError as Error, Result};

pub(super) fn validate_points(points: f64) -> Result<()> {
    let scale = (points * 64.0).round();
    if !points.is_finite() || points <= 0.0 || scale < 1.0 || scale > f64::from(i32::MAX) {
        return Err(Error::InvalidPointSize);
    }
    Ok(())
}

/// 固定最多2^14倍内部精度；大字号逐级降低因子，保留原接口全部有效字号范围。
/// 基础advance、GPOS、kerx与trak都在同一个细尺度内相加，最终glyph出口才统一量化。
pub(crate) fn scale(font: &MarkdownLayoutFont, points: f32, legacy: i32) -> Result<(i32, i32)> {
    let Some(bound) = font.shaping_point_size() else {
        return Ok((legacy, 1)); // 原Font默认路径与原HarfBuzz oracle逐位一致。
    };
    if (bound as f32).to_bits() != points.to_bits() {
        return Err(Error::InvalidPointSize); // 同face另一个字号不取得本实例定位权限。
    }
    bound_scale(bound)
}

/// 字号绑定与平台纯值推进共用同一内部尺度；不能各自舍入后再在绘图端修补差值。
pub(super) fn bound_scale(bound: f64) -> Result<(i32, i32)> {
    let mut divisor = 1_i32 << 14;
    loop {
        let scaled = (bound * 64.0 * f64::from(divisor)).round();
        if (1.0..=f64::from(i32::MAX)).contains(&scaled) {
            return Ok((scaled as i32, divisor));
        }
        if divisor == 1 {
            return Err(Error::InvalidPointSize); // 不能饱和成另一有效尺度。
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
        if !value.is_finite() || coordinates[..index].iter().any(|(prior, _)| prior == tag) {
            return Err(Error::InvalidAxisValue);
        }
        if !font.metadata().axes.iter().any(|axis| axis.tag == *tag) {
            return Err(Error::UnknownAxis);
        }
    }
    if font.metadata().axes.is_empty() {
        return Ok(()); // 静态face只能接受空轴集合；上方已拒绝所有外来tag。
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
            return Err(Error::InvalidAxisValue); // 不用fvar的默认夹取隐藏Native原值越界。
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
        None // 真正缺表才没有重映射；损坏的实际表不能按缺失继续证明等价。
    };
    let mut expected = [F2Dot14::ZERO; 32];
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
