//! 原 Font current 栈中真实重入 Native 编辑和 resize；旧票必须拒绝，后继必须真正显示。

use super::*;
use omni_md_plugin::native_surface::{
    paged_body::{
        NativeMarkdownPagedBodyPreparationProbe, NativeMarkdownPagedBodyPreparationResources,
        NativeMarkdownPagedBodyResourceReceipts,
    },
    paged_text_view::{
        NativeMarkdownPagedCloseReceipt, NativeMarkdownPagedSceneRequest,
        NativeMarkdownPagedTextView,
    },
};

#[derive(Clone, Copy)]
enum Action {
    Edit,
    Resize,
}

struct Interleave {
    view: Weak<NativeMarkdownPagedTextView>,
    window: Weak<NSWindow>,
    action: Action,
    calls: Cell<usize>,
    original_width: Cell<f64>,
    retired: RefCell<Option<NativeMarkdownPagedBodyPreparationResources>>,
}

impl NativeMarkdownPagedBodyPreparationProbe for Interleave {
    fn native_check(
        &self,
        original: &NativeMarkdownPagedSceneRequest,
        resources: NativeMarkdownPagedBodyPreparationResources,
    ) {
        assert_eq!(self.calls.replace(self.calls.get() + 1), 0);
        self.original_width.set(original.bounds_width);
        *self.retired.borrow_mut() = Some(resources);
        let view = self.view.load().expect("原测试正文必须仍存活");
        assert_eq!(view.input_state().unwrap(), original.input);
        match self.action {
            Action::Edit => {
                let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*view);
                // SAFETY: 真实原client和同步存活NSString；仍走原 prepared canonical 事务，
                // 不是直接篡改树、generation或Source。调用期间外层Font current尚未返回。
                unsafe {
                    client.insertText_replacementRange(
                        &NSString::from_str("R"),
                        NSRange::new(NSNotFound as usize, 0),
                    );
                }
            }
            Action::Resize => {
                self.window
                    .load()
                    .expect("原窗口必须仍存活")
                    .setContentSize(NSSize::new(520.0, 240.0));
            }
        }
    }
}

