//! 在原四MiB单段落上添加有限原生输入段；真实Word、历史和撤权均消费同一个Body。

use super::*;
#[path = "word/native.rs"]
mod native;
use objc2::runtime::Sel;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use omni_markdown_core::MarkdownTextSelection;
use omni_md_plugin::native_surface::{
    paged_body::NativeMarkdownPagedBodyResourceReceipts,
    paged_text_view::{NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewDelegate},
};

/// 每次只新增有限测试载荷；不改原文件、直接修改canonical或替换成短文档。
fn insert(view: &NativeMarkdownPagedTextView, text: &str, range: NSRange) {
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(view);
    // SAFETY: 原主线程client与同步完整NSString；明确范围仍交原Input预检并一次提交。
    unsafe { client.insertText_replacementRange(&NSString::from_str(text), range) };
}

/// 系统原selector进入实际NSTextInputClient；失败由原View记录并在真实完整帧门内检查。
fn command(view: &NativeMarkdownPagedTextView, selector: Sel) {
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(view);
    // SAFETY: selector只由下方固定Cocoa动作表传入，仍走原doCommandBySelector入口。
    unsafe { client.doCommandBySelector(selector) };
}

/// 原大正文的每个UTF16单位都以4KiB栈片读回；字节长度同时约束Unicode编码，禁止只抽样头尾。
fn check(
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    fixture: SourceFixture,
    prefix: &str,
    selection: MarkdownTextSelection,
) {
    check_with_suffix(document, fixture, prefix, "", selection);
}

/// EOF原生输入必须落在前面真实编辑后的末端；仍以4KiB栈片验证原正文和有限前后缀。
fn check_with_suffix(
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    fixture: SourceFixture,
    prefix: &str,
    suffix: &str,
    selection: MarkdownTextSelection,
) {
    let original = document.upgrade().unwrap();
    let original = original.borrow();
    let prefix_bytes = prefix.len();
    let suffix_bytes = suffix.len();
    let prefix: Vec<_> = prefix.encode_utf16().collect();
    let suffix: Vec<_> = suffix.encode_utf16().collect();
    let source_end = prefix.len() + fixture.source_utf16();
    assert_eq!(original.utf16_len(), source_end + suffix.len());
    assert_eq!(
        original.byte_len(),
        fixture.edited_source_bytes() + prefix_bytes + suffix_bytes
    );
    assert_eq!(original.selection(), selection);
    let mut units = [0_u16; 2048];
    let repeated = fixture.edited_prefix()[1];
    for start in (0..original.utf16_len()).step_by(units.len()) {
        let length = units.len().min(original.utf16_len() - start);
        original
            .copy_utf16_code_units(start, &mut units[..length])
            .unwrap();
        for (index, unit) in units[..length].iter().enumerate() {
            let offset = start + index;
            let expected = if offset < prefix.len() {
                prefix[offset]
            } else if offset == prefix.len() {
                b'X' as u16 // 原GCB编辑后首字固定为X；新增有限段不得改动原大正文。
            } else if offset < source_end {
                repeated
            } else {
                suffix[offset - source_end]
            }; // 只有这次明确EOF输入属于末尾；旧长度定位会落入原正文并在此逐单位暴露。
            assert_eq!(*unit, expected, "原四MiB正文位置{offset}被词组编辑误改");
        }
    }
}

/// 原观察者等待实际CPU帧；每代真实Source/Reader/worker都进入已有最终严格释放台账。
fn finish(
    stage: &str,
    body: &NativeMarkdownPagedBody,
    frame: Rc<PaintDelivery>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
) {
    deliver(stage, body, frame);
    receipts.push(body.resource_receipts().unwrap());
}

/// 两次真实Native命令在同一个未提交before到达；Undo逐笔验证中间结果，不能只核最终净长度。
fn ordered_words(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    for (text, head, forward, first, first_head, final_text, final_head) in [
        ("one two three ", 14, false, "one two ", 8, "one ", 4),
        (
            "left middle right ",
            5,
            true,
            "left  right ",
            5,
            " right ",
            0,
        ),
    ] {
        let original_selection = view.input_state().unwrap().selection;
        let setup = observe(body, Instant::now());
        insert(view, text, NSRange::new(0, 0));
        finish("source_word_order_setup", body, setup, receipts);
        if forward {
            let top = observe(body, Instant::now());
            command(view, sel!(moveToBeginningOfDocument:));
            finish("source_word_order_origin", body, top, receipts);
            for _ in 0..head {
                let moving = observe(body, Instant::now());
                command(view, sel!(moveRight:));
                finish("source_word_order_caret", body, moving, receipts);
            } // 实际移动至left后的caret，不向Input手填词边界或删除范围。
        }
        let before = view.input_state().unwrap();
        assert_eq!(before.selection, MarkdownTextSelection::caret(head));
        let frame = observe(body, Instant::now());
        command(
            view,
            if forward {
                sel!(deleteWordForward:)
            } else {
                sel!(deleteWordBackward:)
            },
        );
        command(view, sel!(deleteWordBackward:));
        assert_eq!(view.input_state().unwrap(), before); // 两次均已到达，Core不能在入队阶段先改旧before。
        finish("source_word_order_two_commits", body, frame, receipts);
        assert_eq!(
            view.input_state().unwrap().generation,
            before.generation + 2
        );
        check(
            document,
            fixture,
            final_text,
            MarkdownTextSelection::caret(final_head),
        );
        for (selector, expected, selected) in [
            (sel!(undo:), first, MarkdownTextSelection::caret(first_head)),
            (sel!(undo:), text, before.selection),
            (sel!(redo:), first, MarkdownTextSelection::caret(first_head)),
            (
                sel!(redo:),
                final_text,
                MarkdownTextSelection::caret(final_head),
            ),
            (sel!(undo:), first, MarkdownTextSelection::caret(first_head)),
            (sel!(undo:), text, before.selection),
            (sel!(undo:), "", original_selection),
        ] {
            let generation = view.input_state().unwrap().generation;
            let history = observe(body, Instant::now());
            command(view, selector);
            finish("source_word_order_history", body, history, receipts);
            assert_eq!(view.input_state().unwrap().generation, generation + 1);
            check(document, fixture, expected, selected);
        } // 第一、第二词必须是独立真实事务；最后一次Undo只清有限setup，保原四MiB正文。
    }
}

