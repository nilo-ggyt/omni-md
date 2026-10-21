//! Verify actual Core Graphics output against the independently written RGBA
//! fixture. No mirrored planner, private counters, or field snapshots supply
//! the expected visible pixels.

use super::*;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};

/// Write one finite owned pixel fixture, with deliberately nonzero row padding.
/// The production preparation path alone classifies and plans its representation.
fn fixture(width: usize, height: usize, pixel: impl Fn(usize, usize) -> [u8; 4]) -> PreparedImage {
    let stride = (width * 4 + 15) & !15;
    let allocation = allocate(stride * height, Arc::default(), None).unwrap();
    for y in 0..height {
        // SAFETY: This fresh mapping has no context/provider. Each writable row
        // is inside its owned allocation and this borrow spans no Native call.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                allocation.address.as_ptr().cast::<u8>().add(y * stride),
                stride,
            )
        };
        bytes.fill(0xad); // Padding must not be mistaken for visible pixel data.
        for (x, target) in bytes[..width * 4]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            *target = pixel(x, y);
        }
    }
    prepare(allocation, width, height, stride).unwrap()
}

/// Keep fixed original patterns for multicolor overflow, grayscale, solid,
/// reversed color order, and early/late transparency. These Source pixels are
/// independent of the production classifier's representation choice.
fn sample(kind: usize, x: usize, y: usize, width: usize, height: usize) -> [u8; 4] {
    let band = (0..4).find(|index| y < height * (index + 1) / 4).unwrap();
    match kind {
        0 => [x as u8, band as u8, 17, 255],
        1 if band == 0 => {
            let gray = if x.is_multiple_of(2) { 231 } else { 13 };
            [gray, gray, gray, 255]
        }
        1 => [(x % 8) as u8, band as u8, 91, 255],
        2 => [band as u8 * 37, 29, 43, 255],
        3 if x + 1 == width && y + 1 == height => [7, 11, 13, 128],
        3 => [x as u8, band as u8, 17, 255],
        4 => [(255 - x % 256) as u8, band as u8, 17, 255],
        5 => [31, 47, 63, 128],
        _ => panic!("未知有限测试图案"),
    }
}

#[test]
fn actual_cgimage_decode_matches_all_original_rgba_pixels_after_summary_planning() {
    for kind in [0, 1, 2, 3, 4, 5] {
        let (width, height) = (257, 259);
        let original = fixture(width, height, |x, y| sample(kind, x, y, width, height));
        let source = Arc::clone(&original.original);
        let stride = original.row_bytes;
        let observer = source.counters.observer();
        let planned = PreparedRegions::plan(original, 4).unwrap();
        let count = planned.count();
        let images = planned.finish(&[None; 4][..count], None).unwrap();
        let decoded = Bitmap::new(width, height, Arc::clone(&source.counters)).unwrap();
        // Actual CGImage decoding must preserve exact pixel values; interpolation
        // cannot conceal a wrong palette, region boundary, alpha, or row stride.
        CGContext::set_interpolation_quality(
            Some(decoded.context()),
            objc2_core_graphics::CGInterpolationQuality::None,
        );
        for image in &images {
            // Core Graphics' Y-up destination corresponds to the original
            // top-down source row, including odd-height region boundaries.
            let rect = CGRect::new(
                CGPoint::new(0.0, (height - image.first_row - image.height) as f64),
                CGSize::new(width as f64, image.height as f64),
            );
            if image.encoded.presentation == PixelPresentation::OpaqueNeutralMask {
                // Reproduce the actual neutral-mask presentation: opaque white
                // background with black ink through the decoded alpha mask.
                CGContext::save_g_state(Some(decoded.context()));
                CGContext::set_rgb_fill_color(Some(decoded.context()), 1.0, 1.0, 1.0, 1.0);
                CGContext::fill_rect(Some(decoded.context()), rect);
                CGContext::clip_to_mask(Some(decoded.context()), rect, Some(&image.encoded.image));
                CGContext::set_rgb_fill_color(Some(decoded.context()), 0.0, 0.0, 0.0, 1.0);
                CGContext::fill_rect(Some(decoded.context()), rect);
                CGContext::restore_g_state(Some(decoded.context()));
            } else {
                // Indexed, solid, RGB24, and transparent RGBA use their actual
                // image/provider/color-space contract without a test decoder.
                CGContext::draw_image(Some(decoded.context()), rect, Some(&image.encoded.image));
            }
        }
        CGContext::flush(Some(decoded.context()));
        for y in 0..height {
            // Read only these completed synchronous owned contexts; compare all
            // active RGBA bytes against the independent original fixture.
            assert_eq!(
                row(&decoded.allocation, y, width, width * 4),
                row(&source, y, width, stride),
                "图案{kind}第{y}行实际RGBA必须逐byte相同"
            );
        }
        drop((images, decoded, source));
        // Real image/provider/context claims and the original input mapping must
        // all return; the observer itself owns no pixel allocation.
        assert!(observer.is_released());
    }
}