pub(super) fn run(mtm: MainThreadMarker) {
    close_before_first_frame(mtm);
    for (action, stage) in [
        (Action::Edit, "font_current_reentrant_edit"),
        (Action::Resize, "font_current_reentrant_resize"),
    ] {
        let (native, mapping, pixels, resources, retired) = autoreleasepool(|_| {
            let mut file = tempfile::NamedTempFile::new().unwrap();
            for _ in 0..16 {
                file.write_all(&[b'a'; 4096]).unwrap();
            }
            file.flush().unwrap();
            let started = Instant::now();
            let document = Rc::new(RefCell::new(
                MarkdownDocumentBuffer::open(file.path(), 128 * 1024).unwrap(),
            ));
            let document_weak = Rc::downgrade(&document);
            let mapping = document.borrow().original_backing_observer().unwrap();
            let frame = NSRect::new(NSPoint::ZERO, NSSize::new(640.0, 240.0));
            let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
            scroll.setHasVerticalScroller(true);
            let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
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
            let view = body.view().unwrap();
            // SAFETY: 测试唯一borderless窗口；不添加产品标题栏、快捷键或退出动作。
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    frame,
                    NSWindowStyleMask::Borderless,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            // SAFETY: 原Retained栈负责close后的释放，避免系统额外自动release。
            unsafe { window.setReleasedWhenClosed(false) };
            let probe = Rc::new(Interleave {
                view: Weak::from(&**view),
                window: Weak::from(&*window),
                action,
                calls: Cell::new(0),
                original_width: Cell::new(0.0),
                retired: RefCell::new(None),
            });
            let erased: Rc<dyn NativeMarkdownPagedBodyPreparationProbe> = probe.clone();
            body.probe_next_native_preparation(Rc::downgrade(&erased))
                .unwrap();
            drop(erased); // Body只有Weak；测试自己持有唯一实际probe owner。
            let frame_observer = observe(&body, started);
            window.setContentView(Some(&scroll));
            window.orderFrontRegardless();
            window.displayIfNeeded();
            let progress = deliver_frame(stage, &body, frame_observer);
            assert!(!progress.complete && progress.source_total_bytes >= 65_536);
            assert_eq!(probe.calls.get(), 1);
            assert_eq!(body.progress().unwrap().revoked_preparations, 1);
            let retired = probe.retired.borrow_mut().take().unwrap();
            let key = view.capture_ready_scene_key().unwrap();
            assert!(key.source != retired.source_authority);
            match action {
                Action::Edit => {
                    assert_eq!(key.request.input.generation, 1);
                    let document = document_weak.upgrade().unwrap();
                    let document = document.borrow();
                    assert_eq!(document.utf16_len(), 65_537);
                    let mut first = [0];
                    document.copy_utf16_code_units(0, &mut first).unwrap();
                    assert_eq!(first, [b'R' as u16]);
                }
                Action::Resize => {
                    assert_eq!(key.request.input.generation, 0);
                    assert!(key.request.bounds_width < probe.original_width.get());
                    assert_eq!(key.request.bounds_width, view.bounds().size.width);
                }
            }
            let visible = view
                .verify_find_target_visible(&key, NSRange::new(0, 1))
                .unwrap();
            assert_eq!(visible.range(), NSRange::new(0, 1));
            let resources = body.resource_receipts().unwrap();
            let mut native = body.native_weaks().unwrap();
            native.push(("reentry-window", weak(&window)));
            native.push(("reentry-scroll", weak(&scroll)));
            let pixels = view.pixel_observer().unwrap();
            let close = body.begin_close().unwrap();
            assert!(Rc::ptr_eq(&close, &body.begin_close().unwrap()));
            paged_close_event::wait(&close);
            close.verify_released().unwrap();
            assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
            window.setContentView(None);
            window.orderOut(None);
            window.close();
            drop((
                visible, key, probe, view, body, scroll, window, font, paragraph,
            ));
            CATransaction::flush();
            assert!(document_weak.upgrade().is_none());
            (native, mapping, pixels, resources, retired)
        });
        for (role, native) in native {
            assert!(native.load().is_none(), "{stage}: {role}必须实际释放");
        }
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
        assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
        for source in [&resources.source, &retired.source] {
            assert!(source.payload_released());
        }
        assert!(resources.reader.is_some(), "后继完整帧必须创建真实Reader");
        for file in resources
            .reader
            .iter()
            .chain([&resources.writer, &retired.writer])
        {
            let value = file.snapshot();
            assert!(!value.file_open && value.mapped_bytes == 0 && !value.cleanup_failed);
        }
        for worker in [&resources.worker, &retired.worker] {
            assert!(worker.snapshot().unwrap().is_released());
        }
        for (bytes, font) in resources.fonts {
            assert!(
                font.payload_released() && bytes.unmap_succeeded() && bytes.mapped_bytes() == 0
            );
        }
        println!(
            "paged_body_source stage={stage} original_ticket_rejected=true same_native_action_once=true actual_successor_frame=true exact_source_authority_changed=true actual_all_resources_released=true formal=false"
        );
    }
}

/// 在首次字体真实current回调后排入关闭；不等待首行、补建Reader或制造可见帧。
struct CloseBeforeFrame {
    weak: std::rc::Weak<Self>,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    calls: Cell<usize>,
    resources: RefCell<Option<NativeMarkdownPagedBodyResourceReceipts>>,
    close: RefCell<Option<Rc<NativeMarkdownPagedCloseReceipt>>>,
    completed: Cell<bool>,
}

impl NativeMarkdownPagedBodyPreparationProbe for CloseBeforeFrame {
    fn native_check(
        &self,
        _: &NativeMarkdownPagedSceneRequest,
        _: NativeMarkdownPagedBodyPreparationResources,
    ) {
        assert_eq!(self.calls.replace(self.calls.get() + 1), 0);
        let weak = std::sync::Arc::new(dispatch2::MainThreadBound::new(
            self.weak.clone(),
            MainThreadMarker::new().unwrap(),
        ));
        // current仍在原准备栈中，关闭必须等唯一Producer回槽；这个一次性续体只保弱身份。
        // 它先于本次尚未提交的字体试次完成通知，不靠轮询或延长deadline寻找关闭时机。
        dispatch2::DispatchQueue::main().exec_async(move || {
            autoreleasepool(|_| {
                let mtm = MainThreadMarker::new().unwrap();
                let owner = weak.get(mtm).upgrade().expect("原关闭用例仍持有观察者");
                let body = owner.body.upgrade().expect("本次关闭前原Body仍存活");
                let progress = body.progress().unwrap();
                assert_eq!(progress.prepared_lines, 0);
                let resources = body.resource_receipts().unwrap();
                assert!(resources.reader.is_none(), "首行前不得为收据创建Reader");
                // Store也可尚未惰性创建首个FD；观察原状态即可，不能为释放验收强制打开文件。
                *owner.resources.borrow_mut() = Some(resources);
                let close = body.begin_close().unwrap();
                assert!(Rc::ptr_eq(&close, &body.begin_close().unwrap()));
                *owner.close.borrow_mut() = Some(Rc::clone(&close));
                let observed = Rc::downgrade(&owner);
                let run_loop = CFRunLoop::current().unwrap();
                // 只有同一收据的真实资源事件结束等待；本回调不保存Body、字体或Source。
                close.when_released(move || {
                    observed.upgrade().unwrap().completed.set(true);
                    run_loop.stop();
                });
            });
        });
    }
}

