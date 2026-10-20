//! 真实主线程的有限字体机制控制；直接调用生产adapter，不创建NSApplication或窗口。
use std::{cell::Cell, ptr::NonNull};

use objc2::rc::autoreleasepool;
use objc2_app_kit::NSFont;
use objc2_core_foundation::{CFRange, CFString};
use objc2_core_text::CTFont;
use objc2_foundation::MainThreadMarker;
use omni_markdown_core::MarkdownLayoutFontAxis;
use omni_md_plugin::native_surface::paged_font_geometry::{
    NativeMarkdownPagedFontGeometryError, capture_paged_font_metrics, capture_paged_ligature_carets,
};
use omni_md_plugin::native_surface::paged_fonts::{
    NativeMarkdownPagedFont, NativeMarkdownPagedFontError,
};

const MAXIMUM_FONT_BYTES: u64 = 512 * 1024 * 1024;

#[cfg(feature = "native-markdown-runtime-probe")]
#[path = "paged_fonts/punctuation.rs"]
mod punctuation;

fn coverage(font: &CTFont, text: &str) {
    let mut input = [0u16; 8];
    let mut used = 0;
    for unit in text.encode_utf16() {
        assert!(used < input.len(), "固定控制case不能被截断");
        input[used] = unit;
        used += 1;
    }
    assert!(used > 0);
    let mut glyphs = [0u16; 8];
    // SAFETY: 两个固定数组至少有used槽，UTF16是本case完整编码，调用同步且无外部别名。
    let covered = unsafe {
        font.glyphs_for_characters(
            NonNull::new(input.as_mut_ptr()).unwrap(),
            NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            used as _,
        )
    };
    assert!(covered, "实际CTFont没有覆盖有限case");
    let mut index = 0;
    while index < used {
        assert_ne!(glyphs[index], 0, "实际标量首glyph缺失");
        if (0xd800..=0xdbff).contains(&input[index]) {
            assert!(index + 1 < used && (0xdc00..=0xdfff).contains(&input[index + 1]));
            assert_eq!(glyphs[index + 1], 0); // 公开API规定非BMP字符的第二个UTF16槽是零，并非缺字。
            index += 2;
        } else {
            index += 1;
        }
    }
}

fn capture_case(
    base: &CTFont,
    text: &str,
    label: &'static str,
    mtm: MainThreadMarker,
) -> NativeMarkdownPagedFont {
    let value = CFString::from_str(text);
    let length = text.encode_utf16().count();
    assert!(length <= 8); // 此控制的真实case边界，不是未来生产grapheme截断策略。
    // SAFETY: CFString与range是同一个完整有限case；CoreText按公开规则返回+1真实回退Font。
    let actual = unsafe { base.for_string(&value, CFRange::new(0, length as _)) };
    let adapter = NativeMarkdownPagedFont::capture(&actual, MAXIMUM_FONT_BYTES, mtm, &|| true)
        .expect("安全快照与精确Core字体来源");
    coverage(adapter.native_font(), text);
    // 新行几何必须沿原Native字号/轴位取度量；实际中文/emoji回退字体也经过相同门，
    // 不能用单个拉丁字体的常量行高遮住高字形或下行部。
    let metrics = capture_paged_font_metrics(&adapter, &|| true).unwrap();
    let values = metrics.values_for(&adapter).unwrap();
    // SAFETY: adapter持有真实主线程Font；这些独立公开getter用作有限机制oracle。
    let expected = unsafe {
        (
            adapter.native_font().ascent(),
            adapter.native_font().descent(),
            adapter.native_font().leading(),
        )
    };
    assert_eq!((values.ascent, values.descent, values.leading), expected);
    assert!(values.ascent + values.descent > 0.0);
    assert!(matches!(
        capture_paged_font_metrics(&adapter, &|| false),
        Err(NativeMarkdownPagedFontGeometryError::Revoked)
    ));
    assert!(matches!(
        capture_paged_ligature_carets(&adapter, u32::MAX, &|| true),
        Err(NativeMarkdownPagedFontGeometryError::InvalidGlyph)
    ));
    // glyph0也是实际Font内的字形；空GDEF收据不代表正文中的组合字或连字没有内部字素。
    let carets = capture_paged_ligature_carets(&adapter, 0, &|| true).unwrap();
    assert!(
        carets
            .positions_for(&adapter, 0)
            .unwrap()
            .iter()
            .all(|value| value.is_finite())
    );
    assert!(matches!(
        carets.positions_for(&adapter, 1),
        Err(NativeMarkdownPagedFontGeometryError::InvalidGlyph)
    ));
    let metadata = adapter.core_font().metadata();
    assert!(metadata.glyph_count > 0 && metadata.units_per_em > 0);
    assert!(metadata.face_index < metadata.face_count);
    assert!(!adapter.source_face_postscript().is_empty());
    println!(
        "native_paged_font case={label} face={} faces={} axes={} glyph_count={} source_bytes={} actual_ct_coverage=true kernel_unmapped_verified=false native_cache_deallocated_verified=false formal=false",
        metadata.face_index,
        metadata.face_count,
        metadata.axes.len(),
        metadata.glyph_count,
        adapter.bytes_observer().captured_bytes()
    );
    adapter
}

