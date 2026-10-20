//! 原长文活动逻辑行的实际挂载像素；有限尾空行另在原大负载严格关闭后顺序构造。

use super::*;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
    kCGColorSpaceSRGB,
};
use omni_markdown_core::MarkdownTextSelection;
use omni_md_plugin::native_surface::{
    paged_body::NativeMarkdownPagedBodyResourceReceipts,
    paged_text_view::{NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewDelegate},
};

/// 只有单个640×240真实视口，context先归还后释放其RGBA；不保存跨动作图像。
struct Canvas {
    context: CFRetained<CGContext>,
    pixels: Vec<u8>,
}
impl Canvas {
    /// 仅按真实viewport和窗口scale分配有限像素；不接受整篇文档高度或任意输出尺寸。
    fn new(viewport: NSRect, scale: f64) -> Self {
        assert!(viewport.size.width > 0.0 && viewport.size.width <= 640.0);
        assert!(viewport.size.height > 0.0 && viewport.size.height <= 240.0);
        assert!(scale > 0.0 && scale <= 2.0);
        let width = viewport.size.width * scale;
        let height = viewport.size.height * scale;
        assert_eq!(width, width.round());
        assert_eq!(height, height.round());
        let (width, height) = (width as usize, height as usize);
        let mut pixels = vec![0; width * height * 4];
        // SAFETY: 系统公开sRGB常量具有进程寿命；不使用被测Scene的颜色空间对象。
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap();
        // SAFETY: Vec覆盖真实RGBA8 stride*height且不resize；字段顺序保证context先析构。
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
        CGContext::translate_ctm(Some(&context), -viewport.origin.x, -viewport.origin.y);
        CGContext::clip_to_rect(Some(&context), viewport);
        Self { context, pixels }
    }
}

/// 固定夹具颜色由独立CG实绘量化；不从生产Scene像素反推期望颜色。
fn color_pixel(color: [f64; 4]) -> [u8; 4] {
    let mut pixel = [0; 4];
    // SAFETY: 系统公开sRGB静态名在本同步调用中有效。
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap();
    // SAFETY: 栈内四字节覆盖唯一像素，原context在返回像素前显式归还。
    let context = unsafe {
        CGBitmapContextCreate(
            pixel.as_mut_ptr().cast(),
            1,
            1,
            8,
            4,
            Some(&space),
            CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
        )
    }
    .unwrap();
    CGContext::set_rgb_fill_color(Some(&context), color[0], color[1], color[2], color[3]);
    CGContext::fill_rect(
        Some(&context),
        NSRect::new(NSPoint::ZERO, NSSize::new(1.0, 1.0)),
    );
    CGContext::flush(Some(&context));
    drop(context);
    pixel
}

