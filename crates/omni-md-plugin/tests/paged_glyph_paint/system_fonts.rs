//! 正式UIFont与独立CoreText有限oracle对照；完整字符串只限固定短样本，不进入产品。
use super::*;
use std::ptr::NonNull;

use objc2::{AnyThread, runtime::AnyObject};
use objc2_app_kit::{NSFont, NSFontAttributeName, NSFontDescriptorSymbolicTraits};
use objc2_core_text::{CTFontOrientation, CTLine, CTRun};
use objc2_foundation::{NSAttributedString, NSAttributedStringKey, NSDictionary, NSString};

const TEXT: &str = "office AV 0123456789 +-!= a\u{301}";
const MAX_GLYPHS: usize = 128;

pub(super) fn run(mtm: MainThreadMarker) {
    let mut cases = vec![
        ("body_system", NSFont::systemFontOfSize(32.0)),
        ("body_bold", NSFont::boldSystemFontOfSize(32.0)),
        (
            "source_monospace",
            NSFont::monospacedSystemFontOfSize_weight(32.0, 0.0),
        ),
        ("body_actual_15", NSFont::systemFontOfSize(15.0)),
        ("strong_actual_15", NSFont::boldSystemFontOfSize(15.0)),
        (
            "source_actual_14",
            NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0),
        ),
        ("heading_28", NSFont::boldSystemFontOfSize(28.0)),
        ("heading_24", NSFont::boldSystemFontOfSize(24.0)),
        ("heading_20", NSFont::boldSystemFontOfSize(20.0)),
        ("heading_18", NSFont::boldSystemFontOfSize(18.0)),
        ("heading_16", NSFont::boldSystemFontOfSize(16.0)),
        ("heading_15", NSFont::boldSystemFontOfSize(15.0)),
        ("hidden_marker", NSFont::systemFontOfSize(0.1)),
    ];
    // 正式palette的semibold与italic也要核实际Native结果；不能从32pt常规face推定全部样式。
    // SAFETY: AppKit公开静态权重常量在整个进程内有效，不来自用户或正文。
    let semibold = unsafe { objc2_app_kit::NSFontWeightSemibold };
    cases.push((
        "source_strong_actual_14",
        NSFont::monospacedSystemFontOfSize_weight(14.0, semibold),
    ));
    cases.push((
        "table_header_actual_14",
        NSFont::systemFontOfSize_weight(14.0, semibold),
    ));
    for (label, base) in [
        ("body_italic", NSFont::systemFontOfSize(15.0)),
        ("strong_italic", NSFont::boldSystemFontOfSize(15.0)),
        (
            "source_italic",
            NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0),
        ),
    ] {
        let descriptor = base.fontDescriptor();
        let descriptor = descriptor.fontDescriptorWithSymbolicTraits(
            descriptor.symbolicTraits() | NSFontDescriptorSymbolicTraits::TraitItalic,
        );
        let italic = NSFont::fontWithDescriptor_size(&descriptor, base.pointSize())
            .expect("正式palette的实际italic构造必须可观察，不能把base替补当italic通过");
        cases.push((label, italic));
    }
    assert_eq!(cases.len(), 18); // 固定角色集合与正文长度无关，不能遗漏难以捕获的角色。
    let mut mismatched_advances = 0;
    let mut mismatched_positions = 0;
    for (label, base) in cases {
        let before_advances = mismatched_advances;
        let before_positions = mismatched_positions;
        let native: &CTFont = base.as_ref();
        let font = NativeMarkdownPagedFont::capture(native, MAX_FONT_BYTES, mtm, &|| true)
            .unwrap_or_else(|error| panic!("UIFont {label} 的实际capture失败：{error:?}"));
        let font_bytes = font.bytes_observer();
        let (_file, document) = fixture(TEXT);
        let backing = document.original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let (mut job, source_authority, font_authority) = new_job(
            &source,
            &font,
            MarkdownShapingDirection::LeftToRight,
            *b"Latn",
            base.pointSize() as f32,
        );
        ready(&mut job, &source_authority, &font_authority);

        // 用相同原UIFont而非重建CGFont排版，独立观察其真正的glyph/cluster/advance。
        let string = NSString::from_str(TEXT);
        assert!(string.length() <= 64); // 固定有限oracle，不授权为任意Markdown正文创建CTLine。
        let object: &AnyObject = base.as_ref();
        // SAFETY: AppKit常量key不可变，实际NSFont是此属性公开规定的值类型。
        let attributes: objc2::rc::Retained<NSDictionary<NSAttributedStringKey, AnyObject>> =
            unsafe { NSDictionary::from_slices(&[NSFontAttributeName], &[object]) };
        // SAFETY: 字符串是上方完整固定样本，字典含正确Font类型，构造返回本栈独立owner。
        let attributed = unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &string,
                Some(&attributes),
            )
        };
        // SAFETY: Foundation/CFAttributedString公开桥接；原属性字符串持有相同UIFont和完整短样本。
        let line = unsafe { CTLine::with_attributed_string(attributed.as_ref()) };
        // SAFETY: 不可变line存活，Copy数组公开约定仅含CTRun。
        let runs = unsafe { line.glyph_runs() };
        let mut glyphs = [0u16; MAX_GLYPHS];
        let mut indices = [0isize; MAX_GLYPHS];
        let mut advances = [CGSize {
            width: 0.0,
            height: 0.0,
        }; MAX_GLYPHS];
        let mut positions = [CGPoint { x: 0.0, y: 0.0 }; MAX_GLYPHS];
        let mut count = 0;
        for ordinal in 0..runs.len() {
            // SAFETY: ordinal小于真实数组长度，公开CTRun数组保活该元素直到本轮比较结束。
            let raw = unsafe { runs.value_at_index(ordinal as _) };
            let pointer = NonNull::new(raw.cast_mut().cast::<CTRun>()).unwrap();
            // SAFETY: 上方指针来自公开CTLine真实run数组，非空且数组仍在本栈存活。
            let run = unsafe { pointer.as_ref() };
            // SAFETY: 同一不可变run的标量只读getter，无外部存储。
            let run_count = usize::try_from(unsafe { run.glyph_count() }).unwrap();
            assert!(run_count > 0 && run_count <= MAX_GLYPHS - count);
            let range = CFRange::new(0, run_count as _);
            // SAFETY: 四个独占数组各有完整run_count剩余槽，range恰为此真实run的全部glyph。
            unsafe {
                run.glyphs(range, NonNull::new(glyphs[count..].as_mut_ptr()).unwrap());
                run.string_indices(range, NonNull::new(indices[count..].as_mut_ptr()).unwrap());
                run.advances(range, NonNull::new(advances[count..].as_mut_ptr()).unwrap());
                run.positions(
                    range,
                    NonNull::new(positions[count..].as_mut_ptr()).unwrap(),
                );
            }
            count += run_count;
        }
        assert_eq!(
            job.profile().unwrap().glyph_count,
            count,
            "UIFont {label} 的实际glyph数量不同"
        );
        let (mut pen_x, mut pen_y) = (0i64, 0i64);
        for index in 0..count {
            let glyph = job
                .glyph(&source_authority, &font_authority, index)
                .unwrap()
                .unwrap();
            assert_eq!(
                glyph.glyph_id,
                u32::from(glyphs[index]),
                "UIFont {label} glyph={index}"
            );
            assert_eq!(
                glyph.source_utf16_cluster,
                usize::try_from(indices[index]).unwrap(),
                "UIFont {label} cluster={index}"
            );
            assert!(advances[index].width.is_finite() && advances[index].height.is_finite());
            let expected_x = (advances[index].width * 64.0).round() as i64;
            let expected_y = (advances[index].height * 64.0).round() as i64;
            if i64::from(glyph.x_advance) != expected_x || i64::from(glyph.y_advance) != expected_y
            {
                mismatched_advances += 1;
                let mut actual_glyph = glyphs[index];
                let mut raw_advance = CGSize {
                    width: 0.0,
                    height: 0.0,
                };
                // SAFETY: 两个独占栈槽各有一项；只读同一实际CTFont的单glyph原始度量以区别
                // HVAR/尺寸误差与排版后的kerning，不传入或重新排版本文。
                unsafe {
                    font.native_font().advances_for_glyphs(
                        CTFontOrientation::Horizontal,
                        NonNull::from(&mut actual_glyph),
                        &mut raw_advance,
                        1,
                    );
                }
                eprintln!(
                    "native_ui_font_advance_mismatch case={label} points={} index={index} glyph={} core_x={} line_x={expected_x} native_base_x={} native_base_raw={} actual_line_raw={} axes={:?}",
                    font.point_size(),
                    glyph.glyph_id,
                    glyph.x_advance,
                    (raw_advance.width * 64.0).round(),
                    raw_advance.width,
                    advances[index].width,
                    font.core_font().metadata().axes,
                );
            } // 所有差异记录完后统一严格判零；不放宽每glyph相等要求，便于发现全部实际角色。
            // Core采用26.6逐glyph舍入，累计坐标误差最多每项半个量子；不允许改glyph或吞字符。
            let tolerance = (index + 1) as f64 / 128.0 + 1e-9;
            let x = (pen_x + i64::from(glyph.x_offset)) as f64 / 64.0;
            let y = (pen_y + i64::from(glyph.y_offset)) as f64 / 64.0;
            if (positions[index].x - x).abs() > tolerance
                || (positions[index].y - y).abs() > tolerance
            {
                mismatched_positions += 1;
                eprintln!(
                    "native_ui_font_position_mismatch case={label} index={index} core_x={x} core_y={y} native_x={} native_y={} tolerance={tolerance}",
                    positions[index].x, positions[index].y,
                );
            } // 最后严格判零，既不放宽原累计舍入界限，也不因首个角色失败隐藏其他角色。
            pen_x = pen_x.checked_add(i64::from(glyph.x_advance)).unwrap();
            pen_y = pen_y.checked_add(i64::from(glyph.y_advance)).unwrap();
        }
        println!(
            "native_ui_font_oracle case={label} glyphs={count} exact_glyphs=true exact_clusters=true quantized_advances={} bounded_positions={} formal=false",
            mismatched_advances == before_advances,
            mismatched_positions == before_positions,
        );
        drop(job);
        drop(source);
        drop(document);
        drop(font);
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
        assert!(
            font_bytes.unmap_succeeded() && font_bytes.mapped_bytes() == 0 && !font_bytes.failed()
        );
    }
    assert_eq!(
        mismatched_advances, 0,
        "正式palette存在实际进距差异，不能进入Native总装"
    );
    assert_eq!(
        mismatched_positions, 0,
        "正式palette存在实际位置差异，不能进入Native总装"
    );
}
