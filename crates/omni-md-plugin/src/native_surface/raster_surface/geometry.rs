//! 像素预算只由真实视口和实际 backing scale 推导，不降低清晰度或改变文字布局。

use super::*;

pub(super) fn finite(rect: NSRect) -> bool {
    [
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    ]
    .into_iter()
    .all(f64::is_finite)
}

pub(super) fn dimensions(visible: NSRect, scale: f64) -> Result<(usize, usize), String> {
    if !finite(visible)
        || visible.size.width <= 0.0
        || visible.size.height <= 0.0
        || !scale.is_finite()
        || scale <= 0.0
    {
        return Err("光栅视口或 backing scale 无效".into());
    }
    let width = (visible.size.width * scale).ceil();
    let height = (visible.size.height * scale).ceil();
    if !width.is_finite() || !height.is_finite() || width > 4_000_000.0 || height > 4_000_000.0 {
        return Err("光栅视口尺寸超限".into());
    }
    let dimensions = (width as usize, height as usize);
    if dimensions
        .0
        .checked_mul(dimensions.1)
        .is_none_or(|pixels| pixels > 4_000_000)
    {
        return Err("光栅视口超过四百万像素".into());
    }
    Ok(dimensions)
}

pub(super) fn intersects(a: NSRect, b: NSRect) -> bool {
    a.origin.x < b.origin.x + b.size.width
        && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height
        && b.origin.y < a.origin.y + a.size.height
}

pub(super) fn ink(frame: NSRect, bounds: NSRect, origin: NSPoint) -> Result<NSRect, String> {
    let rect = NSRect::new(
        NSPoint::new(
            frame.origin.x + origin.x + bounds.origin.x,
            frame.origin.y + origin.y + bounds.origin.y,
        ),
        bounds.size,
    );
    if !finite(rect)
        || ![
            rect.origin.x + rect.size.width,
            rect.origin.y + rect.size.height,
        ]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err("真实字形 ink 坐标发生非有限溢出".into());
    }
    Ok(rect)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn viewport(width: f64, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(0.0, 20_540.0), NSSize::new(width, height))
    }

    #[test]
    fn backing_scale_preserves_all_pixels_and_rejects_unbounded_views() {
        assert_eq!(
            dimensions(viewport(1120.0, 780.0), 2.0).unwrap(),
            (2240, 1560)
        );
        assert_eq!(dimensions(viewport(10.25, 4.25), 2.0).unwrap(), (21, 9));
        assert!(dimensions(viewport(1120.0, 27_794.0), 2.0).is_err());
        assert!(dimensions(viewport(f64::INFINITY, 1.0), 2.0).is_err());
        assert!(dimensions(viewport(1000.0, 1000.0), 0.0).is_err());
        assert!(dimensions(viewport(2000.0, 2000.0), 1.0).is_ok());
        assert!(dimensions(viewport(2000.1, 2000.0), 1.0).is_err());
    }

    #[test]
    fn ink_can_enter_viewport_while_layout_frame_is_outside() {
        let viewport = NSRect::new(NSPoint::new(0.0, 100.0), NSSize::new(200.0, 50.0));
        let frame = NSRect::new(NSPoint::new(0.0, 160.0), NSSize::new(100.0, 20.0));
        let bounds = NSRect::new(NSPoint::new(-3.0, -15.0), NSSize::new(110.0, 40.0));
        assert!(!intersects(frame, viewport));
        assert!(intersects(
            ink(frame, bounds, NSPoint::ZERO).unwrap(),
            viewport
        ));
        let impossible = NSRect::new(NSPoint::new(f64::MAX, 0.0), NSSize::new(f64::MAX, 1.0));
        assert!(ink(impossible, bounds, NSPoint::new(f64::MAX, 0.0)).is_err());
    }
}
