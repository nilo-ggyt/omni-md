//! 换行GCB没有横向墨迹，但其原行高和屏幕位置必须穿过真实AppKit坐标转换。

use super::*;

/// 用户滚动离开caret后，系统焦点交接的空insertText不能撤图或把正文拉回caret。
/// 原长文仍由同一Body拥有；这里只读有限状态和磁盘历史可用性，不建立全文副本。
pub(super) fn empty_ime_keeps_scrolled_frame(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    scroll: &NSScrollView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
) {
    let key = view.capture_ready_scene_key().unwrap();
    let before = view.input_state().unwrap();
    assert!(key.request.virtual_viewport.document_origin.y > 0.0);
    assert!(before.marked.is_none());
    assert_eq!(before.selection.anchor_utf16, before.selection.head_utf16);
    let history = {
        let document = document.upgrade().unwrap();
        let document = document.borrow();
        (document.can_undo(), document.can_redo())
    };
    let clip = scroll.contentView();
    let bounds = clip.bounds();
    let published = view.cpu_paints().unwrap();
    assert_eq!(view.cpu_paint_pending(), Some(false));
    // 真正的空提交仍走NSTextInputClient→原Core prepare/publish；不以调用私有方法
    // 或直接比较新实现的布尔值代替用户可见的滚动、原Ready图和真实历史后果。
    // SAFETY: 有限空NSString同步存活，原客户自行解释未指定替换范围。
    unsafe {
        ProtocolObject::<dyn NSTextInputClient>::from_ref(view).insertText_replacementRange(
            &NSString::from_str(""),
            NSRange::new(NSNotFound as usize, 0),
        );
    }
    assert_eq!(view.input_state().unwrap(), before);
    assert!(view.capture_ready_scene_key().unwrap().same(&key));
    // 一次有限主循环机会允许原生通知交付；没有新帧应等待的理由，也不注册轮询器
    // 或领取后继key。错误实现若把reveal排队，必须在下面暴露实际clip/原Scene变化。
    unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 0.05, false) };
    assert!(NativeMarkdownPagedTextViewDelegate::scene_current(
        body, &key
    ));
    assert!(view.capture_ready_scene_key().unwrap().same(&key));
    assert_eq!(view.input_state().unwrap(), before);
    assert_eq!(clip.bounds(), bounds);
    assert_eq!(view.cpu_paints(), Some(published));
    assert_eq!(view.cpu_paint_pending(), Some(false));
    let document = document.upgrade().unwrap();
    let document = document.borrow();
    assert_eq!((document.can_undo(), document.can_redo()), history);
    drop(document);
    assert!(view.take_failure().is_none());
    assert!(body.take_failure().is_none());
    println!(
        "paged_body_source stage=empty_ime_scrolled_frame original_complete_input=true original_source_cpu_frame=true actual_clip_unchanged=true history_unchanged=true original_scene_preserved=true physical_ime=false whole_product=false formal=false"
    );
}

/// 复用已挂载的有限协议客户与原关闭账；只替换固定短样本，不创建第二个窗口或正文。
/// LF及CRLF的空行都必须保留自己的完整GCB，不能改用后一行caret通过可见性检查。
pub(super) fn run(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    window: &NSWindow,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
) {
    for (text, ranges) in [
        ("# H\n\n正文\n", [NSRange::new(3, 1), NSRange::new(4, 1)]),
        (
            "# H\r\n\r\n正文\r\n",
            [NSRange::new(3, 2), NSRange::new(5, 2)],
        ),
    ] {
        // 公开选择命令和输入协议形成真实canonical后态；不直接改Core或伪造Ready图。
        let selected = observe(body, Instant::now());
        view.command(sel!(selectAll:)).unwrap();
        deliver_frame("zero_width_screen_select_all", body, selected);
        receipts.push(body.resource_receipts().unwrap());
        let before = view.input_state().unwrap();
        let changed = observe(body, Instant::now());
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(view);
        // SAFETY: 固定短NSString同步存活，原完整选区由上面的真实命令产生。
        unsafe {
            client.insertText_replacementRange(
                &NSString::from_str(text),
                NSRange::new(NSNotFound as usize, 0),
            );
        }
        deliver_frame("zero_width_screen_insert", body, changed);
        let key = view.capture_ready_scene_key().unwrap();
        assert_eq!(key.request.input.generation, before.generation + 1);
        assert_eq!(key.request.input.utf16_len, text.encode_utf16().count());
        let paints = view.cpu_paints().unwrap();
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(body, &key.request).unwrap();
        let current = || {
            NativeMarkdownPagedTextViewDelegate::scene_current(body, &key)
                && view
                    .input_state()
                    .is_ok_and(|state| state == key.request.input)
                && view.cpu_paints() == Some(paints)
                && view.cpu_paint_pending() == Some(false)
        };
        assert!(scene.key().same(&key) && current());

        for range in ranges {
            let first = scene.first_rect(range).unwrap();
            assert_eq!(first.actual_range, range);
            assert_eq!(first.rect.size.width, 0.0);
            assert!(first.rect.size.height > 0.0);
            let visible = view.verify_find_target_visible(&key, range).unwrap();
            assert_eq!(visible.first_rect(), first.rect);
            // 独立对照只变换原GCB竖线的真实上下端点。点转换没有空矩形合同，
            // 不扩宽文字、不借后继GCB，也不从产品screen_rect反推期待高度。
            let lower = view.convertPoint_toView(first.rect.origin, None);
            assert!(current());
            let lower = window.convertPointToScreen(lower);
            assert!(current());
            let upper = view.convertPoint_toView(
                NSPoint::new(
                    first.rect.origin.x,
                    first.rect.origin.y + first.rect.size.height,
                ),
                None,
            );
            assert!(current());
            let upper = window.convertPointToScreen(upper);
            assert!(current());
            let expected = NSRect::new(
                NSPoint::new(lower.x.min(upper.x), lower.y.min(upper.y)),
                NSSize::new((upper.x - lower.x).abs(), (upper.y - lower.y).abs()),
            );
            assert_eq!(expected.size.width, 0.0);
            assert!((expected.size.height - first.rect.size.height).abs() < 0.001);
            let screen = view.screen_rect(range).unwrap();
            assert!(current());
            println!(
                "paged_body_source stage=zero_width_screen_geometry range={range:?} raw={:?} expected={expected:?} actual={:?} original_cpu_input=true formal=false physical_ime=false",
                first.rect, screen.rect,
            );
            assert_eq!(screen.actual_range, range);
            assert!((screen.rect.origin.x - expected.origin.x).abs() < 0.001);
            assert!((screen.rect.origin.y - expected.origin.y).abs() < 0.001);
            assert_eq!(screen.rect.size.width, 0.0);
            assert!((screen.rect.size.height - expected.size.height).abs() < 0.001);
            let mut actual = NSRange::new(NSNotFound as usize, 0);
            // SAFETY: 真实NSTextInputClient和有效栈上out参数；公开协议应返回同一屏幕位置。
            let protocol =
                unsafe { client.firstRectForCharacterRange_actualRange(range, &mut actual) };
            assert!(current());
            assert_eq!(actual, range);
            assert_eq!(protocol, screen.rect);
        }
        receipts.push(body.resource_receipts().unwrap());
    }
}
