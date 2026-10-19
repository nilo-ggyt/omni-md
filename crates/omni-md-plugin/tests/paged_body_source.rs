//! 显式 Source 装配候选的真实窗口控制；不代表默认产品或完整 Markdown 功能验收。

#[path = "paged_close/event.rs"]
mod paged_close_event;
// 复用现有真实Mach/rusage采样，不用RSS中的共享文件页代替产品physical-footprint。
#[path = "../src/native_surface/find_bar/backend/probe/source_body/metrics.rs"]
mod process_memory;

use std::{
    cell::{Cell, RefCell},
    io::Write,
    rc::Rc,
    time::{Duration, Instant},
};

use objc2::{
    MainThreadOnly,
    rc::{Weak, autoreleasepool},
    runtime::{AnyObject, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSFont, NSParagraphStyle,
    NSScrollView, NSTextInputClient, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_text::CTFont;
use objc2_foundation::{MainThreadMarker, NSNotFound, NSPoint, NSRange, NSRect, NSSize, NSString};
use objc2_quartz_core::CATransaction;
use omni_markdown_core::MarkdownDocumentBuffer;
use omni_md_plugin::native_surface::{
    paged_body::{
        NativeMarkdownPagedBody, NativeMarkdownPagedBodyColors,
        NativeMarkdownPagedBodyConfiguration, NativeMarkdownPagedBodyPaintObserver,
        NativeMarkdownPagedBodyProgress,
    },
    paged_text_view::{NativeMarkdownPagedSceneKey, NativeMarkdownPagedTextViewError},
};

const SOURCE_BYTES: usize = 4 * 1024 * 1024;

#[path = "paged_body_source/fixture.rs"]
mod fixture;
use fixture::SourceFixture;

#[path = "paged_body_source/active_line.rs"]
mod active_line;
#[path = "paged_body_source/gutter.rs"]
mod gutter;
#[cfg(feature = "native-markdown-runtime-probe")]
#[path = "paged_body_source/reentry.rs"]
mod reentry;
#[path = "paged_body_source/targets.rs"]
mod targets;
#[path = "paged_body_source/word.rs"]
mod word;

fn weak(value: &AnyObject) -> Weak<AnyObject> {
    Weak::from(value)
}

struct PaintReceipt {
    key: NativeMarkdownPagedSceneKey,
    paints: usize,
    elapsed: Duration,
}

struct PaintDelivery {
    started: Instant,
    run_loop: CFRetained<CFRunLoop>,
    waiting: Cell<bool>,
    receipt: RefCell<Option<PaintReceipt>>,
}

impl NativeMarkdownPagedBodyPaintObserver for PaintDelivery {
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        assert!(self.receipt.borrow().is_none(), "单次帧观察不能重复交付");
        *self.receipt.borrow_mut() = Some(PaintReceipt {
            key: key.clone(),
            paints,
            elapsed: self.started.elapsed(),
        });
        if self.waiting.get() {
            self.run_loop.stop(); // 只停止这次正在交付的原主 runloop，不操作应用退出或产品按键。
        }
    }
}

fn observe(body: &NativeMarkdownPagedBody, started: Instant) -> Rc<PaintDelivery> {
    let observer = Rc::new(PaintDelivery {
        started,
        run_loop: CFRunLoop::current().expect("主线程必须有原 runloop"),
        waiting: Cell::new(false),
        receipt: RefCell::new(None),
    });
    let erased: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = observer.clone();
    body.observe_next_cpu_frame(Rc::downgrade(&erased)).unwrap();
    observer
}

fn deliver(stage: &str, body: &NativeMarkdownPagedBody, observer: Rc<PaintDelivery>) {
    let progress = deliver_frame(stage, body, observer);
    assert!(!progress.complete && progress.source_total_bytes >= SOURCE_BYTES);
    assert!(progress.frontier_source_bytes < SOURCE_BYTES / 100);
    assert!(progress.shaped_input_bytes < SOURCE_BYTES / 100);
}

fn deliver_frame(
    stage: &str,
    body: &NativeMarkdownPagedBody,
    observer: Rc<PaintDelivery>,
) -> NativeMarkdownPagedBodyProgress {
    if observer.receipt.borrow().is_none() {
        observer.waiting.set(true);
        // SAFETY: 保持原三秒有限 deadline；唯一原帧完成事件可以提前停止，不轮询状态。
        // 若原生动作已同步交付该事件，上面的单次事实检查避免再次等待，不补发或绘图。
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 3.0, false) };
        observer.waiting.set(false);
    }
    if let Some(cause) = body.take_failure() {
        panic!("{stage}: 原 Source producer 失败：{cause:?}");
    }
    let view = body.view().unwrap();
    if let Some(cause) = view.take_failure() {
        assert!(
            matches!(cause, NativeMarkdownPagedTextViewError::Pending),
            "{stage}: {cause:?}"
        );
    }
    let progress = body.progress().unwrap();
    println!("paged_body_source stage={stage} actual_progress_before_ready={progress:?}");
    println!(
        "paged_body_source stage={stage} actual_delivery_elapsed_ms={:.3} actual_cpu_paints={:?} actual_cpu_pending={:?} actual_cpu_steps={:?} original_frame_notification={} deadline_seconds=3 formal=false",
        observer.started.elapsed().as_secs_f64() * 1000.0,
        view.cpu_paints(),
        view.cpu_paint_pending(),
        view.cpu_paint_steps(),
        observer.receipt.borrow().is_some(),
    );
    let receipt = observer
        .receipt
        .borrow_mut()
        .take()
        .expect("原三秒 deadline 内必须收到真实完整帧通知");
    let key = view
        .capture_ready_scene_key()
        .expect("真实续体必须已发布同票窗口");
    assert!(receipt.key.same(&key), "完成通知不能被后继 key 补签");
    assert_eq!(view.cpu_paints(), Some(receipt.paints));
    assert_eq!(view.cpu_paint_pending(), Some(false));
    println!(
        "paged_body_source stage={stage} actual_action_to_frame_ms={:.3} actual_original_paints={} original_key_verified=true whole_product=false formal=false",
        receipt.elapsed.as_secs_f64() * 1000.0,
        receipt.paints,
    );
    assert!(progress.maximum_step_bytes <= 4096);
    assert!(progress.maximum_step_scalars <= 256);
    assert!(progress.maximum_step_metadata_units <= 256);
    println!(
        "paged_body_source stage={stage} actual_generation={} frontier_source_bytes={} actual_shape_input_bytes={} composer_step_bytes={} composer_step_scalars={} composer_step_metadata={} maximum_step_bytes={} maximum_step_scalars={} maximum_step_metadata={} trial_source_bytes={} total_bytes={} prepared_lines={} actual_height={} source_open_streamed=true real_source_scene=true whole_product=false formal=false",
        key.request.input.generation,
        progress.frontier_source_bytes,
        progress.shaped_input_bytes,
        progress.composer_step_bytes,
        progress.composer_step_scalars,
        progress.composer_step_metadata_units,
        progress.maximum_step_bytes,
        progress.maximum_step_scalars,
        progress.maximum_step_metadata_units,
        progress.trial_source_bytes,
        progress.source_total_bytes,
        progress.prepared_lines,
        progress.prepared_height
    );
    process_memory::sample(stage).expect("原Source完整帧的内核内存账必须可读取");
    progress
}

