//! 同原Source的短闭合inline完整帧：真实字体、代码底色、链接墨迹/目标与严格回收。

use super::*;
use crate::native_surface::paged_line_fonts::NativeMarkdownLineFont;
use crate::native_surface::paged_text_view::NativeMarkdownPagedTextScene;
use crate::native_surface::text_attributes::{
    MarkdownFontSelectionCache, MarkdownTextAttributePalette,
};
use objc2::{AnyThread, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSFont, NSFontAttributeName};
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

/// 固定夹具从原UTF8字面换算canonical UTF16，不按Parser或新字体part反算期望范围。
fn source_range(text: &str) -> Range<usize> {
    let start = SOURCE.find(text).expect("固定inline样本持原字面");
    let start = SOURCE[..start].encode_utf16().count();
    start..start + text.encode_utf16().count()
}

/// 引用前缀的显隐归真实物理行所有；不把同一跨行代码事件的首行当成后继行的选择来源。
fn quote_markers(
    rig: &Rig,
    active_middle: bool,
) -> Result<Vec<Weak<NativeMarkdownLineFont>>, String> {
    let parts = rig
        .body
        .probe_root_leaf_parts()
        .map_err(|cause| cause.to_string())?;
    autoreleasepool(|_| {
        let palette =
            MarkdownTextAttributePalette::new(NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0));
        // 三行属于同一引用段，强调和代码各自跨过真实 CRLF；Find 只在中间物理行内，
        // 不能令首行或代码结束行一起显露，也不能以该段内任意选择替代行端点事实。
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
                // 同时检查权限、真实角色和已注册字号。只看到 HIDDEN 位无法排除仍按正常字号占位的回归；
                // 当前行则必须回到旧 Native 的真实字体，而不是继续复用前一帧的隐藏槽。
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
                // 旧 resolver 决定实际 Font，CTRun 再证明 face、轴、矩阵与 advance；不猜 PSName。
                let points = original_scalar_font(scalar, part.presentation, &font, &palette)?;
                if part.points != points as f32 || part.native_points != points {
                    return Err("引用前缀的实际注册 Font 不符合旧 Native 字号".into());
                }
            }
        }
        Ok(()) // 原短 scalar 的 CTLine 和属性在当前池结束，绝不随候选字体弱票一起跨过关闭。
    })?;
    Ok(parts.iter().map(|part| part.font_owner.clone()).collect())
}

