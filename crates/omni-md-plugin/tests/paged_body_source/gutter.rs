//! 原行号的实际图层像素、软行归属和位数宽度；小夹具只在原大负载严格释放后顺序运行。

use super::*;
use objc2::{AnyThread, ClassType};
use objc2_app_kit::{
    NSColor, NSEvent, NSEventModifierFlags, NSEventType, NSFontAttributeName,
    NSForegroundColorAttributeName, NSRulerView,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColor, CGColorRenderingIntent,
    CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, kCGColorSpaceSRGB,
};
use objc2_core_text::CTLine;
use objc2_foundation::{
    NSAttributedString, NSAttributedStringKey, NSDictionary, NSEdgeInsets, NSIntersectionRect,
};
use objc2_quartz_core::{CALayer, kCAContentsFormatRGBA8Uint, kCAGravityResize};
use omni_md_plugin::native_surface::{
    paged_body::NativeMarkdownPagedBodyResourceReceipts,
    paged_text_view::{NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewDelegate},
};

/// 全部图像只覆盖实际窄ruler视口；字段顺序使context在其借用的RGBA缓冲之前析构。
struct Canvas {
    context: CFRetained<CGContext>,
    pixels: Vec<u8>,
}

/// 固定夹具预先声明可见折叠语义；不读取生产Scene的may_fold或私有图形状态反推期望。
#[derive(Clone, Copy)]
enum FoldMarker {
    Expanded,
    Collapsed,
}
impl Canvas {
    fn new(bounds: NSRect, scale: f64) -> Self {
        assert!(bounds.size.width > 0.0 && bounds.size.width <= 128.0);
        assert!(bounds.size.height > 0.0 && bounds.size.height <= 240.0);
        assert!(scale > 0.0 && scale <= 2.0);
        let width = (bounds.size.width * scale).ceil() as usize;
        let height = (bounds.size.height * scale).ceil() as usize;
        let mut pixels = vec![0u8; width * height * 4];
        // SAFETY: 公开sRGB名称为静态值；对照不借生产Scene的颜色空间或像素provider。
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap();
        // SAFETY: 固定Vec覆盖stride*height，context存活期不resize，且先于Vec析构。
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * 4,
                Some(&space),
                CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
            )
        }
        .unwrap();
        CGContext::scale_ctm(Some(&context), scale, scale);
        CGContext::translate_ctm(Some(&context), -bounds.origin.x, -bounds.origin.y);
        Self { context, pixels }
    }
}

/// 固定期望标签只经过独立CTLine排版，不读取生产的十个glyph数组或行号snapshot。
fn label(text: &str, font: &NSFont) -> CFRetained<CTLine> {
    assert!(!text.is_empty() && text.len() <= 3 && text.bytes().all(|byte| byte.is_ascii_digit()));
    let string = NSString::from_str(text);
    let color =
        NSColor::colorWithSRGBRed_green_blue_alpha(81.0 / 255.0, 91.0 / 255.0, 109.0 / 255.0, 1.0);
    // SAFETY: 两个AppKit静态属性键分别配真实NSFont/NSColor，固定短字符串没有正文载荷。
    let attributes: objc2::rc::Retained<NSDictionary<NSAttributedStringKey, AnyObject>> = unsafe {
        NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[font.as_ref(), color.as_ref()],
        )
    };
    // SAFETY: 有效有限NSString和真实属性字典在同步构造内存活。
    let attributed = unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &string,
            Some(&attributes),
        )
    };
    // SAFETY: Foundation与CFAttributedString公开桥接；返回CTLine只包含上方最多三位标签。
    unsafe { CTLine::with_attributed_string(attributed.as_ref()) }
}

fn fill(context: &CGContext, rect: NSRect, rgb: [u8; 3]) {
    CGContext::set_rgb_fill_color(
        Some(context),
        f64::from(rgb[0]) / 255.0,
        f64::from(rgb[1]) / 255.0,
        f64::from(rgb[2]) / 255.0,
        1.0,
    );
    CGContext::fill_rect(Some(context), rect);
}