fn main() {
    let mtm = MainThreadMarker::new().expect("测试入口必须在 AppKit 主线程");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("gutter-layouts")) {
        assert!(
            std::env::args_os().nth(2).is_none(),
            "行号布局控制不接受额外参数"
        );
        // 独立重现位数变化的真实scroll/ruler布局；无参和cjk原长文流程仍完整执行同一控制。
        gutter::small(mtm);
        return;
    }
    let fixture = SourceFixture::from_arguments();
    process_memory::sample("source_before_open").unwrap();
    let (weak, mapping, pixels) = autoreleasepool(|_| {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        fixture.write_source(&mut file).unwrap();
        file.flush().unwrap();
        assert_eq!(
            file.as_file().metadata().unwrap().len(),
            fixture.source_bytes() as u64
        );
        println!(
            "paged_body_source fixture={} fixture_source_bytes={} fixture_source_utf16={} maximum_source_write_bytes={} single_paragraph=true formal=false whole_product=false",
            fixture.name(),
            fixture.source_bytes(),
            fixture.source_utf16(),
            fixture.maximum_write_bytes(),
        );
        let first_started = Instant::now();
        let document = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(file.path(), (SOURCE_BYTES * 2) as u64).unwrap(),
        ));
        assert_eq!(document.borrow().line_count(), 1);
        assert_eq!(document.borrow().utf16_len(), fixture.source_utf16());
        let mut initial_prefix = [0_u16; 4];
        document
            .borrow()
            .copy_utf16_code_units(0, &mut initial_prefix)
            .unwrap();
        assert_eq!(initial_prefix, fixture.initial_prefix());
        let doc_weak = Rc::downgrade(&document);
        let mapping = document.borrow().original_backing_observer().unwrap();
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(640.0, 240.0));
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll.setHasVerticalScroller(true);
        let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
        let ct: &CTFont = font.as_ref();
        let paragraph = NSParagraphStyle::defaultParagraphStyle();
        let body = NativeMarkdownPagedBody::attach(
            document,
            &scroll,
            ct,
            &paragraph,
            NativeMarkdownPagedBodyConfiguration {
                presentation_mode: omni_markdown_core::MarkdownPresentationMode::Source,
                epoch: 71,
                maximum_font_bytes: 512 * 1024 * 1024,
                padding: 8.0,
                colors: NativeMarkdownPagedBodyColors {
                    background: [1.0, 1.0, 1.0, 1.0],
                    active_line: [27.0 / 255.0, 32.0 / 255.0, 42.0 / 255.0, 1.0],
                    text: [0.0, 0.0, 0.0, 1.0],
                    selection: [0.7, 0.8, 1.0, 1.0],
                    caret: [0.0, 0.0, 0.0, 1.0],
                },
            },
            mtm,
        )
        .unwrap();
        let first_observer = observe(&body, first_started);
        let view = body.view().unwrap();
        let pixels = view.pixel_observer().unwrap();
        let view_weak = Weak::from(&**view);
        // SAFETY: 合成测试使用唯一 borderless 窗口；没有红黄绿按钮或新增产品交互。
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: Retained 栈持有真实 window 至 close 后，避免额外自动 release。
        unsafe { window.setReleasedWhenClosed(false) };
        window.setContentView(Some(&scroll));
        window.orderFrontRegardless();
        window.displayIfNeeded();
        deliver("first_frame", &body, first_observer);
        let mut receipts = vec![body.resource_receipts().unwrap()];
        assert!(view.cpu_paints().unwrap() > 0, "实际 CPU 图像必须完整发布");
        let key = view.capture_ready_scene_key().unwrap();
        let proof = view
            .verify_find_target_visible(&key, NSRange::new(1, 1))
            .unwrap();
        assert_eq!(proof.range(), NSRange::new(1, 1));
        // 真 NSTextInputClient 消费原 prepared canonical 事务；测试不直接修改第二文档。
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
        let inserted = observe(&body, Instant::now());
        // SAFETY: 真实主线程 client 和同步存活 NSString；原 sentinel 表示使用当前选择，
        // 不是未校验的外来缓冲或越界范围。
        unsafe {
            client.insertText_replacementRange(
                &NSString::from_str("X"),
                NSRange::new(NSNotFound as usize, 0),
            );
        }
        assert_eq!(
            doc_weak.upgrade().unwrap().borrow().utf16_len(),
            fixture.source_utf16() + 1
        );
        deliver("canonical_insert", &body, inserted);
        receipts.push(body.resource_receipts().unwrap());
        let navigated = observe(&body, Instant::now());
        view.command(sel!(moveRight:)).unwrap();
        assert_eq!(view.input_state().unwrap().selection.head_utf16, 2);
        deliver("real_gcb_navigation", &body, navigated);
        let deleted = observe(&body, Instant::now());
        view.command(sel!(deleteBackward:)).unwrap();
        assert_eq!(
            doc_weak.upgrade().unwrap().borrow().utf16_len(),
            fixture.source_utf16()
        );
        deliver("real_gcb_delete", &body, deleted);
        assert_eq!(
            body.progress().unwrap().source_total_bytes,
            fixture.edited_source_bytes()
        );
        let mut prefix = [0_u16; 4];
        doc_weak
            .upgrade()
            .unwrap()
            .borrow()
            .copy_utf16_code_units(0, &mut prefix)
            .unwrap();
        assert_eq!(prefix, fixture.edited_prefix());
        let clip = scroll.contentView();
        for (scroll_stage, return_stage) in [
            ("actual_scroll_1", "actual_return_to_top_1"),
            ("actual_scroll_2", "actual_return_to_top_2"),
            ("actual_scroll_3", "actual_return_to_top_3"),
            ("actual_scroll_4", "actual_return_to_top_4"),
        ] {
            let before = body.progress().unwrap().prepared_lines;
            let old_height = body.prepared_height().unwrap();
            // 只滚到真实已排版内容的末端附近；不按字节、行号或平均高度假造整篇滚动范围。
            // 每轮必须越过上一屏前沿并新增实际prefix，覆盖旧Scene/中间Reader/下一发布的释放顺序。
            let y = old_height - clip.bounds().size.height - 1.0;
            assert!(y > 0.0);
            let scrolled_frame = observe(&body, Instant::now());
            clip.scrollToPoint(NSPoint::new(0.0, y));
            scroll.reflectScrolledClipView(&clip);
            window.displayIfNeeded();
            deliver(scroll_stage, &body, scrolled_frame);
            let scrolled = view.capture_ready_scene_key().unwrap();
            assert!(scrolled.request.virtual_viewport.document_origin.y > 0.0);
            assert!(body.progress().unwrap().prepared_lines > before);
            receipts.push(body.resource_receipts().unwrap());
            let returned_frame = observe(&body, Instant::now());
            clip.scrollToPoint(NSPoint::ZERO);
            scroll.reflectScrolledClipView(&clip);
            window.displayIfNeeded();
            deliver(return_stage, &body, returned_frame);
            let returned = view.capture_ready_scene_key().unwrap();
            // 真正返回文首只要求纵向为零；行号占用的横向Native区域仍在，不能把正文原点写成ZERO。
            assert_eq!(
                returned.request.virtual_viewport.document_origin,
                view.frame().origin
            );
            assert_eq!(returned.request.virtual_viewport.document_origin.y, 0.0);
            let returned_proof = view
                .verify_find_target_visible(&returned, NSRange::new(1, 1))
                .unwrap();
            assert_eq!(returned_proof.range(), NSRange::new(1, 1));
        }
        word::run(&body, &view, &doc_weak, &mut receipts, fixture);
        active_line::run(&body, &window, &scroll, &view, &mut receipts);
        gutter::long(&body, &window, &scroll, &view, &mut receipts);
        let target_handles = targets::run(
            &body,
            &window,
            &scroll,
            &view,
            &doc_weak,
            &mut receipts,
            fixture,
        );
        let mut native = body.native_weaks().unwrap();
        native.push(("test-scroll", weak(&scroll)));
        native.push(("test-window", weak(&window)));
        let close_started = Instant::now();
        let close = body.begin_close().unwrap();
        assert!(Rc::ptr_eq(&close, &body.begin_close().unwrap()));
        paged_close_event::wait(&close);
        close.verify_released().unwrap();
        assert!(target_handles.iter().all(|target| !target.is_pending()));
        println!(
            "paged_body_source stage=explicit_close actual_close_ms={:.3} actual_pixels_zero={} formal=false",
            close_started.elapsed().as_secs_f64() * 1000.0,
            pixels.snapshot().live_pixel_bytes == 0,
        );
        assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
        window.setContentView(None);
        window.orderOut(None);
        window.close();
        drop((
            key, proof, clip, view, scroll, window, body, font, paragraph,
        ));
        CATransaction::flush();
        assert!(
            doc_weak.upgrade().is_none(),
            "唯一 canonical owner 必须实际释放"
        );
        (
            view_weak,
            mapping,
            (pixels, native, receipts, close_started, target_handles),
        )
    });
    assert!(weak.load().is_none(), "正文 NSView 必须实际释放");
    assert!(mapping.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert_eq!(pixels.0.snapshot().live_pixel_bytes, 0);
    assert!(pixels.4.iter().all(|target| !target.is_pending()));
    for (role, weak) in pixels.1 {
        assert!(weak.load().is_none(), "自有 Native role 未实际释放：{role}");
    }
    for (index, receipt) in pixels.2.into_iter().enumerate() {
        assert!(receipt.source.payload_released());
        assert!(receipt.reader.is_some(), "完整帧必须创建真实Reader");
        for observer in receipt
            .reader
            .into_iter()
            .chain(std::iter::once(receipt.writer))
        {
            let actual = observer.snapshot();
            assert!(
                !actual.file_open
                    && actual.mapped_bytes == 0
                    && !actual.cleanup_failed
                    && !actual.reader_failed
                    && actual.source_payload_released,
                "generation resource {index}: {actual:?}"
            );
        }
        let worker = receipt
            .worker
            .snapshot()
            .expect("资源台账 poison 不是释放成功");
        assert!(
            worker.is_released(),
            "generation worker {index}: {worker:?}"
        );
        for (bytes, payload) in receipt.fonts {
            assert!(
                payload.payload_released()
                    && bytes.unmap_succeeded()
                    && bytes.mapped_bytes() == 0
                    && !bytes.failed()
            );
        }
        println!(
            "paged_body_source original_resource_generation={index} actual_source_released=true actual_reader_writer_closed=true complete_worker_released=true actual_font_bytes_unmapped=true formal=false"
        );
    }
    println!(
        "paged_body_source stage=actual_release actual_close_to_all_receipts_ms={:.3} native_weak_nil=true source_worker_font_closed=true actual_mmap_unmapped=true formal=false",
        pixels.3.elapsed().as_secs_f64() * 1000.0,
    );
    // 这些调用方持票在全部Doc/Source/FD/worker/font/NativeWeak/mmap/pixel门通过后才释放。
    drop(pixels.4);
    gutter::small(mtm);
    active_line::eof(mtm);
    #[cfg(feature = "native-markdown-runtime-probe")]
    reentry::run(mtm);
    process_memory::sample("source_after_all_actual_releases").unwrap();
    println!(
        "paged_body_source first_frame=true actual_scroll=true actual_canonical_insert=true actual_gcb_navigation=true actual_gcb_delete=true same_source_find_visible=true actual_pixels_zero=true actual_mmap_unmapped=true actual_native_weak_nil=true whole_product=false full_markdown_parity=false formal=false"
    );
}
