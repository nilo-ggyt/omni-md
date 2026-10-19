//! 无窗口的真实NSView/NSTextInputContext协议控制；不把Pending几何或未绘图说成正文Ready。

#[path = "paged_close/event.rs"]
mod paged_close_event;

use objc2::{
    rc::{Weak, autoreleasepool},
    runtime::ProtocolObject,
    sel,
};
use objc2_app_kit::NSTextInputClient;
use objc2_foundation::{
    MainThreadMarker, NSNotFound, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString,
};
use omni_markdown_core::MarkdownDocumentBuffer;
use omni_md_plugin::native_surface::{
    paged_input::{NativeMarkdownPagedInput, NativeMarkdownPagedInputCommit},
    paged_text_view::{
        NativeMarkdownPagedRenderStrategy, NativeMarkdownPagedSceneKey,
        NativeMarkdownPagedSceneRequest, NativeMarkdownPagedTextScene, NativeMarkdownPagedTextView,
        NativeMarkdownPagedTextViewDelegate, NativeMarkdownPagedTextViewError as Error,
        NativeMarkdownPagedVirtualViewport,
    },
};
use std::{
    cell::{Cell, RefCell},
    io::Write,
    rc::Rc,
};

/// 本控制刻意没有布局producer；任何位置请求都明确Pending，不能由假0坐标或空paint过门。
struct PendingDelegate {
    active: Cell<bool>,
    commits: Cell<usize>,
    requests: RefCell<Vec<NativeMarkdownPagedSceneRequest>>,
}
impl NativeMarkdownPagedTextViewDelegate for PendingDelegate {
    fn current(&self, epoch: u64) -> bool {
        self.active.get() && epoch == 7
    }
    fn virtual_viewport(&self, _epoch: u64) -> Result<NativeMarkdownPagedVirtualViewport, Error> {
        Ok(NativeMarkdownPagedVirtualViewport {
            document_origin: NSPoint::new(0.0, 18000.0),
            scroll_epoch: 1,
        })
    }
    fn ready_scene(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Rc<dyn NativeMarkdownPagedTextScene>, Error> {
        assert!(self.requests.borrow().len() < 32); // 只有固定控制调用，测试不会积事件或反复轮询。
        self.requests.borrow_mut().push(request.clone());
        Err(Error::Pending)
    }
    fn scene_current(&self, _key: &NativeMarkdownPagedSceneKey) -> bool {
        false
    }
    fn committed(&self, _commit: &NativeMarkdownPagedInputCommit) -> Result<(), Error> {
        self.commits.set(self.commits.get() + 1);
        Ok(())
    }
    fn reveal_committed_selection(&self, _: &NativeMarkdownPagedInputCommit) -> Result<(), Error> {
        // 本有限组件没有正文Producer/reveal续体，不能把选择提交伪装成实际可见。
        Err(Error::Pending)
    }
    fn copy_selection(&self, _request: &NativeMarkdownPagedSceneRequest) -> Result<(), Error> {
        Err(Error::Pending) // 本控制不触碰用户系统剪贴板，真实生产者由composition root另接。
    }
}

fn fixture(value: &str) -> (tempfile::NamedTempFile, Rc<RefCell<MarkdownDocumentBuffer>>) {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(value.as_bytes()).unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 65536).unwrap();
    (file, Rc::new(RefCell::new(document)))
}
fn text(document: &RefCell<MarkdownDocumentBuffer>) -> String {
    let doc = document.borrow();
    assert!(doc.utf16_len() < 64); // 仅此短控制允许完整oracle，生产substring始终独立4096上限。
    let mut units = [0; 64];
    let n = doc.utf16_len();
    doc.copy_utf16_code_units(0, &mut units[..n]).unwrap();
    String::from_utf16(&units[..n]).unwrap()
}
fn make_view(
    input: Rc<NativeMarkdownPagedInput>,
    delegate: &Rc<PendingDelegate>,
    mtm: MainThreadMarker,
) -> objc2::rc::Retained<NativeMarkdownPagedTextView> {
    let erased: Rc<dyn NativeMarkdownPagedTextViewDelegate> = delegate.clone();
    NativeMarkdownPagedTextView::new(
        input,
        Rc::downgrade(&erased),
        7,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(640.0, 240.0)),
        mtm,
    )
    .unwrap()
}
fn delegate() -> Rc<PendingDelegate> {
    Rc::new(PendingDelegate {
        active: Cell::new(true),
        commits: Cell::new(0),
        requests: RefCell::new(Vec::new()),
    })
}