/// 普通NSNotFound是随后键入，不是外部范围替换；必须夹在两个已到达Word之间提交。
fn ordered_typing(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    let original_selection = view.input_state().unwrap().selection;
    let setup = observe(body, Instant::now());
    insert(view, "one two ", NSRange::new(0, 0));
    finish("source_word_typing_setup", body, setup, receipts);
    let before = view.input_state().unwrap();
    let frame = observe(body, Instant::now());
    command(view, sel!(deleteWordBackward:));
    insert(
        view,
        "new ",
        NSRange::new(objc2_foundation::NSNotFound as usize, 0),
    );
    command(view, sel!(deleteWordBackward:));
    assert_eq!(view.input_state().unwrap(), before); // 普通输入已接纳，也不得抢在首个已接纳删除前发布。
    finish("source_word_typing_word", body, frame, receipts);
    assert_eq!(
        view.input_state().unwrap().generation,
        before.generation + 3
    );
    check(document, fixture, "one ", MarkdownTextSelection::caret(4));
    for (selector, prefix, head) in [
        (sel!(undo:), "one new ", 8),
        (sel!(undo:), "one ", 4),
        (sel!(undo:), "one two ", 8),
        (sel!(redo:), "one ", 4),
        (sel!(redo:), "one new ", 8),
        (sel!(redo:), "one ", 4),
        (sel!(undo:), "one new ", 8),
        (sel!(undo:), "one ", 4),
        (sel!(undo:), "one two ", 8),
    ] {
        let generation = view.input_state().unwrap().generation;
        let history = observe(body, Instant::now());
        command(view, selector);
        finish("source_word_typing_history", body, history, receipts);
        assert_eq!(view.input_state().unwrap().generation, generation + 1);
        check(
            document,
            fixture,
            prefix,
            MarkdownTextSelection::caret(head),
        );
    } // 逐次Undo/Redo暴露真实三步中间内容，不能用最终字符串相同掩盖合并、反排或丢键。
    let generation = view.input_state().unwrap().generation;
    let restore = observe(body, Instant::now());
    command(view, sel!(undo:));
    finish("source_word_typing_restore", body, restore, receipts);
    assert_eq!(view.input_state().unwrap().generation, generation + 1);
    check(document, fixture, "", original_selection);
}

/// 真实指针坐标只由原有限Scene的单GCB取得；事件交付前结束Reader临时claim。
fn pointer(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    source: usize,
    fraction: f64,
    kind: NSEventType,
    clicks: isize,
    shift: bool,
) -> NSPoint {
    autoreleasepool(|_| {
        let window = view.window().unwrap();
        let point = {
            let key = view.capture_ready_scene_key().unwrap();
            let scene =
                NativeMarkdownPagedTextViewDelegate::ready_scene(body, &key.request).unwrap();
            let first = scene.first_rect(NSRange::new(source, 1)).unwrap();
            assert_eq!(first.actual_range.location, source);
            assert!(first.rect.size.width > 0.0);
            let local = NSPoint::new(
                first.rect.origin.x + first.rect.size.width * fraction,
                first.rect.origin.y + first.rect.size.height * 0.5,
            );
            let point = view.convertPoint_toView(local, None);
            assert!(NativeMarkdownPagedTextViewDelegate::scene_current(
                body, &key
            ));
            point
        };
        let flags = if shift {
            NSEventModifierFlags::Shift
        } else {
            NSEventModifierFlags::empty()
        };
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind, point, flags, 1.0, window.windowNumber(), None, 1, clicks, 1.0).unwrap();
        // SAFETY: 只向真实主线程view发送Down/Drag这两类公开NSResponder鼠标事件，事件同步存活。
        unsafe {
            if kind == NSEventType::LeftMouseDown {
                let _: () = objc2::msg_send![view, mouseDown: &*event];
            } else if kind == NSEventType::LeftMouseDragged {
                let _: () = objc2::msg_send![view, mouseDragged: &*event];
            } else {
                panic!("只有Down/Drag需要原GCB命中坐标");
            }
        }
        point
    })
}

/// Up只结束同一指针手势，消费之前真实Down/Drag的window点；同步选择后不重新领取已撤的Scene。
fn pointer_up(view: &NativeMarkdownPagedTextView, point: NSPoint, clicks: isize, shift: bool) {
    autoreleasepool(|_| {
        let window = view.window().unwrap();
        let flags = if shift {
            NSEventModifierFlags::Shift
        } else {
            NSEventModifierFlags::empty()
        };
        let up = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(NSEventType::LeftMouseUp, point, flags, 1.001, window.windowNumber(), None, 2, clicks, 0.0).unwrap();
        // SAFETY: 原主线程view及同一实际手势的有限点；公开mouseUp同步交付，不触发新命中或补布局。
        unsafe {
            let _: () = objc2::msg_send![view, mouseUp: &*up];
        }
    });
}

