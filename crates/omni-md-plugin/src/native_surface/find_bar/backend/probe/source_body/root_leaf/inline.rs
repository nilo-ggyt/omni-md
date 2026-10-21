//! Verify a complete short closed-inline frame on the original Source: actual fonts, code background,
//! link ink/targets, and strict release.

use super::*;
use crate::native_surface::paged_line_fonts::NativeMarkdownLineFont;
use crate::native_surface::paged_text_view::NativeMarkdownPagedTextScene;
use crate::native_surface::text_attributes::{
    MarkdownFontSelectionCache, MarkdownTextAttributePalette, native_markdown_document_fixed_font,
};
use objc2::{AnyThread, rc::Retained, runtime::AnyObject};
use objc2_app_kit::NSFontAttributeName;
use objc2_core_foundation::{CFRange, CGSize};
use objc2_core_graphics::{CGColor, CGFont};
use objc2_core_text::{
    CTFont, CTFontOrientation, CTFontSymbolicTraits as Traits, CTLine, CTRun, kCTFontAttributeName,
};
use objc2_foundation::{NSAttributedString, NSAttributedStringKey, NSDictionary, NSString};
use std::{io::Read, ops::Range, ptr::NonNull, rc::Weak};

const SOURCE: &str = concat!(
    "outside\n\n[recover](#inline-recover) plain **bold** *italic* ***both*** `wide` [go](https://example.com/a?b=1&amp;c=2) [    ](https://example.com/space)\n\n中文 **粗体** *斜体* ***粗斜***\n\n",
    "> 引文普通 **引文粗体\r\n> 引文续粗** *引文斜体* `引文代码\r\n> 引文续码` 引文末尾\r\n",
);

/// Derive canonical UTF-16 ranges from original UTF-8 fixture literals, without using parser or new
/// font parts as expectation.
fn source_range(text: &str) -> Range<usize> {
    let start = SOURCE.find(text).expect("固定inline样本持原字面");
    let start = SOURCE[..start].encode_utf16().count();
    start..start + text.encode_utf16().count()
}

/// Quote-prefix visibility belongs to actual physical lines; a multiline code event first line cannot
/// supply later-line selection authority.
fn quote_markers(
    rig: &Rig,
    active_middle: bool,
) -> Result<Vec<Weak<NativeMarkdownLineFont>>, String> {
    let parts = rig
        .body
        .probe_root_leaf_parts()
        .map_err(|cause| cause.to_string())?;
    autoreleasepool(|_| {
        let font = native_markdown_document_fixed_font(14.0).map_err(|cause| cause.to_string())?;
        let palette = MarkdownTextAttributePalette::new(font).map_err(|cause| cause.to_string())?;
        // All three lines share one quote paragraph with emphasis/code spanning actual CRLF; Find
        // selects only the middle physical line,
        // without revealing first/final-code-line prefixes or replacing physical-line endpoints with
        // arbitrary paragraph selection.
        for (line, active) in [
            ("> 引文普通", false),
            ("> 引文续粗", active_middle),
            ("> 引文续码", false),
        ] {
            let marker = source_range(line).start;
            for (offset, scalar) in ['>', ' '].into_iter().enumerate() {
                let part = parts
                    .iter()
                    .find(|part| part.source.contains(&(marker + offset)))
                    .ok_or("引用真实前缀缺少 sealed Font 与 Source 覆盖")?;
                // Check authority, actual role, and registered size together; HIDDEN bits alone cannot
                // exclude normal-size layout occupying space;
                // the current line must return to the original actual Native font rather than a
                // previous-frame hidden slot.
                if !part
                    .presentation
                    .contains(MarkdownPresentationStyles::MARKER)
                    || part
                        .presentation
                        .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
                        == active
                    || (!active
                        && (part.role != "hidden"
                            || part.points != 0.1_f32
                            || part.native_points != 0.1))
                    || (active && (part.role == "hidden" || part.native_points <= 1.0))
                {
                    return Err(format!(
                        "引用物理行前缀没有跟随真实 Find 选择显隐：line={line:?} active={active} source={:?} styles={:?} role={} points={}/{}",
                        part.source, part.presentation, part.role, part.points, part.native_points
                    ));
                }
                let font = part
                    .font_owner
                    .upgrade()
                    .ok_or("引用前缀 Font owner 已退休")?;
                // The original resolver chooses actual Font, then CTRun verifies face, axes, matrix,
                // and advance without PostScript-name guesses.
                let points = original_scalar_font(scalar, part.presentation, &font, &palette)?;
                if part.points != points as f32 || part.native_points != points {
                    return Err("引用前缀的实际注册 Font 不符合旧 Native 字号".into());
                }
            }
        }
        Ok(()) // Return short-scalar CTLines/attributes in this pool without retaining them across close alongside weak candidate-font observations.
    })?;
    Ok(parts.iter().map(|part| part.font_owner.clone()).collect())
}

