//! 同一超长单段的真实前缀→原生窗口回归；只验组件，不声明整产品内存或编辑功能对等。

#[path = "paged_close/event.rs"]
mod paged_close_event;

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak as RcWeak},
};

use objc2::{
    MainThreadOnly,
    rc::{Weak, autoreleasepool},
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSFont, NSScrollView,
    NSTextInputClient, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_text::CTFont;
use objc2_foundation::{MainThreadMarker, NSNotFound, NSPoint, NSRange, NSRect, NSSize};
use objc2_quartz_core::CATransaction;
use omni_markdown_core::{
    MarkdownComposedLineKind, MarkdownComposedLines, MarkdownDocumentBuffer,
    MarkdownLayoutTextSource, MarkdownLineComposeCancellation, MarkdownLineComposeError,
    MarkdownLineComposer, MarkdownLineFontCatalog,
};
use omni_md_plugin::native_surface::{
    paged_fonts::NativeMarkdownPagedFont,
    paged_input::NativeMarkdownPagedInput,
    paged_line_fonts::NativeMarkdownLineFont,
    paged_line_viewport::NativeMarkdownComposedViewport,
    paged_text_view::{
        NativeMarkdownPagedEditCommand, NativeMarkdownPagedRenderStrategy,
        NativeMarkdownPagedSceneKey, NativeMarkdownPagedSceneNonce, NativeMarkdownPagedTextScene,
        NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewDelegate,
        NativeMarkdownPagedTextViewError as Error,
    },
    paged_viewport::NativeMarkdownPagedViewport,
};

#[path = "paged_viewport/metrics.rs"]
mod metrics;
mod paged_composed_surface {
    pub mod graphics;
    pub mod lifecycle;
    pub mod producer;
    pub mod scene;
}
use paged_composed_surface::{graphics, producer, scene};

const WIDTH: f64 = 640.0;
const DENSE_WIDTH: f64 = 1137.0;
const DENSE_HEIGHT: f64 = 746.0;

fn weak(value: &AnyObject) -> Weak<AnyObject> {
    Weak::from(value)
}

fn deliver_once() {
    // SAFETY: 原main runloop的公开default mode；仅一次有限交付，不轮询Weak或重试直到成功。
    unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 0.05, false) };
}

fn pending(error: Option<Error>) {
    if let Some(error) = error {
        assert!(
            matches!(error, Error::Pending | Error::Revoked),
            "真实错误不能按尚未Ready忽略：{error:?}"
        );
        println!("paged_composed_surface unpublished_status={error:?} formal=false");
    }
}

/// 只为原窗口已准备的当前几何发布scene；正文高度仅来自真实已完成行数和同UIFont度量。
fn publish_scene(
    output: Rc<RefCell<MarkdownComposedLines>>,
    body: &NativeMarkdownPagedTextView,
    viewport: &Rc<NativeMarkdownPagedViewport>,
    owner: &Rc<scene::Owner>,
    font: &Rc<NativeMarkdownLineFont>,
    paints: &Rc<scene::PaintFacts>,
) -> Rc<scene::Scene> {
    let handles = producer::handles(&output);
    assert!((1..=producer::MAX_PREFIX_LINES).contains(&handles.len()));
    let ranges = handles
        .iter()
        .map(|line| line.facts().source_utf16.clone())
        .collect::<Box<[_]>>();
    let geometry = viewport.geometry().unwrap();
    let request = body.capture_scene_request().unwrap();
    assert_eq!(request.frame_in_parent, geometry.document_visible);
    assert_eq!(body.frame(), geometry.document_visible);
    assert_eq!(
        body.bounds(),
        NSRect::new(NSPoint::ZERO, geometry.document_visible.size)
    );
    let source = output.borrow().source_authority().clone();
    let nonce = NativeMarkdownPagedSceneNonce::new();
    let key = NativeMarkdownPagedSceneKey {
        request,
        source,
        nonce: nonce.authority(),
    };
    // 原虚拟文档的top=0来自attach的实际坐标系；转换到原y-up正文只用同geometry滚动原点。
    // 后继各行的top由窗口组件累计真正Font ascent/descent/leading，不猜全段ordinal位置。
    let first_top = NSPoint::new(
        scene::PADDING,
        body.bounds().size.height + geometry.document_visible.origin.y - scene::PADDING,
    );
    let window = NativeMarkdownComposedViewport::prepare(
        output,
        key.clone(),
        vec![font.clone()],
        handles,
        first_top,
        &|| owner.current(scene::EPOCH) && geometry.is_current(),
    )
    .unwrap();
    // 完成信号的固定tile数量来自这个真实body和实际窗口scale；它不授予seal或Ready权限。
    let scale = body.window().unwrap().backingScaleFactor();
    let size = body.bounds().size;
    let width_pixels = size.width * scale;
    let height_pixels = size.height * scale;
    assert!(width_pixels > 0.0 && height_pixels > 0.0);
    assert!((width_pixels - width_pixels.round()).abs() < 1e-6);
    assert!((height_pixels - height_pixels.round()).abs() < 1e-6);
    let tile_count = (width_pixels.round() as usize).div_ceil(512)
        * (height_pixels.round() as usize).div_ceil(512);
    let selection = if paints.theme.get().mode == graphics::Mode::Selection {
        // 只取实际完全可见原行，横跨六行且两端仍在原ASCII GCB内部；不根据固定行高猜坐标。
        let visible = || {
            ranges.iter().enumerate().filter(|(ordinal, _)| {
                let frame = window.line_frame(*ordinal).unwrap();
                frame.origin.y >= key.request.viewport.origin.y
                    && frame.origin.y + frame.size.height
                        <= key.request.viewport.origin.y + key.request.viewport.size.height
            })
        };
        let first = visible().nth(1).unwrap().1;
        let last = visible().nth(6).unwrap().1;
        assert!(first.end - first.start > 6 && last.end - last.start > 6);
        let range = first.start + 3..last.end - 3;
        assert!(range.start < range.end);
        println!(
            "paged_composed_graphics actual_selection_utf16={range:?} source_visible_intersection=true whole_document_selection=false formal=false"
        );
        Some(range)
    } else {
        None
    };
    let scene = Rc::new(scene::Scene {
        key,
        _nonce: nonce,
        geometry,
        window: Rc::new(window),
        owner: Rc::downgrade(owner),
        paints: paints.clone(),
        progress: scene::FrameProgress::new(tile_count),
        ranges,
        selection,
    });
    pending(body.take_failure()); // 发布前的Pending不能覆盖发布后真实绘图错误。
    let old = owner.scene.replace(Some(scene.clone()));
    drop(old); // 结束RefCell借用和旧owner后，才调用可重入的Native显示入口。
    body.scene_ready().unwrap();
    scene
}