fn assert_released(observer: &omni_markdown_core::MarkdownLayoutFontBytesResourceObserver) {
    assert!(observer.mapping_drop_completed());
    assert!(observer.unmap_succeeded(), "最后Blob没有真实munmap成功收据");
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.backing_descriptor_closed() && observer.source_descriptor_closed());
    assert!(!observer.failed());
    assert_eq!(observer.first_errno(), None);
}

#[cfg(feature = "native-paged-compose-probe")]
fn registered_line_font_case(mtm: MainThreadMarker) {
    use omni_markdown_core::MarkdownLineFontCatalog;
    use omni_md_plugin::native_surface::paged_line_fonts::{
        NativeMarkdownLineFont, NativeMarkdownLineFontError,
    };

    let native = NSFont::systemFontOfSize(15.0);
    let first =
        NativeMarkdownPagedFont::capture(native.as_ref(), MAXIMUM_FONT_BYTES, mtm, &|| true)
            .unwrap();
    let first_bytes = first.bytes_observer();
    let first_payload = first.core_font().resource_observer();
    let catalog = MarkdownLineFontCatalog::new();
    let registered = NativeMarkdownLineFont::register(first, &catalog, &|| true).unwrap();
    let original_registration = registered.registered().registration();
    registered.require(&original_registration).unwrap();
    assert!(registered.metrics().unwrap().ascent > 0.0);
    // 同一真实UIFont的另一次捕获有新Core payload；名称、轴位和所有度量相同仍不能消费旧glyph。
    let other =
        NativeMarkdownPagedFont::capture(native.as_ref(), MAXIMUM_FONT_BYTES, mtm, &|| true)
            .unwrap();
    let other_bytes = other.bytes_observer();
    let other = NativeMarkdownLineFont::register(other, &catalog, &|| true).unwrap();
    assert!(matches!(
        other.require(&original_registration),
        Err(NativeMarkdownLineFontError::FontMismatch)
    ));
    drop(registered);
    assert!(original_registration.lease().is_err());
    assert!(first_payload.payload_released());
    assert_released(&first_bytes); // catalog只有弱槽；旧注册不使匿名字体快照继续存活。
    drop(other);
    assert_released(&other_bytes);
    // 重用槽必须获得新代次；目录仍持有的旧弱票不得借新注册复活。
    let replacement =
        NativeMarkdownPagedFont::capture(native.as_ref(), MAXIMUM_FONT_BYTES, mtm, &|| true)
            .unwrap();
    let replacement_bytes = replacement.bytes_observer();
    let replacement = NativeMarkdownLineFont::register(replacement, &catalog, &|| true).unwrap();
    assert!(replacement.require(&original_registration).is_err());
    replacement
        .require(&replacement.registered().registration())
        .unwrap();
    drop(replacement);
    assert_released(&replacement_bytes);
    drop(catalog);
    println!(
        "native_line_font_registration original_payload=true foreign_capture_rejected=true expired_slot_rejected=true weak_catalog=true actual_unmap=true formal=false"
    );
}