/// Check fixed BMP Chinese character boxes and actual Scene hits without inferring correct paint
/// positions from style bits or font names.
fn quote_characters(scene: &dyn NativeMarkdownPagedTextScene) -> Result<(), String> {
    // Every label consists of complete BMP scalars, so each request occupies exactly one UTF-16 unit;
    // existing Native regressions cover emoji surrogates/graphemes separately, without split-character
    // expectations here.
    for label in [
        "引文普通",
        "引文粗体",
        "引文续粗",
        "引文斜体",
        "引文代码",
        "引文续码",
    ] {
        for index in source_range(label) {
            let first = scene
                .first_rect(NSRange::new(index, 1))
                .map_err(|cause| cause.to_string())?;
            if first.actual_range != NSRange::new(index, 1)
                || first.rect.size.width <= 0.0
                || first.rect.size.height <= 1.0
            {
                return Err(format!("引用原字符{index}没有精确非空 canonical 字符框"));
            }
            // This fixed Chinese text is LTR, so the character left interior should hit its canonical
            // unit;
            // actual boxes provide sample points while fixed source independently provides expected
            // indices, without deriving expectation from hit results.
            let inside = NSPoint::new(
                first.rect.origin.x + first.rect.size.width * 0.1,
                first.rect.origin.y + first.rect.size.height * 0.5,
            );
            let hit = scene.hit(inside).map_err(|cause| cause.to_string())?;
            if hit != index {
                return Err(format!(
                    "引用真实字符框命中{hit}，原 canonical 字符应为{index}"
                ));
            }
        }
    }
    Ok(())
}

/// Read-only Find/font preparation grants no write authority; streaming canonical/disk checks preserve
/// every CRLF, prefix, and inline-syntax character.
fn unchanged(rig: &Rig) -> Result<(), String> {
    let original = rig.document.upgrade().ok_or("inline 原 Doc 已提前归还")?;
    let document = original.borrow();
    // Find may change selection only; unchanged generation, lengths, and chunks exclude deleting syntax
    // merely to achieve correct display.
    if document.generation() != 0 || document.utf16_len() != SOURCE.encode_utf16().count() {
        return Err("inline 只读流程改变了原文档".into());
    }
    let mut expected = SOURCE.encode_utf16();
    let mut units = [0_u16; 128];
    // Verification reads bounded chunks of the actual mmap document, without another full-document
    // String or attributed graph.
    for offset in (0..document.utf16_len()).step_by(units.len()) {
        let used = units.len().min(document.utf16_len() - offset);
        document
            .copy_utf16_code_units(offset, &mut units[..used])
            .map_err(|cause| cause.to_string())?;
        if units[..used]
            .iter()
            .any(|unit| expected.next() != Some(*unit))
        {
            return Err("inline 显示或选择改写了原 canonical 字符".into());
        }
    }
    drop(document);
    drop(original); // End actual document borrowing before file I/O so verification is no second owner across Native callbacks.
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 128];
    // An unchanged document cannot prove no disk write; a separate descriptor checks original byte
    // chunks without normalizing CRLF or escapes.
    for expected in SOURCE.as_bytes().chunks(bytes.len()) {
        file.read_exact(&mut bytes[..expected.len()])
            .map_err(|cause| cause.to_string())?;
        if &bytes[..expected.len()] != expected {
            return Err("inline 只读流程改变了原磁盘字节".into());
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("inline 原磁盘出现额外尾部".into());
    }
    Ok(())
}

/// Observe before each actual Find action and borrow the new Scene only after its complete CPU frame
/// and target-visibility evidence arrive.
fn find_frame(
    rig: &mut Rig,
    label: &str,
    stage: &str,
) -> Result<NativeMarkdownPagedSceneKey, String> {
    rig.bar.query.setStringValue(&NSString::from_str(label));
    // Observe before actual dispatch for synchronous same-viewport commit and asynchronous same-request
    // recovery, without user waits or synthesized shortcuts.
    let frame = observe(&rig.body)?;
    match rig
        .action(MarkdownFindAction::Next)
        .map_err(|cause| cause.to_string())?
    {
        NativeMarkdownFindExecution::Complete => {}
        NativeMarkdownFindExecution::Pending(request) => rig.requests.push(request),
    }
    // The existing three-second complete-frame condition requires actual CPU paint; changed selection
    // coordinates alone cannot claim visibility.
    frame::wait_complete_frame(stage, &rig.body, frame)?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let target = source_range(label);
    let target = NSRange::new(target.start, target.len());
    // Expected label ranges use original literal UTF-16, without deriving canonical ranges from
    // candidate semantics/display indices.
    if key.request.input.primary_range != target {
        return Err(format!("{stage}: 原 Find 没有提交固定 canonical 标签范围"));
    }
    rig.view
        .verify_find_target_visible(&key, target)
        .map_err(|cause| cause.to_string())?;
    Ok(key)
}