/// 有限中文/emoji段保原四MiB正文；真实选词与指针手势不能创建文本版本或吞掉setup的Undo。
fn selected_words(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    let text = "one two 中文😀! ";
    let original_selection = view.input_state().unwrap().selection;
    let setup = observe(body, Instant::now());
    insert(view, text, NSRange::new(0, 0));
    finish("source_word_selection_setup", body, setup, receipts);
    let generation = view.input_state().unwrap().generation;
    let revision = document
        .upgrade()
        .unwrap()
        .borrow()
        .current_revision()
        .unwrap();
    let history = {
        let doc = document.upgrade().unwrap();
        let doc = doc.borrow();
        (doc.can_undo(), doc.can_redo())
    };

    let caret = observe(body, Instant::now());
    let point = pointer(body, view, 9, 0.125, NSEventType::LeftMouseDown, 1, false);
    pointer_up(view, point, 1, false);
    finish("source_word_selection_chinese_caret", body, caret, receipts);
    check(document, fixture, text, MarkdownTextSelection::caret(9));
    let selecting = observe(body, Instant::now());
    let before = view.input_state().unwrap();
    command(view, sel!(selectWord:));
    assert_eq!(view.input_state().unwrap(), before); // 真实原动作已接纳，不能未完成GCB查询就伪造同步选择。
    finish(
        "source_word_selection_chinese_command",
        body,
        selecting,
        receipts,
    );
    check(
        document,
        fixture,
        text,
        MarkdownTextSelection {
            anchor_utf16: 8,
            head_utf16: 10,
        },
    );

    for (source, fraction, expected, stage) in [
        (
            10,
            0.75,
            MarkdownTextSelection {
                anchor_utf16: 10,
                head_utf16: 13,
            },
            "source_word_selection_emoji_backside",
        ),
        (
            5,
            0.125,
            MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 7,
            },
            "source_word_selection_double_click",
        ),
    ] {
        let frame = observe(body, Instant::now());
        let point = pointer(
            body,
            view,
            source,
            fraction,
            NSEventType::LeftMouseDown,
            2,
            false,
        );
        pointer_up(view, point, 2, false);
        finish(stage, body, frame, receipts);
        check(document, fixture, text, expected);
    } // 非BMP右半部命中必须用真实点击侧选回emoji组，不能把低代理项当作查询head。

    let down = observe(body, Instant::now());
    let mut point = pointer(body, view, 1, 0.125, NSEventType::LeftMouseDown, 2, false);
    finish("source_word_selection_drag_origin", body, down, receipts);
    check(
        document,
        fixture,
        text,
        MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 3,
        },
    );
    // 实际CM6.43.9 basicMouseSelection.get的同词内小拖会反转活动端；回首击pos才恢复原向。
    for (source, expected, stage) in [
        (
            2,
            MarkdownTextSelection {
                anchor_utf16: 3,
                head_utf16: 0,
            },
            "source_word_selection_drag_same_word_right",
        ),
        (
            1,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 3,
            },
            "source_word_selection_drag_return_first",
        ),
        (
            0,
            MarkdownTextSelection {
                anchor_utf16: 3,
                head_utf16: 0,
            },
            "source_word_selection_drag_same_word_left",
        ),
        (
            9,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 10,
            },
            "source_word_selection_drag_across_groups",
        ),
        (
            1,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 3,
            },
            "source_word_selection_drag_return_group",
        ),
    ] {
        let frame = observe(body, Instant::now());
        point = pointer(
            body,
            view,
            source,
            0.125,
            NSEventType::LeftMouseDragged,
            2,
            false,
        );
        finish(stage, body, frame, receipts);
        check(document, fixture, text, expected);
    }
    pointer_up(view, point, 2, false);

    let down = observe(body, Instant::now());
    pointer(body, view, 5, 0.125, NSEventType::LeftMouseDown, 2, false);
    finish(
        "source_word_selection_drag_left_origin",
        body,
        down,
        receipts,
    );
    let dragging = observe(body, Instant::now());
    let point = pointer(
        body,
        view,
        1,
        0.125,
        NSEventType::LeftMouseDragged,
        2,
        false,
    );
    finish(
        "source_word_selection_drag_left_full_anchor",
        body,
        dragging,
        receipts,
    );
    check(
        document,
        fixture,
        text,
        MarkdownTextSelection {
            anchor_utf16: 7,
            head_utf16: 0,
        },
    );
    pointer_up(view, point, 2, false);

    let caret = observe(body, Instant::now());
    let mut point = pointer(body, view, 2, 0.125, NSEventType::LeftMouseDown, 1, false);
    pointer_up(view, point, 1, false);
    finish("source_word_selection_shift_caret", body, caret, receipts);
    for (source, kind, expected, stage) in [
        (
            5,
            NSEventType::LeftMouseDown,
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 7,
            },
            "source_word_selection_shift_double",
        ),
        (
            9,
            NSEventType::LeftMouseDragged,
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 10,
            },
            "source_word_selection_shift_drag",
        ),
    ] {
        let frame = observe(body, Instant::now());
        point = pointer(body, view, source, 0.125, kind, 2, true);
        finish(stage, body, frame, receipts);
        check(document, fixture, text, expected);
    }
    pointer_up(view, point, 2, true);
    assert_eq!(view.input_state().unwrap().generation, generation);
    {
        let doc = document.upgrade().unwrap();
        let doc = doc.borrow();
        assert_eq!(doc.current_revision().unwrap(), revision);
        assert_eq!((doc.can_undo(), doc.can_redo()), history);
    }
    for (selector, expected, selected, stage) in [
        (
            sel!(undo:),
            "",
            original_selection,
            "source_word_selection_one_undo_setup",
        ),
        (
            sel!(redo:),
            text,
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 10,
            },
            "source_word_selection_redo_setup",
        ),
        (
            sel!(undo:),
            "",
            original_selection,
            "source_word_selection_restore_original",
        ),
    ] {
        let before = view.input_state().unwrap().generation;
        let frame = observe(body, Instant::now());
        command(view, selector);
        finish(stage, body, frame, receipts);
        assert_eq!(view.input_state().unwrap().generation, before + 1);
        check(document, fixture, expected, selected);
    } // 第一笔Undo实际移除setup证明所有选词无历史；Redo仍恢复真实最后选择方向。
}

