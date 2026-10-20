//! 原NSEvent穿过keyDown/interpretKeyEvents；菜单和字符事务不能越过已接纳的导航。

use super::*;

/// 只构造本次有限按键，真正解释仍由AppKit；Weak验证原队列最终不持有event。
fn key(view: &NativeMarkdownPagedTextView, text: &str, code: u16) -> objc2::rc::Weak<NSEvent> {
    autoreleasepool(|_| {
        let window = view.window().unwrap();
        let string = NSString::from_str(text);
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown, NSPoint::ZERO, NSEventModifierFlags::empty(), 1.0,
            window.windowNumber(), None, &string, &string, false, code,
        ).unwrap();
        let weak = objc2::rc::Weak::new(&*event);
        // SAFETY: 原主线程view消费真实公开keyDown签名，event同步存活，后续只由产品队列retain。
        unsafe {
            let _: () = objc2::msg_send![view, keyDown: &*event];
        }
        weak
    })
}

/// 后继动作沿真实事件：字符/菜单交错、Down/Up往返，以及退格派生GCB不能落到后继字符之后。
pub(super) fn run(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    let window = view.window().unwrap();
    assert!(window.makeFirstResponder(Some(view))); // 原keyboard客户实际激活自己的NSTextInputContext。
    for (vertical, backward_delete) in [(false, false), (true, false), (false, true)] {
        let original = view.input_state().unwrap().selection;
        let lines = "a\r\n".repeat(32);
        let text = format!("{lines}one two\r\n");
        let setup = observe(body, Instant::now());
        insert(view, &text, NSRange::new(0, 0));
        finish("source_native_key_setup", body, setup, receipts);
        let top = observe(body, Instant::now());
        command(view, sel!(moveToBeginningOfDocument:));
        finish("source_native_key_origin", body, top, receipts);
        let frame = observe(body, Instant::now());
        insert(
            view,
            "b",
            NSRange::new(objc2_foundation::NSNotFound as usize, 0),
        );
        for _ in 0..64 {
            command(view, sel!(moveRight:));
        }
        command(view, sel!(moveWordForward:));
        let before_keys = view.input_state().unwrap();
        let (events, prefix, selection, history) = if backward_delete {
            let typing = key(view, "z", 6);
            let delete = key(view, "\u{7f}", 51);
            let next = key(view, "a", 0);
            (
                vec![typing, delete, next],
                format!("b{lines}onea two\r\n"),
                MarkdownTextSelection::caret(101),
                vec![
                    (format!("b{text}"), MarkdownTextSelection::caret(100)),
                    (
                        format!("b{lines}onez two\r\n"),
                        MarkdownTextSelection::caret(101),
                    ),
                    (format!("b{text}"), MarkdownTextSelection::caret(100)),
                ],
            ) // 原Backspace事件派生的GCB须先删z；若排到a之后，正文会错误保留z，逐笔Undo也不同。
        } else if vertical {
            let down = key(view, "\u{f701}", 125);
            let up = key(view, "\u{f700}", 126);
            let typing = key(view, "z", 6);
            (
                vec![down, up, typing],
                format!("b{lines}onez two\r\n"),
                MarkdownTextSelection::caret(101),
                vec![(format!("b{text}"), MarkdownTextSelection::caret(100))],
            ) // Down/Up须等真新Scene与实际目标列，不能把解释Pending当作事件已完成。
        } else {
            let typing = key(view, "z", 6);
            view.command(sel!(moveToBeginningOfDocument:)).unwrap(); // 公开菜单在已接纳key之后执行。
            let next = key(view, "a", 0);
            (
                vec![typing, next],
                format!("ab{lines}onez two\r\n"),
                MarkdownTextSelection::caret(1),
                vec![
                    (
                        format!("b{lines}onez two\r\n"),
                        MarkdownTextSelection::caret(0),
                    ),
                    (format!("b{text}"), MarkdownTextSelection::caret(100)),
                ],
            )
        };
        assert_eq!(view.input_state().unwrap(), before_keys); // 已接纳event尚未解释，不能先改旧caret。
        finish(
            if backward_delete {
                "source_native_key_grapheme_order"
            } else {
                "source_native_key_order"
            },
            body,
            frame,
            receipts,
        );
        check(document, fixture, &prefix, selection);
        let key = view.capture_ready_scene_key().unwrap();
        view.verify_find_target_visible(&key, NSRange::new(selection.head_utf16, 0))
            .unwrap();
        assert!(
            events.iter().all(|event| event.load().is_none()),
            "原event在解释完成后必须全部归还"
        );
        for (prefix, selection) in history.into_iter().chain([
            (text, MarkdownTextSelection::caret(0)),
            (String::new(), original),
        ]) {
            let frame = observe(body, Instant::now());
            let generation = view.input_state().unwrap().generation;
            command(view, sel!(undo:));
            finish("source_native_key_undo", body, frame, receipts);
            assert_eq!(view.input_state().unwrap().generation, generation + 1);
            check(document, fixture, &prefix, selection);
        } // 每个真正字符输入保原独立Undo；原移动/公开菜单没有额外字符历史。
    }
    format_and_table(body, view, document, receipts, fixture);
}