/// 固定 BMP 中文分别核字符框和原 Scene 命中；不凭新样式位或字体名推断文字已经绘到正确位置。
fn quote_characters(scene: &dyn NativeMarkdownPagedTextScene) -> Result<(), String> {
    // 本组标签全为完整 BMP scalar，因此每个请求精确占一个 UTF16 单元；
    // emoji 的代理对与 GCB 另由已有原生回归负责，不在这里拆半个字符造错误期望。
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
            // 固定中文为从左到右文本，点击字符左侧内部应落本 canonical 单元；
            // 采用实际字符框给点，但期望位置独立来自固定源码，不能用 hit 自己反算期望。
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

/// 只读 Find 和字体准备均无写入权限；流式对照 canonical 与磁盘，CRLF、前缀及内联语法必须逐字保留。
fn unchanged(rig: &Rig) -> Result<(), String> {
    let original = rig.document.upgrade().ok_or("inline 原 Doc 已提前归还")?;
    let document = original.borrow();
    // Find 可以改变选择，不能改变内容世代；长度与逐片字符均保持，才能排除删掉语法来换正确显示。
    if document.generation() != 0 || document.utf16_len() != SOURCE.encode_utf16().count() {
        return Err("inline 只读流程改变了原文档".into());
    }
    let mut expected = SOURCE.encode_utf16();
    let mut units = [0_u16; 128];
    // 校验也沿真实 mmap 文档的小片读取，不为验收建立第二个全文 String 或 attributed graph。
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
    drop(original); // 文件 I/O 前释放真实文档借用；校验不成为跨 Native 回调的第二 owner。
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 128];
    // 文档未改不足以证明磁盘未写；独立文件句柄逐片核原字节，CRLF 与转义字符不做归一化。
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

/// 每次 Find 都先观察再交原动作，只有同一真实完整 CPU 帧和目标可见证明到齐才借新 Scene。
fn find_frame(
    rig: &mut Rig,
    label: &str,
    stage: &str,
) -> Result<NativeMarkdownPagedSceneKey, String> {
    rig.bar.query.setStringValue(&NSString::from_str(label));
    // 观察先于真实动作，兼顾同视口同步提交与同一请求异步恢复；没有等待用户或合成快捷键。
    let frame = observe(&rig.body)?;
    match rig
        .action(MarkdownFindAction::Next)
        .map_err(|cause| cause.to_string())?
    {
        NativeMarkdownFindExecution::Complete => {}
        NativeMarkdownFindExecution::Pending(request) => rig.requests.push(request),
    }
    // 原三秒完整帧门同时要求实际 CPU paint，不能仅凭选择坐标已经改变宣称可见。
    frame::wait_complete_frame(stage, &rig.body, frame)?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let target = source_range(label);
    let target = NSRange::new(target.start, target.len());
    // 标签的期望取原字面 UTF16，不消费候选语义或显示索引反造 canonical 范围。
    if key.request.input.primary_range != target {
        return Err(format!("{stage}: 原 Find 没有提交固定 canonical 标签范围"));
    }
    rig.view
        .verify_find_target_visible(&key, target)
        .map_err(|cause| cause.to_string())?;
    Ok(key)
}

/// 固定完整scalar先沿旧TextKit显式回退策略，再读真实CTRun字体；不要求中文凭空有italic face。
fn original_scalar_font(
    scalar: char,
    styles: MarkdownPresentationStyles,
    actual: &NativeMarkdownLineFont,
    palette: &MarkdownTextAttributePalette,
) -> Result<f64, String> {
    let resolved = MarkdownFontSelectionCache::resolve_uncached(styles, scalar, palette, false);
    let mut bytes = [0; 4];
    let string = NSString::from_str(scalar.encode_utf8(&mut bytes));
    let font: &AnyObject = resolved.font.as_ref();
    // SAFETY: 原AppKit静态Font键与真实NSFont匹配；此字典和单scalar字符串只在当前局部池存活。
    let attributes: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> =
        unsafe { NSDictionary::from_slices(&[NSFontAttributeName], &[font]) };
    // SAFETY: 固定完整scalar不超过两个UTF16单元，属性字典只含本次真实旧resolved Font。
    let attributed = unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &string,
            Some(&attributes),
        )
    };
    // SAFETY: Foundation/CFAttributedString公开桥接；不把文稿、长段或viewport片段当成完整文本。
    let line = unsafe { CTLine::with_attributed_string(attributed.as_ref()) };
    // SAFETY: 原不可变短line持有全部真实CTRun，读取不触及产品Source。
    let runs = unsafe { line.glyph_runs() };
    if runs.len() != 1 {
        return Err("固定完整scalar没有得到唯一真实Native run，不能挑一个字体冒充原结果".into());
    }
    // SAFETY: 刚核实际数组含一个公开CTRun，数组保活借用至本次对照结束。
    let run = unsafe { runs.value_at_index(0) };
    let run = NonNull::new(run.cast_mut().cast::<CTRun>()).ok_or("原Native run为空")?;
    // SAFETY: 指针来自上方仍存活的公开run数组；两个getter只读取该不可变原对象。
    let (attributes, matrix, range) = unsafe {
        let run = run.as_ref();
        (run.attributes(), run.text_matrix(), run.string_range())
    };
    // SAFETY: 公开CoreText字体键对应CTFont，字典持有此真实run最终采用的字体。
    let font = unsafe { attributes.value(std::ptr::from_ref(kCTFontAttributeName).cast()) };
    let font = NonNull::new(font.cast_mut().cast::<CTFont>()).ok_or("原Native run没有Font")?;
    // SAFETY: 原字典、line和run仍在当前栈，字体只读查询不借产品图。
    let expected = unsafe { font.as_ref() };
    let actual = actual.native().native_font();
    // SAFETY: 原主线程字体仍由两侧owner持活；Copy真实CGFont而不是把UIFont角色别名当成文件face。
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
    // 没有axis覆盖与空覆盖字典都是原默认轴；非空字典按CF数值相等比较，不靠枚举顺序。
    let same_axes = actual_axes.as_deref().filter(|axes| !axes.is_empty())
        == expected_axes.as_deref().filter(|axes| !axes.is_empty());
    // SAFETY: 两侧都是活CTFont，独立读取相同Native度量；不调用产品几何缓存反造oracle。
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
    // SAFETY: 原name表按CFData真实字节比较；字号、traits、矩阵和轴仍各自独立保真。
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
    // SAFETY: 原run仍由line持活；本固定完整scalar只有一个真实glyph，三个输出均有完整独占槽。
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
    // SAFETY: 返回Copy字号后局部line、属性和NSFont全部归还，不把旧Native图带进产品关闭。
    Ok(unsafe { expected.size() })
}