fn native_input_and_release(mtm: MainThreadMarker) {
    let (weak_view, weak_context, backing, weak_document) = autoreleasepool(|_| {
        let (_file, document) = fixture(" ");
        let backing = document.borrow().original_backing_observer().unwrap();
        let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let delegate = delegate();
        let view = make_view(Rc::clone(&input), &delegate, mtm);
        let weak_view = Weak::from_retained(&view);
        let context = view.inputContext().unwrap();
        let weak_context = Weak::from_retained(&context);
        let actual_client = context.client();
        assert!(std::ptr::eq::<objc2::runtime::AnyObject>(
            AsRef::<objc2::runtime::AnyObject>::as_ref(&*actual_client),
            AsRef::<objc2::runtime::AnyObject>::as_ref(&*view)
        ));
        drop(actual_client);
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*view);
        let unspecified = NSRange::new(NSNotFound as usize, 0);
        let ascii = NSString::from_str("a");
        // SAFETY: 本次真实NSString及合法未指定replacement交生产协议入口，未借Doc。
        unsafe {
            client.insertText_replacementRange(&ascii, unspecified);
        }
        assert_eq!(text(&document), "a ");
        let marked = NSString::from_str("中🙂文");
        // SAFETY: selected完整覆盖emoji的两个UTF16单元，候选为完整有限NSString。
        unsafe {
            client.setMarkedText_selectedRange_replacementRange(
                &marked,
                NSRange::new(1, 2),
                unspecified,
            );
        }
        assert_eq!(text(&document), "a中🙂文 ");
        assert!(client.hasMarkedText());
        assert_eq!(client.markedRange(), NSRange::new(1, 4));
        let final_text = NSString::from_str("中文🙂");
        // SAFETY: 新增最终候选由同一Core原marked事务替换，未手动修改模型选择。
        unsafe {
            client.insertText_replacementRange(&final_text, unspecified);
        }
        assert_eq!(text(&document), "a中文🙂 ");
        assert!(!client.hasMarkedText());
        let before_unmark = document.borrow().generation();
        client.unmarkText();
        assert_eq!(document.borrow().generation(), before_unmark);
        // 一次撤销恢复整次组合前的正文和光标；候选更新不是用户可见的独立历史。
        view.command(sel!(undo:)).unwrap();
        assert_eq!(text(&document), "a ");
        assert_eq!(client.selectedRange(), NSRange::new(1, 0));
        // 此前普通输入仍单独撤销，再逐项重做，不能为合并组合而吞掉前一笔键入。
        view.command(sel!(undo:)).unwrap();
        assert_eq!(text(&document), " ");
        view.command(sel!(redo:)).unwrap();
        assert_eq!(text(&document), "a ");
        view.command(sel!(redo:)).unwrap();
        assert_eq!(text(&document), "a中文🙂 ");
        assert_eq!(client.selectedRange(), NSRange::new(5, 0));
        let g = document.borrow().generation();
        view.command(sel!(selectAll:)).unwrap();
        assert_eq!(document.borrow().generation(), g);
        assert_eq!(client.selectedRange(), NSRange::new(0, 6));

        let mut actual = NSRange::new(99, 99);
        // SAFETY: actual是本栈完整可写NSRange；所请求范围与真实Core长度一致。
        let substring = unsafe {
            client.attributedSubstringForProposedRange_actualRange(NSRange::new(1, 4), &mut actual)
        }
        .unwrap();
        assert_eq!(actual, NSRange::new(1, 4));
        assert_eq!(substring.string().to_string(), "中文🙂");
        let mut pending = NSRange::new(88, 88);
        // SAFETY: 未Ready几何只能返回NSZeroRect+NotFound；这里不把该返回签成caret位置。
        let no_rect = unsafe {
            client.firstRectForCharacterRange_actualRange(NSRange::new(0, 0), &mut pending)
        };
        assert_eq!(no_rect, NSRect::ZERO);
        assert_eq!(pending.location, NSNotFound as usize);
        assert!(matches!(view.take_failure(), Some(Error::Pending)));
        assert!(!view.respondsToSelector(sel!(attributedString))); // 没有全篇可选getter或隐藏NSTextView。
        let state = view.input_state().unwrap();
        delegate.active.set(false);
        let rejected = NSString::from_str("不能插入");
        // SAFETY: 类型正确但原前台撤销，必须在同一个生产入口拒绝而非静默提交。
        unsafe {
            client.insertText_replacementRange(&rejected, unspecified);
        }
        assert_eq!(document.borrow().generation(), state.generation);
        assert_eq!(text(&document), "a中文🙂 ");
        assert!(matches!(view.take_failure(), Some(Error::Revoked)));
        delegate.active.set(true);
        view.command(sel!(selectAll:)).unwrap();
        let empty = NSString::from_str("");
        // SAFETY: 删除真实完整选择后再输入，不要求一个伪造的empty caret几何Ready。
        unsafe {
            client.insertText_replacementRange(&empty, unspecified);
        }
        assert_eq!(text(&document), "");
        // SAFETY: 同一个生产client在真实canonical空文档中接受新增字符。
        unsafe {
            client.insertText_replacementRange(&ascii, unspecified);
        }
        assert_eq!(text(&document), "a");
        assert_eq!(substring.string().to_string(), "中文🙂"); // 已归还调用者的有限不可变值不随Core编辑改写。
        drop(substring);
        let weak_document = Rc::downgrade(&document);
        let close = view.begin_close().unwrap();
        assert!(Rc::ptr_eq(&close, &view.begin_close().unwrap()));
        paged_close_event::wait(&close);
        close.verify_released().unwrap();
        assert!(view.inputContext().is_none());
        drop(view);
        // 外部仍持原context时先核view实际nil，才能实证context.client没有强持其owner。
        assert!(weak_view.load().is_none());
        assert!(weak_context.load().is_some());
        drop(context);
        drop(input);
        drop(document);
        (weak_view, weak_context, backing, weak_document)
    });
    assert!(weak_view.load().is_none() && weak_context.load().is_none());
    assert!(weak_document.upgrade().is_none());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    println!(
        "native_paged_text_view input=true marked=true unicode=true history=true exact_selection=true empty_input=true pending_geometry_rejected=true context_client_nonretaining=true actual_weak_nil=true actual_unmap=true formal=false"
    );
}

