//! 一次区间摘要与保留的原逐区分类独立差分；计费、色序、奇数行和真实CGImage解码分别核对。

use super::*;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};

fn fixture(width: usize, height: usize, pixel: impl Fn(usize, usize) -> [u8; 4]) -> PreparedImage {
    let stride = (width * 4 + 15) & !15;
    let allocation = allocate(stride * height, Arc::default(), None).unwrap();
    for y in 0..height {
        // SAFETY: 本测试独占新分配且没有context/provider；逐有效行写RGBA，padding故意保留不同值。
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                allocation.address.as_ptr().cast::<u8>().add(y * stride),
                stride,
            )
        };
        bytes.fill(0xad);
        for (x, target) in bytes[..width * 4]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            *target = pixel(x, y);
        }
    }
    let prepared = prepare(allocation, width, height, stride).unwrap();
    let expected = plan(&prepared.original, width, height, stride);
    assert_same_plan(&prepared.plan, &expected);
    prepared // 每种实际输入的整体prepare也与未融合的原分类器独立差分，不能只核局部摘要。
}

fn assert_same_plan(actual: &Plan, expected: &Plan) {
    match (actual, expected) {
        (Plan::Rgba, Plan::Rgba) | (Plan::Rgb24, Plan::Rgb24) | (Plan::Neutral, Plan::Neutral) => {}
        (Plan::Solid(a), Plan::Solid(b)) => assert_eq!(a, b),
        (Plan::Indexed(a), Plan::Indexed(b)) => {
            assert_eq!(a.count, b.count);
            assert_eq!(&a.colors[..a.count], &b.colors[..b.count]);
            assert_eq!(a.slots, b.slots); // 首遇顺序及真实hash槽都相同，不能只比较集合后改变索引编码。
        }
        _ => panic!("新摘要必须保持原逐区分类的实际表示"),
    }
}

fn assert_summary(original: &PreparedImage, first_row: usize, height: usize, scan: RegionScan) {
    let expected = plan_region(
        &original.original,
        original.width,
        height,
        original.row_bytes,
        first_row,
    ); // 原分类器仍是prepare使用的独立实现，不从新摘要导出预期。
    let first: [u8; 4] = row(
        &original.original,
        first_row,
        original.width,
        original.row_bytes,
    )[..4]
        .try_into()
        .unwrap();
    assert_eq!(scan.first, first);
    match &expected {
        Plan::Solid(value) => {
            assert!(scan.solid);
            assert_eq!(scan.opaque, value[3] == 255);
            assert_eq!(
                scan.neutral,
                value[3] == 255 && value[0] == value[1] && value[1] == value[2]
            );
        }
        Plan::Neutral => {
            assert!(!scan.solid && scan.opaque && scan.neutral && scan.palette_complete)
        }
        Plan::Indexed(_) => {
            assert!(!scan.solid && scan.opaque && !scan.neutral && scan.palette_complete)
        }
        Plan::Rgb24 => {
            assert!(!scan.solid && scan.opaque && !scan.neutral && !scan.palette_complete)
        }
        Plan::Rgba => assert!(!scan.solid && !scan.opaque && !scan.neutral),
    }
    assert_same_plan(&scan.into_plan(), &expected);
}