/// Resolve a complete fixed scalar through original TextKit fallback, then inspect actual CTRun Font
/// without assuming an available Chinese italic face.
fn original_scalar_font(
    scalar: char,
    styles: MarkdownPresentationStyles,
    actual: &NativeMarkdownLineFont,
    palette: &MarkdownTextAttributePalette,
) -> Result<f64, String> {
    let resolved = MarkdownFontSelectionCache::resolve_uncached(styles, scalar, palette, false)
        .map_err(|cause| cause.to_string())?;
    let mut bytes = [0; 4];
    let string = NSString::from_str(scalar.encode_utf8(&mut bytes));
    let font: &AnyObject = resolved.font.as_ref();
    // SAFETY: The static AppKit font key matches actual NSFont; this dictionary/single-scalar string
    // lives in the local pool only.
    let attributes: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> =
        unsafe { NSDictionary::from_slices(&[NSFontAttributeName], &[font]) };
    // SAFETY: This complete scalar uses at most two UTF-16 units, with the actual original resolved
    // font in its attribute dictionary.
    let attributed = unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &string,
            Some(&attributes),
        )
    };
    // SAFETY: Use the public Foundation/CFAttributedString bridge for this scalar only, never a
    // document, long paragraph, or viewport fragment as complete text.
    let line = unsafe { CTLine::with_attributed_string(attributed.as_ref()) };
    // SAFETY: The original immutable short line retains all actual CTRuns; reads access no product
    // Source.
    let runs = unsafe { line.glyph_runs() };
    if runs.len() != 1 {
        return Err("固定完整scalar没有得到唯一真实Native run，不能挑一个字体冒充原结果".into());
    }
    // SAFETY: The verified actual array contains one public CTRun and retains its borrow through this
    // comparison.
    let run = unsafe { runs.value_at_index(0) };
    let run = NonNull::new(run.cast_mut().cast::<CTRun>()).ok_or("原Native run为空")?;
    // SAFETY: The pointer comes from the live public run array; getters read that immutable original
    // object only.
    let (attributes, matrix, range) = unsafe {
        let run = run.as_ref();
        (run.attributes(), run.text_matrix(), run.string_range())
    };
    // SAFETY: The public Core Text font key holds CTFont, with its dictionary retaining the actual
    // final run font.
    let font = unsafe { attributes.value(std::ptr::from_ref(kCTFontAttributeName).cast()) };
    let font = NonNull::new(font.cast_mut().cast::<CTFont>()).ok_or("原Native run没有Font")?;
    // SAFETY: Original dictionary/line/run stay on this stack; read-only font queries borrow no product
    // graph.
    let expected = unsafe { font.as_ref() };
    let actual = actual.native().native_font();
    // SAFETY: Both owners retain original main-thread fonts; copy actual CGFonts rather than treating
    // UI role aliases as file faces.
    let (actual_graphics, expected_graphics, actual_axes, expected_axes) = unsafe {
        (
            actual.graphics_font(std::ptr::null_mut()),
            expected.graphics_font(std::ptr::null_mut()),
            actual.variation(),
            expected.variation(),
        )
    };
    let actual_name = CGFont::table_for_tag(Some(&actual_graphics), u32::from_be_bytes(*b"name"))
        .ok_or("分页实际CGFont没有真实name表")?;
    let expected_name =
        CGFont::table_for_tag(Some(&expected_graphics), u32::from_be_bytes(*b"name"))
            .ok_or("原Native CGFont没有真实name表")?;
    // Absent/empty variation dictionaries both denote original defaults; compare nonempty axes by CF
    // numeric equality rather than enumeration order.
    let same_axes = actual_axes.as_deref().filter(|axes| !axes.is_empty())
        == expected_axes.as_deref().filter(|axes| !axes.is_empty());
    // SAFETY: Both CTFonts live while reading independent actual Native metrics, without deriving
    // references from product geometry caches.
    let metrics = |font: &CTFont| unsafe {
        [
            font.ascent(),
            font.descent(),
            font.leading(),
            font.cap_height(),
            font.x_height(),
            font.underline_position(),
            font.underline_thickness(),
        ]
    };
    let same_metrics = metrics(actual) == metrics(expected);
    // SAFETY: Compare original name tables as actual CFData bytes while independently verifying size,
    // traits, matrix, and axes.
    let equal = actual_name == expected_name
        && same_axes
        && same_metrics
        && unsafe {
            actual.size() == expected.size()
                && actual.symbolic_traits() == expected.symbolic_traits()
                && actual.matrix() == expected.matrix()
        };
    let mut units = [0_u16; 2];
    let used = scalar.encode_utf16(&mut units).len();
    let mut glyphs = [0_u16; 2];
    let (mut native_glyph, mut actual_advance, mut native_advance) =
        (0_u16, CGSize::ZERO, CGSize::ZERO);
    // SAFETY: The line retains the original run; this scalar must have one actual glyph and each output
    // has complete exclusive storage.
    let same_glyph = unsafe {
        let run = run.as_ref();
        if run.glyph_count() != 1 {
            return Err("固定完整scalar的原Native glyph不能被截成单个比较".into());
        }
        run.glyphs(CFRange::new(0, 1), NonNull::from(&mut native_glyph));
        run.advances(CFRange::new(0, 1), NonNull::from(&mut native_advance));
        let covered = actual.glyphs_for_characters(
            NonNull::new(units.as_mut_ptr()).unwrap(),
            NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            used as _,
        );
        actual.advances_for_glyphs(
            CTFontOrientation::Horizontal,
            NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            &mut actual_advance,
            1,
        );
        covered && glyphs[0] == native_glyph && actual_advance == native_advance
    };
    if !equal
        || !same_glyph
        || range.location != 0
        || usize::try_from(range.length).ok() != Some(scalar.len_utf16())
        || (matrix.a, matrix.b, matrix.c, matrix.d, matrix.tx, matrix.ty)
            != (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
    {
        return Err(format!(
            "实际分页Font与原TextKit回退/CTRun不符：scalar={scalar:?} styles={styles:?} font_equal={equal} name_table_equal={} axes_equal={same_axes} metrics_equal={same_metrics} glyph_equal={same_glyph} glyph={:?}/{native_glyph} advance={actual_advance:?}/{native_advance:?} original_run_matrix={matrix:?} original_range={range:?}",
            actual_name == expected_name,
            glyphs[0]
        ));
    }
    // SAFETY: Return Copy point size only, releasing local line/attributes/font before product close
    // rather than retaining the old Native graph.
    Ok(unsafe { expected.size() })
}

/// Actual independent Core Graphics drawing quantizes declared preview colors instead of guessing sRGB
/// bytes by multiplying floats by 255.
fn pixel(color: [f64; 4]) -> Result<[u8; 4], String> {
    let canvas = selection::Canvas::scaled(NSRect::new(NSPoint::ZERO, NSSize::new(1.0, 1.0)), 1.0)?;
    // The independent reference explicitly uses Source sRGB; device-dependent DeviceRGB cannot define
    // these declared component expectations.
    // Create a CGColor for this fixed one-pixel reference without product Scene/viewport color-setting
    // methods.
    let paint = CGColor::new_srgb(color[0], color[1], color[2], color[3]);
    CGContext::set_fill_color_with_color(Some(&canvas.context), Some(&paint));
    CGContext::fill_rect(
        Some(&canvas.context),
        NSRect::new(NSPoint::ZERO, NSSize::new(1.0, 1.0)),
    );
    CGContext::flush(Some(&canvas.context));
    Ok(canvas.pixels[..4].try_into().expect("独立单像素RGBA"))
}