/// 后续删除必须等选词真实Commit；普通输入夹在中间时同样替换完整选词，而非旧caret的一部分。
fn ordered_selection_inputs(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    for typing in [false, true] {
        let original_selection = view.input_state().unwrap().selection;
        let setup = observe(body, Instant::now());
        insert(view, "one two ", NSRange::new(0, 0));
        finish("source_word_select_order_setup", body, setup, receipts);
        let caret = observe(body, Instant::now());
        let point = pointer(body, view, 5, 0.125, NSEventType::LeftMouseDown, 1, false);
        pointer_up(view, point, 1, false);
        finish("source_word_select_order_caret", body, caret, receipts);
        let before = view.input_state().unwrap();
        assert_eq!(before.selection, MarkdownTextSelection::caret(5));
        let frame = observe(body, Instant::now());
        command(view, sel!(selectWord:));
        if typing {
            insert(
                view,
                "new ",
                NSRange::new(objc2_foundation::NSNotFound as usize, 0),
            );
        }
        command(view, sel!(deleteWordBackward:));
        assert_eq!(view.input_state().unwrap(), before);
        finish(
            if typing {
                "source_word_select_typing_delete_order"
            } else {
                "source_word_select_delete_order"
            },
            body,
            frame,
            receipts,
        );
        assert_eq!(
            view.input_state().unwrap().generation,
            before.generation + if typing { 2 } else { 1 }
        );
        check(document, fixture, "one  ", MarkdownTextSelection::caret(4));
        let selected = MarkdownTextSelection {
            anchor_utf16: 4,
            head_utf16: 7,
        };
        let intermediate = if typing { "one new  " } else { "one two " };
        let intermediate_selection = if typing {
            MarkdownTextSelection::caret(8)
        } else {
            selected
        };
        for (selector, prefix, selection) in [
            (sel!(undo:), intermediate, intermediate_selection),
            (sel!(redo:), "one  ", MarkdownTextSelection::caret(4)),
            (sel!(undo:), intermediate, intermediate_selection),
        ] {
            let old = view.input_state().unwrap().generation;
            let history = observe(body, Instant::now());
            command(view, selector);
            finish("source_word_select_order_history", body, history, receipts);
            assert_eq!(view.input_state().unwrap().generation, old + 1);
            check(document, fixture, prefix, selection);
        }
        if typing {
            let old = view.input_state().unwrap().generation;
            let history = observe(body, Instant::now());
            command(view, sel!(undo:));
            finish(
                "source_word_select_order_undo_typing",
                body,
                history,
                receipts,
            );
            assert_eq!(view.input_state().unwrap().generation, old + 1);
            check(document, fixture, "one two ", selected);
        } // 独立输入的Undo必须恢复先前实际完整词组选区；Select自身没有可撤销字符事务。
        let old = view.input_state().unwrap().generation;
        let restore = observe(body, Instant::now());
        command(view, sel!(undo:));
        finish(
            "source_word_select_order_restore_original",
            body,
            restore,
            receipts,
        );
        assert_eq!(view.input_state().unwrap().generation, old + 1);
        check(document, fixture, "", original_selection);
    }
}

/// 原四MiB正文保持不变，系统八种Word动作只移动真实选择；字符事务仍只有setup/typing。
fn move_words(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    let original_selection = view.input_state().unwrap().selection;
    let text = "one two 中文😀! ";
    let setup = observe(body, Instant::now());
    insert(view, text, NSRange::new(0, 0));
    finish("source_word_move_setup", body, setup, receipts);
    let top = observe(body, Instant::now());
    command(view, sel!(moveToBeginningOfDocument:));
    finish("source_word_move_origin", body, top, receipts);
    let generation = view.input_state().unwrap().generation;
    let revision = document
        .upgrade()
        .unwrap()
        .borrow()
        .current_revision()
        .unwrap();
    for (selector, anchor, head) in [
        (sel!(moveWordForward:), 3, 3),
        (sel!(moveWordRight:), 7, 7),
        (sel!(moveWordLeft:), 4, 4),
        (sel!(moveWordBackward:), 0, 0),
        (sel!(moveWordForwardAndModifySelection:), 0, 3),
        (sel!(moveWordRightAndModifySelection:), 0, 7),
        (sel!(moveWordBackwardAndModifySelection:), 0, 4),
        (sel!(moveWordLeftAndModifySelection:), 0, 0),
        (sel!(moveWordRight:), 3, 3),
        (sel!(moveWordRight:), 7, 7),
        (sel!(moveWordBackwardAndModifySelection:), 7, 4),
        (sel!(moveWordBackward:), 4, 4),
        (sel!(moveWordRight:), 7, 7),
        (sel!(moveWordRightAndModifySelection:), 7, 10),
        (sel!(moveWordForwardAndModifySelection:), 7, 13),
        (sel!(moveWordBackwardAndModifySelection:), 7, 10),
        (sel!(moveWordLeftAndModifySelection:), 7, 8),
        (sel!(moveWordLeftAndModifySelection:), 7, 4),
        (sel!(moveWordForward:), 7, 7),
    ] {
        let frame = observe(body, Instant::now());
        command(view, selector);
        finish("source_word_move_exact_selection", body, frame, receipts);
        assert_eq!(view.input_state().unwrap().generation, generation);
        check(
            document,
            fixture,
            text,
            MarkdownTextSelection {
                anchor_utf16: anchor,
                head_utf16: head,
            },
        );
        assert_eq!(
            document
                .upgrade()
                .unwrap()
                .borrow()
                .current_revision()
                .unwrap(),
            revision
        );
    }
    let top = observe(body, Instant::now());
    command(view, sel!(moveToBeginningOfDocument:));
    finish("source_word_move_queue_origin", body, top, receipts);
    let before = view.input_state().unwrap();
    let frame = observe(body, Instant::now());
    command(view, sel!(moveWordForward:));
    insert(
        view,
        "new ",
        NSRange::new(objc2_foundation::NSNotFound as usize, 0),
    );
    command(view, sel!(selectWord:));
    assert!(matches!(
        view.take_failure(),
        Some(
            omni_md_plugin::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Busy
        )
    ));
    command(view, sel!(moveWordBackward:));
    assert_eq!(view.input_state().unwrap(), before); // 已接纳动作仍在原队列，旧hit拒绝不能抢掉文字。
    finish("source_word_move_typing_move_order", body, frame, receipts);
    assert_eq!(view.input_state().unwrap().generation, generation + 1);
    check(
        document,
        fixture,
        "onenew  two 中文😀! ",
        MarkdownTextSelection::caret(0),
    );
    let undo = observe(body, Instant::now());
    command(view, sel!(undo:));
    finish("source_word_move_one_typing_undo", body, undo, receipts);
    check(document, fixture, text, MarkdownTextSelection::caret(3));
    let restore = observe(body, Instant::now());
    command(view, sel!(undo:));
    finish("source_word_move_restore_original", body, restore, receipts);
    check(document, fixture, "", original_selection); // 第一笔Undo仅撤typing，第二笔才撤setup；Move没有插入历史。
}