/// 真实Word尚在准备时接纳格式/插表，再交后继系统key；逐笔Undo暴露动作被丢或反排的中间结果。
fn format_and_table(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    const TABLE: &str = "| 列 1 | 列 2 | 列 3 |\n| :--- | :---: | ---: |\n|  |  |  |\n|  |  |  |";
    for table in [false, true] {
        let original = view.input_state().unwrap().selection;
        let setup = observe(body, Instant::now());
        insert(view, "one two\r\n", NSRange::new(0, 0));
        finish("source_queued_menu_setup", body, setup, receipts);
        let top = observe(body, Instant::now());
        command(view, sel!(moveToBeginningOfDocument:));
        finish("source_queued_menu_origin", body, top, receipts);
        let before = view.input_state().unwrap();
        let frame = observe(body, Instant::now());
        command(view, sel!(moveWordForward:));
        if table {
            view.insert_default_table().unwrap();
        } else {
            view.apply_format(omni_markdown_core::MarkdownFormatAction::Bold)
                .unwrap();
        }
        let typed = key(view, "z", 6);
        assert_eq!(view.input_state().unwrap(), before); // 接纳动作不等于在旧caret先提交模板或丢掉后继key。
        finish("source_queued_menu_key", body, frame, receipts);
        let (expected, selection, prior, prior_selection) = if table {
            (
                format!("one\n\n{}\n\n two\r\n", TABLE.replacen("| ", "| z", 1)),
                MarkdownTextSelection::caret(8),
                format!("one\n\n{TABLE}\n\n two\r\n"),
                MarkdownTextSelection::caret(7),
            )
        } else {
            (
                "one**z** two\r\n".to_owned(),
                MarkdownTextSelection::caret(6),
                "one**加粗文字** two\r\n".to_owned(),
                MarkdownTextSelection {
                    anchor_utf16: 5,
                    head_utf16: 9,
                },
            )
        }; // 空Bold选中原中文占位；真实后继key应替换该选择，不能插到格式之后。
        check(document, fixture, &expected, selection);
        assert!(typed.load().is_none());
        for (prefix, selection) in [
            (prior, prior_selection),
            ("one two\r\n".to_owned(), MarkdownTextSelection::caret(3)),
            (String::new(), original),
        ] {
            let frame = observe(body, Instant::now());
            command(view, sel!(undo:));
            finish("source_queued_menu_exact_undo", body, frame, receipts);
            check(document, fixture, &prefix, selection);
        }
    }
}

/// 外部明确范围仍为权威接管；它取消的是原未解释event，不能把它搬到新caret稍后补写。
pub(super) fn external_replacement(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    let original = view.input_state().unwrap().selection;
    let setup = observe(body, Instant::now());
    insert(view, "one two ", NSRange::new(0, 0));
    finish("source_native_cancel_setup", body, setup, receipts);
    let top = observe(body, Instant::now());
    command(view, sel!(moveToBeginningOfDocument:));
    finish("source_native_cancel_origin", body, top, receipts);
    let before = view.input_state().unwrap();
    command(view, sel!(moveWordForward:));
    let event = key(view, "q", 12);
    assert!(event.load().is_some(), "未解释原event应由唯一产品队列保留");
    assert_eq!(view.input_state().unwrap(), before);
    let frame = observe(body, Instant::now());
    insert(view, "R", NSRange::new(0, 0));
    assert!(
        event.load().is_none(),
        "外部replacement撤原链时必须归还尚未解释event"
    );
    finish("source_native_cancel_replacement", body, frame, receipts);
    check(
        document,
        fixture,
        "Rone two ",
        MarkdownTextSelection::caret(1),
    );
    for (prefix, selection) in [("one two ", before.selection), ("", original)] {
        let frame = observe(body, Instant::now());
        command(view, sel!(undo:));
        finish("source_native_cancel_undo", body, frame, receipts);
        check(document, fixture, prefix, selection);
    }
}

/// 原窗口已把焦点移交后，旧事件只释放；不能把新输入客户的字符补写进原正文。
pub(super) fn focus_transfer(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    let original = view.input_state().unwrap().selection;
    let setup = observe(body, Instant::now());
    insert(view, "one two ", NSRange::new(0, 0));
    finish("source_native_focus_setup", body, setup, receipts);
    let top = observe(body, Instant::now());
    command(view, sel!(moveToBeginningOfDocument:));
    finish("source_native_focus_origin", body, top, receipts);
    let frame = observe(body, Instant::now());
    command(view, sel!(moveWordForward:));
    let event = key(view, "q", 12);
    assert!(event.load().is_some());
    let window = view.window().unwrap();
    assert!(window.makeFirstResponder(None)); // 真实AppKit退激活原输入context，没有伪造marked结束。
    finish("source_native_focus_transfer", body, frame, receipts);
    check(
        document,
        fixture,
        "one two ",
        MarkdownTextSelection::caret(3),
    );
    assert!(event.load().is_none(), "原正文失焦必须归还未解释event");
    assert!(window.makeFirstResponder(Some(view)));
    let frame = observe(body, Instant::now());
    command(view, sel!(undo:));
    finish("source_native_focus_undo", body, frame, receipts);
    check(document, fixture, "", original); // 导航与取消事件不制造正文历史，唯一Undo撤原setup。
}

/// 原最终关闭前制造一个真正未解释event；此处不运行任务，释放事实交原close收据核验。
pub(super) fn before_close(view: &NativeMarkdownPagedTextView) -> objc2::rc::Weak<NSEvent> {
    command(view, sel!(moveToBeginningOfDocument:));
    let before = view.input_state().unwrap();
    command(view, sel!(moveWordForward:));
    let event = key(view, "q", 12);
    assert_eq!(view.input_state().unwrap(), before);
    assert!(event.load().is_some());
    event
}