fn draw_and_hit(
    stage: &str,
    body: &NativeMarkdownPagedTextView,
    window: &NSWindow,
    scene: &scene::Scene,
) -> usize {
    let before = scene.paints.calls.get();
    let cpu_before = body.cpu_paints().unwrap();
    let steps_before = body.cpu_paint_steps().unwrap();
    let started = std::time::Instant::now();
    scene.paints.ink.set(0);
    scene.paints.selected_fill.set(0);
    scene.paints.selected_text.set(0);
    if scene.paints.dense.get() {
        scene.progress.arm();
    } // 完成信号只属于本scene原有有限tile；没有测试timer进入产品路径。
    body.setNeedsDisplayInRect(body.bounds());
    body.displayIfNeeded();
    window.displayIfNeeded();
    if scene.paints.dense.get() {
        // SAFETY: 原主线程 default runloop 仍只运行一次有限5秒；完整帧发布的原通知才能 stop。
        // 最后字形、像素封存和 Native 发布是不同阶段，不能再用字形完成提前结束本等待。
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 5.0, false) };
        assert!(
            scene.progress.complete(),
            "密集候选在有限交付窗口内没有完成"
        );
        assert_eq!(body.cpu_paint_pending(), Some(false));
        CATransaction::flush(); // stop会跳过runloop尾部observer；显式提交实际整帧，不能测尚未交付CA的候选。
        assert!(
            scene.progress.visible_glyphs() > 4096,
            "密集负载必须是真正可见字形超过旧同步门，不能累计多个tile重复字形"
        );
    } else {
        deliver_once(); // 普通有限组件仍只消费已发布scene，不等待producer追赶。
    }
    let draw_elapsed_us = started.elapsed().as_micros();
    let actual_steps = body.cpu_paint_steps().unwrap() - steps_before;
    assert!(body.take_failure().is_none(), "同票真实CPU绘图失败");
    assert!(scene.current());
    assert!(scene.paints.calls.get() > before && body.cpu_paints().unwrap() > cpu_before);
    assert!(scene.paints.ink.get() > 0, "本stage没有真实字形墨迹");
    let key = body.capture_ready_scene_key().unwrap();
    assert!(key.same(&scene.key));
    let presentations = graphics::presentations(body, scene);
    #[cfg(feature = "native-markdown-runtime-probe")]
    let reused_images = body.cpu_reused_images();
    #[cfg(not(feature = "native-markdown-runtime-probe"))]
    let reused_images: Option<usize> = None;
    // 只有密集夹具首批真实行已覆盖整窗和两行overscan，后继同顶端prefix才保证可见像素不变。
    // 普通12行夹具扩到24行时会在底部新增真实字形；相同viewport/新nonce不能授权旧图复用。
    let exact_reuse_required = cfg!(feature = "native-markdown-runtime-probe")
        && scene.paints.dense.get()
        && stage == "second_prefix_top";
    let exact_reuse = !exact_reuse_required || reused_images.is_some_and(|count| count > 0);
    let selected_pixels = scene.selection.is_none()
        || (scene.paints.selected_fill.get() > 0 && scene.paints.selected_text.get() > 0);
    if presentations.is_err() || !selected_pixels || !exact_reuse {
        scene.paints.graphics_failed.set(true);
    } // 图像表示或选中文字像素失败保持sticky，仍实际关闭全部资源后才总断言。
    println!(
        "paged_composed_graphics stage={stage} mode={:?} actual_published_presentations={presentations:?} actual_reused_images={reused_images:?} same_viewport_new_nonce_exact_reuse_required={exact_reuse_required} same_viewport_new_nonce_exact_reuse={exact_reuse} actual_selected_fill_pixels={} actual_selected_text_pixels={} actual_text_over_selection={selected_pixels} complete_rgba_copy=false formal=false",
        scene.paints.theme.get().mode,
        scene.paints.selected_fill.get(),
        scene.paints.selected_text.get(),
    );
    // 选真实完全进入clip的首行；不能拿viewport外行的坐标给first_rect/命中补签。
    let (ordinal, range) = scene
        .ranges
        .iter()
        .enumerate()
        .find(|(ordinal, _)| {
            let rect = scene.window.line_frame(*ordinal).unwrap();
            rect.origin.y >= key.request.viewport.origin.y
                && rect.origin.y + rect.size.height
                    <= key.request.viewport.origin.y + key.request.viewport.size.height
        })
        .unwrap();
    let position = range.start + 1; // 夹具首word为ASCII；确定位于真实同一行的内部GCB，不依赖平均字宽。
    let first = scene.first_rect(NSRange::new(position, 1)).unwrap();
    assert_eq!(first.actual_range, NSRange::new(position, 1));
    let hit = scene
        .hit(NSPoint::new(
            first.rect.origin.x,
            first.rect.origin.y + first.rect.size.height * 0.5,
        ))
        .unwrap();
    assert_eq!(hit, position, "真实glyph caret的命中没有返回原UTF16位置");
    let proof = body
        .verify_find_target_visible(&key, NSRange::new(position, 1))
        .unwrap();
    assert_eq!(proof.range(), NSRange::new(position, 1));
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(body);
    let mut actual = NSRange::new(NSNotFound as usize, 0);
    // SAFETY: 栈输出完整可写；生产TextInputClient会把同票本地first_rect转入实际窗口屏幕坐标。
    let screen = unsafe {
        client.firstRectForCharacterRange_actualRange(NSRange::new(position, 1), &mut actual)
    };
    assert_eq!(actual, first.actual_range);
    assert!(screen.size.height > 0.0 && screen.size.width > 0.0);
    assert!(matches!(
        body.command(objc2::sel!(moveDown:)),
        Err(Error::Pending)
    ));
    assert!(matches!(
        scene.edit_plan(NativeMarkdownPagedEditCommand::InsertNewline),
        Err(Error::Pending)
    ));
    let memory = metrics::sample(stage, scene.geometry.content_height);
    if !memory.process_within_bound || !(0..100_000_000).contains(&memory.graphics) {
        scene.paints.memory_failed.set(true);
    }
    println!(
        "paged_composed_surface stage={stage} lines={} actual_visible_line={ordinal} actual_utf16={position} source_end={} ink={} paints={} unique_visible_glyphs={} actual_steps={actual_steps} draw_elapsed_us={draw_elapsed_us} dense={} real_cpu_publish=true real_first_rect=true real_hit=true full_editing_parity=false whole_product=false formal=false",
        scene.ranges.len(),
        scene.window.source_range().end,
        scene.paints.ink.get(),
        scene.paints.calls.get(),
        scene.progress.visible_glyphs(),
        scene.paints.dense.get()
    );
    position
}

