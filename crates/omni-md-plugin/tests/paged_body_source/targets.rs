//! 实际远端目标、旧前缀回溯和输入/宽度撤权；仍用原三秒帧事件与完整关闭账本。

use super::*;
use objc2::rc::Retained;
use objc2_app_kit::NSViewFrameDidChangeNotification;
use objc2_foundation::{NSNotification, NSNotificationCenter};
use omni_md_plugin::native_surface::{
    paged_body::{
        NativeMarkdownPagedBodyError, NativeMarkdownPagedBodyResourceReceipts,
        NativeMarkdownPagedBodySourceTarget,
    },
    paged_input::NativeMarkdownPagedInputState,
    paged_text_view::NativeMarkdownPagedTextView,
};

/// 只观察原resize产生的真实frame通知；不替换View/委托，也不触发第二次同步或后继任务。
/// token由本作用域归还，block仅持原View弱身份和有限结果，不延长正文或Source生命周期。
struct ResizeGeometryObservation {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    view: Weak<NativeMarkdownPagedTextView>,
    previous_notifications: bool,
    pending_seen: Rc<Cell<bool>>,
    failure: Rc<RefCell<Option<String>>>,
}

impl ResizeGeometryObservation {
    /// 同步setFrame回调中只领取当前几何；未发布时须返回Pending，绝不借旧Ready或旧像素。
    fn new(view: &NativeMarkdownPagedTextView, input: NativeMarkdownPagedInputState) -> Self {
        let center = NSNotificationCenter::defaultCenter();
        let previous_notifications = view.postsFrameChangedNotifications();
        view.setPostsFrameChangedNotifications(true);
        let original = Weak::from(view);
        let callback_view = original.clone();
        let pending_seen = Rc::new(Cell::new(false));
        let pending = pending_seen.clone();
        let failure = Rc::new(RefCell::new(None));
        let failed = failure.clone();
        let observation = std::sync::Arc::new(dispatch2::MainThreadBound::new(
            move || {
                // 只读事实收完再由调用方断言，不能在AppKit通知的外部栈中panic或持RefMut进Native。
                let result = (|| -> Result<(), String> {
                    let view = callback_view.load().ok_or("原resize客户已释放")?;
                    match view.capture_scene_request() {
                        Err(NativeMarkdownPagedTextViewError::Pending) => {}
                        Err(cause) => return Err(format!("同步resize当前几何查询失败：{cause}")),
                        Ok(_) => return Ok(()), // 只有同步窗口的负结果才证明本次过渡，不把别的通知计入。
                    }
                    if view.input_state().map_err(|e| e.to_string())? != input {
                        return Err("resize通知改变了原完整Input".into());
                    }
                    if !matches!(
                        view.capture_ready_scene_key(),
                        Err(NativeMarkdownPagedTextViewError::Pending)
                    ) {
                        return Err("同步resize期间错误签发当前Ready票".into());
                    }
                    let mut actual = NSRange::new(0, 0);
                    // SAFETY: 同一真实NSTextInputClient和栈上有效out参数；只查原首插入点。
                    let rect = unsafe {
                        ProtocolObject::<dyn NSTextInputClient>::from_ref(&*view)
                            .firstRectForCharacterRange_actualRange(NSRange::new(0, 0), &mut actual)
                    };
                    if rect != NSRect::ZERO || actual != NSRange::new(NSNotFound as usize, 0) {
                        return Err("同步resize期间候选坐标错误使用旧几何".into());
                    }
                    if let Some(cause) = view.take_failure()
                        && !matches!(cause, NativeMarkdownPagedTextViewError::Pending)
                    {
                        return Err(format!("同步resize负坐标污染了原输入首错：{cause}"));
                    }
                    pending.set(true);
                    Ok(())
                })();
                if let Err(cause) = result {
                    let mut slot = failed.borrow_mut();
                    if slot.is_none() {
                        *slot = Some(cause);
                    }
                }
            },
            view.mtm(),
        ));
        let block =
            block2::RcBlock::new(move |_notification: std::ptr::NonNull<NSNotification>| {
                if let Some(mtm) = MainThreadMarker::new() {
                    observation.get(mtm)();
                } // 外来线程不能借真实通知名访问主线程Rc/Native；也不能贡献本次Pending证明。
            });
        // SAFETY: 只订阅原View标准frame通知；可发送block仅持MainThreadBound，
        // Rc/Native查询先核真实主线程，不接管生产委托、几何或布局。
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSViewFrameDidChangeNotification),
                Some(view),
                None,
                &block,
            )
        };
        Self {
            center,
            token,
            view: original,
            previous_notifications,
            pending_seen,
            failure,
        }
    }
}

