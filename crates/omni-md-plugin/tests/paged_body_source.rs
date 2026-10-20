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
use objc2_foundation::{
    MainThreadMarker, NSNotFound, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString,
};
use objc2_quartz_core::CATransaction;
use omni_markdown_core::{MarkdownDocumentBuffer, MarkdownTextSelection};
use omni_md_plugin::native_surface::{
    paged_body::{
        NativeMarkdownPagedBody, NativeMarkdownPagedBodyColors,
        NativeMarkdownPagedBodyConfiguration, NativeMarkdownPagedBodyPaintObserver,
        NativeMarkdownPagedBodyProgress, NativeMarkdownPagedBodyResourceReceipts,
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
#[path = "paged_body_source/ime.rs"]
mod ime;
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

/// 关闭后的检查只消费原生产观察者；保留这些弱收据不能延长 Source、字体、FD 或 worker。
/// 首帧之前可以确实没有 Reader；需要完整帧的调用方在取得收据时另核 Reader 已存在，
/// 不能为了统一收据形状创建读者，也不能把尚未创建与曾创建但泄漏混为一谈。
fn verify_source_release(receipts: Vec<NativeMarkdownPagedBodyResourceReceipts>) {
    for (index, receipt) in receipts.into_iter().enumerate() {
        assert!(receipt.source.payload_released());
        for cell in &receipt.cells {
            cell.verify_released()
                .unwrap_or_else(|cause| panic!("generation {index} 原格资源未归还：{cause}"));
        }
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
            .expect("资源观察损坏不能视为释放完成");
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
}

/// 字面预期来自这个有限协议样本；不调用可选全文 getter，也不把实际返回值作为期望。
/// 文稿借用在函数返回时结束，后面的 Native 输入、关闭与候选回调没有 RefCell 借用穿越。
fn client_text(document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>, expected: &str) {
    let owner = document.upgrade().expect("原协议文稿必须仍由 Body 持有");
    let document = owner.borrow();
    let count = expected.encode_utf16().count();
    let mut units = [0_u16; 64];
    assert!(count <= units.len());
    assert_eq!(document.utf16_len(), count);
    document
        .copy_utf16_code_units(0, &mut units[..count])
        .unwrap();
    assert!(units[..count].iter().copied().eq(expected.encode_utf16()));
}

/// 协议次序消费正式 Body/View 与原 NSTextInputClient；没有永久 Pending 的测试委托。
/// 每次文字或选择改变后都等原 CPU 帧，再核正文和历史，显示后果不能代替 canonical 提交。
fn native_client_protocol(mtm: MainThreadMarker) {
    let (context, context_weak, view_weak, native, mapping, pixels, receipts, document) =
        autoreleasepool(|_| {
            let mut file = tempfile::NamedTempFile::new().unwrap();
            file.write_all(b" ").unwrap();
            file.flush().unwrap();
            let document = Rc::new(RefCell::new(
                MarkdownDocumentBuffer::open(file.path(), 65_536).unwrap(),
            ));
            let document_weak = Rc::downgrade(&document);
            let mapping = document.borrow().original_backing_observer().unwrap();
            let frame = NSRect::new(NSPoint::ZERO, NSSize::new(640.0, 240.0));
            let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
            let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
            let paragraph = NSParagraphStyle::defaultParagraphStyle();
            let body = NativeMarkdownPagedBody::attach(
                document,
                &scroll,
                font.as_ref(),
                &paragraph,
                NativeMarkdownPagedBodyConfiguration {
                    presentation_mode: omni_markdown_core::MarkdownPresentationMode::Source,
                    epoch: 74,
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
            let context = view.inputContext().expect("正式 View 必须提供原输入上下文");
            let context_weak = Weak::from_retained(&context);
            let view_weak = Weak::from(&**view);
            let actual_client = context.client();
            assert!(std::ptr::eq::<AnyObject>(
                AsRef::<AnyObject>::as_ref(&*actual_client),
                AsRef::<AnyObject>::as_ref(&**view),
            ));
            drop(actual_client); // 输入上下文的客户 claim 不跨后续输入与关闭。
            let first = observe(&body, Instant::now());
            // SAFETY: 合成样本沿正式无边框 NSWindow 挂载；外层 Retained 负责关闭后的唯一释放。
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    frame,
                    NSWindowStyleMask::Borderless,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            // SAFETY: 与其余原窗口验收相同，不让系统额外释放 Rust 的独立 claim。
            unsafe { window.setReleasedWhenClosed(false) };
            window.setContentView(Some(&scroll));
            window.orderFrontRegardless();
            window.displayIfNeeded();
            deliver_frame("client_protocol_first", &body, first);
            let mut receipts = vec![body.resource_receipts().unwrap()];
            let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
            let unspecified = NSRange::new(NSNotFound as usize, 0);

            // 普通键入先形成独立一笔历史；其后两个候选更新必须仍归同一组合事务。
            let inserted = observe(&body, Instant::now());
            // SAFETY: 有限 NSString 同步存活，NSNotFound 由原 Input 解释为当前有向选择。
            unsafe { client.insertText_replacementRange(&NSString::from_str("a"), unspecified) };
            deliver_frame("client_protocol_insert", &body, inserted);
            client_text(&document_weak, "a ");
            assert!(document_weak.upgrade().unwrap().borrow().can_undo());
            receipts.push(body.resource_receipts().unwrap());
            for (candidate, selection, expected, marked) in [
                ("zh", NSRange::new(2, 0), "azh ", NSRange::new(1, 2)),
                ("中🙂文", NSRange::new(1, 2), "a中🙂文 ", NSRange::new(1, 4)),
            ] {
                let frame = observe(&body, Instant::now());
                // SAFETY: 组合内选区来自固定完整 UTF16 scalar 边界，不读或改变当前系统输入源。
                unsafe {
                    client.setMarkedText_selectedRange_replacementRange(
                        &NSString::from_str(candidate),
                        selection,
                        unspecified,
                    );
                }
                deliver_frame("client_protocol_marked", &body, frame);
                client_text(&document_weak, expected);
                assert!(client.hasMarkedText());
                assert_eq!(client.markedRange(), marked);
                assert_eq!(
                    client.selectedRange(),
                    NSRange::new(1 + selection.location, selection.length)
                );
                receipts.push(body.resource_receipts().unwrap());
            }
            let committed = observe(&body, Instant::now());
            // SAFETY: 原客户确认同一组合，最终值仅四个 UTF16 单元；这不是物理输入法验收。
            unsafe {
                client.insertText_replacementRange(&NSString::from_str("中文🙂"), unspecified)
            };
            deliver_frame("client_protocol_commit", &body, committed);
            client_text(&document_weak, "a中文🙂 ");
            assert!(!client.hasMarkedText());
            receipts.push(body.resource_receipts().unwrap());
            let before_unmark = view.input_state().unwrap().generation;
            client.unmarkText();
            assert_eq!(view.input_state().unwrap().generation, before_unmark);

            // 方向由原 Cocoa 扩选命令产生；代理对仍完整，不能只核排序后的 NSRange。
            // 选择与折回 caret 不产生文字历史，随后的 Undo 仍只能撤销原组合。
            for _ in 0..2 {
                let frame = observe(&body, Instant::now());
                view.command(sel!(moveLeftAndModifySelection:)).unwrap();
                deliver_frame("client_protocol_reverse_selection", &body, frame);
                receipts.push(body.resource_receipts().unwrap());
            }
            assert_eq!(
                view.input_state().unwrap().selection,
                MarkdownTextSelection {
                    anchor_utf16: 5,
                    head_utf16: 2,
                }
            );
            assert_eq!(client.selectedRange(), NSRange::new(2, 3));
            assert_eq!(view.input_state().unwrap().generation, before_unmark);
            let collapsed = observe(&body, Instant::now());
            view.command(sel!(moveRight:)).unwrap();
            deliver_frame("client_protocol_collapse_selection", &body, collapsed);
            assert_eq!(client.selectedRange(), NSRange::new(5, 0));
            assert_eq!(view.input_state().unwrap().generation, before_unmark);
            receipts.push(body.resource_receipts().unwrap());

            // 组合确认只能是一笔撤销；再一次撤销才回到普通键入之前，重做恢复原精确 caret。
            for (selector, expected, caret, can_undo, can_redo) in [
                (sel!(undo:), "a ", 1, true, true),
                (sel!(undo:), " ", 0, false, true),
                (sel!(redo:), "a ", 1, true, true),
                (sel!(redo:), "a中文🙂 ", 5, true, false),
            ] {
                let before = view.input_state().unwrap().generation;
                let frame = observe(&body, Instant::now());
                view.command(selector).unwrap();
                deliver_frame("client_protocol_history", &body, frame);
                client_text(&document_weak, expected);
                assert_eq!(view.input_state().unwrap().generation, before + 1);
                assert_eq!(client.selectedRange(), NSRange::new(caret, 0));
                let owner = document_weak.upgrade().unwrap();
                let document = owner.borrow();
                assert_eq!(document.can_undo(), can_undo);
                assert_eq!(document.can_redo(), can_redo);
                drop(document);
                drop(owner);
                receipts.push(body.resource_receipts().unwrap());
            }
            let generation = view.input_state().unwrap().generation;
            let selected = observe(&body, Instant::now());
            view.command(sel!(selectAll:)).unwrap();
            deliver_frame("client_protocol_select_all", &body, selected);
            assert_eq!(view.input_state().unwrap().generation, generation);
            assert_eq!(client.selectedRange(), NSRange::new(0, 6));
            receipts.push(body.resource_receipts().unwrap());
            let mut actual = NSRange::new(99, 99);
            // SAFETY: actual 是可写栈值；这次有限不可变返回必须保持完整 emoji，且不跟随后续编辑。
            let substring = unsafe {
                client.attributedSubstringForProposedRange_actualRange(
                    NSRange::new(1, 4),
                    &mut actual,
                )
            }
            .unwrap();
            assert_eq!(actual, NSRange::new(1, 4));
            assert_eq!(substring.string().to_string(), "中文🙂");
            assert!(!view.respondsToSelector(sel!(attributedString)));
            for (value, expected) in [("", ""), ("a", "a")] {
                let frame = observe(&body, Instant::now());
                // SAFETY: 先真实删除全选，再在原空正文输入；仍由同一 canonical Input 提交。
                unsafe {
                    client.insertText_replacementRange(&NSString::from_str(value), unspecified)
                };
                deliver_frame("client_protocol_empty_input", &body, frame);
                client_text(&document_weak, expected);
                assert!(document_weak.upgrade().unwrap().borrow().can_undo());
                receipts.push(body.resource_receipts().unwrap());
            }
            assert_eq!(substring.string().to_string(), "中文🙂");
            drop(substring);
            assert_eq!(std::fs::read(file.path()).unwrap(), b" "); // 未保存协议编辑无权回写原文件。

            let mut native = body.native_weaks().unwrap();
            native.push(("client-protocol-window", weak(&window)));
            native.push(("client-protocol-scroll", weak(&scroll)));
            let pixels = view.pixel_observer().unwrap();
            let close = body.begin_close().unwrap();
            assert!(Rc::ptr_eq(&close, &body.begin_close().unwrap()));
            paged_close_event::wait(&close);
            close.verify_released().unwrap();
            assert!(view.inputContext().is_none());
            window.setContentView(None);
            window.orderOut(None);
            window.close();
            drop((view, body, scroll, window, font, paragraph));
            CATransaction::flush();
            // context 刻意由调用方持过原图释放；它不能因此强持 View、Input 或文稿。
            (
                context,
                context_weak,
                view_weak,
                native,
                mapping,
                pixels,
                receipts,
                document_weak,
            )
        });
    assert!(view_weak.load().is_none()); // 外部 context 仍活着时，先证明它没有强持原 View。
    assert!(document.upgrade().is_none());
    assert!(context_weak.load().is_some());
    drop(context);
    assert!(context_weak.load().is_none());
    assert!(
        native
            .into_iter()
            .all(|(_, object)| object.load().is_none())
    );
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
    assert!(receipts.iter().all(|receipt| receipt.reader.is_some()));
    verify_source_release(receipts);
    println!(
        "paged_body_source stage=client_protocol actual_body=true marked_unicode=true exact_history=true empty_input=true context_client_nonretaining=true source_disk_unchanged=true actual_native_mmap_font_pixel_released=true physical_ime=false whole_product=false formal=false"
    );
}

/// attach 已装配正式委托，但原调度尚未运行，因而这里的 Pending 是真实首帧前状态。
/// 公开字符协议只读有限子串；随后直接关闭同一 Body，不借测试端口强制布局或注入 Ready。
fn bounded_pending_client(mtm: MainThreadMarker) {
    let (native, mapping, pixels) = autoreleasepool(|_| {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for _ in 0..2400 {
            file.write_all("a😀中".as_bytes()).unwrap();
        }
        file.flush().unwrap();
        let document = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(file.path(), 65_536).unwrap(),
        ));
        let document_weak = Rc::downgrade(&document);
        let mapping = document.borrow().original_backing_observer().unwrap();
        let before = document.borrow().current_revision().unwrap();
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(640.0, 240.0));
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
        let paragraph = NSParagraphStyle::defaultParagraphStyle();
        let body = NativeMarkdownPagedBody::attach(
            document,
            &scroll,
            font.as_ref(),
            &paragraph,
            NativeMarkdownPagedBodyConfiguration {
                presentation_mode: omni_markdown_core::MarkdownPresentationMode::Source,
                epoch: 75,
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
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
        let context = view.inputContext().unwrap();
        let mut native = body.native_weaks().unwrap();
        native.push(("bounded-pending-scroll", weak(&scroll)));
        native.push(("bounded-pending-context", weak(&context)));
        let pixels = view.pixel_observer().unwrap();
        assert_eq!(view.cpu_paints(), Some(0));
        assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
        assert!(matches!(
            view.scene_ready(),
            Err(NativeMarkdownPagedTextViewError::Pending)
        ));
        let mut actual = NSRange::new(99, 99);
        // SAFETY: proposed 从 emoji 的低代理项开始，原 Input 必须向完整 scalar 边界归一化。
        let substring = unsafe {
            client
                .attributedSubstringForProposedRange_actualRange(NSRange::new(2, 9000), &mut actual)
        }
        .unwrap();
        assert!(actual.length <= 4096 && actual.location == 1);
        {
            let document = document_weak.upgrade().unwrap();
            let document = document.borrow();
            document.byte_offset_for_utf16(actual.location).unwrap();
            document
                .byte_offset_for_utf16(actual.location + actual.length)
                .unwrap();
            assert!(document.current_revision().unwrap() == before);
            assert!(!document.can_undo() && !document.can_redo());
        }
        assert_eq!(substring.length(), actual.length);
        let mut pending = NSRange::new(88, 88);
        // SAFETY: 尚无实际 Scene 时只能返回协议负值；这不是零坐标的可见 caret。
        let rect = unsafe {
            client.firstRectForCharacterRange_actualRange(NSRange::new(0, 0), &mut pending)
        };
        assert_eq!(rect, NSRect::ZERO);
        assert_eq!(pending.location, NSNotFound as usize);
        assert_eq!(
            client.characterIndexForPoint(NSPoint::new(0.0, 0.0)),
            NSNotFound as usize
        );
        assert!(view.take_failure().is_none());
        assert_eq!(view.input_state().unwrap().generation, 0);
        drop(substring);
        // 这里只比较真实 NSView 的移动与完整输入事实；原生可见元数据的安装顺序不是字符协议合同。
        // Scene 未就绪时不签可见坐标；已有 Ready 的几何票撤权由原 targets 流程验证。
        let previous = view.frame();
        let input_before_move = view.input_state().unwrap();
        view.setFrameOrigin(NSPoint::new(0.0, 12000.0));
        let current = view.frame();
        assert_eq!(current.origin.y, 12000.0);
        assert_eq!(current.size, previous.size);
        assert_eq!(view.input_state().unwrap(), input_before_move);
        assert!(matches!(
            view.scene_ready(),
            Err(NativeMarkdownPagedTextViewError::Pending)
        ));
        assert_eq!(view.cpu_paints(), Some(0));
        assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
        assert!(view.take_failure().is_none());
        // 原 Producer 可仍未创建或在异步拥有者手中，首帧前不能借资源快照的 Busy 推断泄漏或缺读者。
        // 直接接受正式关闭：它负责退休实际 Producer、join 字体 worker、归还字体和 Input，
        // 清理首错留在同一收据；只有原像素及行号资源事件真正完成后，verify 才能授权成功。
        let close = body.begin_close().unwrap();
        paged_close_event::wait(&close);
        close.verify_released().unwrap();
        drop((context, view, body, scroll, font, paragraph));
        CATransaction::flush();
        assert!(document_weak.upgrade().is_none());
        (native, mapping, pixels)
    });
    assert!(
        native
            .into_iter()
            .all(|(_, object)| object.load().is_none())
    );
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
    println!(
        "paged_body_source stage=bounded_pending_client actual_body=true bounded_substring=true scalar_edges=true natural_scene_pending=true pending_geometry_rejected=true frame_origin_changed=true full_input_unchanged=true no_frame_fabricated=true original_close_receipt_verified=true actual_native_doc_mmap_pixel_released=true whole_product=false formal=false"
    );
}

fn main() {
    let mtm = MainThreadMarker::new().expect("测试入口必须在 AppKit 主线程");
    let app = NSApplication::sharedApplication(mtm);
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("gutter-layouts")) {
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        assert!(
            std::env::args_os().nth(2).is_none(),
            "行号布局控制不接受额外参数"
        );
        // 独立重现位数变化的真实scroll/ruler布局；无参和cjk原长文流程仍完整执行同一控制。
        gutter::small(mtm);
        return;
    }
    // 真实输入法入口只改变验收驱动；样本仍是原四MiB单段落，不注入组合文字。
    let ime_mode = match std::env::args_os().nth(1).as_deref() {
        Some(argument) if argument == std::ffi::OsStr::new("ime") => Some(ime::Mode::Manual),
        Some(argument) if argument == std::ffi::OsStr::new("system-ime") => {
            Some(ime::Mode::Software)
        }
        _ => None,
    };
    let interactive_ime = ime_mode.is_some();
    // 临时输入法验收需要可由本人点击的Dock入口；产品仍沿原Accessory策略，不在这里改产品装配。
    // 其余组件控制保持无Dock入口，不能为无键盘交互的测试抢占前台应用。
    app.setActivationPolicy(if interactive_ime {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    });
    let fixture = if interactive_ime {
        assert!(std::env::args_os().nth(2).is_none());
        // 实际键盘入口完成AppKit启动通知，输入法才绑定当前应用而非无事件循环的测试进程。
        app.finishLaunching();
        SourceFixture::Cjk
    } else {
        SourceFixture::from_arguments()
    };
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
        // 输入法窗口沿正式浮动层级；默认控制仍使用原唯一无边框窗口。
        let window = if interactive_ime {
            ime::window(frame, mtm)
        } else {
            // SAFETY: 有效主线程frame和标准NSWindow initializer；Retained接管唯一无边框窗口。
            unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    frame,
                    NSWindowStyleMask::Borderless,
                    NSBackingStoreType::Buffered,
                    false,
                )
            }
        };
        // SAFETY: Retained 栈持有真实 window 至 close 后，避免额外自动 release。
        unsafe { window.setReleasedWhenClosed(false) };
        window.setContentView(Some(&scroll));
        if interactive_ime {
            // 与正式窗口一致，在首次显示前指定正文客户；makeFirstResponder成功本身不证明客户接受了焦点。
            window.setInitialFirstResponder(Some(&view));
        }
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
        if let Some(mode) = ime_mode {
            ime::run(&app, &window, &body, &view, &doc_weak, &mut receipts, mode);
        }
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
        let pending_key = word::native_before_close(&view);
        let close_started = Instant::now();
        let close = body.begin_close().unwrap();
        assert!(Rc::ptr_eq(&close, &body.begin_close().unwrap()));
        paged_close_event::wait(&close);
        close.verify_released().unwrap();
        assert!(
            pending_key.load().is_none(),
            "真实关闭必须归还尚未交给IME解释的原event"
        );
        assert!(target_handles.iter().all(|target| !target.is_pending()));
        println!(
            "paged_body_source stage=explicit_close actual_close_ms={:.3} actual_pixels_zero={} formal=false",
            close_started.elapsed().as_secs_f64() * 1000.0,
            pixels.snapshot().live_pixel_bytes == 0,
        );
        assert_eq!(pixels.snapshot().live_pixel_bytes, 0);
        window.setInitialFirstResponder(None); // 临时窗口归还首次焦点配置，不能保留已关闭正文客户。
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
    assert!(
        pixels.2.iter().all(|receipt| receipt.reader.is_some()),
        "完整帧必须创建真实Reader"
    );
    verify_source_release(pixels.2);
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
    if matches!(fixture, SourceFixture::Ascii) {
        // 独立短协议与首帧前 Pending 各自拥有真实 Body；原四 MiB 图已严格释放。
        // cjk 入口仍保完整长文流程，不重复跑同一固定短协议或要求任何输入法操作。
        native_client_protocol(mtm);
        bounded_pending_client(mtm);
    }
    println!(
        "paged_body_source first_frame=true actual_scroll=true actual_canonical_insert=true actual_gcb_navigation=true actual_gcb_delete=true same_source_find_visible=true actual_pixels_zero=true actual_mmap_unmapped=true actual_native_weak_nil=true whole_product=false full_markdown_parity=false formal=false"
    );
}