fn compare_quarters_and_merged_halves(original: &PreparedImage, count: usize) {
    let mut scans = scan_regions(original, count);
    let observed: usize = scans
        .iter()
        .flatten()
        .map(|scan| scan.observed_pixels)
        .sum();
    assert_eq!(observed, original.width * original.height);
    if count == 4 {
        for half in 0..2 {
            let first = original.height * half / 2;
            let end = original.height * (half + 1) / 2;
            let combined = RegionScan::merge(
                scans[half * 2].as_ref().unwrap(),
                scans[half * 2 + 1].as_ref().unwrap(),
            );
            assert_eq!(combined.observed_pixels, 0); // 合并只处理已有有限色表，不重新访问原像素。
            assert_summary(original, original.first_row + first, end - first, combined);
        }
    }
    for (index, scan) in scans[..count].iter_mut().enumerate() {
        let first = original.height * index / count;
        let end = original.height * (index + 1) / count;
        let actual = scan.take().unwrap();
        assert_eq!(actual.observed_pixels, original.width * (end - first));
        assert_summary(original, original.first_row + first, end - first, actual);
    } // 直接消费本轮scan_regions实际摘要，不能另扫一遍正确区间替错误的规划结果作证。
}

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
fn quarter_fields_colors_order_and_real_observations_match_the_original_region_classifier() {
    for height in [5, 259, 511] {
        for kind in 0..6 {
            let original = fixture(257, height, |x, y| sample(kind, x, y, 257, height));
            compare_quarters_and_merged_halves(&original, 4);
        }
    }
    for height in [2, 3] {
        let original = fixture(17, height, |x, y| sample(4, x, y, 17, height));
        compare_quarters_and_merged_halves(&original, 2);
    } // 高度不足四行只生成两个非空half，不能用空quarter补造原first字段。
}

#[test]
fn fused_prepare_visits_each_pixel_once_and_planning_only_consumes_the_same_saved_summaries() {
    let before = REGION_OBSERVATIONS.with(std::cell::Cell::get);
    let original = fixture(509, 511, |x, y| sample(0, x, y, 509, 511));
    assert!(matches!(original.plan, Plan::Rgb24));
    let pixels = 509 * 511;
    assert_eq!(original.observed_pixels, pixels);
    assert_eq!(
        REGION_OBSERVATIONS.with(std::cell::Cell::get) - before,
        pixels
    );
    let saved = original.regions.as_ref().unwrap();
    assert_eq!(saved.count, 4);
    assert_eq!(
        saved
            .parts
            .iter()
            .flatten()
            .map(|scan| scan.observed_pixels)
            .sum::<usize>(),
        pixels
    );
    let expected = legacy_regions(&original, 2);
    let before_planning = REGION_OBSERVATIONS.with(std::cell::Cell::get);
    let observer = original.original.counters.observer();
    let actual = PreparedRegions::plan(original, 2).unwrap();
    assert_eq!(
        REGION_OBSERVATIONS.with(std::cell::Cell::get),
        before_planning
    );
    assert!(actual.original.regions.is_none());
    assert_eq!(actual.fragments.len(), expected.len());
    for (actual, expected) in actual.fragments.iter().zip(expected) {
        assert_eq!(
            (actual.first_row, actual.height),
            (expected.first_row, expected.height)
        );
        assert_same_plan(&actual.plan, &expected.plan);
    }
    drop(actual);
    assert!(observer.is_released()); // 保存摘要只有固定metadata，取消计划后不能拖住原RGBA映射。
}

#[test]
fn fused_whole_classification_preserves_priority_and_the_original_transparency_early_exit() {
    for kind in 0..4 {
        let before = REGION_OBSERVATIONS.with(std::cell::Cell::get);
        let original = fixture(257, 9, |x, y| match kind {
            0 => [31, 47, 63, 128],
            1 => [19, 23, 29, 255],
            2 => {
                let value = ((x + y) % 256) as u8;
                [value, value, value, 255]
            }
            _ => [(x % 8) as u8, 19, 37, 255],
        });
        assert_eq!(original.observed_pixels, 257 * 9);
        assert_eq!(
            REGION_OBSERVATIONS.with(std::cell::Cell::get) - before,
            257 * 9
        );
        assert!(original.regions.is_none());
        match kind {
            0 => assert!(matches!(original.plan, Plan::Solid([31, 47, 63, 128]))),
            1 => assert!(matches!(original.plan, Plan::Solid([19, 23, 29, 255]))),
            2 => assert!(matches!(original.plan, Plan::Neutral)),
            _ => assert!(matches!(original.plan, Plan::Indexed(_))),
        }
    }
    let before = REGION_OBSERVATIONS.with(std::cell::Cell::get);
    let early = fixture(257, 9, |x, y| {
        if x == 0 && y == 0 {
            [1, 2, 3, 128]
        } else {
            [4, 5, 6, 128]
        }
    });
    assert!(matches!(early.plan, Plan::Rgba));
    assert!(early.regions.is_none());
    assert_eq!(early.observed_pixels, 2);
    assert_eq!(REGION_OBSERVATIONS.with(std::cell::Cell::get) - before, 2);
    let before = REGION_OBSERVATIONS.with(std::cell::Cell::get);
    let late = fixture(257, 9, |x, y| {
        if y == 4 && x == 7 {
            [7, 11, 13, 128]
        } else {
            [x as u8, y as u8, 17, 255]
        }
    });
    assert!(matches!(late.plan, Plan::Rgba));
    assert!(late.regions.is_none());
    assert_eq!(late.observed_pixels, 4 * 257 + 8);
    assert_eq!(
        REGION_OBSERVATIONS.with(std::cell::Cell::get) - before,
        4 * 257 + 8
    );
    let single = fixture(509, 1, |x, _| [x as u8, (x >> 8) as u8, 17, 255]);
    assert!(matches!(single.plan, Plan::Rgb24));
    assert!(single.regions.is_none());
    assert_eq!(single.observed_pixels, 509);
    assert_eq!(PreparedRegions::plan(single, 4).unwrap().count(), 1); // 单行RGB24不制造空分区或索取不存在的摘要。
}