impl Drop for ResizeGeometryObservation {
    /// 失败断言也先撤本次通知，再恢复原客户的通知设置，不留下观察者或强正文边。
    fn drop(&mut self) {
        // SAFETY: token是本center为上方原View返回的唯一订阅，不取消其他owner的监听。
        unsafe { self.center.removeObserver((*self.token).as_ref()) };
        if let Some(view) = self.view.load() {
            view.setPostsFrameChangedNotifications(self.previous_notifications);
        }
    }
}

pub(super) fn run(
    body: &NativeMarkdownPagedBody,
    window: &NSWindow,
    scroll: &NSScrollView,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) -> Vec<NativeMarkdownPagedBodySourceTarget> {
    let mut retained = Vec::new();
    let original = view.capture_ready_scene_key().unwrap();
    let before = body.progress().unwrap().prepared_lines;
    for (stage, start) in [
        ("source_target_beyond_64k", 70_001),
        ("source_target_older_prefix", 35_001),
        ("source_target_return_to_top", 1),
    ] {
        let frame = observe(body, Instant::now());
        let range = NSRange::new(start, 9_000);
        let target = body.request_source_target(range).unwrap();
        assert!(target.is_pending());
        let progress = deliver_frame(stage, body, frame);
        let proof = body.take_source_target_visible(&target).unwrap().unwrap();
        assert!(!target.is_pending());
        assert_eq!(proof.range(), range);
        let key = view.capture_ready_scene_key().unwrap();
        assert!(proof.key().same(&key));
        assert!(key.source == original.source);
        assert_eq!(key.request.input, original.request.input); // Reveal不能偷偷改选区或制造编辑事务。
        view.verify_find_target_visible(proof.key(), range).unwrap();
        assert!(!progress.complete);
        assert!(progress.prepared_lines > before);
        assert!(
            progress.frontier_source_bytes < SOURCE_BYTES
                && progress.shaped_input_bytes < SOURCE_BYTES
        );
        if start == 70_001 {
            // 此时此前只准备了顶部数屏。真正首scalar进入可见区后，原9000单位匹配
            // 尾端仍未进入frontier/shaper，不能用“少于整篇”替代首位置懒准备的证据。
            let match_end = start + range.length;
            let match_end_bytes = fixture.edited_byte_end(match_end);
            assert!(progress.frontier_source_bytes < match_end_bytes);
            assert!(progress.shaped_input_bytes < match_end_bytes);
            println!(
                "paged_body_source stage={stage} fixture={} lazy_match_end_utf16={match_end} lazy_match_end_bytes={match_end_bytes} only_first_scalar_required=true formal=false",
                fixture.name(),
            );
            assert!(progress.target_checkpoint_attempts > 0);
            assert!(progress.target_checkpoint_attempts < progress.prepared_lines - before);
            assert!(progress.target_max_batch_lines > 1 && progress.target_max_batch_lines <= 32);
            assert_eq!(progress.target_unmeasured_lines, 0);
        }
        if start == 1 {
            assert!(key.request.virtual_viewport.document_origin.y < 20.0);
        } else {
            assert!(key.request.virtual_viewport.document_origin.y > 0.0);
        }
        println!(
            "paged_body_source stage={stage} actual_target_start={start} actual_target_first_scalar_visible=true original_source_preserved=true target_lookup_records={} target_metric_parts={} target_walk_lines={} target_checkpoint_attempts={} target_checkpoint_millis={:.3} target_max_batch_lines={} target_batch_index_records={} formal=false whole_product=false",
            progress.target_lookup_records,
            progress.target_metric_parts,
            progress.target_walk_lines,
            progress.target_checkpoint_attempts,
            progress.target_checkpoint_millis,
            progress.target_max_batch_lines,
            progress.target_batch_index_records,
        );
        receipts.push(body.resource_receipts().unwrap());
        retained.push(target); // 已完成票保留到Body关闭以后，不能靠先drop外部票通过资源门。
    }
    let edit_origin = view.capture_ready_scene_key().unwrap();
    let before_edit = view.input_state().unwrap();
    let before_edit_serial = body.progress().unwrap().producer_serial;
    let editing = observe(body, Instant::now());
    let obsolete = body.request_source_target(NSRange::new(80_001, 1)).unwrap();
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(view);
    // SAFETY: 合成数据的真实原client和同步NSString；仍经过原prepared canonical事务。
    unsafe {
        client.insertText_replacementRange(&NSString::from_str("R"), NSRange::new(0, 0));
    }
    deliver("source_target_revoked_by_edit", body, editing);
    assert!(matches!(
        body.take_source_target_visible(&obsolete),
        Err(NativeMarkdownPagedBodyError::View(
            NativeMarkdownPagedTextViewError::Revoked
        ))
    ));
    assert!(!obsolete.is_pending());
    let edited = view.capture_ready_scene_key().unwrap();
    assert!(edited.source != edit_origin.source);
    assert!(!edited.same(&edit_origin));
    assert_eq!(
        edited.request.input.generation,
        before_edit.generation.checked_add(1).unwrap()
    );
    assert_eq!(edited.request.input.utf16_len, before_edit.utf16_len + 1);
    assert_eq!(edited.request.input, view.input_state().unwrap());
    assert!(body.progress().unwrap().producer_serial > before_edit_serial);
    view.verify_find_target_visible(&edited, NSRange::new(0, 1))
        .unwrap();
    let actual_document = document.upgrade().unwrap();
    let mut first = [0];
    actual_document
        .borrow()
        .copy_utf16_code_units(0, &mut first)
        .unwrap();
    assert_eq!(first, [b'R' as u16]);
    drop(actual_document);
    receipts.push(body.resource_receipts().unwrap());
    let resize_origin = view.capture_ready_scene_key().unwrap();
    let old_width = view.bounds().size.width;
    let resizing = observe(body, Instant::now());
    let obsolete_width = body.request_source_target(NSRange::new(80_001, 1)).unwrap();
    let content = scroll.frame().size;
    let resize_geometry = ResizeGeometryObservation::new(view, resize_origin.request.input);
    window.setContentSize(NSSize::new(content.width - 80.0, content.height));
    window.displayIfNeeded();
    deliver("source_target_revoked_by_resize", body, resizing);
    let geometry_failure = resize_geometry.failure.borrow_mut().take();
    assert!(geometry_failure.is_none(), "{geometry_failure:?}");
    assert!(
        resize_geometry.pending_seen.get(),
        "真实frame通知必须覆盖尚未发布新几何的同步窗口"
    );
    drop(resize_geometry); // 新Ready之后不再观察后继动作；原资源关闭账本仍归原Body。
    assert!(matches!(
        body.take_source_target_visible(&obsolete_width),
        Err(NativeMarkdownPagedBodyError::View(
            NativeMarkdownPagedTextViewError::Revoked
        ))
    ));
    assert!(!obsolete_width.is_pending());
    assert!(view.bounds().size.width < old_width);
    let resized = view.capture_ready_scene_key().unwrap();
    assert!(resized.source != resize_origin.source);
    assert!(!resized.same(&resize_origin));
    assert_eq!(resized.request.bounds_width, view.bounds().size.width);
    assert!(resized.request.bounds_width < resize_origin.request.bounds_width);
    assert_eq!(resized.request.input, resize_origin.request.input); // 只有布局变更，不能额外编辑或改变选择。
    view.verify_find_target_visible(&resized, NSRange::new(0, 1))
        .unwrap();
    receipts.push(body.resource_receipts().unwrap());
    // 旧请求一直保留到撤权后的真实后继帧，证明失败不是把仍待完成的意图静默丢掉。
    retained.extend([obsolete, obsolete_width]);
    let closing = body.request_source_target(NSRange::new(90_001, 1)).unwrap();
    assert!(closing.is_pending());
    retained.push(closing); // 原队列尚未运行即close；外部原pending票也必须随真实业务关闭准确撤权。
    retained
}