/// The original four-MiB source remains streamed; a fresh edit invalidates every prepared line before the burst.
fn visual_queued_across_unprepared_crlf(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    let original = view.input_state().unwrap().selection;
    let text = "a\r\n".repeat(32);
    let setup = observe(body, Instant::now());
    insert(view, &text, NSRange::new(0, 0));
    finish("source_visual_crlf_setup", body, setup, receipts);
    let top = observe(body, Instant::now());
    command(view, sel!(moveToBeginningOfDocument:));
    finish("source_visual_crlf_origin", body, top, receipts);
    let before = view.input_state().unwrap();
    let frame = observe(body, Instant::now());
    insert(
        view,
        "b",
        NSRange::new(objc2_foundation::NSNotFound as usize, 0),
    );
    let after_first = view.input_state().unwrap();
    assert_eq!(after_first.generation, before.generation + 1);
    assert_eq!(after_first.selection, MarkdownTextSelection::caret(1));
    for _ in 0..64 {
        command(view, sel!(moveRight:));
    } // Two real GCB intents per ASCII+CRLF line; no guessed average line height.
    insert(
        view,
        "!",
        NSRange::new(objc2_foundation::NSNotFound as usize, 0),
    );
    command(view, sel!(moveLeft:));
    insert(
        view,
        "?",
        NSRange::new(objc2_foundation::NSNotFound as usize, 0),
    );
    assert_eq!(
        view.input_state().unwrap(),
        after_first,
        "Accepted visual/typing intents must not submit on the old native stack"
    );
    finish(
        "source_visual_unprepared_lines_typing_chain",
        body,
        frame,
        receipts,
    );
    assert_eq!(
        view.input_state().unwrap().generation,
        before.generation + 3
    );
    check(
        document,
        fixture,
        &format!("b{text}?!"),
        MarkdownTextSelection::caret(98),
    );
    autoreleasepool(|_| {
        // 最后一笔键入必须用同一收据让caret真正出现在屏内；仅发布BOF帧不能算完成。
        // 原可见性接口将实际visibleRect与bounds求交并核对同key，不借整个虚拟正文高度。
        let key = view.capture_ready_scene_key().unwrap();
        let caret = {
            let scene =
                NativeMarkdownPagedTextViewDelegate::ready_scene(body, &key.request).unwrap();
            let caret = scene.first_rect(NSRange::new(98, 0)).unwrap();
            assert_eq!(caret.actual_range, NSRange::new(98, 0));
            caret.rect
        };
        let proof = view
            .verify_find_target_visible(&key, NSRange::new(98, 0))
            .unwrap();
        assert_eq!(proof.first_rect(), caret);
        let clip = key.request.viewport;
        assert!(
            caret.origin.x >= clip.origin.x
                && caret.origin.y >= clip.origin.y
                && caret.origin.x + caret.size.width <= clip.origin.x + clip.size.width
                && caret.origin.y + caret.size.height <= clip.origin.y + clip.size.height,
            "最终键入caret必须完整位于实际可见裁剪内：caret={caret:?} clip={clip:?}"
        );
    });
    for (stage, prefix, selection) in [
        (
            "source_visual_undo_last_typing",
            format!("b{text}!"),
            MarkdownTextSelection::caret(97),
        ),
        (
            "source_visual_undo_first_queued_typing",
            format!("b{text}"),
            MarkdownTextSelection::caret(97),
        ),
        (
            "source_visual_undo_fresh_source",
            text.clone(),
            MarkdownTextSelection::caret(0),
        ),
        ("source_visual_restore_original", String::new(), original),
    ] {
        let frame = observe(body, Instant::now());
        command(view, sel!(undo:));
        finish(stage, body, frame, receipts);
        check(document, fixture, &prefix, selection);
    } // Exactly four character transactions; neither movement nor side-only commits create Undo records.
}

/// 未排版的跨视口移动后，退格、前删和键入必须按真实到达次序消费完整GCB。
fn ordinary_delete_after_pending_visual(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    let original = view.input_state().unwrap().selection;
    let lines = "a\r\n".repeat(32);
    let family = "👩‍👩‍👧‍👦";
    let text = format!("{lines}e\u{301}{family}!");
    let setup = observe(body, Instant::now());
    insert(view, &text, NSRange::new(0, 0));
    finish("source_grapheme_pending_setup", body, setup, receipts);
    let top = observe(body, Instant::now());
    command(view, sel!(moveToBeginningOfDocument:));
    finish("source_grapheme_pending_origin", body, top, receipts);
    let before = view.input_state().unwrap();
    let frame = observe(body, Instant::now());
    insert(
        view,
        "b",
        NSRange::new(objc2_foundation::NSNotFound as usize, 0),
    );
    let after_first = view.input_state().unwrap();
    for _ in 0..64 {
        command(view, sel!(moveRight:));
    } // 每个真实ASCII与CRLF各一簇；移动穿过视口，仍不预先准备整段行几何。
    command(view, sel!(deleteBackward:));
    command(view, sel!(deleteForward:));
    insert(
        view,
        "?",
        NSRange::new(objc2_foundation::NSNotFound as usize, 0),
    );
    command(view, sel!(deleteForward:)); // 最后一个动作仍须以实际删除收据定位caret，不能依靠后继键入。
    assert_eq!(view.input_state().unwrap(), after_first);
    finish("source_grapheme_pending_order", body, frame, receipts);
    assert_eq!(
        view.input_state().unwrap().generation,
        before.generation + 5
    );
    let shortened = format!("b{}a", "a\r\n".repeat(31));
    check(
        document,
        fixture,
        &format!("{shortened}?!"),
        MarkdownTextSelection::caret(96),
    );
    let key = view.capture_ready_scene_key().unwrap();
    view.verify_find_target_visible(&key, NSRange::new(96, 0))
        .unwrap(); // 真实可见区域必须容纳最后删除后的caret，发布旧BOF帧不算成功。
    for (stage, prefix, selection) in [
        (
            "source_grapheme_pending_undo_emoji",
            format!("{shortened}?{family}!"),
            MarkdownTextSelection::caret(96),
        ),
        (
            "source_grapheme_pending_undo_typing",
            format!("{shortened}{family}!"),
            MarkdownTextSelection::caret(95),
        ),
        (
            "source_grapheme_pending_undo_combining",
            format!("{shortened}e\u{301}{family}!"),
            MarkdownTextSelection::caret(95),
        ),
        (
            "source_grapheme_pending_undo_crlf",
            format!("b{text}"),
            MarkdownTextSelection::caret(97),
        ),
        (
            "source_grapheme_pending_undo_fresh_source",
            text,
            MarkdownTextSelection::caret(0),
        ),
        ("source_grapheme_pending_restore", String::new(), original),
    ] {
        let history = observe(body, Instant::now());
        command(view, sel!(undo:));
        finish(stage, body, history, receipts);
        check(document, fixture, &prefix, selection);
    } // 逐笔Undo暴露真实中间文本；原四MiB正文每个UTF16单位均由现有check流式比对。
}