fn bounded_substring_and_geometry(mtm: MainThreadMarker) {
    let (weak_view, weak_context, backing) = autoreleasepool(|_| {
        let (_file, document) = fixture(&"a😀中".repeat(2400));
        let backing = document.borrow().original_backing_observer().unwrap();
        assert!(backing.mapped_bytes() > 0); // 此例确有原始文件映射，不能拿空文档零页冒充munmap。
        let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let delegate = delegate();
        let view = make_view(input, &delegate, mtm);
        let weak_view = Weak::from_retained(&view);
        let context = view.inputContext().unwrap();
        let weak_context = Weak::from_retained(&context);
        drop(context);
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*view);
        let mut actual = NSRange::new(0, 0);
        // SAFETY: 请求刻意大于性能窗口；返回实际range须明确≤4096且不切低代理项。
        let value = unsafe {
            client
                .attributedSubstringForProposedRange_actualRange(NSRange::new(2, 9000), &mut actual)
        }
        .unwrap();
        assert!(actual.length <= 4096 && actual.location == 1);
        document
            .borrow()
            .byte_offset_for_utf16(actual.location)
            .unwrap();
        document
            .borrow()
            .byte_offset_for_utf16(actual.location + actual.length)
            .unwrap();
        assert_eq!(value.length(), actual.length);
        assert!(matches!(
            view.screen_hit(NSPoint::new(20.0, 20.0)),
            Err(Error::Pending)
        ));
        view.setFrameOrigin(NSPoint::new(0.0, 12000.0));
        assert!(matches!(view.scene_ready(), Err(Error::Pending)));
        let requests = delegate.requests.borrow();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].geometry_epoch > requests[0].geometry_epoch);
        assert_eq!(requests[1].frame_in_parent.origin.y, 12000.0);
        assert_eq!(requests[1].virtual_viewport.document_origin.y, 18000.0);
        assert_eq!(requests[1].viewport.size.height, 240.0); // 真实body仍仅viewport高，虚拟文档Y不增长frame高。
        drop(requests);
        drop(value);
        let close = view.begin_close().unwrap();
        assert!(Rc::ptr_eq(&close, &view.begin_close().unwrap()));
        paged_close_event::wait(&close);
        close.verify_released().unwrap();
        println!(
            "native_paged_text_view bounded_substring=true scalar_edges=true virtual_y_explicit=true frame_origin_revokes=true ready_scene_verified=false actual_glyph_paint_verified=false formal=false"
        );
        (weak_view, weak_context, backing)
    });
    assert!(weak_view.load().is_none() && weak_context.load().is_none());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