/// 独立CG实绘量化预览约定颜色，避免把浮点乘255的猜测当作实际sRGB像素。
fn pixel(color: [f64; 4]) -> Result<[u8; 4], String> {
    let canvas = selection::Canvas::scaled(NSRect::new(NSPoint::ZERO, NSSize::new(1.0, 1.0)), 1.0)?;
    // 独立参考也必须明确使用源sRGB；DeviceRGB会随设备转换，不能作为这些约定分量的期望。
    // 这里只为固定的单像素参考创建一次CGColor，不复用产品的Scene/Viewport颜色设置方法。
    let paint = CGColor::new_srgb(color[0], color[1], color[2], color[3]);
    CGContext::set_fill_color_with_color(Some(&canvas.context), Some(&paint));
    CGContext::fill_rect(
        Some(&canvas.context),
        NSRect::new(NSPoint::ZERO, NSSize::new(1.0, 1.0)),
    );
    CGContext::flush(Some(&canvas.context));
    Ok(canvas.pixels[..4].try_into().expect("独立单像素RGBA"))
}

/// 原字符框遮罩与实际窗口使用相同backing scale；纯空格link的蓝色只能来自真实下划线。
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

/// 只保留四个空格内的有限纯值；先归还产品图和位图，再创建独立旧 Native 排版。
struct WhitespaceUnderline {
    viewport: NSRect,
    document_origin_y: f64,
    width: f64,
    scale: f64,
    span: NSRect,
    metrics: crate::native_surface::paged_font_geometry::NativeMarkdownPagedFontMetricValues,
    pixels: Vec<(usize, [u8; 4])>,
}

/// 初始真实 CJK part 只携纯值跨过严格关闭；完整旧 Native 上下文只能在候选全部归还后构造。
struct InlineAdvance {
    width: f64,
    parts: Vec<super::paragraph_oracle::PartAdvance>,
}

/// 实际字符框只负责圈定待比像素，不用其下沿加 CT descent 反推已经采用系统行槽的 Preview 基线。
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
        .collect(); // 只带走四个字符框内的 RGBA，不保留整幅位图、Native context、Scene 或 Reader。
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