/// 关闭可发生在真实字体试次等待中；所有已创建资源仍须按原事件和外层pool边界归还。
fn close_before_first_frame(mtm: MainThreadMarker) {
    let (native, mapping, pixels, resources) = autoreleasepool(|_| {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for _ in 0..16 {
            file.write_all(&[b'a'; 4096]).unwrap();
        } // 真实原文件由固定4KiB块生成，关闭路径不能借此把正文预读进内存。
        file.flush().unwrap();
        let document = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(file.path(), 128 * 1024).unwrap(),
        ));
        let document_weak = Rc::downgrade(&document);
        let mapping = document.borrow().original_backing_observer().unwrap();
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(640.0, 240.0));
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll.setHasVerticalScroller(true);
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
        let view = body.view().unwrap();
        let paints = observe(&body, Instant::now());
        let observer = Rc::new_cyclic(|weak| CloseBeforeFrame {
            weak: weak.clone(),
            body: Rc::downgrade(&body),
            calls: Cell::new(0),
            resources: RefCell::new(None),
            close: RefCell::new(None),
            completed: Cell::new(false),
        });
        let erased: Rc<dyn NativeMarkdownPagedBodyPreparationProbe> = observer.clone();
        body.probe_next_native_preparation(Rc::downgrade(&erased))
            .unwrap();
        // SAFETY: 唯一合成无边框窗口仍走真实Native挂接；不增加任何产品控件或退出键。
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: 原Retained负责close后的释放，不允许系统重复归还同一claim。
        unsafe { window.setReleasedWhenClosed(false) };
        let mut native = body.native_weaks().unwrap();
        native.push(("early-close-window", weak(&window)));
        native.push(("early-close-scroll", weak(&scroll)));
        let pixels = view.pixel_observer().unwrap();
        window.setContentView(Some(&scroll));
        window.orderFrontRegardless();
        window.displayIfNeeded();
        // SAFETY: 与原帧/关闭验收相同的三秒上限，唯一真实关闭完成事件可以提前返回。
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 3.0, false) };
        assert_eq!(observer.calls.get(), 1);
        assert!(observer.completed.get(), "首帧前关闭必须交付原资源完成事件");
        observer
            .close
            .borrow()
            .as_ref()
            .unwrap()
            .verify_released()
            .unwrap();
        assert!(paints.receipt.borrow().is_none(), "关闭不能补发首帧");
        assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
        let resources = observer.resources.borrow_mut().take().unwrap();
        window.setContentView(None);
        window.orderOut(None);
        window.close();
        drop((
            paints, observer, erased, view, body, scroll, window, font, paragraph,
        ));
        CATransaction::flush();
        assert!(document_weak.upgrade().is_none());
        (native, mapping, pixels, resources)
    });
    // 原外层pool退出后逐项核实际归还；保留收据本身不能延长任何业务载荷或文件映射。
    assert!(
        native
            .into_iter()
            .all(|(_, native)| native.load().is_none())
    );
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
    assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
    assert!(resources.reader.is_none() && resources.source.payload_released());
    let writer = resources.writer.snapshot();
    assert!(!writer.file_open && writer.mapped_bytes == 0 && !writer.cleanup_failed);
    assert!(resources.worker.snapshot().unwrap().is_released());
    for (bytes, font) in resources.fonts {
        assert!(font.payload_released() && bytes.unmap_succeeded() && bytes.mapped_bytes() == 0);
    }
    println!(
        "paged_body_source stage=close_before_first_frame original_preparation_once=true reader_never_created=true actual_native_mmap_worker_font_pixel_released=true formal=false whole_product=false"
    );
}