fn cpu_pending_owns_and_releases_its_exact_layer(mtm: MainThreadMarker) {
    let (view, context, layer, backing, pixels) = autoreleasepool(|_| {
        let (_file, document) = fixture("中文🙂");
        let backing = document.borrow().original_backing_observer().unwrap();
        let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        let delegate = delegate();
        let erased: Rc<dyn NativeMarkdownPagedTextViewDelegate> = delegate.clone();
        let view = NativeMarkdownPagedTextView::new_with_render_strategy(
            input,
            Rc::downgrade(&erased),
            7,
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(640.0, 240.0)),
            NativeMarkdownPagedRenderStrategy::CpuViewport,
            mtm,
        )
        .unwrap();
        let weak_view = Weak::from_retained(&view);
        let context = view.inputContext().unwrap();
        let weak_context = Weak::from_retained(&context);
        drop(context);
        let layer = view.layer().unwrap();
        let weak_layer = Weak::from_retained(&layer);
        drop(layer);
        let pixels = view.pixel_observer().unwrap();
        assert!(pixels.is_released());
        assert!(matches!(view.scene_ready(), Err(Error::Pending)));
        assert_eq!(view.cpu_paints(), Some(0)); // Pending不能借新建layer或零像素伪称真正发布。
        let close = view.begin_close().unwrap();
        assert!(Rc::ptr_eq(&close, &view.begin_close().unwrap()));
        paged_close_event::wait(&close);
        close.verify_released().unwrap();
        drop(view);
        drop(erased);
        drop(delegate);
        drop(document);
        (weak_view, weak_context, weak_layer, backing, pixels)
    });
    // 公开提交原构造/关闭事务，不等延时或反复Weak加载；原Native实际nil判据仍保持。
    objc2_quartz_core::CATransaction::flush();
    assert!(view.load().is_none() && context.load().is_none() && layer.load().is_none());
    assert!(pixels.is_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    println!(
        "native_paged_text_view cpu_pending=true actual_paints=0 exact_layer_nil=true pixel_resources_released=true actual_unmap=true formal=false"
    );
}

fn main() {
    let mtm = MainThreadMarker::new().unwrap();
    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("body-find-transactions"))
    {
        assert!(
            std::env::args_os().nth(2).is_none(),
            "事务控制不接受额外参数"
        );
        omni_md_plugin::native_surface::run_native_paged_find_body_transaction_control(mtm)
            .unwrap();
        return; // 明确独立诊断；无参完整序列和其原3秒/远端资源门继续保留。
    }
    assert!(
        std::env::args_os().nth(1).is_none(),
        "正文输入控制不接受未知参数"
    );
    native_input_and_release(mtm);
    bounded_substring_and_geometry(mtm);
    cpu_pending_owns_and_releases_its_exact_layer(mtm);
    #[cfg(all(
        feature = "native-paged-find-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    omni_md_plugin::native_surface::run_native_paged_find_backend_control(mtm).unwrap();
}