#[test]
fn neutral_and_solid_summaries_keep_only_actual_colors_when_merged_with_colored_neighbors() {
    let original = fixture(8, 4, |x, y| match y {
        0 => {
            if x.is_multiple_of(2) {
                [231, 231, 231, 255]
            } else {
                [13, 13, 13, 255]
            }
        }
        1 => [91, x as u8, 17, 255],
        2 => [211, 3, 7, 255],
        3 => [19, 23, 29, 255],
        _ => unreachable!(),
    });
    let scans = scan_regions(&original, 4);
    let neutral = scans[0].as_ref().unwrap();
    assert!(neutral.neutral && neutral.palette_complete);
    assert_eq!(
        &neutral.palette.colors[..neutral.palette.count],
        &[0xe7e7e7, 0x0d0d0d]
    );
    let colored = RegionScan::merge(neutral, scans[1].as_ref().unwrap());
    assert_eq!(colored.palette.count, 10);
    assert_eq!(&colored.palette.colors[..2], &[0xe7e7e7, 0x0d0d0d]);
    assert_summary(&original, 0, 2, colored);
    let solids = RegionScan::merge(scans[2].as_ref().unwrap(), scans[3].as_ref().unwrap());
    assert_eq!(
        &solids.palette.colors[..solids.palette.count],
        &[0xd30307, 0x13171d]
    );
    assert_summary(&original, 2, 2, solids);
}

#[test]
fn the_257th_color_stays_overflowed_and_reversed_first_encounters_are_not_sorted() {
    let original = fixture(257, 5, |x, y| {
        if x == 256 {
            [19, 23, 29, 255]
        } else {
            [(255 - x) as u8, y as u8, 17, 255]
        }
    });
    let scans = scan_regions(&original, 4);
    for scan in scans.iter().flatten() {
        assert!(!scan.palette_complete);
        assert_eq!(scan.palette.count, 256);
        assert_eq!(scan.palette.colors[0] >> 16, 255);
        assert_eq!(scan.palette.colors[255] >> 16, 0);
    }
    let combined = RegionScan::merge(scans[0].as_ref().unwrap(), scans[1].as_ref().unwrap());
    assert!(!combined.palette_complete);
    assert_summary(&original, 0, 2, combined);
}

#[test]
fn merging_complete_quarters_keeps_256_real_colors_and_rejects_the_257th() {
    let original = fixture(256, 4, |x, y| match y {
        0 | 3 => [x as u8, 1, 17, 255],
        1 => [(255 - x) as u8, 1, 17, 255],
        2 => [19, 23, 29, 255],
        _ => unreachable!(),
    });
    let scans = scan_regions(&original, 4);
    assert!(scans.iter().flatten().all(|scan| scan.palette_complete));
    let exact = RegionScan::merge(scans[0].as_ref().unwrap(), scans[1].as_ref().unwrap());
    assert!(exact.palette_complete);
    assert_eq!(exact.palette.count, 256);
    assert_eq!(exact.palette.colors[0], 0x000111);
    assert_eq!(exact.palette.colors[255], 0xff0111);
    assert_summary(&original, 0, 2, exact);
    let overflow = RegionScan::merge(scans[2].as_ref().unwrap(), scans[3].as_ref().unwrap());
    assert!(!overflow.palette_complete);
    assert_eq!(overflow.palette.count, 256);
    assert_eq!(overflow.palette.colors[0], 0x13171d);
    assert_summary(&original, 2, 2, overflow);
}

