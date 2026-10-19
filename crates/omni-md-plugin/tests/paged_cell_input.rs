//! 无窗口的真实Cell输入协议控制；几何生产者明确Pending，不以输入成功冒充绘图或完整field替换。

#[path = "paged_close/event.rs"]
mod paged_close_event;

use objc2::{
    rc::{Weak, autoreleasepool},
    runtime::ProtocolObject,
    sel,
};
use objc2_app_kit::NSTextInputClient;
use objc2_foundation::{MainThreadMarker, NSNotFound, NSPoint, NSRange, NSRect, NSSize, NSString};
use omni_markdown_core::markdown_document_buffer::{
    MarkdownTableWindowStep, MarkdownTableWindowTarget,
};
use omni_markdown_core::{MarkdownCellDraftError, MarkdownDocumentBuffer, MarkdownTextSelection};
use omni_md_plugin::native_surface::{
    paged_input::{
        NativeMarkdownPagedCellSession, NativeMarkdownPagedInput, NativeMarkdownPagedInputCommit,
        NativeMarkdownPagedInputError,
    },
    paged_text_view::{
        NativeMarkdownPagedSceneKey, NativeMarkdownPagedSceneRequest, NativeMarkdownPagedTextScene,
        NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewDelegate,
        NativeMarkdownPagedTextViewError as Error, NativeMarkdownPagedVirtualViewport,
    },
};
use std::{
    cell::{Cell, RefCell},
    io::Write,
    rc::Rc,
};

const SOURCE: &str = "| H | C |\n| --- | --- |\n| base | other |\n";
type Fixture = (
    tempfile::NamedTempFile,
    Rc<RefCell<MarkdownDocumentBuffer>>,
    Rc<NativeMarkdownPagedCellSession>,
    Rc<NativeMarkdownPagedInput>,
);