/// 产品图关闭后，参考基线来自相同实际列宽的原 TextKit 完整短例；CG 仍逐 RGBA 零容差比较。
fn whitespace_underline_pixels(sample: &WhitespaceUnderline) -> Result<(usize, usize), String> {
    let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
    let original = super::paragraph_oracle::measure(SOURCE, sample.width, &font, &[])?;
    drop(font);
    let spaces = source_range("[    ]");
    let label = spaces.start + 1..spaces.end - 1;
    let original = original
        .iter()
        .find(|line| line.source.start <= label.start && label.end <= line.source.end)
        .ok_or("独立旧 Native 短例没有完整覆盖原四个空格的真实视觉行")?;
    // 这里只转换原窗口 y-up 坐标并去掉两侧既有 8pt padding，不消费候选的段落或字体行槽策略。
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
    // 原TextKit基线只提供真实下划线几何，配色由同一独立sRGB合同解释，不能沿设备空间漂移。
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

/// 只在首错已发生后读取现有权限标量；不布局、不推进runloop，也不把后继Ready拿来重试。
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

/// 观察已封存part的实际Native face和度量，再消费同一Scene的真实像素及Source绑定目标。
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
    // 同步NSResponder夹具不经NSApplication.sendEvent；先建立真实单击前的窗口焦点，再捕原Scene票。
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
        stage.set("cjk_original_font");
        let mut advance_parts = Vec::<super::paragraph_oracle::PartAdvance>::new();
        autoreleasepool(|_| -> Result<(), String> {
            let palette = MarkdownTextAttributePalette::new(
                NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0),
            );
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
                    } // 普通、引用与代码各有固定原角色；精确组合成立才交旧 Font 策略，不允许任意附加位。
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
                // 先保完整实际 part 的纯值；标点与词不能切成孤立短串重排，完整旧参考须等候选关闭。
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
            // 纯空白链接也沿旧 resolver 和真实 CTRun 核完整 Font；后继参考线不能只信候选传来的厚度。
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
            Ok(()) // 同池归还所有oracle临时Native claim，再继续原帧/像素和最终字体释放验收。
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
        // 沿实际窗口backing scale绘图；亚像素线条以独立CG抗锯齿RGBA逐像素核对，
        // 不能把有无opaque蓝色像素作为线条是否存在的依据。
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
        // 引用容器必须在同一真实 CPU 图中同时保中文墨迹、块背景和跨行代码底色；
        // 颜色取旧 Native palette 的外部约定，不能把当前候选默认前景抄成自己的 oracle。
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
        drop((actual, scene)); // 先归还绘图Reader和位图，目标恢复仍只借原sealed Scene，不把测试图带入后继量子。
        stage.set("restore_link_targets");
        let press = super::links::point(rig, source_range("recover").start)?;
        super::links::command_mouse(rig, press, objc2_app_kit::NSEventType::LeftMouseDown)?;
        super::links::command_mouse(rig, press, objc2_app_kit::NSEventType::LeftMouseUp)?;
        // CJK末块完成后，先由同块开头的真实片段click驱动原唯一请求恢复；绝不点击原https目标。
        // 原有限闭合块的 Parser 同时提取了 recover 与 go 的真实标签、解码目标；
        // 正链接事实不依赖样式窗口扩大。typed拒绝只证明完成，不冒充anchor导航。
        if !matches!(
            super::links::settle(rig)?,
            Some(ViewError::LinkAnchorUnsupported)
        ) {
            return Err("inline真实片段click没有完成原目标恢复及明确anchor拒绝".into());
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
        drop(scene); // Find改变选择前归还原Reader，不能靠旧帧补签新标记状态。
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
        drop((actual, scene)); // 选择变更之前归还旧图，引用后继的原 prefix Reader 才能真实恢复。
        stage.set("quote_find_middle_line");
        let quote = find_frame(rig, "引文斜体", "root_leaf_quote_middle_line_selection")?;
        if quote.source != key.source
            || rig.copy::<4>(source_range("引文斜体").start)?
                != ['引', '文', '斜', '体'].map(|scalar| scalar as u16)
        {
            return Err("引用 Find 改变了原 Source 或 canonical 中文".into());
        }
        // 选择改变后重获真实 parts 和字体弱票；旧帧的 source 相同也不能代签新 marker 显隐。
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
        // 新 Scene 自己签同 context 的有界绘图完成票；复用旧位图会掩盖当前行没有显露的错误。
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
        drop((actual, scene)); // 不持新绘图 Reader 去执行下一次 Find，不能把旧几何误作后继隐藏票。
        stage.set("quote_find_leave_line");
        let outside = find_frame(rig, "plain", "root_leaf_quote_leave_line_selection")?;
        if outside.source != key.source {
            return Err("离开引用行的 Find 改变了原 Source 身份".into());
        }
        // 再离开该物理行，必须真实恢复隐藏字号；这一步捕获选择只显露而从不收回的交互回归。
        owners.extend(quote_markers(rig, false)?);
        unchanged(rig)?;
        stage.set("remember_receipt");
        rig.remember_receipt()?;
        println!(
            "native_root_inline actual_traits=true strong_emphasis_combined=true cjk_original_textkit_run_fonts=true cjk_original_run_matrix=true whitespace_original_textkit_run_font=true scoped_quote_cjk_run_fonts=true scoped_quote_actual_ink_background=true scoped_quote_canonical_boxes_and_hits=true scoped_quote_physical_marker_find_reveal_hide=true source_and_disk_unchanged=true inline_code_monospace=true code_background_pixels={code_pixels} link_color_pixels={link_pixels} actual_source_bound_target=true target_recovered_by_original_click=true marker_not_clickable=true selection_over_code=true actual_backing_scale={scale} paint_steps={steps} external_url_open_tested=false internal_anchor_navigation_tested=false whole_product=false formal=false"
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

/// 样式矩阵只活在独立合成Rig；即使正文验证失败也先严格关闭，再核全部原字体owner已释放。
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
        let retirement = release::close(rig, false)?;
        Ok((retirement, publication, result))
    })?;
    retirement.verify(false)?;
    if publication?.upgrade().is_some() {
        return Err("inline关闭后原RootLeaf publication未释放".into());
    }
    let (owners, underline, advances) = result?;
    for owner in owners {
        if owner.upgrade().is_some() {
            return Err("inline关闭后真实字体owner及inner Native/Core字体仍被项目持有".into());
        }
    }
    // 实际旧软行与逐 glyph 边缘同归完整固定短例的 TextKit，关闭后只比较来源及整数推进。
    let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
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
    // 原 Source、字体、位图和全部 Native owner 已结束；标准短例现在才创建，不能同活两张正文图。
    let (ink, opaque) = whitespace_underline_pixels(&underline)?;
    println!(
        "native_root_inline_underline whitespace_link_underline_pixels={ink} whitespace_link_opaque_pixels={opaque} exact_independent_cg_underline=true actual_old_native_baseline=true fixed_tolerance_26_6=1 rgba_tolerance=0 simultaneous_body_graphs=false formal=false whole_product=false"
    );
    Ok(())
}