/// 只在同一完整CPU帧中读取真实CALayer；独立位置遮罩来自GCB几何，不读取活动行元数据。
fn pixels(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    window: &NSWindow,
    stage: &str,
    samples: &[(usize, bool)],
    selected_space: Option<usize>,
    clipped: bool,
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
        assert!(scene.key().same(&key) && current());
        scene.validate_ready(&current).unwrap();
        let viewport = key.request.viewport;
        let scale = window.backingScaleFactor();
        let mask = Canvas::new(viewport, scale);
        let mut rows = Vec::with_capacity(samples.len());
        for &(source, active) in samples {
            let length = usize::from(source < key.request.input.utf16_len);
            let first = scene.first_rect(NSRange::new(source, length)).unwrap();
            assert_eq!(first.actual_range.location, source);
            let bottom = first.rect.origin.y.max(viewport.origin.y);
            let top = (first.rect.origin.y + first.rect.size.height)
                .min(viewport.origin.y + viewport.size.height);
            assert!(top > bottom);
            let y = (bottom + top) / 2.0;
            if let Some((_, previous)) = rows.iter().find(|(known, _)| *known == y) {
                assert_eq!(
                    *previous, active,
                    "{stage}: 不同逻辑行采样错误地共享实际GCB行框，source={source} rect={:?} viewport={viewport:?}",
                    first.rect
                );
            } else {
                rows.push((y, active)); // 真实纵坐标去重；保留期望以拒绝后画遮罩覆盖错误的EOF几何。
            }
            // 左右各2pt避开8pt正文padding；同一逻辑行的不同真实softwrap都必须整宽着色。
            CGContext::set_rgb_fill_color(
                Some(&mask.context),
                if active { 1.0 } else { 0.0 },
                if active { 0.0 } else { 1.0 },
                0.0,
                1.0,
            );
            for x in [
                viewport.origin.x + 2.0,
                viewport.origin.x + viewport.size.width - 4.0,
            ] {
                CGContext::fill_rect(
                    Some(&mask.context),
                    NSRect::new(NSPoint::new(x, y - 0.5), NSSize::new(2.0, 1.0)),
                );
            }
        }
        if clipped {
            // 首GCB必须确实横跨原viewport上缘，才有裁切边缘像素可验，普通整行不替代它。
            let first = scene.first_rect(NSRange::new(samples[0].0, 1)).unwrap();
            let edge = viewport.origin.y + viewport.size.height;
            assert!(
                first.rect.origin.y < edge && first.rect.origin.y + first.rect.size.height > edge
            );
            CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
            CGContext::fill_rect(
                Some(&mask.context),
                NSRect::new(
                    NSPoint::new(viewport.origin.x + viewport.size.width - 4.0, edge - 1.0),
                    NSSize::new(2.0, 1.0),
                ),
            );
        }
        if let Some(space) = selected_space {
            // 选中的是固定ASCII空格；中心应为原选择蓝，不靠字形抗锯齿颜色猜测选区存在。
            let first = scene.first_rect(NSRange::new(space, 1)).unwrap();
            assert_eq!(first.actual_range, NSRange::new(space, 1));
            assert!(first.rect.size.width > 2.0);
            CGContext::set_rgb_fill_color(Some(&mask.context), 0.0, 0.0, 1.0, 1.0);
            CGContext::fill_rect(
                Some(&mask.context),
                NSRect::new(
                    NSPoint::new(
                        first.rect.origin.x + first.rect.size.width / 2.0 - 0.5,
                        first.rect.origin.y + first.rect.size.height / 2.0 - 0.5,
                    ),
                    NSSize::new(1.0, 1.0),
                ),
            );
        }
        let bounds = view.bounds();
        let backing = view.layer().unwrap();
        // SAFETY: 同步栈保活原view/backing，只读取当前实际挂载子图，不建立影子LayerPool。
        let children = unsafe { backing.sublayers() }.unwrap();
        assert_eq!(children.len(), 1);
        let mount = children.objectAtIndex(0);
        assert_eq!(mount.frame(), bounds);
        assert_eq!(mount.bounds(), NSRect::new(NSPoint::ZERO, bounds.size));
        assert!(current());
        let rendered = Canvas::new(viewport, scale);
        CGContext::translate_ctm(Some(&rendered.context), bounds.origin.x, bounds.origin.y);
        mount.renderInContext(&rendered.context);
        CGContext::flush(Some(&rendered.context));
        CGContext::flush(Some(&mask.context));
        scene.validate_ready(&current).unwrap();
        let expected = [
            color_pixel([27.0 / 255.0, 32.0 / 255.0, 42.0 / 255.0, 1.0]),
            color_pixel([1.0; 4]),
            color_pixel([0.7, 0.8, 1.0, 1.0]),
        ];
        let mut locations = [0_usize; 3];
        for (index, (pixel, marker)) in rendered
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(mask.pixels.as_chunks::<4>().0)
            .enumerate()
        {
            let sample = match *marker {
                [255, 0, 0, 255] => 0,
                [0, 255, 0, 255] => 1,
                [0, 0, 255, 255] => 2,
                _ => continue, // 非完整遮罩像素包含边缘抗锯齿，不能用容差将其冒充纯色证据。
            };
            locations[sample] += 1;
            assert_eq!(
                *pixel, expected[sample],
                "{stage}: 实际挂载像素{index}类别{sample}"
            );
        }
        assert!(locations[0] > 0);
        if samples.iter().any(|sample| !sample.1) {
            assert!(locations[1] > 0); // 未活动的真实相邻逻辑行必须也有独立白底像素证据。
        }
        if selected_space.is_some() {
            assert!(locations[2] > 0); // 选区覆盖要求实像素存在，不能因遮罩越界空集而通过。
        }
        if samples.len() >= 4 {
            assert!(rows.len() >= 3, "必须实际覆盖至少三个不同softwrap行");
        }
        assert!(current());
        println!(
            "paged_body_source stage={stage} actual_layer_active_line_pixels={locations:?} actual_sampled_visual_rows={} actual_backing_scale={scale} clipped_gcb={clipped} exact_rgba=true source_range_oracle=false screen_present_verified=false whole_product=false formal=false",
            rows.len()
        );
    }); // 本阶段的全部像素、context、Scene和Native临时引用到此归还。
}