/// 跨视口续行与行号跨位数后仍保所有已接纳动作；末尾无变化不伪造Undo或丢掉caret。
fn source_commands_after_pending_visual_and_word(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    for initial_lines in [9, 34, 99] {
        let original = view.input_state().unwrap().selection;
        let preceding = initial_lines - 2;
        let lines = "a\r\n".repeat(preceding);
        let text = format!("{lines}- item\r\n");
        let setup_before = view.input_state().unwrap();
        let setup = observe(body, Instant::now());
        insert(view, &text, NSRange::new(0, 0));
        assert_eq!(
            view.input_state().unwrap().generation,
            setup_before.generation + 1
        );
        assert_eq!(
            view.input_state().unwrap().selection,
            MarkdownTextSelection::caret(text.encode_utf16().count()),
        ); // 显式合法插入须同步发布；后续显示失败不能被误诊为正文未改而重放。
        assert_eq!(
            document.upgrade().unwrap().borrow().line_count(),
            initial_lines
        );
        finish(
            &format!("source_command_pending_setup_{initial_lines}"),
            body,
            setup,
            receipts,
        );
        let setup_key = view.capture_ready_scene_key().unwrap();
        view.verify_find_target_visible(&setup_key, NSRange::new(text.encode_utf16().count(), 0))
            .unwrap(); // 尾CRLF后的真实下游caret也须准备，不能只发布旧viewport像素。
        let top = observe(body, Instant::now());
        command(view, sel!(moveToBeginningOfDocument:));
        finish(
            &format!("source_command_pending_origin_{initial_lines}"),
            body,
            top,
            receipts,
        );
        let before = view.input_state().unwrap();
        let frame = observe(body, Instant::now());
        insert(
            view,
            "b",
            NSRange::new(objc2_foundation::NSNotFound as usize, 0),
        );
        let after_first = view.input_state().unwrap();
        for _ in 0..2 * preceding + 2 {
            command(view, sel!(moveRight:));
        } // 每行ASCII和CRLF各一簇，再越过列表标记；99行样本实际跨多个视口。
        command(view, sel!(moveWordForward:));
        insert(
            view,
            "!",
            NSRange::new(objc2_foundation::NSNotFound as usize, 0),
        ); // Source前的真实键入必须先提交，不能在输入占位退出后误执行后继命令。
        command(view, sel!(insertNewline:));
        insert(
            view,
            "z",
            NSRange::new(objc2_foundation::NSNotFound as usize, 0),
        ); // 这一步已在Return前接纳，不能被9→10或99→100行号自有宽度变化静默取消。
        command(view, sel!(insertTab:));
        command(view, sel!(insertBacktab:));
        command(view, sel!(insertBacktab:));
        assert_eq!(view.input_state().unwrap(), after_first);
        finish(
            &format!("source_command_pending_order_{initial_lines}"),
            body,
            frame,
            receipts,
        );
        assert_eq!(
            view.input_state().unwrap().generation,
            before.generation + 6
        );
        assert_eq!(
            document.upgrade().unwrap().borrow().line_count(),
            initial_lines + 1
        );
        let returned = format!("b{lines}- item!\r\n- \r\n");
        let final_text = format!("b{lines}- item!\r\n- z\r\n");
        let indented = format!("b{lines}- item!\r\n  - z\r\n");
        // 所有期望位置只由有限固定样本给出，不从被测排版或实际选择反推。
        let word_end = 1 + preceding * 3 + 6;
        let return_before = word_end + 1;
        let returned_head = return_before + 4;
        let final_head = returned_head + 1;
        check(
            document,
            fixture,
            &final_text,
            MarkdownTextSelection::caret(final_head),
        );
        let key = view.capture_ready_scene_key().unwrap();
        view.verify_find_target_visible(&key, NSRange::new(final_head, 0))
            .unwrap(); // 最后noop沿上一真实收据显示caret，不造空事务或只验证逻辑head。
        for (stage, prefix, selection) in [
            (
                "undo_backtab",
                indented,
                MarkdownTextSelection::caret(final_head + 2),
            ),
            (
                "undo_tab",
                final_text,
                MarkdownTextSelection::caret(final_head),
            ),
            (
                "undo_post_return_typing",
                returned,
                MarkdownTextSelection::caret(returned_head),
            ),
            (
                "undo_return",
                format!("b{lines}- item!\r\n"),
                MarkdownTextSelection::caret(return_before),
            ),
            (
                "undo_first_typing",
                format!("b{text}"),
                MarkdownTextSelection::caret(word_end),
            ),
            ("undo_fresh_source", text, MarkdownTextSelection::caret(0)),
            ("restore", String::new(), original),
        ] {
            let history = observe(body, Instant::now());
            command(view, sel!(undo:));
            finish(
                &format!("source_command_pending_{stage}_{initial_lines}"),
                body,
                history,
                receipts,
            );
            check(document, fixture, &prefix, selection);
        } // 真正Typing、Return和有效缩进各一笔历史，末尾noop与自有行号布局没有额外Undo。
    }
}