/// Character-box masks share actual window backing scale; blue in a spaces-only link must come from its
/// actual underline.
fn colored_pixels(
    scene: &dyn NativeMarkdownPagedTextScene,
    key: &NativeMarkdownPagedSceneKey,
    actual: &selection::Canvas,
    scale: f64,
    range: Range<usize>,
    expected: [u8; 4],
) -> Result<usize, String> {
    let mask = selection::Canvas::scaled(key.request.viewport, scale)?;
    CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
    for index in range {
        let first = scene
            .first_rect(NSRange::new(index, 1))
            .map_err(|cause| cause.to_string())?;
        if first.actual_range != NSRange::new(index, 1) || first.rect.size.width <= 0.0 {
            return Err("真实inline字符没有原单元非空几何".into());
        }
        CGContext::fill_rect(Some(&mask.context), first.rect);
    }
    CGContext::flush(Some(&actual.context));
    CGContext::flush(Some(&mask.context));
    Ok(actual
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(mask.pixels.as_chunks::<4>().0)
        .filter(|(value, region)| **region == [255, 0, 0, 255] && **value == expected)
        .count())
}

/// Retain finite pure facts inside four spaces only; return product graph/bitmap before constructing
/// independent original Native layout.
struct WhitespaceUnderline {
    viewport: NSRect,
    document_origin_y: f64,
    width: f64,
    scale: f64,
    span: NSRect,
    metrics: crate::native_surface::paged_font_geometry::NativeMarkdownPagedFontMetricValues,
    pixels: Vec<(usize, [u8; 4])>,
}

/// Carry pure actual CJK-part facts across strict close only; construct complete original Native
/// context after all candidate resources return.
struct InlineAdvance {
    width: f64,
    parts: Vec<super::paragraph_oracle::PartAdvance>,
}

/// Actual boxes select pixels to compare only; box bottom plus Core Text descent cannot infer Preview
/// baseline under system line metrics.
fn capture_whitespace_underline(
    scene: &dyn NativeMarkdownPagedTextScene,
    key: &NativeMarkdownPagedSceneKey,
    actual: &selection::Canvas,
    scale: f64,
    range: Range<usize>,
    metrics: crate::native_surface::paged_font_geometry::NativeMarkdownPagedFontMetricValues,
) -> Result<WhitespaceUnderline, String> {
    let mask = selection::Canvas::scaled(key.request.viewport, scale)?;
    CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
    let mut span: Option<NSRect> = None;
    for index in range {
        let first = scene
            .first_rect(NSRange::new(index, 1))
            .map_err(|cause| cause.to_string())?;
        if first.actual_range != NSRange::new(index, 1) || first.rect.size.width <= 0.0 {
            return Err("原空白链接没有精确非空GCB几何".into());
        }
        span = Some(span.map_or(first.rect, |span| {
            objc2_foundation::NSUnionRect(span, first.rect)
        }));
        CGContext::fill_rect(Some(&mask.context), first.rect);
    }
    let span = span.expect("本固定合成label含四个真实空格");
    for context in [&actual.context, &mask.context] {
        CGContext::flush(Some(context));
    }
    let pixels = actual
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(mask.pixels.as_chunks::<4>().0)
        .enumerate()
        .filter_map(|(index, (actual, region))| {
            (*region == [255, 0, 0, 255]).then_some((index, *actual))
        })
        .collect(); // Retain RGBA inside four character boxes only, without complete bitmap, Native context, Scene, or Reader.
    Ok(WhitespaceUnderline {
        viewport: key.request.viewport,
        document_origin_y: key.request.virtual_viewport.document_origin.y,
        width: ((key.request.virtual_viewport.layout_width - 16.0) * 64.0).round() / 64.0,
        scale,
        span,
        metrics,
        pixels,
    })
}