#[cfg(feature = "native-paged-compose-probe")]
fn paragraph_style_case(mtm: MainThreadMarker) {
    use objc2::{AnyThread, runtime::AnyObject};
    use objc2_app_kit::{
        NSMutableParagraphStyle, NSParagraphStyle, NSTabColumnTerminatorsAttributeName,
        NSTextAlignment, NSTextTab, NSTextTabOptionKey,
    };
    use objc2_foundation::{NSArray, NSDictionary};
    use omni_markdown_core::MarkdownLineTabAlignment;
    use omni_md_plugin::native_surface::paged_paragraph_style::{
        NativeMarkdownParagraphStyleError, capture_paged_paragraph_style,
    };

    let original = NSParagraphStyle::defaultParagraphStyle();
    let captured = capture_paged_paragraph_style(&original, Some(false), mtm, &|| true).unwrap();
    let actual = original.tabStops();
    assert_eq!(captured.core_style().tab_stops.len(), actual.len());
    for ordinal in 0..actual.len() {
        let actual = actual.objectAtIndex(ordinal);
        assert_eq!(
            captured.core_style().tab_stops[ordinal].location_26_6,
            (actual.location() * 64.0).round() as i64
        );
    }
    assert_eq!(
        captured.core_style().default_tab_interval_26_6,
        Some((original.defaultTabInterval() * 64.0).round() as i64)
    );

    let empty = NSDictionary::<NSTextTabOptionKey, AnyObject>::new();
    let terminators = NSTextTab::columnTerminatorsForLocale(None);
    let terminator_object: &AnyObject = terminators.as_ref();
    // SAFETY: 真实AppKit key规定NSCharacterSet值，字典保存本测试原系统集合，未猜locale分隔符。
    let decimal_options = unsafe {
        NSDictionary::from_slices(&[NSTabColumnTerminatorsAttributeName], &[terminator_object])
    };
    let mut tabs = Vec::new();
    for (alignment, location, options) in [
        (NSTextAlignment::Left, 45.25, &*empty),
        (NSTextAlignment::Right, 90.5, &*empty),
        (NSTextAlignment::Center, 135.75, &*empty),
        (NSTextAlignment::Right, 181.0, &*decimal_options),
    ] {
        // SAFETY: 位置均有限递增，options为空或含实际NSCharacterSet且泛型与公开合同一致。
        tabs.push(unsafe {
            NSTextTab::initWithTextAlignment_location_options(
                NSTextTab::alloc(),
                alignment,
                location,
                options,
            )
        });
    }
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setTabStops(Some(&NSArray::from_retained_slice(&tabs)));
    paragraph.setDefaultTabInterval(36.25);
    let frozen = capture_paged_paragraph_style(&paragraph, Some(false), mtm, &|| true).unwrap();
    assert_eq!(frozen.core_style().default_tab_interval_26_6, Some(2320));
    assert_eq!(
        frozen
            .core_style()
            .tab_stops
            .iter()
            .map(|stop| stop.alignment)
            .collect::<Vec<_>>(),
        vec![
            MarkdownLineTabAlignment::Left,
            MarkdownLineTabAlignment::Right,
            MarkdownLineTabAlignment::Center,
            MarkdownLineTabAlignment::Decimal
        ]
    );
    let copied = frozen.terminators_for(3).unwrap();
    for scalar in [u32::from('.'), u32::from(','), 0x066b, 0x3002] {
        assert_eq!(
            copied.longCharacterIsMember(scalar),
            terminators.longCharacterIsMember(scalar)
        );
    }
    paragraph.setDefaultTabInterval(72.0);
    assert_eq!(frozen.core_style().default_tab_interval_26_6, Some(2320)); // 交出的值来自实际不可变快照。
    assert!(matches!(
        capture_paged_paragraph_style(&paragraph, Some(false), mtm, &|| false),
        Err(NativeMarkdownParagraphStyleError::Revoked)
    ));

    // Natural只有真实UBA段方向可消歧，不能沿用户语言或首字符猜方向。
    // SAFETY: 固定有效Natural枚举和有限正位置，空字典无异型值，返回由本栈接管的真实tab。
    let natural = unsafe {
        NSTextTab::initWithTextAlignment_location_options(
            NSTextTab::alloc(),
            NSTextAlignment::Natural,
            28.0,
            &empty,
        )
    };
    paragraph.setTabStops(Some(&NSArray::from_retained_slice(&[natural])));
    assert!(matches!(
        capture_paged_paragraph_style(&paragraph, None, mtm, &|| true),
        Err(NativeMarkdownParagraphStyleError::DirectionRequired)
    ));
    assert_eq!(
        capture_paged_paragraph_style(&paragraph, Some(true), mtm, &|| true)
            .unwrap()
            .core_style()
            .tab_stops[0]
            .alignment,
        MarkdownLineTabAlignment::Right
    );
    println!(
        "native_paragraph_style actual_default_stops=true actual_interval=true actual_decimal_terminators=true immutable_snapshot=true natural_direction_required=true revoked_rejected=true formal=false"
    );
}