/// 点击用原Scene实际GCB左侧命中；真正mouseDown/Shift仍交原view输入协议处理。
fn click(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    window: &NSWindow,
    source: usize,
    shift: bool,
) {
    let key = view.capture_ready_scene_key().unwrap();
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(body, &key.request).unwrap();
    let first = scene.first_rect(NSRange::new(source, 1)).unwrap();
    let local = NSPoint::new(
        first.rect.origin.x + 0.125,
        first.rect.origin.y + first.rect.size.height / 2.0,
    );
    assert_eq!(scene.hit(local).unwrap(), source);
    let point = view.convertPoint_toView(local, None);
    let flags = if shift {
        NSEventModifierFlags::Shift // 原系统Shift语义保留前一个真实anchor。
    } else {
        NSEventModifierFlags::empty() // 普通点击沿原路径折叠为所命中的caret。
    };
    let down = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDown, point, flags, 1.0, window.windowNumber(), None, 1, 1, 1.0).unwrap();
    let up = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseUp, point, flags, 1.001, window.windowNumber(), None, 2, 1, 0.0).unwrap();
    // SAFETY: 真实主线程view及同步存活的固定公开鼠标事件，按NSResponder标准void ABI各交付一次。
    unsafe {
        let _: () = objc2::msg_send![view, mouseDown: &*down];
        let _: () = objc2::msg_send![view, mouseUp: &*up];
    }
}

/// 每个实际动作仍等原三秒完整帧；原大负载懒加载和真实资源票继续沿主测试核验。
fn finish(
    body: &NativeMarkdownPagedBody,
    stage: &str,
    frame: Rc<PaintDelivery>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
) {
    deliver(stage, body, frame);
    receipts.push(body.resource_receipts().unwrap());
}