/// After product-graph close, reference baseline comes from a complete same-width TextKit short
/// fixture; compare Core Graphics RGBA exactly.
fn whitespace_underline_pixels(sample: &WhitespaceUnderline) -> Result<(usize, usize), String> {
    let font = native_markdown_document_fixed_font(14.0).map_err(|cause| cause.to_string())?;
    let original = super::paragraph_oracle::measure(SOURCE, sample.width, &font, &[])?;
    drop(font);
    let spaces = source_range("[    ]");
    let label = spaces.start + 1..spaces.end - 1;
    let original = original
        .iter()
        .find(|line| line.source.start <= label.start && label.end <= line.source.end)
        .ok_or("独立旧 Native 短例没有完整覆盖原四个空格的真实视觉行")?;
    // Convert original window Y-up coordinates/remove existing eight-point padding only, without
    // candidate paragraph/font-line policy.
    let baseline =
        sample.document_origin_y + sample.viewport.size.height - original.baseline.y - 8.0;
    let bottom = sample.document_origin_y + sample.viewport.size.height
        - original.frame.origin.y
        - original.frame.size.height
        - 8.0;
    let tolerance = 1.0 / 64.0;
    if (sample.span.origin.y - bottom).abs() > tolerance
        || (sample.span.size.height - original.frame.size.height).abs() > tolerance
    {
        return Err(format!(
            "空白 link 实际字符框与独立旧 Native 行槽不同：actual={:?} original_source={:?} original_frame={:?} original_baseline={:?} expected_bottom={bottom} tolerance={tolerance}",
            sample.span, original.source, original.frame, original.baseline
        ));
    }
    let reference = selection::Canvas::scaled(sample.viewport, sample.scale)?;
    CGContext::set_rgb_fill_color(Some(&reference.context), 1.0, 1.0, 1.0, 1.0);
    CGContext::fill_rect(Some(&reference.context), sample.viewport);
    let metrics = sample.metrics;
    let line = NSRect::new(
        NSPoint::new(
            sample.span.origin.x,
            baseline + metrics.underline_position - metrics.underline_thickness / 2.0,
        ),
        NSSize::new(sample.span.size.width, metrics.underline_thickness),
    );
    // The original TextKit baseline provides actual underline geometry; independent sRGB color
    // semantics must not drift with device space.
    let link_color = CGColor::new_srgb(0.40, 0.68, 1.0, 1.0);
    CGContext::set_fill_color_with_color(Some(&reference.context), Some(&link_color));
    CGContext::fill_rect(Some(&reference.context), line);
    CGContext::flush(Some(&reference.context));
    let white = pixel([1.0; 4])?;
    let blue = pixel([0.40, 0.68, 1.0, 1.0])?;
    let (mut ink, mut opaque) = (0, 0);
    for (index, actual) in &sample.pixels {
        let expected = &reference.pixels.as_chunks::<4>().0[*index];
        if actual != expected {
            return Err(format!(
                "空白link真实RGBA不等于独立旧 Native 基线CG参考：line={line:?} scale={} original_baseline={:?} actual={actual:?} expected={expected:?}",
                sample.scale, original.baseline
            ));
        }
        ink += usize::from(*expected != white);
        opaque += usize::from(*actual == blue);
    }
    if ink == 0 {
        return Err(format!(
            "独立CG参考未在原空白字符框产生真实线墨迹：line={line:?} scale={}",
            sample.scale
        ));
    }
    Ok((ink, opaque))
}

/// After primary failure, observe existing authority facts only without layout, run-loop advancement,
/// or retry using successor Ready.
fn verification_failure(
    rig: &Rig,
    original: &NativeMarkdownPagedSceneKey,
    stage: &str,
    cause: String,
) -> String {
    let request =
        |request: &crate::native_surface::paged_text_view::NativeMarkdownPagedSceneRequest| {
            format!(
                "generation={} selection={:?} marked={:?} utf16={} geometry={} viewport={:?} bounds_width={} frame={:?} virtual={:?}",
                request.input.generation,
                request.input.primary_range,
                request.input.marked,
                request.input.utf16_len,
                request.geometry_epoch,
                request.viewport,
                request.bounds_width,
                request.frame_in_parent,
                request.virtual_viewport
            )
        };
    let ready = rig.view.capture_ready_scene_key().map(|current| {
        format!(
            "same_key={} same_source={} same_nonce={} {}",
            current.same(original),
            current.source == original.source,
            current.nonce.same(&original.nonce),
            request(&current.request)
        )
    });
    let actual = rig
        .view
        .capture_scene_request()
        .map(|actual| request(&actual));
    let progress = rig.body.progress().map(|progress| {
        (
            progress.phase,
            progress.scheduled_serial,
            progress.producer_serial,
            progress.preparing,
            progress.font_trial_in_flight,
            progress.prepared_lines,
            progress.prepared_height,
            progress.complete,
            progress.revoked_preparations,
        )
    });
    format!(
        "inline.{stage}: {cause}; original=[{}]; current_ready={ready:?}; actual_request={actual:?}; body=(phase,scheduled,serial,preparing,font_in_flight,lines,height,complete,revoked){progress:?}",
        request(&original.request)
    )
}

/// Inspect actual sealed-part Native faces/metrics, then consume same-Scene actual pixels and
/// Source-bound targets.
fn verify(
    rig: &mut Rig,
) -> Result<
    (
        Vec<Weak<NativeMarkdownLineFont>>,
        WhitespaceUnderline,
        InlineAdvance,
    ),
    String,