fn close_output(output: Rc<RefCell<MarkdownComposedLines>>) {
    Rc::try_unwrap(output)
        .ok()
        .expect("scene仍保留原Reader")
        .into_inner()
        .close()
        .unwrap();
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next();
    let dense = matches!(
        mode.as_deref(),
        Some("dense" | "dense-unbroken" | "dense-dark" | "dense-selection")
    );
    let graphics_mode = match mode.as_deref() {
        Some("dense-dark") => graphics::Mode::Dark,
        Some("dense-selection") => graphics::Mode::Selection,
        _ => graphics::Mode::White,
    };
    let (kind, scheduler_control, lifecycle_mode) = match mode.as_deref() {
        None => (
            producer::Kind::Natural,
            false,
            paged_composed_surface::lifecycle::Mode::Normal,
        ),
        Some("unbroken") => (
            producer::Kind::Unbroken,
            false,
            paged_composed_surface::lifecycle::Mode::Normal,
        ),
        Some("dense" | "dense-dark" | "dense-selection") => (
            producer::Kind::Natural,
            false,
            paged_composed_surface::lifecycle::Mode::Normal,
        ),
        Some("dense-unbroken") => (
            producer::Kind::Unbroken,
            false,
            paged_composed_surface::lifecycle::Mode::Normal,
        ),
        Some("scheduler") => (
            producer::Kind::Natural,
            true,
            paged_composed_surface::lifecycle::Mode::Normal,
        ),
        Some("scheduler-close") => (
            producer::Kind::Natural,
            false,
            paged_composed_surface::lifecycle::Mode::PendingClose,
        ),
        Some("close-replaced") => (
            producer::Kind::Natural,
            false,
            paged_composed_surface::lifecycle::Mode::ReplacedBacking,
        ),
        Some("close-reparented") => (
            producer::Kind::Natural,
            false,
            paged_composed_surface::lifecycle::Mode::ReparentedMount,
        ),
        Some("postcommit") => (
            producer::Kind::Natural,
            false,
            paged_composed_surface::lifecycle::Mode::PostCommitRevoked,
        ),
        _ => panic!(
            "只接受默认、unbroken、dense、dense-unbroken、dense-dark、dense-selection或明确生命周期控制模式"
        ),
    };
    assert!(args.next().is_none());
    println!(
        "paged_composed_surface fixture={kind:?} dense={dense} whole_product=false formal=false"
    );
    let mtm = MainThreadMarker::new().expect("真正原生窗口必须在主线程创建");
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    let initial_memory = metrics::sample("before_composed_graph", 0.0);
    let (
        native,
        mapping,
        source_receipt,
        font_bytes,
        font_payload,
        readers,
        writer,
        worker_resources,
        doc_weak,
        input_weak,
        viewport_weak,
        pixels,
        memory_failed,
        graphics_failed,
        lifecycle_passed,
    ) = autoreleasepool(|_| {
        let source_bytes = if dense {
            4 * producer::SOURCE_BYTES
        } else {
            producer::SOURCE_BYTES
        };
        let file = producer::fixture(kind, source_bytes);
        let document = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(file.path(), (2 * source_bytes) as u64).unwrap(),
        ));
        assert_eq!(
            document.borrow().line_count(),
            1,
            "夹具必须是没有换行的超长单段"
        );
        let mapping = document.borrow().original_backing_observer().unwrap();
        let doc_weak = Rc::downgrade(&document);
        let source = MarkdownLayoutTextSource::capture(&document.borrow());
        let source_receipt = source.resource_observer();
        assert!(source.byte_len() >= source_bytes);
        let system_font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
        let ct: &CTFont = system_font.as_ref();
        let captured =
            NativeMarkdownPagedFont::capture(ct, 512 * 1024 * 1024, mtm, &|| true).unwrap();
        let font_bytes = captured.bytes_observer();
        let font_payload = captured.core_font().resource_observer();
        let catalog = MarkdownLineFontCatalog::new();
        let font = Rc::new(NativeMarkdownLineFont::register(captured, &catalog, &|| true).unwrap());
        let metrics = font.metrics().unwrap();
        let row_height = metrics.ascent + metrics.descent + metrics.leading;
        assert!(row_height.is_finite() && row_height > 0.0);
        // 大窗行数从当前系统Font真实行高得出，只有两行overscan；不从全文大小猜文档高度。
        let first_lines = if dense {
            (DENSE_HEIGHT / row_height).ceil() as usize + 2
        } else {
            producer::FIRST_LINES
        };
        let second_lines = if dense {
            first_lines * 2
        } else {
            producer::SECOND_LINES
        };
        assert!(first_lines > 0 && second_lines <= producer::MAX_PREFIX_LINES);
        let first_content_height = row_height * first_lines as f64 + 2.0 * scene::PADDING;
        let height = if dense {
            DENSE_HEIGHT
        } else {
            first_content_height
        };
        let width_points = if dense { DENSE_WIDTH } else { WIDTH };
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(width_points, height));
        let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let input_weak = Rc::downgrade(&input);
        let owner = Rc::new(scene::Owner {
            active: Cell::new(true),
            revoke_after_mount: Cell::new(false),
            revoke_fired: Cell::new(false),
            inspecting_mount: Cell::new(false),
            old_group: Cell::new(0),
            watched: RefCell::new(None),
            viewport: RefCell::new(RcWeak::new()),
            input: input_weak.clone(),
            scene: RefCell::new(None),
        });
        let erased: Rc<dyn NativeMarkdownPagedTextViewDelegate> = owner.clone();
        let body = NativeMarkdownPagedTextView::new_with_render_strategy(
            input.clone(),
            Rc::downgrade(&erased),
            scene::EPOCH,
            frame,
            NativeMarkdownPagedRenderStrategy::CpuViewport,
            mtm,
        )
        .unwrap();
        let pixels = body.pixel_observer().expect("本测试必须走真正CPU像素owner");
        let context = body.inputContext().unwrap();
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll.setHasVerticalScroller(false); // 测试只用公开clip滚动，不向产品新增控件、菜单或按键。
        // SAFETY: 唯一主线程合成borderless窗口，有限几何且没有用户文稿或标题栏按钮。
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: 本栈Retained持有窗口至close以后，避免系统自动再release同一owner。
        unsafe { window.setReleasedWhenClosed(false) };
        let theme = graphics::Theme::capture(graphics_mode, &window);
        window.setContentView(Some(&scroll));
        let viewport =
            NativeMarkdownPagedViewport::attach(&scroll, &body, first_content_height, mtm).unwrap();
        let viewport_weak = Rc::downgrade(&viewport);
        *owner.viewport.borrow_mut() = viewport_weak.clone();
        window.orderFrontRegardless();
        window.displayIfNeeded();
        deliver_once();
        pending(body.take_failure());
        if dense {
            assert_eq!(
                window.backingScaleFactor(),
                2.0,
                "密集截图负载必须实际2×，不能降分辨率"
            );
            assert_eq!(body.bounds().size, NSSize::new(DENSE_WIDTH, DENSE_HEIGHT));
        }
        println!(
            "paged_composed_surface actual_width={} actual_height={} actual_scale={} row_height={row_height} first_prefix_lines={first_lines} second_prefix_lines={second_lines} actual_font_monospace=true formal=false",
            body.bounds().size.width,
            body.bounds().size.height,
            window.backingScaleFactor()
        );
        let initial_request = body.capture_scene_request().unwrap();
        let width = ((initial_request.bounds_width - 2.0 * scene::PADDING) * 64.0).round() as i64;
        let mut worker = MarkdownLineComposer::new_with_frontier(
            source.clone(),
            &source.authority(),
            catalog.clone(),
            width,
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let writer = worker.resource_observer().unwrap();
        let worker_resources = worker.worker_resource_observer();
        assert!(worker.publish_ready_prefix().unwrap().is_none());
        let first_last = producer::next_lines(&mut worker, &font, first_lines);
        if kind == producer::Kind::Unbroken {
            assert_eq!(first_last.kind, MarkdownComposedLineKind::Emergency);
        } // 无空格真实紧急断行不能被冒签成自然换行或整词已读完。
        let first = Rc::new(RefCell::new(
            worker.publish_ready_prefix().unwrap().unwrap(),
        ));
        let first_reader = first.borrow().resource_observer();
        assert!(first_reader.snapshot().file_open && first_reader.snapshot().mapped_bytes == 0);
        assert!(writer.snapshot().file_open && writer.snapshot().mapped_bytes == 0);
        assert_eq!(
            first.borrow().source_range().end,
            first_last.source_utf16.end
        );
        assert!(first_last.source_utf16.end < source.utf16_len() / 100);
        let paints = Rc::new(scene::PaintFacts::default());
        paints.dense.set(dense);
        paints.theme.set(theme);
        let mut first_scene =
            publish_scene(first.clone(), &body, &viewport, &owner, &font, &paints);
        if scheduler_control {
            let before_publish = body.cpu_paints().unwrap();
            body.displayIfNeeded();
            assert_eq!(body.cpu_paint_pending(), Some(true));
            assert_eq!(body.cpu_paints(), Some(before_publish)); // 第一个有界step不能发布部分frame。
            let steps = body.cpu_paint_steps().unwrap();
            let bytes = pixels.snapshot().live_pixel_bytes;
            assert!(bytes > 0);
            #[cfg(feature = "native-markdown-runtime-probe")]
            body.probe_cpu_nested_delivery(deliver_once).unwrap();
            #[cfg(not(feature = "native-markdown-runtime-probe"))]
            assert!(!scheduler_control, "scheduler独立负控必须启用runtime-probe");
            assert_eq!(body.cpu_paint_steps(), Some(steps));
            assert_eq!(pixels.snapshot().live_pixel_bytes, bytes);
            body.invalidate_geometry().unwrap(); // 正常真实撤权入口立即归还候选全部实际context/像素。
            assert_eq!(body.cpu_paint_pending(), Some(false));
            assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
            deliver_once(); // 旧排队serial实际交付，但没有权利消费后继候选或新Source。
            assert_eq!(body.cpu_paint_steps(), Some(steps));
            pending(body.take_failure());
            first_scene = publish_scene(first.clone(), &body, &viewport, &owner, &font, &paints);
            assert!(first_scene.current());
            body.displayIfNeeded();
            assert_eq!(body.cpu_paint_pending(), Some(true));
            assert!(pixels.snapshot().live_pixel_bytes > 0);
            first_scene = publish_scene(first.clone(), &body, &viewport, &owner, &font, &paints);
            assert_eq!(body.cpu_paint_pending(), Some(false));
            assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
            println!(
                "paged_composed_surface actual_nested_delivery=true deferred_same_serial=true geometry_cancel_actual_pixels_zero=true nonce_cancel_actual_pixels_zero=true no_partial_publish=true independent_scheduler_control=true formal=false"
            );
        }
        let first_key = first_scene.key.clone();
        let top_position = draw_and_hit("first_prefix_top", &body, &window, &first_scene);
        let old_range = first.borrow().source_range();
        let second_last = producer::next_lines(&mut worker, &font, second_lines - first_lines);
        if kind == producer::Kind::Unbroken {
            assert_eq!(second_last.kind, MarkdownComposedLineKind::Emergency);
        }
        let second = Rc::new(RefCell::new(
            worker.publish_ready_prefix().unwrap().unwrap(),
        ));
        let second_reader = second.borrow().resource_observer();
        assert!(second_reader.snapshot().file_open && second_reader.snapshot().mapped_bytes == 0);
        assert!(writer.snapshot().file_open && writer.snapshot().mapped_bytes == 0);
        assert!(second_last.source_utf16.end > first_last.source_utf16.end);
        assert!(second_last.source_utf16.end < source.utf16_len() / 100);
        assert_eq!(first.borrow().source_range(), old_range);
        assert_eq!(
            first.borrow_mut().last_line().unwrap().unwrap().facts(),
            &first_last
        );
        let work = worker.frontier_work().unwrap();
        assert!(
            work.source_bytes < source.byte_len() / 100
                && work.input_bytes < source.byte_len() / 100
        );
        println!(
            "paged_composed_surface source_bytes={} source_utf16={} first_end={} second_end={} frontier_work={work:?} worker_still_alive=true source_whole_materialized=false font=actual_system_monospace_14pt features_changed=false formal=false",
            source.byte_len(),
            source.utf16_len(),
            first_last.source_utf16.end,
            second_last.source_utf16.end
        );
        // 后继Reader先在完全相同的原几何发布；拒旧key在此只能由真实新nonce造成，
        // 不能让滚动、resize或文稿变化替nonce检查遮掩一个错误的实现。
        let second_scene = publish_scene(second.clone(), &body, &viewport, &owner, &font, &paints);
        assert!(first_scene.geometry.is_current());
        assert!(first_key.request.same(&second_scene.key.request));
        assert!(matches!(
            body.verify_find_target_visible(&first_key, NSRange::new(top_position, 1)),
            Err(Error::Revoked)
        ));
        assert!(matches!(
            first_scene.first_rect(NSRange::new(top_position, 1)),
            Err(Error::Revoked)
        ));
        assert!(!first_key.nonce.same(&second_scene.key.nonce));
        draw_and_hit("second_prefix_top", &body, &window, &second_scene);
        // 只把实际已封存前缀行的真实度量作为当前可滚范围，未知全文高度没有被猜成一个大常量。
        let content_height = row_height * second_lines as f64 + 2.0 * scene::PADDING;
        viewport.set_content_height(content_height).unwrap();
        assert!(!second_scene.geometry.is_current());
        let expanded = publish_scene(second.clone(), &body, &viewport, &owner, &font, &paints);
        let clip = scroll.contentView();
        clip.scrollToPoint(NSPoint::new(0.0, content_height - height));
        scroll.reflectScrolledClipView(&clip);
        assert!(
            !expanded.geometry.is_current(),
            "实际clip滚动没有撤销旧几何"
        );
        let scrolled = publish_scene(second.clone(), &body, &viewport, &owner, &font, &paints);
        let later_position = draw_and_hit("second_prefix_scrolled", &body, &window, &scrolled);
        assert!(later_position > top_position);
        if !dense {
            assert!(later_position >= first_last.source_utf16.end);
        } // 大窗含两行overscan，滚到真实底部仍可见第一前缀末行；不能把它误报为没有滚动。
        assert!(matches!(
            scrolled.first_rect(NSRange::new(top_position, 1)),
            Err(Error::Pending)
        ));
        assert!(matches!(
            body.verify_find_target_visible(&second_scene.key, NSRange::new(top_position, 1)),
            Err(Error::Revoked)
        ));
        // 原输出handle不能被后继reader接受；只牺牲旧reader，不损坏仍显示的第二份输出。
        let foreign_line = second.borrow_mut().last_line().unwrap().unwrap();
        assert!(matches!(
            first.borrow_mut().part(&foreign_line, 0),
            Err(MarkdownLineComposeError::SourceMismatch)
        ));
        assert!(!first_reader.snapshot().file_open);
        // 同一Doc、相同generation的另一次capture仍是不同Source，不能借文本相同取得旧输出权限。
        let unrelated_source = MarkdownLayoutTextSource::capture(&document.borrow());
        assert!(matches!(
            second.borrow().require(&unrelated_source.authority()),
            Err(MarkdownLineComposeError::SourceMismatch)
        ));
        second.borrow().require(&source.authority()).unwrap();
        drop(unrelated_source); // 只保存实际弱身份负控，结束后不延长任何Source映射。
        let lifecycle = if lifecycle_mode != paged_composed_surface::lifecycle::Mode::Normal {
            Some(paged_composed_surface::lifecycle::Case::start(
                lifecycle_mode,
                paged_composed_surface::lifecycle::Context {
                    body: &body,
                    window: &window,
                    viewport: &viewport,
                    owner: &owner,
                    font: &font,
                    paints: &paints,
                    pixels: &pixels,
                },
                second.clone(),
            ))
        } else {
            None
        };
        if lifecycle.is_none() {
            // geometry保持有效时通过真实Input事务改变文稿generation；绕过Input直接改Doc会被
            // StateChanged一致性门拒绝，不能拿该错误替代本处所需的旧scene撤权证据。
            assert!(scrolled.geometry.is_current());
            let before_generation = input.state().unwrap().generation;
            let permit = input
                .capture(scene::EPOCH, &|epoch| owner.current(epoch))
                .unwrap();
            let generation_commit = input
                .insert_text(&permit, "x", NSRange::new(0, 0), &|epoch| {
                    owner.current(epoch)
                })
                .unwrap();
            assert_ne!(input.state().unwrap().generation, before_generation);
            assert_eq!(input.state().unwrap(), generation_commit.state);
            assert!(scrolled.geometry.is_current());
            assert!(matches!(
                body.verify_find_target_visible(&scrolled.key, NSRange::new(later_position, 1)),
                Err(Error::Pending | Error::Revoked)
            ));
            pending(body.take_failure());
            // 真resize另行撤销原宽度排版；没有实际重排producer时必须Pending，不能重签旧前缀。
            window.setContentSize(NSSize::new(480.0, height - row_height));
            scroll.setFrameSize(NSSize::new(480.0, height - row_height));
            scroll.layoutSubtreeIfNeeded();
            assert!(!scrolled.geometry.is_current());
            assert!(matches!(
                body.capture_ready_scene_key(),
                Err(Error::Pending | Error::Revoked)
            ));
            pending(body.take_failure());
        }
        let shim = viewport.document_view().unwrap();
        let body_layer = body.layer().unwrap();
        let shim_layer = shim.layer().unwrap();
        let mut native = vec![
            ("body", weak(&body)),
            ("input_context", weak(&context)),
            ("shim", weak(&shim)),
            ("body_layer", weak(&body_layer)),
            ("shim_layer", weak(&shim_layer)),
            ("scroll", weak(&scroll)),
            ("clip", weak(&clip)),
            ("window", weak(&window)),
        ];
        native.extend(
            viewport
                .observer_weaks()
                .unwrap()
                .into_iter()
                .enumerate()
                .map(|(i, weak)| {
                    (
                        if i == 0 {
                            "bounds_observer"
                        } else {
                            "frame_observer"
                        },
                        weak,
                    )
                }),
        );
        if let Some(layers) = body.cpu_layer_weaks() {
            assert_eq!(
                layers.len(),
                643,
                "必须登记mount、两组和128个slot各自的容器及两组前景/遮罩"
            );
            native.extend(layers.into_iter().map(|layer| ("cpu_owned_layer", layer)));
        }
        if let Some(lifecycle) = &lifecycle {
            native.extend(lifecycle.weaks());
        }
        drop((shim, body_layer, shim_layer, context));
        assert!(!viewport.is_busy() && !body.is_busy());
        let lifecycle_before = if let Some(lifecycle) = &lifecycle {
            lifecycle.before_close(&body)
        } else {
            viewport.close().unwrap();
            true
        };
        let close_started = std::time::Instant::now();
        let close = body.begin_close().unwrap();
        assert!(Rc::ptr_eq(&close, &body.begin_close().unwrap()));
        paged_close_event::wait(&close);
        let close_elapsed_us = close_started.elapsed().as_micros();
        let close_pixels = pixels.snapshot();
        let lifecycle_passed = if let Some(lifecycle) = lifecycle {
            viewport.close().unwrap(); // 原View收据已经真实完成，再由外层停止viewport通知。
            lifecycle.after_close(&body, &pixels, &close) && lifecycle_before
        } else {
            close.verify_released().unwrap();
            true
        };
        println!(
            "paged_composed_surface actual_body_close_elapsed_us={close_elapsed_us} actual_close_pixels={close_pixels:?} native_flush_in_close=true polling=false formal=false"
        );
        owner.active.set(false);
        let discarded = owner.scene.borrow_mut().take();
        drop(discarded);
        window.setContentView(None);
        window.orderOut(None);
        window.close();
        drop((first_scene, second_scene, expanded, scrolled, foreign_line));
        // 负控已真实毒化旧reader，正常close不能再报成功；第二份healthy输出必须正常close。
        assert!(matches!(
            Rc::try_unwrap(first).ok().unwrap().into_inner().close(),
            Err(MarkdownLineComposeError::Poisoned)
        ));
        close_output(second);
        drop(worker); // writer在全部实际append后归还真实FD/映射，不用取消掩盖reader生命周期。
        drop((
            body,
            clip,
            scroll,
            window,
            viewport,
            input,
            owner,
            erased,
            font,
            catalog,
            source,
            document,
            system_font,
        ));
        CATransaction::flush(); // 只提交一次公开事务边界，不增加延时或轮询框架缓存。
        let memory_failed = paints.memory_failed.get();
        let graphics_failed = paints.graphics_failed.get();
        (
            native,
            mapping,
            source_receipt,
            font_bytes,
            font_payload,
            [first_reader, second_reader],
            writer,
            worker_resources,
            doc_weak,
            input_weak,
            viewport_weak,
            pixels,
            memory_failed,
            graphics_failed,
            lifecycle_passed,
        )
    });
    let mut native_released = true;
    for (index, (role, weak)) in native.into_iter().enumerate() {
        let released = weak.load().is_none();
        println!(
            "paged_composed_surface release_role={role} role_index={index} actual_weak_nil={released} formal=false"
        );
        native_released &= released;
    }
    let rust_released = doc_weak.upgrade().is_none()
        && input_weak.upgrade().is_none()
        && viewport_weak.upgrade().is_none();
    let source_released = source_receipt.payload_released()
        && mapping.unmap_succeeded()
        && mapping.mapped_bytes() == 0
        && !mapping.failed();
    let font_released = font_payload.payload_released()
        && font_bytes.unmap_succeeded()
        && font_bytes.mapped_bytes() == 0
        && !font_bytes.failed();
    let mut files_closed = true;
    for (role, observer) in [
        ("old_prefix", &readers[0]),
        ("current_prefix", &readers[1]),
        ("writer", &writer),
    ] {
        let state = observer.snapshot();
        let closed = !state.file_open
            && state.mapped_bytes == 0
            && !state.cleanup_failed
            && state.source_payload_released;
        println!(
            "paged_composed_surface release_role={role} resources={state:?} actual_closed={closed} intentional_foreign_reader={}",
            role == "old_prefix"
        );
        files_closed &= closed;
    }
    let pixels_released = pixels.is_released();
    let worker_snapshot = worker_resources
        .snapshot()
        .expect("原worker资源台账poison不能以空收据通过");
    let worker_released = worker_snapshot.is_released();
    println!(
        "paged_composed_surface release_role=complete_worker resources={worker_snapshot:?} actual_released={worker_released} formal=false"
    );
    let final_memory = metrics::sample("composed_released", 0.0);
    println!(
        "paged_composed_surface release native={native_released} rust={rust_released} source_mapping={source_released} font_mapping={font_released} files_closed={files_closed} pixels_released={pixels_released} pixels={:?} formal=false",
        pixels.snapshot()
    );
    assert!(
        native_released
            && rust_released
            && source_released
            && font_released
            && files_closed
            && pixels_released
            && worker_released
    );
    assert!(
        lifecycle_passed,
        "异常生命周期失败，全部实际资源事实已先记录，不能签收"
    );
    assert!(
        !graphics_failed,
        "实际图像表示或原可见选中文字像素检查失败，不能签收"
    );
    assert!(
        initial_memory.process_within_bound && final_memory.process_within_bound && !memory_failed,
        "组件实际footprint/RSS/graphics未通过100MB门槛，不能签收"
    );
    let generation_and_resize_exercised =
        lifecycle_mode == paged_composed_surface::lifecycle::Mode::Normal;
    println!(
        "paged_composed_surface real_bounded_prefixes=true dense={dense} same_worker_continues=true source_end_below_one_percent=true real_native_window=true actual_glyph_pixels=true actual_first_rect_hit=true actual_scroll_revocation=true actual_resize_revocation={generation_and_resize_exercised} actual_generation_revocation={generation_and_resize_exercised} actual_resources_released=true full_editing_parity=false whole_product=false formal=false"
    );
}