/// 原CSS位数占位使用连续9；独立CTLine量实际advance，不复制生产数字width算法。
fn width(ruler: &NSRulerView, lines: usize) -> f64 {
    let placeholder = match lines {
        9 => "9",
        10 | 99 => "99",
        100 => "999",
        _ => panic!("行号宽度回归只接受明确的9/10/99/100业务边界"),
    };
    let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
    let line = label(placeholder, &font);
    // SAFETY: 不可变短CTLine存活；三个度量输出不需要时传null。
    let advance = unsafe {
        line.typographic_bounds(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    // 行号沿原左5/右3与1pt分隔线，另外公开预留10pt折叠槽；号码不占这个标记热区。
    let expected = (advance + 5.0 + 3.0).max(20.0) + 1.0 + 10.0;
    assert_eq!(
        ruler.ruleThickness(),
        expected,
        "原border-box宽度必须由当前总行数位数撑开"
    );
    expected
}

/// 只渲染实际挂载ruler的现图层；期望标签由独立CTLine生成，颜色和号码不从Scene元数据反推。
fn pixels(
    body: &NativeMarkdownPagedBody,
    window: &NSWindow,
    scroll: &NSScrollView,
    view: &NativeMarkdownPagedTextView,
    stage: &str,
    samples: &[(usize, Option<&str>, bool, Option<FoldMarker>)],
    entire_active_without_number: bool,
) {
    autoreleasepool(|_| {
        let key = view.capture_ready_scene_key().unwrap();
        let paints = view.cpu_paints().unwrap();
        let current = || {
            NativeMarkdownPagedTextViewDelegate::scene_current(body, &key)
                && view.cpu_paints() == Some(paints)
                && view.cpu_paint_pending() == Some(false)
        };
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(body, &key.request).unwrap();
        scene.validate_ready(&current).unwrap();
        window.displayIfNeeded();
        let ruler = scroll
            .verticalRulerView()
            .expect("Source必须挂实际行号ruler");
        assert!(scroll.rulersVisible() && scroll.hasVerticalRuler());
        assert!(
            ruler
                .clientView()
                .as_deref()
                .is_some_and(|actual| std::ptr::eq(actual, view.as_super()))
        );
        ruler.displayIfNeeded(); // 标准AppKit脏区域交付，不直接调用updateLayer或手跑生产executor。
        scene.validate_ready(&current).unwrap();
        let bounds = ruler.bounds();
        // 原生父坐标里正文与ruler必须实际分占空间，不能只让行号宽度元数据变化。
        let body_in_scroll = scroll.convertRect_fromView(view.bounds(), Some(view));
        let ruler_in_scroll = scroll.convertRect_fromView(bounds, Some(&ruler));
        assert!(
            body_in_scroll.origin.x >= ruler_in_scroll.origin.x + ruler_in_scroll.size.width,
            "{stage}: 实际正文frame不得延伸到行号栏下方：body={body_in_scroll:?} ruler={ruler_in_scroll:?}"
        );
        assert_eq!(key.request.bounds_width, view.bounds().size.width);
        let layer = ruler.layer().unwrap();
        assert_eq!(layer.bounds(), bounds);
        assert!(
            // SAFETY: 原自有图层在本主线程栈内存活，仅检查公开 CF contents 是否存在，不向其发送对象消息。
            unsafe { layer.contents() }.is_some(),
            "{stage}: 实际行号图不能为空"
        );
        let scale = window.backingScaleFactor();
        let source = Canvas::new(bounds, scale);
        let actual = Canvas::new(bounds, scale);
        let mask = Canvas::new(bounds, scale);
        // 生产有限图在真实ruler边界内绘制；分数device-pixel边界也必须采用同一公开裁剪，
        // 不能让独立CTLine图额外涂到ceil分配的末列，再把那部分当成实际可见内容。
        CGContext::clip_to_rect(Some(&source.context), bounds);
        fill(
            &source.context,
            bounds,
            if entire_active_without_number {
                [27, 32, 42]
            } else {
                [19, 22, 28]
            },
        );
        let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
        let mut regions = Vec::with_capacity(samples.len());
        for &(position, number, active, marker) in samples {
            let rect = scene
                .first_rect(NSRange::new(
                    position,
                    usize::from(position < key.request.input.utf16_len),
                ))
                .unwrap();
            assert_eq!(rect.actual_range.location, position);
            let glyph_in_scroll = scroll.convertRect_fromView(rect.rect, Some(view));
            assert!(
                glyph_in_scroll.origin.x >= body_in_scroll.origin.x,
                "{stage}: 原Scene的实际字符矩形不能越过正文左边进入行号栏"
            );
            if position <= 1 {
                // 两套固定夹具文首都有真实非空字符；有限采样只读实际正文图层，避开caret的首像素。
                // 不用字符矩形存在冒充glyph已经绘制，也不把行号墨迹当正文墨迹。
                let glyph_band = NSIntersectionRect(
                    NSRect::new(
                        NSPoint::new(rect.rect.origin.x + 2.0, rect.rect.origin.y),
                        NSSize::new(
                            (rect.rect.size.width - 3.0).min(32.0),
                            rect.rect.size.height,
                        ),
                    ),
                    view.bounds(),
                );
                assert!(glyph_band.size.width > 0.0 && glyph_band.size.height <= 32.0);
                let glyph_pixels = Canvas::new(glyph_band, window.backingScaleFactor());
                fill(&glyph_pixels.context, glyph_band, [255; 3]);
                let body_layer = view.layer().unwrap();
                assert_eq!(body_layer.bounds().size, view.bounds().size);
                body_layer.renderInContext(&glyph_pixels.context);
                CGContext::flush(Some(&glyph_pixels.context));
                assert!(
                    glyph_pixels.pixels.as_chunks::<4>().0.iter().any(|pixel| {
                        pixel[3] == 255 && pixel[..3].iter().all(|component| *component < 16)
                    }),
                    "{stage}: 行号右侧的实际正文局部图必须含真实黑色字形墨迹"
                );
                scene.validate_ready(&current).unwrap();
            }
            let frame = ruler.convertRect_fromView(rect.rect, Some(view));
            let row = NSRect::new(
                NSPoint::new(bounds.origin.x, frame.origin.y),
                NSSize::new(bounds.size.width, frame.size.height),
            );
            assert!(
                row.origin.y + row.size.height > bounds.origin.y
                    && row.origin.y < bounds.origin.y + bounds.size.height
            );
            assert!(
                !regions
                    .iter()
                    .any(|old: &(NSRect, Option<&str>, bool)| old.0.origin.y == row.origin.y),
                "每个样本必须来自不同真实soft行"
            );
            if active {
                // 活动行要求连续实色，分数软行边界不能额外与未活动底色混出横缝。
                CGContext::save_g_state(Some(&source.context));
                CGContext::set_should_antialias(Some(&source.context), false);
                fill(&source.context, row, [27, 32, 42]);
                CGContext::restore_g_state(Some(&source.context));
            }
            // 未活动行沿用已绘制的整体底色；重复填充会在分数ruler边缘再次混合裁剪覆盖率，
            // 改变期望像素本身，不能把这种对照重画误认为实际图层的颜色差异。
            if let Some(number) = number {
                let line = label(number, &font);
                let (mut ascent, mut descent) = (0.0, 0.0);
                // SAFETY: 两个栈度量输出有效；line只含当前固定期望号码，不借生产glyph。
                let advance = unsafe {
                    line.typographic_bounds(&mut ascent, &mut descent, std::ptr::null_mut())
                };
                CGContext::set_text_position(
                    Some(&source.context),
                    bounds.origin.x + bounds.size.width - 1.0 - 3.0 - 10.0 - advance,
                    frame.origin.y + frame.size.height / 2.0 - (ascent - descent) / 2.0,
                );
                // SAFETY: 本同步栈独占有限CPU context，固定CTLine和真实字体在绘制期间存活。
                unsafe { line.draw(&source.context) };
            }
            if let Some(marker) = marker {
                // 公开10pt槽内的固定右/下三角表示收起/展开；只借真实行中心，不借生产marker路径。
                // 与数字同一独立sRGB画布逐像素对照，图层插值或抗锯齿差异都不能用容差掩盖。
                let center = NSPoint::new(
                    bounds.origin.x + bounds.size.width - 7.0,
                    frame.origin.y + frame.size.height * 0.5,
                );
                let vertices = match marker {
                    FoldMarker::Expanded => [
                        NSPoint::new(center.x - 3.0, center.y + 2.0),
                        NSPoint::new(center.x + 3.0, center.y + 2.0),
                        NSPoint::new(center.x, center.y - 2.0),
                    ],
                    FoldMarker::Collapsed => [
                        NSPoint::new(center.x - 2.0, center.y - 3.0),
                        NSPoint::new(center.x + 2.0, center.y),
                        NSPoint::new(center.x - 2.0, center.y + 3.0),
                    ],
                };
                CGContext::set_rgb_fill_color(
                    Some(&source.context),
                    81.0 / 255.0,
                    91.0 / 255.0,
                    109.0 / 255.0,
                    1.0,
                );
                CGContext::begin_path(Some(&source.context));
                CGContext::move_to_point(Some(&source.context), vertices[0].x, vertices[0].y);
                for vertex in &vertices[1..] {
                    CGContext::add_line_to_point(Some(&source.context), vertex.x, vertex.y);
                }
                CGContext::close_path(Some(&source.context));
                CGContext::fill_path(Some(&source.context));
            }
            regions.push((row, number, active));
        }
        let separator = NSRect::new(
            NSPoint::new(bounds.origin.x + bounds.size.width - 1.0, bounds.origin.y),
            NSSize::new(1.0, bounds.size.height),
        );
        fill(&source.context, separator, [37, 42, 51]);
        CGContext::flush(Some(&source.context));
        // 独立有限CTLine图走相同公开图像显示几何，保留分数point宽度的真实采样；不靠容差抹平差异。
        let bitmap_image = CGBitmapContextCreateImage(Some(&source.context)).unwrap();
        let provider = CGImage::data_provider(Some(&bitmap_image)).unwrap();
        let space = CGImage::color_space(Some(&bitmap_image)).unwrap();
        // CGBitmapContextCreateImage默认允许插值，生产只读CGImage明确不插值；共用对照图自己的
        // provider封装相同采样属性，不拷生产像素，也不重画或偏移任何独立CTLine标签。
        // SAFETY: 宽高/stride/位数和颜色空间全部来自同一有效对照bitmap，provider覆盖这些字节；
        // decode=null保留默认通道映射，图像不允许后续写入或借用源文档。
        let image = unsafe {
            CGImage::new(
                CGImage::width(Some(&bitmap_image)),
                CGImage::height(Some(&bitmap_image)),
                CGImage::bits_per_component(Some(&bitmap_image)),
                CGImage::bits_per_pixel(Some(&bitmap_image)),
                CGImage::bytes_per_row(Some(&bitmap_image)),
                Some(&space),
                CGImage::bitmap_info(Some(&bitmap_image)),
                Some(&provider),
                std::ptr::null(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        }
        .unwrap();
        let expected_layer = CALayer::new();
        expected_layer.setBounds(bounds);
        expected_layer.setFrame(bounds);
        expected_layer.setContentsScale(scale);
        // 独立画布已用ruler本地y-up坐标绘制真实row；其根图层保留默认方向，不再翻图。
        // AppKit backing的geometryFlipped描述其原父图关系，直接复制会把已定位的行Y再翻一次。
        assert!(layer.masksToBounds(), "实际ruler必须裁在自身有限bounds内");
        expected_layer.setMasksToBounds(true);
        // SAFETY: 公开RGBA8常量对应上方真实32bit sRGB图，与产品既有显示格式一致。
        unsafe { expected_layer.setContentsFormat(kCAContentsFormatRGBA8Uint) };
        // 实际ruler的backing自构造起有原CSS不透明背景；分数宽图像边缘会与它真实合成。
        // 独立对照也必须覆盖这个显示环境，不能拿透明layer的边缘alpha要求不透明产品图相等。
        let background = CGColor::new_srgb(19.0 / 255.0, 22.0 / 255.0, 28.0 / 255.0, 1.0);
        expected_layer.setBackgroundColor(Some(&background));
        // SAFETY: 公开QuartzCore静态重排常量有效，图像仅包含固定期望标签。
        unsafe { expected_layer.setContentsGravity(kCAGravityResize) };
        // SAFETY: CALayer.contents公开接纳CF CGImage；只桥接真实同一图像，不对其发送NSObject消息。
        unsafe {
            expected_layer.setContents(Some(&*std::ptr::from_ref(&*image).cast::<AnyObject>()))
        };
        let expected = Canvas::new(bounds, scale);
        layer.renderInContext(&actual.context);
        expected_layer.renderInContext(&expected.context);
        CGContext::flush(Some(&actual.context));
        CGContext::flush(Some(&expected.context));
        scene.validate_ready(&current).unwrap();
        if entire_active_without_number {
            let pixel_width = (bounds.size.width * scale).ceil() as usize;
            let first_difference = actual
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .zip(expected.pixels.as_chunks::<4>().0)
                .enumerate()
                .find(|(_, (actual, expected))| actual != expected)
                .map(|(index, (actual, expected))| {
                    (index % pixel_width, index / pixel_width, actual, expected)
                });
            assert!(
                actual.pixels == expected.pixels,
                "{stage}: 长逻辑行中部必须只有活动底色和分隔线；首差异(x,y,actual,expected)={first_difference:?} actual_bytes={} expected_bytes={}",
                actual.pixels.len(),
                expected.pixels.len()
            );
        } else {
            let pixel_width = (bounds.size.width * scale).ceil() as usize;
            for (row, number, active) in regions {
                CGContext::clear_rect(Some(&mask.context), bounds);
                fill(&mask.context, row, [255, 0, 0]);
                CGContext::flush(Some(&mask.context));
                let mut count = 0usize;
                let mut ink = 0usize;
                let mut background = None;
                for (index, ((actual, expected), marker)) in actual
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(expected.pixels.as_chunks::<4>().0)
                    .zip(mask.pixels.as_chunks::<4>().0)
                    .enumerate()
                {
                    if *marker != [255, 0, 0, 255] {
                        continue; // 边缘不完整遮罩没有完整像素义务，不用颜色容差代替真实内部像素。
                    }
                    count += 1;
                    assert_eq!(
                        actual, expected,
                        "{stage}: 原ruler行号码{number:?}/活动{active}像素{index}"
                    );
                    let bg = *background.get_or_insert(*expected);
                    let x = (index % pixel_width) as f64 / scale;
                    if x >= 5.0 && x < bounds.size.width - 4.0 && *expected != bg {
                        ink += 1; // 只数独立CTLine真的非底色像素，右分隔线不能冒充数字已绘制。
                    }
                }
                assert!(count > 0, "{stage}: 行号采样遮罩不得为空");
                if number.is_some() {
                    assert!(ink > 0, "{stage}: 固定期望号码必须有实际可见墨迹");
                }
            }
        }
        // SAFETY: 只清本对照图层原contents，使有限CGImage/provider在pool结束前归还。
        unsafe { expected_layer.setContents(None) };
        assert!(current());
        println!(
            "paged_body_source stage={stage} actual_ruler_layer_pixels=true independent_short_ctline_numbers=true real_scene_row_centers=true exact_rgba=true actual_body_outside_ruler=true long_line_no_floating_number={entire_active_without_number} screen_present_verified=false whole_product=false formal=false"
        );
    }); // 所有对照像素、CTLine、Layer和Scene只活当前阶段，不跨动作保留。
}

/// 原四MiB长逻辑行不变；实际中滚后的整幅窄图必须只有活动底色和分隔线。
pub(super) fn long(
    body: &NativeMarkdownPagedBody,
    window: &NSWindow,
    scroll: &NSScrollView,
    view: &NativeMarkdownPagedTextView,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
) {
    pixels(
        body,
        window,
        scroll,
        view,
        "gutter_original_long_bof",
        &[
            (1, Some("1"), true, None),
            (80, None, true, None),
            (160, None, true, None),
        ],
        false,
    );
    let key = view.capture_ready_scene_key().unwrap();
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(body, &key.request).unwrap();
    let first = scene.first_rect(NSRange::new(1, 1)).unwrap().rect;
    drop(scene); // 几何只取真实标量；不把旧Reader保到后继滚动和producer发布中。
    let clip = scroll.contentView();
    let frame = observe(body, Instant::now());
    clip.scrollToPoint(NSPoint::new(0.0, 8.0 + first.size.height * 2.5));
    scroll.reflectScrolledClipView(&clip);
    window.displayIfNeeded();
    deliver("gutter_long_middle_frame", body, frame);
    let resources = body.resource_receipts().unwrap();
    assert!(resources.reader.is_some(), "真实完整帧必须已创建原Reader");
    receipts.push(resources);
    assert!(
        view.capture_ready_scene_key()
            .unwrap()
            .request
            .virtual_viewport
            .document_origin
            .y
            > first.size.height
    );
    pixels(
        body,
        window,
        scroll,
        view,
        "gutter_long_middle_no_number",
        &[],
        true,
    );
    let frame = observe(body, Instant::now());
    clip.scrollToPoint(NSPoint::ZERO);
    scroll.reflectScrolledClipView(&clip);
    window.displayIfNeeded();
    deliver("gutter_long_restore_bof", body, frame);
    let resources = body.resource_receipts().unwrap();
    assert!(resources.reader.is_some(), "真实完整帧必须已创建原Reader");
    receipts.push(resources);
    pixels(
        body,
        window,
        scroll,
        view,
        "gutter_long_restored_number",
        &[
            (1, Some("1"), true, None),
            (80, None, true, None),
            (160, None, true, None),
        ],
        false,
    );
    assert!(
        view.pixel_observer()
            .unwrap()
            .snapshot()
            .peak_reserved_pixel_bytes
            <= 32 * 1024 * 1024
    );
}

/// 九/九十九行分别独占一套真实图；每套完全释放后才创建下一套，原大负载不由它替换。
pub(super) fn small(mtm: MainThreadMarker) {
    for (before, after, lines) in [
        ("gutter_first_9_before", "gutter_first_9_after", 9usize),
        ("gutter_first_99_before", "gutter_first_99_after", 99),
        ("gutter_reopen_9_before", "gutter_reopen_9_after", 9),
        ("gutter_reopen_99_before", "gutter_reopen_99_after", 99),
    ] {
        process_memory::sample(before).unwrap();
        small_case(mtm, lines);
        process_memory::sample(after).unwrap();
    } // 同样的两个真实场景各重开一次；只记录原Mach分类，不用四次样本冒签长期内存稳定。
}

/// 固定九行夹具的首段有真实后续行；通过挂载ruler的标准鼠标入口切换原折叠，不直接改Input。
fn click_first_fold(
    body: &NativeMarkdownPagedBody,
    window: &NSWindow,
    scroll: &NSScrollView,
    view: &NativeMarkdownPagedTextView,
) {
    let key = view.capture_ready_scene_key().unwrap();
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(body, &key.request).unwrap();
    let first = scene.first_rect(NSRange::new(0, 1)).unwrap().rect;
    let ruler = scroll.verticalRulerView().unwrap();
    let frame = ruler.convertRect_fromView(first, Some(view));
    let bounds = ruler.bounds();
    let point = ruler.convertPoint_toView(
        NSPoint::new(
            bounds.origin.x + bounds.size.width - 7.0,
            frame.origin.y + frame.size.height * 0.5,
        ),
        None,
    );
    drop(scene); // 后继实际折叠前归还旧Scene强边，不让对照自己的Reader阻碍生产退休。
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDown,
        point,
        NSEventModifierFlags::empty(),
        1.0,
        window.windowNumber(),
        None,
        1,
        1,
        1.0,
    )
    .unwrap();
    // SAFETY: 同步存活的公开NSEvent交给真实NSRulerView标准void入口；不是系统物理鼠标验收。
    unsafe {
        let _: () = objc2::msg_send![&*ruler, mouseDown: &*event];
    }
}

fn small_case(mtm: MainThreadMarker, lines: usize) {
    let (mapping, document, view, native, pixels, resources, gutter, close) = autoreleasepool(
        |_| {
            let mut file = tempfile::NamedTempFile::new().unwrap();
            for _ in 1..lines {
                file.write_all(b"x\r\n").unwrap(); // 每次仅三个字节；原CRLF分隔与最后空行由固定配方确定。
            }
            file.flush().unwrap();
            let document = Rc::new(RefCell::new(
                MarkdownDocumentBuffer::open(file.path(), 4096).unwrap(),
            ));
            assert_eq!(document.borrow().line_count(), lines);
            let revision = document.borrow().current_revision().unwrap();
            let original_length = 3 * (lines - 1);
            assert_eq!(document.borrow().utf16_len(), original_length);
            let doc_weak = Rc::downgrade(&document);
            let mapping = document.borrow().original_backing_observer().unwrap();
            let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(640.0, 240.0));
            let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), bounds);
            scroll.setHasVerticalScroller(true);
            let original_clip_settings = {
                let clip = scroll.contentView();
                if lines == 9 {
                    // 原九行场景同时覆盖调用方已有手工inset；不新增文档或缩小原负载。
                    clip.setAutomaticallyAdjustsContentInsets(false);
                    clip.setContentInsets(NSEdgeInsets {
                        top: 0.0,
                        left: 5.0,
                        bottom: 0.0,
                        right: 3.0,
                    });
                }
                (
                    clip.automaticallyAdjustsContentInsets(),
                    clip.contentInsets(),
                )
            }; // 仅保存Copy值，测试不能额外强持clip跨越原Native释放门。
            let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
            let paragraph = NSParagraphStyle::defaultParagraphStyle();
            let body = NativeMarkdownPagedBody::attach(
                document,
                &scroll,
                font.as_ref(),
                &paragraph,
                NativeMarkdownPagedBodyConfiguration {
                    presentation_mode: omni_markdown_core::MarkdownPresentationMode::Source,
                    epoch: 73,
                    maximum_font_bytes: 512 * 1024 * 1024,
                    padding: 8.0,
                    colors: NativeMarkdownPagedBodyColors {
                        background: [1.0; 4],
                        active_line: [27.0 / 255.0, 32.0 / 255.0, 42.0 / 255.0, 1.0],
                        text: [0.0, 0.0, 0.0, 1.0],
                        selection: [0.7, 0.8, 1.0, 1.0],
                        caret: [0.0, 0.0, 0.0, 1.0],
                    },
                },
                mtm,
            )
            .unwrap();
            let first = observe(&body, Instant::now());
            let view = body.view().unwrap();
            let view_weak = weak(&view);
            let pixels_observer = view.pixel_observer().unwrap();
            // SAFETY: 只创建既有合成无边框窗口，没有产品标题栏按钮或额外快捷键。
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    bounds,
                    NSWindowStyleMask::Borderless,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            // SAFETY: Retained栈持有window直到close后，禁止系统再多做一次autorelease。
            unsafe { window.setReleasedWhenClosed(false) };
            window.setContentView(Some(&scroll));
            window.orderFrontRegardless();
            window.displayIfNeeded();
            deliver_frame("gutter_small_initial_frame", &body, first);
            autoreleasepool(|_| {
                // 本夹具固定从无缩进的LTR“x\r\n”开始，Source首part从本行X=0起排，
                // attach明确给8pt正文padding；ruler/调用方inset只移动正文frame，不能再加进局部caret。
                // 期望值直接来自这份公开交互合同，不从待测layout_origin或可见frame反推。
                let key = view.capture_ready_scene_key().unwrap();
                let scene =
                    NativeMarkdownPagedTextViewDelegate::ready_scene(&*body, &key.request).unwrap();
                let caret = scene.first_rect(NSRange::new(0, 0)).unwrap();
                assert_eq!(caret.actual_range, NSRange::new(0, 0));
                assert_eq!(
                    caret.rect.origin.x,
                    8.0,
                    "gutter_small_initial_frame lines={lines}: 原首字caret必须仍是局部8pt；virtual={:?} body_frame={:?} clip_bounds={:?} clip_insets={:?}",
                    key.request.virtual_viewport,
                    key.request.frame_in_parent,
                    scroll.contentView().bounds(),
                    scroll.contentView().contentInsets(),
                );
            }); // 不保留Scene/Reader到后续选择、折叠或实际关闭收据。
            pixels(
                &body,
                &window,
                &scroll,
                &view,
                "gutter_small_initial_crlf_numbers",
                &[
                    (0, Some("1"), true, Some(FoldMarker::Expanded)),
                    (3, Some("2"), false, None),
                ],
                false,
            );
            let first_resources = body.resource_receipts().unwrap();
            assert!(
                first_resources.reader.is_some(),
                "首个真实完整帧必须有原Reader"
            );
            let mut resources = vec![first_resources];
            if lines == 9 {
                for (stage, marker) in [
                    ("gutter_small_fold_marker", FoldMarker::Collapsed),
                    ("gutter_small_unfold_marker", FoldMarker::Expanded),
                ] {
                    // 同一九行资源图内核实际折叠/展开；每次等原完整CPU帧，再独立绘图核箭头方向。
                    // 不改行号、UTF16长度或正文版本来适配显示结果，随后原跨位数/Undo流程照常运行。
                    let frame = observe(&body, Instant::now());
                    click_first_fold(&body, &window, &scroll, &view);
                    deliver_frame(stage, &body, frame);
                    resources.push(body.resource_receipts().unwrap());
                    let document = doc_weak.upgrade().unwrap();
                    assert_eq!(document.borrow().line_count(), lines);
                    assert_eq!(document.borrow().utf16_len(), original_length);
                    assert_eq!(document.borrow().current_revision().unwrap(), revision);
                    drop(document);
                    pixels(
                        &body,
                        &window,
                        &scroll,
                        &view,
                        stage,
                        &[(0, Some("1"), true, Some(marker))],
                        false,
                    );
                }
            }
            let frame = observe(&body, Instant::now());
            view.command(sel!(moveToEndOfDocument:)).unwrap();
            deliver_frame("gutter_small_tail_empty_frame", &body, frame);
            let frame_resources = body.resource_receipts().unwrap();
            assert!(
                frame_resources.reader.is_some(),
                "后继真实完整帧必须有原Reader"
            );
            resources.push(frame_resources);
            let before = view.capture_ready_scene_key().unwrap();
            let before_width = width(&scroll.verticalRulerView().unwrap(), lines);
            let before_body_width = before.request.bounds_width;
            let previous_number = (lines - 1).to_string();
            let last_number = lines.to_string();
            pixels(
                &body,
                &window,
                &scroll,
                &view,
                "gutter_small_tail_crlf_numbers",
                &[
                    (original_length - 3, Some(&previous_number), false, None),
                    (original_length, Some(&last_number), true, None),
                ],
                false,
            );
            let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
            let frame = observe(&body, Instant::now());
            // SAFETY: 真输入仅两个CRLF字节，原NSTextInputClient执行同canonical编辑与一次Undo。
            unsafe {
                client.insertText_replacementRange(
                    &NSString::from_str("\r\n"),
                    NSRange::new(NSNotFound as usize, 0),
                )
            };
            deliver_frame("gutter_small_cross_digit_insert", &body, frame);
            let frame_resources = body.resource_receipts().unwrap();
            assert!(
                frame_resources.reader.is_some(),
                "后继真实完整帧必须有原Reader"
            );
            resources.push(frame_resources);
            assert_eq!(doc_weak.upgrade().unwrap().borrow().line_count(), lines + 1);
            assert_eq!(
                doc_weak.upgrade().unwrap().borrow().utf16_len(),
                original_length + 2
            );
            let frame = observe(&body, Instant::now());
            view.command(sel!(moveToEndOfDocument:)).unwrap();
            deliver_frame("gutter_small_new_tail_visible", &body, frame);
            let frame_resources = body.resource_receipts().unwrap();
            assert!(
                frame_resources.reader.is_some(),
                "后继真实完整帧必须有原Reader"
            );
            resources.push(frame_resources);
            let after = view.capture_ready_scene_key().unwrap();
            let after_width = width(&scroll.verticalRulerView().unwrap(), lines + 1);
            if !(after_width > before_width && after.request.bounds_width < before_body_width) {
                // 只在原几何断言已经失败时读取同一挂载图；诊断不重签Scene或改变布局。
                let clip = scroll.contentView();
                let ruler = scroll.verticalRulerView().unwrap();
                eprintln!(
                    "paged_gutter_width_failure lines={lines} scroll_bounds={:?} scroll_frame={:?} scroll_insets={:?} scroll_auto_insets={} clip_bounds={:?} clip_frame={:?} clip_insets={:?} clip_auto_insets={} ruler_bounds={:?} ruler_frame={:?} ruler_hidden={} rulers_visible={} has_vertical_ruler={} before_body_width={before_body_width} after_body_width={} formal=false",
                    scroll.bounds(),
                    scroll.frame(),
                    scroll.contentInsets(),
                    scroll.automaticallyAdjustsContentInsets(),
                    clip.bounds(),
                    clip.frame(),
                    clip.contentInsets(),
                    clip.automaticallyAdjustsContentInsets(),
                    ruler.bounds(),
                    ruler.frame(),
                    ruler.isHidden(),
                    scroll.rulersVisible(),
                    scroll.hasVerticalRuler(),
                    after.request.bounds_width,
                );
            }
            assert!(
                after_width > before_width && after.request.bounds_width < before_body_width,
                "跨位数必须同步变更真实行号与正文宽度：lines={lines}，ruler={before_width}→{after_width}，body={before_body_width}→{}",
                after.request.bounds_width,
            );
            assert!(!after.same(&before));
            let new_number = (lines + 1).to_string();
            pixels(
                &body,
                &window,
                &scroll,
                &view,
                "gutter_small_expanded_digit_numbers",
                &[
                    (original_length, Some(&last_number), false, None),
                    (original_length + 2, Some(&new_number), true, None),
                ],
                false,
            );
            let frame = observe(&body, Instant::now());
            view.command(sel!(undo:)).unwrap();
            deliver_frame("gutter_small_digit_undo", &body, frame);
            let frame_resources = body.resource_receipts().unwrap();
            assert!(
                frame_resources.reader.is_some(),
                "后继真实完整帧必须有原Reader"
            );
            resources.push(frame_resources);
            let document = doc_weak.upgrade().unwrap();
            assert_eq!(document.borrow().line_count(), lines);
            assert_eq!(document.borrow().utf16_len(), original_length);
            assert_eq!(document.borrow().current_revision().unwrap(), revision);
            drop(document);
            assert_eq!(
                width(&scroll.verticalRulerView().unwrap(), lines),
                before_width
            );
            assert_eq!(
                view.capture_ready_scene_key().unwrap().request.bounds_width,
                before_body_width
            );
            pixels(
                &body,
                &window,
                &scroll,
                &view,
                "gutter_small_undo_tail_numbers",
                &[
                    (original_length - 3, Some(&previous_number), false, None),
                    (original_length, Some(&last_number), true, None),
                ],
                false,
            );
            let mut native = body.native_weaks().unwrap();
            let gutter = {
                let ruler = scroll.verticalRulerView().unwrap();
                (Weak::from(&*ruler), Weak::from(&*ruler.layer().unwrap()))
            }; // 只保原ruler/原层的Typed Weak；实际空载荷仍由原close收据核验，不能要求系统缓存析构。
            native.push(("gutter-small-scroll", weak(&scroll)));
            native.push(("gutter-small-window", weak(&window)));
            assert!(pixels_observer.snapshot().peak_reserved_pixel_bytes <= 32 * 1024 * 1024);
            let close = body.begin_close().unwrap();
            paged_close_event::wait(&close);
            close.verify_released().unwrap();
            {
                let clip = scroll.contentView();
                assert_eq!(
                    clip.automaticallyAdjustsContentInsets(),
                    original_clip_settings.0
                );
                if !original_clip_settings.0 {
                    assert_eq!(
                        clip.contentInsets(),
                        original_clip_settings.1,
                        "关闭必须归还调用方实际手工inset，不能只恢复false开关"
                    );
                }
            }
            window.setContentView(None);
            window.orderOut(None);
            window.close();
            drop((before, after, view, body, window, scroll, font, paragraph));
            CATransaction::flush();
            (
                mapping,
                doc_weak,
                view_weak,
                native,
                pixels_observer,
                resources,
                gutter,
                close,
            )
        },
    );
    assert!(document.upgrade().is_none() && view.load().is_none());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
    assert!(pixels.is_released());
    close.verify_released().unwrap(); // 局部构造/关闭池结束后再核一次具体空壳证明及原真实像素事件。
    for (role, weak) in native {
        assert!(weak.load().is_none(), "{role}: 原正文图没有真实析构");
    }
    autoreleasepool(|_| {
        if let Some(ruler) = gutter.0.load() {
            // SAFETY: 仅查询原主线程控件的公开父关系；字段是否清空由上方close的具体payload证明补齐。
            assert!(unsafe { ruler.superview() }.is_none());
            assert!(ruler.clientView().is_none() && ruler.scrollView().is_none());
            assert!(ruler.window().is_none() && ruler.layer().is_none() && !ruler.wantsLayer());
        }
        if let Some(layer) = gutter.1.load() {
            // SAFETY: 只核原公开CF contents是否为空，不对其发送NSObject消息。
            assert!(unsafe { layer.contents() }.is_none());
            assert!(layer.delegate().is_none() && layer.superlayer().is_none());
            assert!(layer.backgroundColor().is_none() && layer.mask().is_none());
            // SAFETY: 原层同步存活，只读子层数组；不扫描或移除系统私有缓存。
            assert!(unsafe { layer.sublayers() }.is_none_or(|children| children.is_empty()));
        }
    }); // 允许AppKit缓存空ruler/空层，实际业务映射、像素和回调不能因此逃过资源门。
    for resources in resources {
        assert!(resources.source.payload_released());
        for observer in resources
            .reader
            .into_iter()
            .chain(std::iter::once(resources.writer))
        {
            let actual = observer.snapshot();
            assert!(
                !actual.file_open
                    && actual.mapped_bytes == 0
                    && !actual.cleanup_failed
                    && !actual.reader_failed
                    && actual.source_payload_released
            );
        }
        assert!(resources.worker.snapshot().unwrap().is_released());
        for (bytes, payload) in resources.fonts {
            assert!(
                payload.payload_released()
                    && bytes.unmap_succeeded()
                    && bytes.mapped_bytes() == 0
                    && !bytes.failed()
            );
        }
    }
    println!(
        "paged_body_source stage=gutter_small_actual_release original_lines={lines} crlf_tail_numbers=true actual_digit_width_and_undo=true cached_ruler_actual_payload_empty=true original_body_native_nil=true shared_pixels_32mib_released=true original_large_workload_substituted=false whole_product=false formal=false"
    );
}