> {
    // This synchronous NSResponder fixture bypasses NSApplication.sendEvent; establish actual window
    // focus before capturing the original Scene ticket.
    rig.panel.makeKeyWindow();
    if !rig.panel.isKeyWindow() {
        return Err("inline链接夹具没有取得原keyWindow".into());
    }
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| format!("inline.capture_initial_ready: {cause}"))?;
    let stage = std::cell::Cell::new("capture_parts");
    let result = (|| -> Result<_, String> {
        let parts = rig
            .body
            .probe_root_leaf_parts()
            .map_err(|cause| cause.to_string())?;
        // Actual registered Native traits and size must match each fixed ASCII role, rather than style
        // flags alone.
        stage.set("ascii_fonts");
        for (label, bold, italic, mono, points) in [
            ("plain", false, false, true, 14.0),
            ("bold", true, false, false, 15.0),
            ("italic", false, true, true, 14.0),
            ("both", true, true, false, 15.0),
            ("wide", false, false, true, 14.0),
        ] {
            for index in source_range(label) {
                let part = parts
                    .iter()
                    .find(|part| part.source.contains(&index))
                    .ok_or("inline内容缺少真实sealed part")?;
                if part.points != points
                    || part.native_points != f64::from(points)
                    || part.traits.contains(Traits::TraitBold) != bold
                    || part.traits.contains(Traits::TraitItalic) != italic
                    || part.traits.contains(Traits::TraitMonoSpace) != mono
                    || part.metrics.ascent <= 0.0
                    || part.metrics.descent < 0.0
                    || part.metrics.ascent + part.metrics.descent <= 0.0
                {
                    return Err(format!(
                        "inline实际face/字号/度量不符：label={label}, source={:?}, traits={:?}, points={}, native={}, metrics={:?}",
                        part.source, part.traits, part.points, part.native_points, part.metrics
                    ));
                }
            }
        }
        // CJK fallback is checked against independent original Native results; semantic emphasis does
        // not guarantee an italic face exists.
        stage.set("cjk_original_font");
        let mut advance_parts = Vec::<super::paragraph_oracle::PartAdvance>::new();
        autoreleasepool(|_| -> Result<(), String> {
            let font =
                native_markdown_document_fixed_font(14.0).map_err(|cause| cause.to_string())?;
            let palette =
                MarkdownTextAttributePalette::new(font).map_err(|cause| cause.to_string())?;
            for (label, bold, italic, quote, code) in [
                ("中文", false, false, false, false),
                ("粗体", true, false, false, false),
                ("斜体", false, true, false, false),
                ("粗斜", true, true, false, false),
                ("引文普通", false, false, true, false),
                ("引文粗体", true, false, true, false),
                ("引文续粗", true, false, true, false),
                ("引文斜体", false, true, true, false),
                ("引文代码", false, false, true, true),
                ("引文续码", false, false, true, true),
            ] {
                let label_range = source_range(label);
                let mut position = label_range.start;
                for scalar in label.chars() {
                    let part = parts
                        .iter()
                        .find(|part| part.source.contains(&position))
                        .ok_or("中文内容没有真实sealed part")?;
                    for (style, expected) in [
                        (MarkdownPresentationStyles::STRONG, bold),
                        (MarkdownPresentationStyles::EMPHASIS, italic),
                        (MarkdownPresentationStyles::BLOCK_QUOTE, quote),
                        (MarkdownPresentationStyles::INLINE_CODE, code),
                    ] {
                        if part.presentation.contains(style) != expected {
                            return Err(format!("中文{label}缺少原粗体、斜体、引用或代码组合"));
                        }
                    }
                    if !part
                        .presentation
                        .without(MarkdownPresentationStyles::STRONG)
                        .without(MarkdownPresentationStyles::EMPHASIS)
                        .without(MarkdownPresentationStyles::BLOCK_QUOTE)
                        .without(MarkdownPresentationStyles::INLINE_CODE)
                        .is_empty()
                    {
                        return Err(format!(
                            "中文{label}混入其他字体角色，或丢失原引用/代码角色"
                        ));
                    } // Plain/quote/code have fixed original roles; require the exact style combination before original font resolution, without arbitrary extra bits.
                    let font = part
                        .font_owner
                        .upgrade()
                        .ok_or("中文实际Font owner已退休")?;
                    let points = original_scalar_font(scalar, part.presentation, &font, &palette)?;
                    if part.points != points as f32
                        || part.native_points != points
                        || part.traits != font.traits()
                    {
                        return Err("中文sealed part字号或trait没有来自同一个实际注册Font".into());
                    }
                    position += scalar.len_utf16();
                }
                // Retain pure complete-part facts first; do not reflow isolated punctuation/words, and
                // defer the full original reference until candidate close.
                for part in parts.iter().filter(|part| {
                    part.source.start < label_range.end && label_range.start < part.source.end
                }) {
                    if !advance_parts
                        .iter()
                        .any(|actual| actual.source == part.source)
                    {
                        advance_parts.push(super::paragraph_oracle::PartAdvance {
                            source: part.source.clone(),
                            advance_26_6: part.advance_26_6,
                        });
                    }
                }
            }
            // Verify a whitespace link full Font through original resolver/actual CTRun too; later
            // underline reference cannot simply trust candidate thickness.
            let index = source_range("[    ]").start + 1;
            let part = parts
                .iter()
                .find(|part| part.source.contains(&index))
                .ok_or("空白链接缺少真实 sealed Font")?;
            let font = part
                .font_owner
                .upgrade()
                .ok_or("空白链接的原 Font owner 已退休")?;
            original_scalar_font(' ', part.presentation, &font, &palette)?;
            Ok(()) // Return all temporary reference Native claims in this pool before original frame/pixel work and final font-release checks.
        })?;
        println!("native_root_inline_stage stage=cjk_original_font_complete");
        let mut owners: Vec<_> = parts.iter().map(|part| part.font_owner.clone()).collect();
        stage.set("quote_inactive_markers");
        owners.extend(quote_markers(rig, false)?);
        let current = || {
            rig.view
                .capture_ready_scene_key()
                .is_ok_and(|actual| actual.same(&key))
        };
        stage.set("first_ready_scene");
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
            .map_err(|cause| cause.to_string())?;
        // Draw at actual backing scale and verify subpixel lines against independent antialiased Core
        // Graphics RGBA per pixel,
        // rather than treating presence/absence of opaque blue pixels as proof of line existence.
        stage.set("first_scale_canvas");
        let scale = rig.panel.backingScaleFactor();
        let actual = selection::Canvas::scaled(key.request.viewport, scale)?;
        stage.set("first_cpu_paint");
        let steps = selection::paint_scene(scene.clone(), &key, &actual, &current)?;
        let code = source_range("wide");
        let code_background = pixel([0.13, 0.15, 0.20, 1.0])?;
        let link_color = pixel([0.40, 0.68, 1.0, 1.0])?;
        stage.set("code_pixels");
        let code_pixels =
            colored_pixels(&*scene, &key, &actual, scale, code.clone(), code_background)?;
        stage.set("link_pixels");
        let link_pixels = colored_pixels(
            &*scene,
            &key,
            &actual,
            scale,
            source_range("go"),
            link_color,
        )?;
        let spaces = source_range("[    ]");
        let label = spaces.start + 1..spaces.end - 1;
        let metrics = parts
            .iter()
            .find(|part| part.source.contains(&label.start))
            .ok_or("空白link缺少真实字体度量")?
            .metrics;
        stage.set("capture_underline_pixels");
        let underline =
            capture_whitespace_underline(&*scene, &key, &actual, scale, label, metrics)?;
        if code_pixels == 0 || link_pixels == 0 {
            return Err(format!(
                "inline原Scene像素缺少代码底色或真实link文字：scale={scale} code={code_pixels} link={link_pixels}"
            ));
        }
        // The same actual CPU quote graph must retain Chinese ink, block background, and multiline-code
        // background together;
        // colors come from the original palette external contract instead of copying candidate default
        // foreground into reference expectations.
        stage.set("quote_character_boxes_and_pixels");
        quote_characters(&*scene)?;
        let quote_background = pixel([0.10, 0.12, 0.16, 1.0])?;
        let quote_ink = pixel([0.58, 0.66, 0.78, 1.0])?;
        for label in [
            "引文普通",
            "引文粗体",
            "引文续粗",
            "引文斜体",
            "引文代码",
            "引文续码",
        ] {
            if colored_pixels(
                &*scene,
                &key,
                &actual,
                scale,
                source_range(label),
                quote_ink,
            )? == 0
            {
                return Err(format!("引用{label}没有按旧 Native 约定绘制真实中文墨迹"));
            }
        }
        if colored_pixels(
            &*scene,
            &key,
            &actual,
            scale,
            source_range("引文普通"),
            quote_background,
        )? == 0
        {
            return Err("引用普通中文的实际字符框内缺少块背景".into());
        }
        for label in ["引文代码", "引文续码"] {
            if colored_pixels(
                &*scene,
                &key,
                &actual,
                scale,
                source_range(label),
                code_background,
            )? == 0
            {
                return Err(format!("引用跨行代码{label}丢失原真实代码底色"));
            }
        }
        drop((actual, scene)); // Return paint Reader/bitmap first; target recovery borrows the original sealed Scene without carrying a test graph into later quanta.
        stage.set("restore_link_targets");
        let press = super::links::point(rig, source_range("recover").start)?;
        super::links::command_mouse(rig, press, objc2_app_kit::NSEventType::LeftMouseDown)?;
        super::links::command_mouse(rig, press, objc2_app_kit::NSEventType::LeftMouseUp)?;
        // After the final CJK block, an actual same-block fragment click drives unique-request recovery
        // without clicking its HTTPS target.
        // The original bounded closed-block parser extracted actual recover/go labels and decoded
        // targets together;
        // positive link facts require no larger style window; this fixture lacks the heading and
        // verifies explicit NotFound after actual Source lookup.
        if !matches!(
            super::links::settle(rig)?,
            Some(ViewError::LinkAnchorNotFound)
        ) {
            return Err("inline真实片段click没有完成原目标恢复及明确标题未找到".into());
        }
        let recovered = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        if !recovered.same(&key) {
            return Err("inline目标恢复改变原Source、Scene nonce或输入/几何票".into());
        }
        stage.set("link_target");
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
            .map_err(|cause| cause.to_string())?;
        let label = source_range("go");
        let first = scene
            .first_rect(NSRange::new(label.start, 1))
            .map_err(|cause| cause.to_string())?;
        let inside = NSPoint::new(
            first.rect.origin.x + first.rect.size.width * 0.25,
            first.rect.origin.y + first.rect.size.height / 2.0,
        );
        if scene
            .link_target(inside)
            .map_err(|cause| cause.to_string())?
            .as_deref()
            != Some("https://example.com/a?b=1&c=2")
        {
            return Err("链接没有从原Parser的解码目标与当前Source/publication取得真实URL".into());
        }
        // Hidden syntax remains Source-backed but must not become a clickable link label.
        stage.set("marker_target");
        let marker = source_range("[go]").start;
        let first = scene
            .first_rect(NSRange::new(marker, 1))
            .map_err(|cause| cause.to_string())?;
        let point = NSPoint::new(
            first.rect.origin.x + first.rect.size.width / 2.0,
            first.rect.origin.y + first.rect.size.height / 2.0,
        );
        if scene
            .link_target(point)
            .map_err(|cause| cause.to_string())?
            .is_some()
        {
            return Err("隐藏Markdown marker被错误当成可点击链接正文".into());
        }
        drop(scene); // Return the original Reader before Find selection changes; old frames cannot certify new marker visibility.
        stage.set("find_wide");
        let selected = find_frame(rig, "wide", "root_leaf_inline_code_selection")?;
        if selected.request.input.primary_range != NSRange::new(code.start, code.len())
            || rig.copy::<4>(code.start)? != [119, 105, 100, 101]
        {
            return Err("inline代码Find没有保持原canonical字符及精确范围".into());
        }
        let current = || {
            rig.view
                .capture_ready_scene_key()
                .is_ok_and(|actual| actual.same(&selected))
        };
        stage.set("selected_ready_scene");
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &selected.request)
            .map_err(|cause| cause.to_string())?;
        let scale = rig.panel.backingScaleFactor();
        let actual = selection::Canvas::scaled(selected.request.viewport, scale)?;
        stage.set("selected_cpu_paint");
        selection::paint_scene(scene.clone(), &selected, &actual, &current)?;
        stage.set("selected_pixels");
        if colored_pixels(
            &*scene,
            &selected,
            &actual,
            scale,
            code.clone(),
            selection::selection_pixel()?,
        )? == 0
            || colored_pixels(&*scene, &selected, &actual, scale, code, code_background)? != 0
        {
            return Err("真实代码底色覆盖了原选区反馈或Find选区没有绘制".into());
        }
        drop((actual, scene)); // Return the old graph before selection changes so the quote successor original prefix Reader can actually recover.
        stage.set("quote_find_middle_line");
        let quote = find_frame(rig, "引文斜体", "root_leaf_quote_middle_line_selection")?;
        if quote.source != key.source
            || rig.copy::<4>(source_range("引文斜体").start)?
                != ['引', '文', '斜', '体'].map(|scalar| scalar as u16)
        {
            return Err("引用 Find 改变了原 Source 或 canonical 中文".into());
        }
        // Reacquire actual parts/weak font identities after selection change; equal Source in an old
        // frame cannot certify new marker visibility.
        owners.extend(quote_markers(rig, true)?);
        let current = || {
            rig.view
                .capture_ready_scene_key()
                .is_ok_and(|actual| actual.same(&quote))
        };
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &quote.request)
            .map_err(|cause| cause.to_string())?;
        quote_characters(&*scene)?;
        let actual = selection::Canvas::scaled(quote.request.viewport, scale)?;
        // The new Scene certifies bounded painting for its actual context; old bitmap reuse could hide
        // failure to reveal this line.
        selection::paint_scene(scene.clone(), &quote, &actual, &current)?;
        let marker = source_range("> 引文续粗").start;
        if colored_pixels(
            &*scene,
            &quote,
            &actual,
            scale,
            marker..marker + 1,
            pixel([0.48, 0.56, 0.68, 1.0])?,
        )? == 0
            || colored_pixels(
                &*scene,
                &quote,
                &actual,
                scale,
                source_range("引文斜体"),
                selection::selection_pixel()?,
            )? == 0
        {
            return Err("引用 Find 没有实绘当前物理行的 marker 墨迹或选区反馈".into());
        }
        drop((actual, scene)); // Return the new paint Reader before another Find; old geometry cannot certify later hidden-marker state.
        stage.set("quote_find_leave_line");
        let outside = find_frame(rig, "plain", "root_leaf_quote_leave_line_selection")?;
        if outside.source != key.source {
            return Err("离开引用行的 Find 改变了原 Source 身份".into());
        }
        // Leaving the physical line must restore actual hidden size, catching selection visibility that
        // reveals markers without hiding them again.
        owners.extend(quote_markers(rig, false)?);
        unchanged(rig)?;
        stage.set("remember_receipt");
        rig.remember_receipt()?;
        println!(
            "native_root_inline actual_traits=true strong_emphasis_combined=true cjk_original_textkit_run_fonts=true cjk_original_run_matrix=true whitespace_original_textkit_run_font=true scoped_quote_cjk_run_fonts=true scoped_quote_actual_ink_background=true scoped_quote_canonical_boxes_and_hits=true scoped_quote_physical_marker_find_reveal_hide=true source_and_disk_unchanged=true inline_code_monospace=true code_background_pixels={code_pixels} link_color_pixels={link_pixels} actual_source_bound_target=true target_recovered_by_original_click=true marker_not_clickable=true selection_over_code=true actual_backing_scale={scale} paint_steps={steps} external_url_open_tested=false whole_product=false formal=false"
        );
        Ok((
            owners,
            underline,
            InlineAdvance {
                width: ((key.request.virtual_viewport.layout_width - 16.0) * 64.0).round() / 64.0,
                parts: advance_parts,
            },
        ))
    })();
    result.map_err(|cause| verification_failure(rig, &key, stage.get(), cause))
}