pub(super) fn run(
    body: &NativeMarkdownPagedBody,
    window: &NSWindow,
    scroll: &NSScrollView,
    view: &NativeMarkdownPagedTextView,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
) {
    pixels(
        body,
        view,
        window,
        "active_original_long_line",
        &[(1, true), (80, true), (160, true), (240, true), (320, true)],
        None,
        false,
    );
    let key = view.capture_ready_scene_key().unwrap();
    // 返回文首保留真实行号横向占区；Scene必须引用同一正文frame，而不是把左侧遮挡算作正文。
    assert_eq!(
        key.request.virtual_viewport.document_origin,
        view.frame().origin
    );
    assert_eq!(key.request.virtual_viewport.document_origin.y, 0.0);
    let first = view
        .verify_find_target_visible(&key, NSRange::new(1, 1))
        .unwrap()
        .first_rect();
    let clip = scroll.contentView();
    let frame = observe(body, Instant::now());
    clip.scrollToPoint(NSPoint::new(0.0, 8.0 + first.size.height / 2.0));
    scroll.reflectScrolledClipView(&clip);
    window.displayIfNeeded();
    finish(body, "active_long_line_clipped", frame, receipts);
    pixels(
        body,
        view,
        window,
        "active_long_line_clipped_pixels",
        &[(1, true), (80, true), (160, true), (240, true)],
        None,
        true,
    );
    let frame = observe(body, Instant::now());
    clip.scrollToPoint(NSPoint::ZERO);
    scroll.reflectScrolledClipView(&clip);
    window.displayIfNeeded();
    finish(body, "active_long_line_restore_clip", frame, receipts);
    let original = view.input_state().unwrap();
    let original_revision = key
        .request
        .document
        .upgrade()
        .unwrap()
        .borrow()
        .current_revision()
        .unwrap();
    let first_line = "alpha ".repeat(40); // 只新增240字节有限输入，原四MiB正文不读出、不替换。
    let prefix = format!("{first_line}\r\nsecond word\r\n");
    let second = first_line.len() + 2;
    let tail = prefix.len();
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(view);
    let frame = observe(body, Instant::now());
    // SAFETY: 只有本次有限新输入，明确插入原BOF，原NSTextInputClient负责canonical事务。
    unsafe {
        client.insertText_replacementRange(&NSString::from_str(&prefix), NSRange::new(0, 0));
    }
    finish(body, "active_crlf_prefix", frame, receipts);
    for (stage, source, shift, anchor, active_first, selected) in [
        ("active_first_caret", 1, false, 1, true, false),
        (
            "active_second_caret",
            second + 2,
            false,
            second + 2,
            false,
            false,
        ),
        ("active_reverse_selection", 1, true, second + 2, true, true),
        ("active_forward_origin", 1, false, 1, true, false),
        ("active_forward_selection", second + 2, true, 1, false, true),
    ] {
        // 每次都交实际原生点击，不直接改Doc选择；first/second的期望来自本固定CRLF输入。
        let frame = observe(body, Instant::now());
        click(body, view, window, source, shift);
        finish(body, stage, frame, receipts);
        assert_eq!(
            view.input_state().unwrap().directed_selection(),
            Some(MarkdownTextSelection {
                anchor_utf16: anchor,
                head_utf16: source
            })
        );
        pixels(
            body,
            view,
            window,
            stage,
            &[
                (1, active_first),
                (80, active_first),
                (160, active_first),
                (220, active_first),
                (second + 2, !active_first),
                (tail + 1, false),
            ],
            selected.then_some(5),
            false,
        );
    }
    let frame = observe(body, Instant::now());
    view.command(sel!(undo:)).unwrap();
    finish(body, "active_restore_original_source", frame, receipts);
    let restored = view.input_state().unwrap();
    assert_eq!(restored.utf16_len, original.utf16_len);
    assert_eq!(restored.directed_selection(), original.directed_selection());
    assert_eq!(
        view.capture_ready_scene_key()
            .unwrap()
            .request
            .document
            .upgrade()
            .unwrap()
            .borrow()
            .current_revision()
            .unwrap(),
        original_revision
    );
    pixels(
        body,
        view,
        window,
        "active_restored_long_line",
        &[(1, true), (80, true), (160, true), (240, true)],
        None,
        false,
    );
}