/// 文档端点必须等在途移动和键入真正提交；EOF输入证明长度更新，扩选证明不继承旧anchor。
fn document_boundaries_after_pending_input(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    for extend in [false, true] {
        let original = view.input_state().unwrap().selection;
        let lines = "a\r\n".repeat(32);
        let text = format!("{lines}one two\r\n");
        let setup = observe(body, Instant::now());
        insert(view, &text, NSRange::new(0, 0));
        finish("source_document_pending_setup", body, setup, receipts);
        let top = observe(body, Instant::now());
        command(view, sel!(moveToBeginningOfDocument:));
        finish("source_document_pending_origin", body, top, receipts);
        let before = view.input_state().unwrap();
        let frame = observe(body, Instant::now());
        insert(
            view,
            "b",
            NSRange::new(objc2_foundation::NSNotFound as usize, 0),
        );
        let after_first = view.input_state().unwrap();
        for _ in 0..64 {
            command(view, sel!(moveRight:));
        }
        command(view, sel!(moveWordForward:));
        insert(
            view,
            "!",
            NSRange::new(objc2_foundation::NSNotFound as usize, 0),
        );
        let middle = format!("b{lines}one! two\r\n");
        let cases = if extend {
            command(view, sel!(moveToBeginningOfDocumentAndModifySelection:));
            command(view, sel!(moveToBeginningOfDocumentAndModifySelection:));
            insert(
                view,
                "R",
                NSRange::new(objc2_foundation::NSNotFound as usize, 0),
            );
            command(view, sel!(moveToEndOfDocumentAndModifySelection:));
            command(view, sel!(moveToBeginningOfDocumentAndModifySelection:));
            insert(
                view,
                "Q",
                NSRange::new(objc2_foundation::NSNotFound as usize, 0),
            );
            vec![
                (
                    "undo_last_replacement",
                    "R two\r\n".to_owned(),
                    "",
                    MarkdownTextSelection {
                        anchor_utf16: 1,
                        head_utf16: 0,
                    },
                    false,
                ),
                (
                    "undo_first_replacement",
                    middle.clone(),
                    "",
                    MarkdownTextSelection {
                        anchor_utf16: 101,
                        head_utf16: 0,
                    },
                    false,
                ),
            ] // 扩选只保前一步真实anchor；重复BOF与往返EOF不产生文字Undo。
        } else {
            command(view, sel!(moveToEndOfDocument:));
            command(view, sel!(moveToEndOfDocument:));
            insert(
                view,
                "EOF",
                NSRange::new(objc2_foundation::NSNotFound as usize, 0),
            );
            command(view, sel!(moveToBeginningOfDocument:));
            command(view, sel!(moveToBeginningOfDocument:));
            insert(
                view,
                "?",
                NSRange::new(objc2_foundation::NSNotFound as usize, 0),
            );
            vec![
                (
                    "undo_first_insertion",
                    middle.clone(),
                    "EOF",
                    MarkdownTextSelection::caret(0),
                    false,
                ),
                (
                    "undo_eof_insertion_then_home",
                    middle.clone(),
                    "",
                    MarkdownTextSelection::caret(fixture.source_utf16() + 107),
                    true,
                ),
            ] // EOF文字必须追加在middle真实107单位之后；不用旧总长，也不要求先排版全文。
        };
        let final_selection = if extend {
            command(view, sel!(moveToBeginningOfDocumentAndModifySelection:));
            command(view, sel!(moveToBeginningOfDocumentAndModifySelection:));
            MarkdownTextSelection {
                anchor_utf16: 1,
                head_utf16: 0,
            }
        } else {
            command(view, sel!(moveToBeginningOfDocument:));
            command(view, sel!(moveToBeginningOfDocument:));
            MarkdownTextSelection::caret(0)
        }; // 最后重复BOF真实Unchanged，仍应保留前一步选择的显示意图且不增加文字Undo。
        assert_eq!(view.input_state().unwrap(), after_first);
        finish("source_document_pending_order", body, frame, receipts);
        assert_eq!(
            view.input_state().unwrap().generation,
            before.generation + 4
        );
        if extend {
            check(document, fixture, "Q two\r\n", final_selection);
        } else {
            check_with_suffix(
                document,
                fixture,
                &format!("?{middle}"),
                "EOF",
                final_selection,
            );
        }
        let key = view.capture_ready_scene_key().unwrap();
        view.verify_find_target_visible(&key, NSRange::new(final_selection.head_utf16, 0))
            .unwrap();
        for (stage, prefix, suffix, selection, home_before_frame) in cases {
            let history = observe(body, Instant::now());
            let generation = view.input_state().unwrap().generation;
            command(view, sel!(undo:));
            assert_eq!(view.input_state().unwrap().generation, generation + 1);
            check_with_suffix(document, fixture, &prefix, suffix, selection);
            let visible = if home_before_frame {
                // Undo必须先真实恢复EOF收据；同步核全文和精确caret后，才用原Home动作回首屏。
                // 这里只验输入/历史保序，不把尚未准备的EOF宣称可见，也不要求中间态先排版全文。
                command(view, sel!(moveToBeginningOfDocument:));
                MarkdownTextSelection::caret(0)
            } else {
                selection
            };
            finish(stage, body, history, receipts);
            check_with_suffix(document, fixture, &prefix, suffix, visible);
            let key = view.capture_ready_scene_key().unwrap();
            view.verify_find_target_visible(&key, NSRange::new(visible.head_utf16, 0))
                .unwrap();
        }
        for (stage, prefix, selection) in [
            (
                "source_document_undo_pending_typing",
                format!("b{text}"),
                MarkdownTextSelection::caret(100),
            ),
            (
                "source_document_undo_fresh_source",
                text,
                MarkdownTextSelection::caret(0),
            ),
            ("source_document_restore", String::new(), original),
        ] {
            let history = observe(body, Instant::now());
            command(view, sel!(undo:));
            finish(stage, body, history, receipts);
            check(document, fixture, &prefix, selection);
        } // 五笔真实字符历史含setup；移动、扩选和端点noop没有伪造额外Undo。
    }
}