/// This style matrix lives in an independent synthetic Rig; strictly close even after verification
/// failure, then check actual font-owner release.
pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, publication, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(SOURCE.as_bytes())
            .map_err(|cause| cause.to_string())?;
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            SOURCE.encode_utf16().count(),
            SOURCE.lines().count() + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            true,
        )?;
        let result = verify(&mut rig);
        let publication = rig
            .body
            .probe_root_leaf_publication()
            .map_err(|cause| cause.to_string());
        let retirement = release::close(rig)?;
        Ok((retirement, publication, result))
    })?;
    retirement.verify()?;
    if publication?.upgrade().is_some() {
        return Err("inline关闭后原RootLeaf publication未释放".into());
    }
    let (owners, underline, advances) = result?;
    for owner in owners {
        if owner.upgrade().is_some() {
            return Err("inline关闭后真实字体owner及inner Native/Core字体仍被项目持有".into());
        }
    }
    // Original soft lines/per-glyph edges come from the same complete short TextKit fixture; after
    // close compare Source and integer advances only.
    let font = native_markdown_document_fixed_font(14.0).map_err(|cause| cause.to_string())?;
    let lines = super::paragraph_oracle::measure(SOURCE, advances.width, &font, &advances.parts)?;
    let expected = super::paragraph_oracle::part_advances(SOURCE, &lines, &advances.parts)?;
    drop(font);
    for (part, expected) in advances.parts.iter().zip(expected) {
        if part.advance_26_6 != expected {
            return Err(format!(
                "中文封存 glyph 推进不等于完整旧 Native 软行上下文：source={:?} actual={} expected={expected}",
                part.source, part.advance_26_6
            ));
        }
    }
    // Create the standard short fixture only after original Source/fonts/bitmap/Native owners end,
    // without two live body graphs.
    let (ink, opaque) = whitespace_underline_pixels(&underline)?;
    println!(
        "native_root_inline_underline whitespace_link_underline_pixels={ink} whitespace_link_opaque_pixels={opaque} exact_independent_cg_underline=true actual_old_native_baseline=true fixed_tolerance_26_6=1 rgba_tolerance=0 simultaneous_body_graphs=false formal=false whole_product=false"
    );
    Ok(())
}