fn legacy_regions(original: &PreparedImage, maximum: usize) -> Vec<Fragment> {
    if !matches!(original.plan, Plan::Rgb24) || maximum < 2 {
        return Vec::new();
    }
    let mut best_cost =
        allocation_length(rgb_length(original.width, original.height).unwrap()).unwrap();
    let mut best = Vec::new();
    for count in [2usize, 4] {
        if count > maximum || count > original.height {
            continue;
        }
        let mut fragments = Vec::new();
        let mut cost = 0;
        for index in 0..count {
            let first_row = original.height * index / count;
            let end = original.height * (index + 1) / count;
            let height = end - first_row;
            let plan = plan_region(
                &original.original,
                original.width,
                height,
                original.row_bytes,
                original.first_row + first_row,
            );
            cost = add_cost(
                cost,
                output_lengths(&plan, original.width, height, None, true).unwrap(),
            )
            .unwrap();
            fragments.push(Fragment {
                first_row,
                height,
                plan,
            });
        }
        if cost < best_cost {
            best_cost = cost;
            best = fragments;
        }
    }
    best // 保留原2次独立逐区扫描与strict/tie顺序，不能从新摘要的结果生成预期。
}

#[test]
fn chosen_fragments_keep_original_cost_ties_bounds_and_all_exact_plan_fields() {
    for (width, height) in [(17, 3), (256, 2), (257, 259), (509, 511)] {
        for kind in 0..6 {
            for maximum in [1, 2, 3, 4] {
                let original = fixture(width, height, |x, y| sample(kind, x, y, width, height));
                let expected = legacy_regions(&original, maximum);
                let actual = PreparedRegions::plan(original, maximum).unwrap();
                assert_eq!(actual.fragments.len(), expected.len());
                for (actual, expected) in actual.fragments.iter().zip(expected) {
                    assert_eq!(
                        (actual.first_row, actual.height),
                        (expected.first_row, expected.height)
                    );
                    assert_same_plan(&actual.plan, &expected.plan);
                }
            }
        }
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
        CGContext::set_interpolation_quality(
            Some(decoded.context()),
            objc2_core_graphics::CGInterpolationQuality::None,
        );
        for image in &images {
            let rect = CGRect::new(
                CGPoint::new(0.0, (height - image.first_row - image.height) as f64),
                CGSize::new(width as f64, image.height as f64),
            );
            if image.encoded.presentation == PixelPresentation::OpaqueNeutralMask {
                CGContext::save_g_state(Some(decoded.context()));
                CGContext::set_rgb_fill_color(Some(decoded.context()), 1.0, 1.0, 1.0, 1.0);
                CGContext::fill_rect(Some(decoded.context()), rect);
                CGContext::clip_to_mask(Some(decoded.context()), rect, Some(&image.encoded.image));
                CGContext::set_rgb_fill_color(Some(decoded.context()), 0.0, 0.0, 0.0, 1.0);
                CGContext::fill_rect(Some(decoded.context()), rect);
                CGContext::restore_g_state(Some(decoded.context()));
            } else {
                CGContext::draw_image(Some(decoded.context()), rect, Some(&image.encoded.image));
            }
        }
        CGContext::flush(Some(decoded.context()));
        for y in 0..height {
            assert_eq!(
                row(&decoded.allocation, y, width, width * 4),
                row(&source, y, width, stride),
                "图案{kind}第{y}行实际RGBA必须逐byte相同"
            );
        }
        drop((images, decoded, source));
        assert!(observer.is_released()); // 真实image/provider/context和原RGBA所有自有页都必须归还。
    }
}