pub(super) fn run(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) {
    let original_revision = document
        .upgrade()
        .unwrap()
        .borrow()
        .current_revision()
        .unwrap();
    for (text, forward, selected, expected, head, label) in [
        ("one ", false, false, "", 0, "source_word_ascii_space"),
        (
            "中文😀!",
            false,
            false,
            "中文",
            2,
            "source_word_backward_non_bmp",
        ),
        ("中文😀!", true, false, "😀!", 0, "source_word_forward_cjk"),
        (
            "abcd",
            false,
            true,
            "ab",
            2,
            "source_word_selected_backward",
        ),
        ("abcd", true, true, "ab", 2, "source_word_selected_forward"),
    ] {
        let original_selection = view.input_state().unwrap().selection;
        let setup = observe(body, Instant::now());
        insert(view, text, NSRange::new(0, 0));
        finish("source_word_finite_prefix", body, setup, receipts);
        if selected {
            // 原非空选择直接删除，两个方向都不能擅自扩大成词组；选择仍由原生修改选择动作形成。
            for _ in 0..2 {
                let extending = observe(body, Instant::now());
                command(view, sel!(moveLeftAndModifySelection:));
                finish("source_word_select_existing", body, extending, receipts);
            }
        } else if forward {
            let top = observe(body, Instant::now());
            command(view, sel!(moveToBeginningOfDocument:));
            finish("source_word_forward_caret", body, top, receipts);
        }
        let before = view.input_state().unwrap();
        check(document, fixture, text, before.selection);
        let deleting = observe(body, Instant::now());
        command(
            view,
            if forward {
                sel!(deleteWordForward:)
            } else {
                sel!(deleteWordBackward:)
            },
        );
        if !selected {
            // 非空选择复用原同步删除；仅空caret的词边界计算应真实停在旧状态等待原续体。
            assert_eq!(
                view.input_state().unwrap(),
                before,
                "接纳异步删除不能提前改原选择或文本"
            );
        }
        finish(label, body, deleting, receipts);
        let after = view.input_state().unwrap();
        assert_eq!(after.generation, before.generation + 1);
        check(
            document,
            fixture,
            expected,
            MarkdownTextSelection::caret(head),
        );
        for (selector, stage, content, selection) in [
            (
                sel!(undo:),
                "source_word_single_undo",
                text,
                before.selection,
            ),
            (
                sel!(redo:),
                "source_word_single_redo",
                expected,
                after.selection,
            ),
            (
                sel!(undo:),
                "source_word_restore_prefix",
                text,
                before.selection,
            ),
            (
                sel!(undo:),
                "source_word_restore_original",
                "",
                original_selection,
            ),
        ] {
            let before_history = view.input_state().unwrap();
            let frame = observe(body, Instant::now());
            command(view, selector);
            finish(stage, body, frame, receipts);
            assert_eq!(
                view.input_state().unwrap().generation,
                before_history.generation + 1
            );
            check(document, fixture, content, selection);
        }
        assert_eq!(
            document
                .upgrade()
                .unwrap()
                .borrow()
                .current_revision()
                .unwrap(),
            original_revision
        );
    }

    visual_queued_across_unprepared_crlf(body, view, document, receipts, fixture);
    ordinary_delete_after_pending_visual(body, view, document, receipts, fixture);
    source_commands_after_pending_visual_and_word(body, view, document, receipts, fixture);
    document_boundaries_after_pending_input(body, view, document, receipts, fixture);
    native::run(body, view, document, receipts, fixture);
    native::external_replacement(body, view, document, receipts, fixture);
    native::focus_transfer(body, view, document, receipts, fixture);
    move_words(body, view, document, receipts, fixture);
    ordered_words(body, view, document, receipts, fixture);
    ordered_typing(body, view, document, receipts, fixture);
    selected_words(body, view, document, receipts, fixture);
    ordered_selection_inputs(body, view, document, receipts, fixture);

    for (endpoint, delete, stage) in [
        (
            sel!(moveToBeginningOfDocument:),
            sel!(deleteWordBackward:),
            "source_word_bof_noop",
        ),
        (
            sel!(moveToEndOfDocument:),
            sel!(deleteWordForward:),
            "source_word_eof_noop",
        ),
    ] {
        // 标准端点先提交真实caret，随后立即发送Word；不借测试直接设置Input或跳过原续体。
        command(view, endpoint);
        let before = view.input_state().unwrap();
        let history = {
            let doc = document.upgrade().unwrap();
            let doc = doc.borrow();
            (doc.can_undo(), doc.can_redo())
        };
        let frame = observe(body, Instant::now());
        command(view, delete);
        finish(stage, body, frame, receipts);
        assert_eq!(view.input_state().unwrap(), before);
        check(document, fixture, "", before.selection);
        let doc = document.upgrade().unwrap();
        let doc = doc.borrow();
        assert_eq!((doc.can_undo(), doc.can_redo()), history);
        assert_eq!(doc.current_revision().unwrap(), original_revision);
    }

    let top = observe(body, Instant::now());
    command(view, sel!(moveToBeginningOfDocument:));
    finish("source_word_cancel_origin", body, top, receipts);
    let setup = observe(body, Instant::now());
    insert(view, "pending ", NSRange::new(0, 0));
    finish("source_word_cancel_prefix", body, setup, receipts);
    let before = view.input_state().unwrap();
    let successor = observe(body, Instant::now());
    command(view, sel!(deleteWordBackward:));
    assert_eq!(view.input_state().unwrap(), before); // 原实际pending尚未发布删除，才可测试同票撤权。
    insert(view, "R", NSRange::new(0, 0));
    finish("source_word_revoked_by_input", body, successor, receipts);
    assert_eq!(
        view.input_state().unwrap().generation,
        before.generation + 1
    );
    check(
        document,
        fixture,
        "Rpending ",
        MarkdownTextSelection::caret(1),
    );
    for (stage, prefix, selection) in [
        (
            "source_word_cancel_undo_input",
            "pending ",
            before.selection,
        ),
        (
            "source_word_cancel_restore",
            "",
            MarkdownTextSelection::caret(0),
        ),
    ] {
        let frame = observe(body, Instant::now());
        command(view, sel!(undo:));
        finish(stage, body, frame, receipts);
        check(document, fixture, prefix, selection);
    }
    assert_eq!(
        document
            .upgrade()
            .unwrap()
            .borrow()
            .current_revision()
            .unwrap(),
        original_revision
    );
    println!(
        "paged_body_source word_delete_native=true word_select_native=true double_click_native=true word_drag_native=true shift_word_drag_native=true same_word_drag_direction_matches_codemirror=true selection_no_text_history=true select_then_delete_order_verified=true select_typing_delete_order_verified=true exact_original_four_mebibytes=true single_undo_redo=true bof_eof_no_history=true input_revoked_real_pending=true repeat_order_verified=true reverse_order_verified=true ordinary_nsnotfound_order_verified=true ime_order_verified=false visual_bidi_word_motion_verified=false formal=false"
    );
}

/// 关闭仍由原验收入口执行，这里只交出本次实际未解释event的弱释放票。
pub(super) fn native_before_close(view: &NativeMarkdownPagedTextView) -> objc2::rc::Weak<NSEvent> {
    native::before_close(view)
}