/// 只在原四MiB的Doc/Native/映射已严格释放后运行；此小样本仅证明真实EOF空行像素。
pub(super) fn eof(mtm: MainThreadMarker) {
    let (mapping, doc_weak, view_weak, native, pixels, resources) = autoreleasepool(|_| {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"head\r\n").unwrap();
        let document = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(file.path(), 4096).unwrap(),
        ));
        document
            .borrow_mut()
            .set_selection(MarkdownTextSelection::caret(6))
            .unwrap();
        let mapping = document.borrow().original_backing_observer().unwrap();
        let doc_weak = Rc::downgrade(&document);
        let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(640.0, 240.0));
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), bounds);
        scroll.setHasVerticalScroller(true);
        let font = native_markdown_document_fixed_font(14.0)
            .expect("实际活动行控制必须取得生产文稿等宽字体");
        let paragraph = NSParagraphStyle::defaultParagraphStyle();
        let body = NativeMarkdownPagedBody::attach(
            document,
            &scroll,
            font.as_ref(),
            &paragraph,
            NativeMarkdownPagedBodyConfiguration {
                presentation_mode: omni_markdown_core::MarkdownPresentationMode::Source,
                epoch: 72,
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
        let frame = observe(&body, Instant::now());
        let view = body.view().unwrap();
        let view_weak = Weak::from(&**view);
        let pixels = view.pixel_observer().unwrap();
        // SAFETY: 独立合成EOF夹具沿原无边框窗口装配，不产生产品关闭按钮或快捷键。
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                bounds,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: Retained在close之后仍持有window，关闭不额外autorelease。
        unsafe { window.setReleasedWhenClosed(false) };
        window.setContentView(Some(&scroll));
        window.orderFrontRegardless();
        window.displayIfNeeded();
        deliver_frame("active_small_eof_frame", &body, frame);
        assert_eq!(
            view.input_state().unwrap().directed_selection(),
            Some(MarkdownTextSelection::caret(6))
        );
        let downstream = {
            let key = view.capture_ready_scene_key().unwrap();
            let scene =
                NativeMarkdownPagedTextViewDelegate::ready_scene(&*body, &key.request).unwrap();
            let eof = scene.first_rect(NSRange::new(6, 0)).unwrap();
            let text = scene.first_rect(NSRange::new(1, 1)).unwrap();
            assert_eq!(eof.actual_range, NSRange::new(6, 0));
            assert!(
                eof.rect.origin.y < text.rect.origin.y,
                "真EOF尾空行不能借前行几何：eof={:?} first_text={:?}",
                eof.rect,
                text.rect
            );
            // 真实空行中的原命中也必须回同一EOF；不按上一行x/y或固定行高制造位置。
            assert_eq!(
                scene
                    .hit(NSPoint::new(
                        eof.rect.origin.x + 0.125,
                        eof.rect.origin.y + eof.rect.size.height / 2.0
                    ))
                    .unwrap(),
                6
            );
            eof.rect
        };
        self::pixels(
            &body,
            &view,
            &window,
            "active_small_eof_pixels",
            &[(6, true), (1, false)],
            None,
            false,
        );
        let mut resources = vec![body.resource_receipts().unwrap()];
        let before = view.input_state().unwrap();
        let frame = observe(&body, Instant::now());
        // 原系统DocumentEnd意图使同一head明确取得上游侧，不能以相同整数位置当作无动作。
        view.command(sel!(moveToEndOfDocument:)).unwrap();
        deliver_frame("active_small_eof_upstream_frame", &body, frame);
        let upstream = view.capture_ready_scene_key().unwrap();
        assert_eq!(upstream.request.input.generation, before.generation);
        assert_eq!(
            upstream.request.input.directed_selection(),
            Some(MarkdownTextSelection::caret(6))
        );
        assert!(upstream.request.input.selection_serial > before.selection_serial);
        assert_eq!(
            view.verify_find_target_visible(&upstream, NSRange::new(6, 0))
                .unwrap()
                .first_rect(),
            downstream
        );
        self::pixels(
            &body,
            &view,
            &window,
            "active_small_eof_upstream_pixels",
            &[(6, true), (1, false)],
            None,
            false,
        );
        resources.push(body.resource_receipts().unwrap());
        let mut native = body.native_weaks().unwrap();
        native.push(("active-eof-scroll", weak(&scroll)));
        native.push(("active-eof-window", weak(&window)));
        let close = body.begin_close().unwrap();
        paged_close_event::wait(&close);
        close.verify_released().unwrap();
        window.setContentView(None);
        window.orderOut(None);
        window.close();
        drop((view, body, window, scroll, font, paragraph));
        CATransaction::flush();
        (mapping, doc_weak, view_weak, native, pixels, resources)
    });
    assert!(doc_weak.upgrade().is_none() && view_weak.load().is_none());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
    assert!(native.into_iter().all(|(_, weak)| weak.load().is_none()));
    assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
    for resources in resources {
        assert!(resources.source.payload_released());
        assert!(resources.reader.is_some(), "完整帧必须创建真实Reader");
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
                payload.payload_released() && bytes.unmap_succeeded() && bytes.mapped_bytes() == 0
            );
        }
    }
    println!(
        "paged_body_source stage=active_small_eof_release actual_mmap_native_pixels_worker_released=true original_large_workload_substituted=false screen_present_verified=false whole_product=false formal=false"
    );
}