struct Delegate {
    active: Cell<bool>,
    commits: Cell<usize>,
}
impl NativeMarkdownPagedTextViewDelegate for Delegate {
    fn current(&self, epoch: u64) -> bool {
        self.active.get() && epoch == 7
    }
    fn virtual_viewport(&self, _: u64) -> Result<NativeMarkdownPagedVirtualViewport, Error> {
        Ok(NativeMarkdownPagedVirtualViewport {
            document_origin: NSPoint::ZERO,
            scroll_epoch: 1,
        })
    }
    fn ready_scene(
        &self,
        _: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Rc<dyn NativeMarkdownPagedTextScene>, Error> {
        Err(Error::Pending)
    }
    fn scene_current(&self, _: &NativeMarkdownPagedSceneKey) -> bool {
        false
    }
    fn committed(&self, _: &NativeMarkdownPagedInputCommit) -> Result<(), Error> {
        self.commits.set(self.commits.get() + 1);
        Ok(())
    }
    fn reveal_committed_selection(&self, _: &NativeMarkdownPagedInputCommit) -> Result<(), Error> {
        // 本有限组件没有正文Producer/reveal续体，不能把选择提交伪装成实际可见。
        Err(Error::Pending)
    }
    fn copy_selection(&self, _: &NativeMarkdownPagedSceneRequest) -> Result<(), Error> {
        Err(Error::Pending)
    }
}

fn fixture() -> Fixture {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(SOURCE.as_bytes()).unwrap();
    let document = Rc::new(RefCell::new(
        MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap(),
    ));
    let verified = {
        let doc = document.borrow();
        let mut request = doc
            .table_window_request(
                MarkdownTableWindowTarget::AtUtf16(0),
                0..doc.utf16_len(),
                1,
                0..0,
            )
            .unwrap();
        let mut ready = None;
        for _ in 0..10000 {
            match request.advance(&doc, false, 4096).unwrap() {
                MarkdownTableWindowStep::Progress { .. } => {}
                MarkdownTableWindowStep::Ready { .. } => {
                    ready = request.take_ready_for(&doc).unwrap();
                    break;
                }
                other => panic!("原表格未Ready：{other:?}"),
            }
        }
        ready.expect("有限真实表格认证必须结束")
    };
    let session =
        NativeMarkdownPagedCellSession::begin(Rc::downgrade(&document), Rc::new(verified), 1, 0)
            .unwrap();
    let input = NativeMarkdownPagedInput::new_cell(Rc::downgrade(&session)).unwrap();
    (file, document, session, input)
}

fn select(input: &Rc<NativeMarkdownPagedInput>, selection: MarkdownTextSelection) {
    let permit = input.capture(7, &|epoch| epoch == 7).unwrap();
    input
        .set_selection(&permit, selection, &|epoch| epoch == 7)
        .unwrap();
}
fn local(input: &Rc<NativeMarkdownPagedInput>) -> String {
    let length = input.state().unwrap().utf16_len;
    assert!(length <= 32); // 仅短分组oracle物化；大草稿只能走有限substring/固定数组验证。
    let permit = input.capture(7, &|epoch| epoch == 7).unwrap();
    let mut units = [0; 32];
    input
        .copy_utf16_code_units(&permit, 0, &mut units[..length], &|epoch| epoch == 7)
        .unwrap();
    String::from_utf16(&units[..length]).unwrap()
}
fn canonical(document: &RefCell<MarkdownDocumentBuffer>) {
    let doc = document.borrow();
    let mut units = [0; 128];
    let n = doc.utf16_len();
    assert!(n <= units.len());
    doc.copy_utf16_code_units(0, &mut units[..n]).unwrap();
    assert_eq!(String::from_utf16(&units[..n]).unwrap(), SOURCE);
    assert!(!doc.can_undo() && !doc.can_redo());
}
fn unspecified() -> NSRange {
    NSRange::new(NSNotFound as usize, 0)
}

fn run_case(mtm: MainThreadMarker, case: usize) {
    let (view_weak, context_weak, document_weak, session_weak, input_weak, backing, resources) =
        autoreleasepool(|_| {
            let (_file, document, session, input) = fixture();
            let backing = document.borrow().original_backing_observer().unwrap();
            let resources = session.resource_observer().unwrap();
            let delegate = Rc::new(Delegate {
                active: Cell::new(true),
                commits: Cell::new(0),
            });
            let erased: Rc<dyn NativeMarkdownPagedTextViewDelegate> = delegate.clone();
            let view = NativeMarkdownPagedTextView::new(
                input.clone(),
                Rc::downgrade(&erased),
                7,
                NSRect::new(NSPoint::ZERO, NSSize::new(640., 240.)),
                mtm,
            )
            .unwrap();
            let view_weak = Weak::from_retained(&view);
            let context = view.inputContext().unwrap();
            let context_weak = Weak::from_retained(&context);
            let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*view);
            if case == 4 {
                view.command(sel!(selectAll:)).unwrap();
                // 这是一次真实新增载荷，短暂Rust输入在NSString建立后立即退场，不复制原始cell或Doc。
                let added = NSString::from_str(&"x".repeat(2 * 1024 * 1024));
                // SAFETY: 真实完整NSString与原NSTextInputClient；没有Doc/Scene/RefCell借用跨Native调用。
                unsafe {
                    client.insertText_replacementRange(&added, unspecified());
                }
                drop(added);
                assert_eq!(input.state().unwrap().utf16_len, 2 * 1024 * 1024);
                let first = session.capture_source().unwrap();
                let second = session.capture_source().unwrap();
                assert_eq!(resources.snapshot().read_roots, 2);
                let state = input.state().unwrap();
                let permit = input.capture(7, &|epoch| epoch == 7).unwrap();
                assert!(matches!(
                    input.prepare_cell_commit(&permit, &|epoch| epoch == 7),
                    Err(NativeMarkdownPagedInputError::Cell(
                        MarkdownCellDraftError::CommitLimit
                    ))
                ));
                assert_eq!(input.state().unwrap(), state);
                let mut actual = NSRange::new(99, 99);
                // SAFETY: actual为可写栈值；无论原请求多大，生产substring只返回≤4096个真实UTF16单元。
                let substring = unsafe {
                    client.attributedSubstringForProposedRange_actualRange(
                        NSRange::new(0, state.utf16_len),
                        &mut actual,
                    )
                }
                .unwrap();
                assert_eq!(actual, NSRange::new(0, 4096));
                assert_eq!(substring.length(), 4096);
                let expected = NSString::from_str(&"x".repeat(4096));
                assert!(substring.string().isEqualToString(&expected));
                assert_eq!(resources.snapshot().read_roots, 2);
                drop(substring);
                drop(expected);
                drop(first);
                drop(second);
                println!(
                    "native_paged_cell_input case=large bytes=2097152 two_real_readers=true substring4096=true admission64Ki_rejected=true draft_preserved=true formal=false"
                );
            } else {
                if case == 0 {
                    view.command(sel!(selectAll:)).unwrap();
                } else {
                    select(&input, MarkdownTextSelection::caret(4));
                }
                if case == 0 {
                    for value in ["n", "ni"] {
                        let value = NSString::from_str(value);
                        // SAFETY: 完整真实候选与候选末端selection；生产原cap只捕获一次并按值消费。
                        unsafe {
                            client.setMarkedText_selectedRange_replacementRange(
                                &value,
                                NSRange::new(value.length(), 0),
                                unspecified(),
                            );
                        }
                        assert!(client.hasMarkedText());
                    }
                    let value = NSString::from_str("你");
                    // SAFETY: 沿同marked协议最终提交原真实候选，不直接修改草稿root。
                    unsafe {
                        client.insertText_replacementRange(&value, unspecified());
                    }
                } else {
                    for value in ["a", "b"] {
                        let value = NSString::from_str(value);
                        // SAFETY: 原真实输入client与完整新增字符串，未持可重入借用。
                        unsafe {
                            client.insertText_replacementRange(&value, unspecified());
                        }
                    }
                    if case == 2 {
                        select(&input, MarkdownTextSelection::caret(0));
                    }
                    if case == 3 {
                        let permit = input.capture(7, &|epoch| epoch == 7).unwrap();
                        input
                            .break_undo_coalescing(&permit, &|epoch| epoch == 7)
                            .unwrap();
                    }
                    let value = NSString::from_str(if case == 2 { "x" } else { "c" });
                    // SAFETY: 同一系统协议再次插入一个完整scalar，不另建TextKit字符串存储。
                    unsafe {
                        client.insertText_replacementRange(&value, unspecified());
                    }
                }
                assert!(!client.hasMarkedText());
                let undo = if case < 2 {
                    vec![(
                        "base",
                        if case == 0 {
                            NSRange::new(0, 4)
                        } else {
                            NSRange::new(4, 0)
                        },
                    )]
                } else {
                    vec![
                        ("baseab", NSRange::new(if case == 2 { 0 } else { 6 }, 0)),
                        ("base", NSRange::new(4, 0)),
                    ]
                };
                for (expected, selected) in &undo {
                    view.command(sel!(undo:)).unwrap();
                    assert_eq!(local(&input), *expected);
                    assert_eq!(client.selectedRange(), *selected);
                }
                view.command(sel!(undo:)).unwrap();
                assert_eq!(local(&input), "base");
                let redo = if case == 0 {
                    vec![("你", NSRange::new(1, 0))]
                } else if case == 1 {
                    vec![("baseabc", NSRange::new(7, 0))]
                } else {
                    vec![
                        ("baseab", NSRange::new(6, 0)),
                        (
                            if case == 2 { "xbaseab" } else { "baseabc" },
                            NSRange::new(if case == 2 { 1 } else { 7 }, 0),
                        ),
                    ]
                };
                for (expected, selected) in &redo {
                    view.command(sel!(redo:)).unwrap();
                    assert_eq!(local(&input), *expected);
                    assert_eq!(client.selectedRange(), *selected);
                }
                println!(
                    "native_paged_cell_input case={case} original_groups={} undo_redo_text_selection_exact=true canonical_unchanged=true formal=false",
                    undo.len()
                );
            }
            canonical(&document);
            assert!(delegate.commits.get() > 0);
            let document_weak = Rc::downgrade(&document);
            let session_weak = Rc::downgrade(&session);
            let input_weak = Rc::downgrade(&input);
            let close = view.begin_close().unwrap();
            assert!(Rc::ptr_eq(&close, &view.begin_close().unwrap()));
            paged_close_event::wait(&close);
            close.verify_released().unwrap();
            session.close().unwrap();
            delegate.active.set(false);
            assert!(resources.is_released());
            drop(view);
            assert!(view_weak.load().is_none());
            assert!(context_weak.load().is_some()); // 外方context仍活不允许保活已关闭View，不能把它列入忽略名单。
            drop(context);
            drop(input);
            drop(session);
            drop(document);
            drop(erased);
            drop(delegate);
            (
                view_weak,
                context_weak,
                document_weak,
                session_weak,
                input_weak,
                backing,
                resources,
            )
        });
    assert!(view_weak.load().is_none() && context_weak.load().is_none());
    assert!(
        document_weak.upgrade().is_none()
            && session_weak.upgrade().is_none()
            && input_weak.upgrade().is_none()
    );
    assert!(resources.is_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    println!(
        "native_paged_cell_input case={case} actual_native_weak_nil=true actual_rust_weak_nil=true draft_released=true actual_source_unmap=true formal=false"
    );
}

fn main() {
    let mtm = MainThreadMarker::new().expect("原生输入控制必须主线程");
    for case in 0..5 {
        run_case(mtm, case);
    }
    // SAFETY: value是完整可写libc rusage，RUSAGE_SELF只读取当前进程公开内核计数，不操作其他进程。
    let usage = unsafe {
        let mut value = std::mem::zeroed::<libc::rusage>();
        (libc::getrusage(libc::RUSAGE_SELF, &mut value) == 0).then_some(value)
    };
    match usage {
        Some(value) => println!(
            "native_paged_cell_input max_rss_bytes={} all5=true formal=false",
            value.ru_maxrss
        ),
        None => println!("native_paged_cell_input max_rss_available=false all5=true formal=false"),
    };
}