fn main() {
    assert!(
        std::env::args_os().nth(1).is_none(),
        "字体控制不接受未知参数"
    );
    let mtm = MainThreadMarker::new().expect("字体控制必须在真实主线程执行");
    autoreleasepool(|_| {
        #[cfg(feature = "native-paged-compose-probe")]
        registered_line_font_case(mtm);
        #[cfg(feature = "native-paged-compose-probe")]
        paragraph_style_case(mtm);
        // 任意 UI 字体仍独立验证真实来源捕获、轴派生和归还；这不是正式文稿字体的排版支持承诺。
        // 生产文稿角色的默认 glyph/advance 严格对照由下方 punctuation 完整上下文用例承担。
        for (label, font) in [
            ("interface_system", NSFont::systemFontOfSize(15.0)),
            ("interface_bold", NSFont::boldSystemFontOfSize(15.0)),
            (
                "interface_monospace",
                NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0),
            ),
        ] {
            let core: &CTFont = font.as_ref();
            let actual = NativeMarkdownPagedFont::capture(core, MAXIMUM_FONT_BYTES, mtm, &|| true)
                .unwrap_or_else(|error| panic!("UI字体{label}的真实来源捕获机制失败：{error:?}"));
            let bytes = actual.bytes_observer();
            let metrics = capture_paged_font_metrics(&actual, &|| true).unwrap();
            assert!(metrics.values_for(&actual).unwrap().ascent > 0.0);
            let derived = actual
                .derive_axes(
                    &[MarkdownLayoutFontAxis {
                        tag: *b"wght",
                        value: 500.0,
                    }],
                    &|| true,
                )
                .unwrap();
            assert_eq!(
                derived.core_font().metadata().face_index,
                actual.core_font().metadata().face_index
            );
            assert_eq!(
                derived
                    .core_font()
                    .metadata()
                    .axes
                    .iter()
                    .find(|axis| axis.tag == *b"wght")
                    .unwrap()
                    .value,
                500.0
            );
            println!(
                "native_paged_font_ui_mechanism case={label} source={} exact_metrics=true document_shaping_parity_claimed=false",
                actual.source_face_postscript()
            );
            drop(actual);
            assert!(!bytes.mapping_drop_completed()); // 派生轴位共享原字体snapshot；不能新捕获同文件后谎称复用。
            drop(derived);
            assert_released(&bytes); // 仍活的metrics只存弱票，不得延长正式palette字体snapshot。
            drop(metrics);
        }
        let name = CFString::from_str("Helvetica");
        // SAFETY: 名称是固定有效CFString，18pt为有限正值，None矩阵无外部指针。
        let base = unsafe { CTFont::with_name(&name, 18.0, std::ptr::null()) };
        let latin = capture_case(&base, "Ab", "latin", mtm);
        // 再捕获同一个CTFont仍是不同Core Font allocation；旧度量/插入点不能被同名实例补签。
        let metrics = capture_paged_font_metrics(&latin, &|| true).unwrap();
        let carets = capture_paged_ligature_carets(&latin, 0, &|| true).unwrap();
        // 实际连字而非只有.notdef空列表：同一CTFont的公开API是此有限原语的独立oracle。
        let ligature_name = CFString::from_str("fi");
        // SAFETY: 真实主线程Font与有效固定名称贯穿同步getter，无外部指针。
        let glyph = unsafe { latin.native_font().glyph_with_name(&ligature_name) };
        assert_ne!(glyph, 0, "正式机器的Helvetica须含实际fi字形");
        let ligature = capture_paged_ligature_carets(&latin, u32::from(glyph), &|| true).unwrap();
        // SAFETY: null/0是公开的数量查询；原glyph已由相同Native Font返回。
        let count = unsafe {
            latin
                .native_font()
                .ligature_caret_positions(glyph, std::ptr::null_mut(), 0)
        };
        let mut expected = vec![f64::NAN; usize::try_from(count).unwrap()];
        if count > 0 {
            assert_eq!(
                // SAFETY: expected真实初始化count槽，Native同步写入同一字形的全部坐标。
                unsafe {
                    latin.native_font().ligature_caret_positions(
                        glyph,
                        expected.as_mut_ptr(),
                        count,
                    )
                },
                count
            );
        }
        assert_eq!(
            ligature.positions_for(&latin, u32::from(glyph)).unwrap(),
            expected
        );
        println!(
            "native_paged_font_geometry actual_ligature_glyph={glyph} gdef_carets={count} exact_native_metrics=true"
        );
        let calls = Cell::new(0usize);
        let current = || {
            let next = calls.get() + 1;
            calls.set(next);
            next < 4
        };
        assert!(matches!(
            capture_paged_font_metrics(&latin, &current),
            Err(NativeMarkdownPagedFontGeometryError::Revoked)
        ));
        calls.set(0);
        assert!(matches!(
            capture_paged_ligature_carets(&latin, u32::from(glyph), &current),
            Err(NativeMarkdownPagedFontGeometryError::Revoked)
        ));
        // 第四次权限核位于真实Native getter返回后；不能只测入口false而漏掉中途撤权。
        assert_eq!(calls.get(), 4);
        let other =
            NativeMarkdownPagedFont::capture(latin.native_font(), MAXIMUM_FONT_BYTES, mtm, &|| {
                true
            })
            .unwrap();
        let other_bytes = other.bytes_observer();
        assert!(matches!(
            metrics.values_for(&other),
            Err(NativeMarkdownPagedFontGeometryError::FontMismatch)
        ));
        assert!(matches!(
            carets.positions_for(&other, 0),
            Err(NativeMarkdownPagedFontGeometryError::FontMismatch)
        ));
        drop(other);
        assert_released(&other_bytes);
        let latin_bytes = latin.bytes_observer();
        let latin_font = latin.core_font().clone();
        let latin_payload = latin_font.resource_observer();
        drop(latin);
        assert!(!latin_bytes.mapping_drop_completed()); // 活Core Font仍保同一个snapshot Blob。
        drop(latin_font);
        assert!(latin_payload.payload_released());
        assert_released(&latin_bytes); // 最后Core Font归还真实VM，同时保留失败历史的独立门禁。
        // metrics/carets仍在当前栈中，真实snapshot已unmap，证明它们没有隐藏Font/Blob强边。
        drop((metrics, carets, ligature));

        let cjk = capture_case(&base, "中文", "cjk", mtm);
        assert!(
            cjk.core_font().metadata().face_count > 1,
            "当前正式机器须实际覆盖TTC，而非只测face0"
        );
        let original_axes = cjk.core_font().metadata().axes.clone();
        let bytes = cjk.bytes_observer();
        let weight = MarkdownLayoutFontAxis {
            tag: *b"wght",
            value: 550.0,
        };
        let varied = cjk
            .derive_axes(&[weight], &|| true)
            .expect("真实原CGFont派生wght550");
        coverage(varied.native_font(), "中文");
        assert_eq!(
            varied.core_font().metadata().face_index,
            cjk.core_font().metadata().face_index
        );
        assert_eq!(
            varied
                .core_font()
                .metadata()
                .axes
                .iter()
                .find(|axis| axis.tag == *b"wght")
                .unwrap()
                .value,
            550.0
        );
        assert_eq!(cjk.core_font().metadata().axes, original_axes); // Native派生不能改原不可变实例。
        assert!(matches!(
            cjk.derive_axes(&[weight, weight], &|| true),
            Err(NativeMarkdownPagedFontError::InvalidVariation)
        ));
        let current = Cell::new(false);
        assert!(matches!(
            cjk.derive_axes(&[weight], &|| current.get()),
            Err(NativeMarkdownPagedFontError::Revoked)
        ));
        assert!(matches!(
            NativeMarkdownPagedFont::capture(cjk.native_font(), MAXIMUM_FONT_BYTES, mtm, &|| false),
            Err(NativeMarkdownPagedFontError::Revoked)
        ));
        drop(cjk);
        assert!(!bytes.mapping_drop_completed()); // 派生仍持同一Blob；不创建第二份字体快照。
        drop(varied);
        assert_released(&bytes);

        let emoji = capture_case(&base, "😀", "emoji", mtm);
        let bytes = emoji.bytes_observer();
        drop(emoji);
        assert_released(&bytes);
        println!(
            "native_paged_font_control cases=4 actual_native_fonts=true drop_steps_verified=true kernel_unmapped_verified=true native_cache_deallocated_verified=false formal=false"
        );
        #[cfg(feature = "native-markdown-runtime-probe")]
        punctuation::run(mtm);
    });
}
