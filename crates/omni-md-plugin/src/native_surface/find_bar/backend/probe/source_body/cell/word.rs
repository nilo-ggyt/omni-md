//! 同一Cell原生词组删除、局部历史和输入撤权；末尾恢复seed后继续原两MiB草稿验收。

use super::*;
use objc2::runtime::Sel;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};

/// 所有动作均由真实NSTextInputClient解释；不向Core传手工算出的删除范围。
fn command(rig: &Rig, selector: Sel) {
    // SAFETY: 固定Cocoa编辑selector及原活主线程client；失败仍由View原错误槽记录。
    unsafe { client(rig).doCommandBySelector(selector) };
}

/// 有限新增样本只交原输入协议；不是重新打开短文档或物化现有Cell。
fn insert(rig: &Rig, text: &str, range: NSRange) {
    // SAFETY: 完整同步NSString和实际原Cell client；明确范围仍由原Input认证。
    unsafe { client(rig).insertText_replacementRange(&NSString::from_str(text), range) };
}

/// 每一步保留原三秒完整帧及原资源票，当前同一Cell Source的UTF8长度也须精确。
fn finish(rig: &mut Rig, stage: &str, frame: Rc<frame::Frame>) -> Result<(), String> {
    frame::wait_complete_frame(stage, &rig.body, frame)?;
    rig.remember_receipt()
}

/// 短样本用固定32unit缓冲逐unit核验；canonical父文稿和邻格一直保持原值。
fn check(rig: &Rig, expected: &str, selection: MarkdownTextSelection) -> Result<(), String> {
    let input = input(rig)?;
    let state = input.state().map_err(|cause| cause.to_string())?;
    let length = expected.encode_utf16().count();
    if length > 32 || state.utf16_len != length || state.selection != selection {
        return Err("Cell词组删除的实际长度或完整选择不符".into());
    }
    let permit = input
        .capture(EPOCH, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    let mut units = [0_u16; 32];
    input
        .copy_utf16_code_units(&permit, 0, &mut units[..length], &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    if units[..length].iter().copied().ne(expected.encode_utf16())
        || rig
            .body
            .progress()
            .map_err(|cause| cause.to_string())?
            .source_total_bytes
            != expected.len()
    {
        return Err("Cell词组删除未保持精确UTF16与UTF8内容".into());
    }
    selection_pixels(rig, "cell_word_exact_selection", selection)?;
    unchanged(rig)
}

/// 相邻同向和反向命令都真实入队；只在原Cell逐笔提交后才可出现最终帧。
fn ordered_words(rig: &mut Rig) -> Result<(), String> {
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
        let setup = observe(&rig.body)?;
        select(rig, 0..4)?;
        let original_selection = rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .selection;
        insert(rig, text, unspecified());
        finish(rig, "cell_word_order_setup", setup)?;
        if forward {
            let moving = observe(&rig.body)?;
            select(rig, head..head)?;
            finish(rig, "cell_word_order_caret", moving)?;
        }
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        if before.selection != MarkdownTextSelection::caret(head) {
            return Err("Cell连按的原caret没有到达固定样本位置".into());
        }
        let frame = observe(&rig.body)?;
        command(
            rig,
            if forward {
                sel!(deleteWordForward:)
            } else {
                sel!(deleteWordBackward:)
            },
        );
        command(rig, sel!(deleteWordBackward:));
        if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
            return Err("Cell连按在接纳阶段提前修改了原before".into());
        }
        finish(rig, "cell_word_order_two_commits", frame)?;
        if rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .generation
            != before.generation + 2
        {
            return Err("Cell连按没有精确发布两笔真实词组事务".into());
        }
        check(rig, final_text, MarkdownTextSelection::caret(final_head))?;
        for (selector, content, selection) in [
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
            (sel!(undo:), "seed", original_selection),
        ] {
            let old = rig.view.input_state().map_err(|cause| cause.to_string())?;
            let history = observe(&rig.body)?;
            command(rig, selector);
            finish(rig, "cell_word_order_history", history)?;
            if rig
                .view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .generation
                != old.generation + 1
            {
                return Err("Cell连按的一次Undo/Redo没有只发布一步".into());
            }
            check(rig, content, selection)?;
        } // 逐次恢复中间字符串和caret；父canonical/邻格及原资源门一直保持。
    }
    Ok(())
}

/// 两个Word之间的普通输入仍携NSNotFound；不能将它偷换成显式replacement取消负控。
fn ordered_typing(rig: &mut Rig) -> Result<(), String> {
    let setup = observe(&rig.body)?;
    select(rig, 0..4)?;
    let original_selection = rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .selection;
    insert(rig, "one two ", unspecified());
    finish(rig, "cell_word_typing_setup", setup)?;
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let frame = observe(&rig.body)?;
    command(rig, sel!(deleteWordBackward:));
    insert(rig, "new ", unspecified());
    command(rig, sel!(deleteWordBackward:));
    if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
        return Err("Cell普通输入抢在此前已接纳的Word之前提交".into());
    }
    finish(rig, "cell_word_typing_word", frame)?;
    if rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .generation
        != before.generation + 3
    {
        return Err("Cell Word/普通输入/Word没有精确发布三笔事务".into());
    }
    check(rig, "one ", MarkdownTextSelection::caret(4))?;
    for (selector, text, head) in [
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
        let old = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let history = observe(&rig.body)?;
        command(rig, selector);
        finish(rig, "cell_word_typing_history", history)?;
        if rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .generation
            != old.generation + 1
        {
            return Err("Cell顺序输入的一次Undo/Redo跳过或重复了真实步骤".into());
        }
        check(rig, text, MarkdownTextSelection::caret(head))?;
    } // 每个真实中间结果都检查原父文档与像素选择，不只比较三步的净差值。
    let old = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let restore = observe(&rig.body)?;
    command(rig, sel!(undo:));
    finish(rig, "cell_word_typing_restore", restore)?;
    if rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .generation
        != old.generation + 1
    {
        return Err("Cell顺序输入的setup没有一次恢复seed".into());
    }
    check(rig, "seed", original_selection)
}

/// 使用同一Cell可见GCB的真实本地框构造系统鼠标事件；旧Scene在事件交付前真实归还。
fn pointer(
    rig: &Rig,
    source: usize,
    fraction: f64,
    kind: NSEventType,
    shift: bool,
) -> Result<NSPoint, String> {
    autoreleasepool(|_| {
        let window = rig.view.window().ok_or("原Cell窗口已释放")?;
        let point = {
            let key = rig
                .view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
                .map_err(|cause| cause.to_string())?;
            let first = scene
                .first_rect(NSRange::new(source, 1))
                .map_err(|cause| cause.to_string())?;
            if first.actual_range.location != source || first.rect.size.width <= 0.0 {
                return Err("Cell双击样本缺少原GCB几何".into());
            }
            let local = NSPoint::new(
                first.rect.origin.x + first.rect.size.width * fraction,
                first.rect.origin.y + first.rect.size.height * 0.5,
            );
            let point = rig.view.convertPoint_toView(local, None);
            if !NativeMarkdownPagedTextViewDelegate::scene_current(&*rig.body, &key) {
                return Err("Cell鼠标事件前原Scene已撤权".into());
            }
            point
        };
        let flags = if shift {
            NSEventModifierFlags::Shift
        } else {
            NSEventModifierFlags::empty()
        };
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind, point, flags, 1.0, window.windowNumber(), None, 1, 2, 1.0).ok_or("无法构造原Cell鼠标事件")?;
        // SAFETY: 原主线程view和完整同步事件；只允许三个既有公开NSResponder鼠标语义。
        unsafe {
            if kind == NSEventType::LeftMouseDown {
                let _: () = objc2::msg_send![&**rig.view, mouseDown: &*event];
            } else if kind == NSEventType::LeftMouseDragged {
                let _: () = objc2::msg_send![&**rig.view, mouseDragged: &*event];
            } else {
                return Err("只有Cell Down/Drag需要原GCB命中坐标".into());
            }
        }
        Ok(point)
    })
}

/// 原鼠标抬起仅结束同一手势，沿最后真实点交付；不为清指针槽重新申请已撤的Source/Scene。
fn pointer_up(rig: &Rig, point: NSPoint, shift: bool) -> Result<(), String> {
    autoreleasepool(|_| {
        let window = rig.view.window().ok_or("原Cell窗口已释放")?;
        let flags = if shift {
            NSEventModifierFlags::Shift
        } else {
            NSEventModifierFlags::empty()
        };
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(NSEventType::LeftMouseUp, point, flags, 1.001, window.windowNumber(), None, 2, 2, 0.0).ok_or("无法构造原Cell鼠标抬起事件")?;
        // SAFETY: 原活主线程view及同一次手势的有限window点，公开mouseUp只同步清原指针槽。
        unsafe {
            let _: () = objc2::msg_send![&**rig.view, mouseUp: &*event];
        }
        Ok(())
    })
}

/// 局部选择只改真实Cell选区；中文/非BMP、双击及拖动不能污染父canonical或增加草稿历史。
fn selected_words(rig: &mut Rig) -> Result<(), String> {
    let text = "one two 中文😀! ";
    select(rig, 0..4)?;
    let original_selection = rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .selection;
    let setup = observe(&rig.body)?;
    insert(rig, text, unspecified());
    finish(rig, "cell_word_selection_setup", setup)?;
    let generation = rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .generation;
    let caret = observe(&rig.body)?;
    select(rig, 9..9)?;
    finish(rig, "cell_word_selection_chinese_caret", caret)?;
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let frame = observe(&rig.body)?;
    command(rig, sel!(selectWord:));
    if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
        return Err("Cell选词在原GCB完成前提前发布".into());
    }
    finish(rig, "cell_word_selection_chinese_command", frame)?;
    check(
        rig,
        text,
        MarkdownTextSelection {
            anchor_utf16: 8,
            head_utf16: 10,
        },
    )?;

    for (source, fraction, expected, stage) in [
        (
            10,
            0.75,
            MarkdownTextSelection {
                anchor_utf16: 10,
                head_utf16: 13,
            },
            "cell_word_selection_emoji_backside",
        ),
        (
            5,
            0.125,
            MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 7,
            },
            "cell_word_selection_double_click",
        ),
    ] {
        let frame = observe(&rig.body)?;
        let point = pointer(rig, source, fraction, NSEventType::LeftMouseDown, false)?;
        pointer_up(rig, point, false)?;
        finish(rig, stage, frame)?;
        check(rig, text, expected)?;
    } // 右半emoji命中必须回到真实完整GCB，不能把UTF16低代理项用作Core查询位置。

    let down = observe(&rig.body)?;
    let mut point = pointer(rig, 1, 0.125, NSEventType::LeftMouseDown, false)?;
    finish(rig, "cell_word_selection_drag_origin", down)?;
    check(
        rig,
        text,
        MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 3,
        },
    )?;
    for (source, expected, stage) in [
        (
            2,
            MarkdownTextSelection {
                anchor_utf16: 3,
                head_utf16: 0,
            },
            "cell_word_selection_same_word_right",
        ),
        (
            1,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 3,
            },
            "cell_word_selection_return_first",
        ),
        (
            0,
            MarkdownTextSelection {
                anchor_utf16: 3,
                head_utf16: 0,
            },
            "cell_word_selection_same_word_left",
        ),
        (
            9,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 10,
            },
            "cell_word_selection_drag_across_groups",
        ),
        (
            1,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 3,
            },
            "cell_word_selection_return_group",
        ),
    ] {
        let frame = observe(&rig.body)?;
        point = pointer(rig, source, 0.125, NSEventType::LeftMouseDragged, false)?;
        finish(rig, stage, frame)?;
        check(rig, text, expected)?;
    } // 同词小拖和回初点的活动端严格沿真实CM6.43.9 get输出，范围相同也核方向。
    pointer_up(rig, point, false)?;

    let down = observe(&rig.body)?;
    pointer(rig, 5, 0.125, NSEventType::LeftMouseDown, false)?;
    finish(rig, "cell_word_selection_left_origin", down)?;
    let drag = observe(&rig.body)?;
    let point = pointer(rig, 1, 0.125, NSEventType::LeftMouseDragged, false)?;
    finish(rig, "cell_word_selection_left_full_anchor", drag)?;
    check(
        rig,
        text,
        MarkdownTextSelection {
            anchor_utf16: 7,
            head_utf16: 0,
        },
    )?;
    pointer_up(rig, point, false)?;

    let caret = observe(&rig.body)?;
    select(rig, 2..2)?;
    finish(rig, "cell_word_selection_shift_caret", caret)?;
    let mut point = None;
    for (source, kind, expected, stage) in [
        (
            5,
            NSEventType::LeftMouseDown,
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 7,
            },
            "cell_word_selection_shift_double",
        ),
        (
            9,
            NSEventType::LeftMouseDragged,
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 10,
            },
            "cell_word_selection_shift_drag",
        ),
    ] {
        let frame = observe(&rig.body)?;
        point = Some(pointer(rig, source, 0.125, kind, true)?);
        finish(rig, stage, frame)?;
        check(rig, text, expected)?;
    }
    pointer_up(rig, point.expect("固定Shift手势已实际交付Down和Drag"), true)?;
    if rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .generation
        != generation
    {
        return Err("Cell选择手势错误创建了字符版本".into());
    }
    for (selector, expected, selection, stage) in [
        (
            sel!(undo:),
            "seed",
            original_selection,
            "cell_word_selection_one_undo_setup",
        ),
        // Cell原Typing memento固定输入确认时的caret，不把后来的纯选择作为输入的Redo终态。
        (
            sel!(redo:),
            text,
            MarkdownTextSelection::caret(14),
            "cell_word_selection_redo_setup",
        ),
        (
            sel!(undo:),
            "seed",
            original_selection,
            "cell_word_selection_restore_seed",
        ),
    ] {
        let before = rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .generation;
        let frame = observe(&rig.body)?;
        command(rig, selector);
        finish(rig, stage, frame)?;
        if rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .generation
            != before + 1
        {
            return Err("Cell选词错误插入了历史项或吞掉setup事务".into());
        }
        check(rig, expected, selection)?;
    }
    Ok(())
}

/// 原Cell同槽的选词、普通键入和删除必须消费各自真实前一步收据，不能从旧caret删除半个词。
fn ordered_selection_inputs(rig: &mut Rig) -> Result<(), String> {
    for typing in [false, true] {
        select(rig, 0..4)?;
        let original_selection = rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .selection;
        let setup = observe(&rig.body)?;
        insert(rig, "one two ", unspecified());
        finish(rig, "cell_word_select_order_setup", setup)?;
        let caret = observe(&rig.body)?;
        select(rig, 5..5)?;
        finish(rig, "cell_word_select_order_caret", caret)?;
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let frame = observe(&rig.body)?;
        command(rig, sel!(selectWord:));
        if typing {
            insert(rig, "new ", unspecified());
        }
        command(rig, sel!(deleteWordBackward:));
        if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
            return Err("Cell后续删除提前越过了已接纳选词".into());
        }
        finish(
            rig,
            if typing {
                "cell_word_select_typing_delete_order"
            } else {
                "cell_word_select_delete_order"
            },
            frame,
        )?;
        if rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .generation
            != before.generation + if typing { 2 } else { 1 }
        {
            return Err("Cell选择/输入/删除的真实事务数量不符".into());
        }
        check(rig, "one  ", MarkdownTextSelection::caret(4))?;
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
        for (selector, text, selection) in [
            (sel!(undo:), intermediate, intermediate_selection),
            (sel!(redo:), "one  ", MarkdownTextSelection::caret(4)),
            (sel!(undo:), intermediate, intermediate_selection),
        ] {
            let old = rig
                .view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .generation;
            let history = observe(&rig.body)?;
            command(rig, selector);
            finish(rig, "cell_word_select_order_history", history)?;
            if rig
                .view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .generation
                != old + 1
            {
                return Err("Cell选词队列Undo/Redo跳过或重放了事务".into());
            }
            check(rig, text, selection)?;
        }
        if typing {
            let old = rig
                .view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .generation;
            let history = observe(&rig.body)?;
            command(rig, sel!(undo:));
            finish(rig, "cell_word_select_order_undo_typing", history)?;
            if rig
                .view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .generation
                != old + 1
            {
                return Err("Cell普通键入没有独立的一次Undo".into());
            }
            check(rig, "one two ", selected)?;
        } // 撤销键入恢复原完整词组选区；查询自身不插入字符历史，父文稿仍由check核原值。
        let old = rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .generation;
        let restore = observe(&rig.body)?;
        command(rig, sel!(undo:));
        finish(rig, "cell_word_select_order_restore_seed", restore)?;
        if rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .generation
            != old + 1
        {
            return Err("Cell选词队列setup未一次恢复原seed".into());
        }
        check(rig, "seed", original_selection)?;
    }
    Ok(())
}

/// 在同一实际帧事件中领取这一动作的错误，随后仍沿原Frame核完整票、三秒和原像素门。
struct WordMoveFrame {
    frame: Rc<frame::Frame>,
    view: Rc<Retained<NativeMarkdownPagedTextView>>,
    error:
        RefCell<Option<crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError>>,
}

impl NativeMarkdownPagedBodyPaintObserver for WordMoveFrame {
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        *self.error.borrow_mut() = self.view.take_failure(); // 真实错误只移交给本用例，不转成Pending或吞掉Source错误。
        self.frame.frame_published(key, paints);
    }
}

/// 原Cell使用同一Body和草稿；移动无字符历史，Typing后新Source必须重新取得UBA事实。
fn move_words(rig: &mut Rig) -> Result<(), String> {
    select(rig, 0..4)?;
    let original_selection = rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .selection;
    let text = "one two 中文😀! ";
    let setup = observe(&rig.body)?;
    insert(rig, text, unspecified());
    finish(rig, "cell_word_move_setup", setup)?;
    let top = observe(&rig.body)?;
    command(rig, sel!(moveToBeginningOfDocument:));
    finish(rig, "cell_word_move_origin", top)?;
    let generation = rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .generation;
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
    ] {
        let frame = observe(&rig.body)?;
        command(rig, selector);
        finish(rig, "cell_word_move_exact_selection", frame)?;
        if rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .generation
            != generation
        {
            return Err("Cell按词移动错误建立了字符事务".into());
        }
        check(
            rig,
            text,
            MarkdownTextSelection {
                anchor_utf16: anchor,
                head_utf16: head,
            },
        )?;
    }
    let top = observe(&rig.body)?;
    command(rig, sel!(moveToBeginningOfDocument:));
    finish(rig, "cell_word_move_queue_origin", top)?;
    let frame = observe(&rig.body)?;
    command(rig, sel!(moveWordForward:));
    insert(rig, "new ", unspecified());
    command(rig, sel!(selectWord:));
    if !matches!(
        rig.view.take_failure(),
        Some(crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Busy)
    ) {
        return Err("旧Cell选择请求没有在文字链前明确拒绝".into());
    }
    command(rig, sel!(moveWordBackward:));
    finish(rig, "cell_word_move_typing_move_order", frame)?;
    check(rig, "onenew  two 中文😀! ", MarkdownTextSelection::caret(0))?;
    let undo = observe(&rig.body)?;
    command(rig, sel!(undo:));
    finish(rig, "cell_word_move_one_typing_undo", undo)?;
    check(rig, text, MarkdownTextSelection::caret(3))?;
    let restore = observe(&rig.body)?;
    command(rig, sel!(undo:));
    finish(rig, "cell_word_move_restore_seed", restore)?;
    check(rig, "seed", original_selection)?;

    // 同一真实队列核逻辑前后、RTL 屏幕左右、非空收拢及保锚扩选；后继 Typing 必须落在精确目标。
    for (text, from, to, selector, moved, expected, final_head) in [
        (
            "one אבג ",
            0,
            0,
            sel!(moveWordForward:),
            MarkdownTextSelection::caret(3),
            "onex אבג ",
            4,
        ),
        (
            "one אבג ",
            8,
            8,
            sel!(moveWordBackward:),
            MarkdownTextSelection::caret(4),
            "one xאבג ",
            5,
        ),
        (
            "سلام عالم",
            0,
            0,
            sel!(moveWordLeft:),
            MarkdownTextSelection::caret(4),
            "سلامx عالم",
            5,
        ),
        (
            "سلام عالم",
            9,
            9,
            sel!(moveWordRight:),
            MarkdownTextSelection::caret(5),
            "سلام xعالم",
            6,
        ),
        (
            "سلام عالم",
            1,
            3,
            sel!(moveWordLeft:),
            MarkdownTextSelection::caret(3),
            "سلاxم عالم",
            4,
        ),
        (
            "سلام عالم",
            1,
            3,
            sel!(moveWordRight:),
            MarkdownTextSelection::caret(1),
            "سxلام عالم",
            2,
        ),
        (
            "سلام عالم",
            0,
            0,
            sel!(moveWordLeftAndModifySelection:),
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 4,
            },
            "x عالم",
            1,
        ),
        (
            "سلام عالم",
            9,
            9,
            sel!(moveWordRightAndModifySelection:),
            MarkdownTextSelection {
                anchor_utf16: 9,
                head_utf16: 5,
            },
            "سلام x",
            6,
        ),
    ] {
        select(rig, 0..4)?;
        let original_selection = rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .selection;
        let setup = observe(&rig.body)?;
        insert(rig, text, unspecified());
        finish(rig, "cell_word_four_directions_setup", setup)?;
        let selection = observe(&rig.body)?;
        select(rig, from..to)?;
        finish(rig, "cell_word_four_directions_origin", selection)?;
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        if let Some(cause) = rig.view.take_failure() {
            return Err(format!(
                "四向 Word 原位置准备失败 selector={selector:?}: {cause}"
            ));
        }
        let frame = frame::pending()?;
        let observed = Rc::new(WordMoveFrame {
            frame: Rc::clone(&frame),
            view: Rc::clone(&rig.view),
            error: RefCell::new(None),
        });
        let erased: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = observed.clone();
        rig.body
            .observe_next_cpu_frame(Rc::downgrade(&erased))
            .map_err(|cause| cause.to_string())?;
        command(rig, selector);
        insert(rig, "x", unspecified());
        finish(rig, "cell_word_four_directions_ordered_typing", frame)?;
        if let Some(cause) = observed.error.borrow_mut().take() {
            return Err(format!(
                "四向 Word 实际动作失败 selector={selector:?}: {cause}"
            ));
        }
        drop((erased, observed)); // 本帧观察的 Native 强边在 Undo 或关闭前真实归还。
        if rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .generation
            != before.generation + 1
        {
            return Err("Word 建立了额外字符事务或丢失后继 Typing".into());
        }
        check(rig, expected, MarkdownTextSelection::caret(final_head))?;
        for (expected, selection) in [(text, moved), ("seed", original_selection)] {
            let undo = observe(&rig.body)?;
            command(rig, sel!(undo:));
            finish(rig, "cell_word_four_directions_exact_undo", undo)?;
            check(rig, expected, selection)?;
        }
    }
    Ok(())
}

pub(super) fn run(rig: &mut Rig) -> Result<(), String> {
    for (head, selector, stage) in [
        (0, sel!(deleteWordBackward:), "cell_word_bof_noop"),
        (4, sel!(deleteWordForward:), "cell_word_eof_noop"),
    ] {
        select(rig, head..head)?;
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let frame = observe(&rig.body)?;
        command(rig, selector);
        finish(rig, stage, frame)?;
        if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
            return Err("Cell BOF/EOF词组空操作修改了原状态或历史".into());
        }
        check(rig, "seed", MarkdownTextSelection::caret(head))?;
        command(rig, sel!(undo:)); // 初始空历史必须仍为空：实际Undo也不允许多出一次状态变化。
        if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
            return Err("Cell端点空删除产生了可撤销事务".into());
        }
    }
    for (text, forward, selected, expected, head, stage) in [
        (
            "base one ",
            false,
            false,
            "base ",
            5,
            "cell_word_ascii_space",
        ),
        (
            "中文😀!",
            false,
            false,
            "中文",
            2,
            "cell_word_backward_non_bmp",
        ),
        ("中文😀!", true, false, "😀!", 0, "cell_word_forward_cjk"),
        ("abcd", false, true, "ab", 2, "cell_word_selected_backward"),
        ("abcd", true, true, "ab", 2, "cell_word_selected_forward"),
    ] {
        let setup = observe(&rig.body)?;
        select(rig, 0..4)?;
        let setup_selection = rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .selection;
        insert(rig, text, unspecified());
        finish(rig, "cell_word_finite_input", setup)?;
        if selected {
            // 已有非空原选择必须直接删除；两个Word方向都不得继续扩大或留下局部内容。
            let selecting = observe(&rig.body)?;
            select(rig, 2..4)?;
            finish(rig, "cell_word_select_existing", selecting)?;
        } else if forward {
            let first = observe(&rig.body)?;
            select(rig, 0..0)?;
            finish(rig, "cell_word_forward_origin", first)?;
        }
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        check(rig, text, before.selection)?;
        let deleting = observe(&rig.body)?;
        command(
            rig,
            if forward {
                sel!(deleteWordForward:)
            } else {
                sel!(deleteWordBackward:)
            },
        );
        if !selected && rig.view.input_state().map_err(|cause| cause.to_string())? != before {
            return Err("Cell Word接纳阶段提前改了原草稿或选择".into());
        }
        finish(rig, stage, deleting)?;
        let after = rig.view.input_state().map_err(|cause| cause.to_string())?;
        if after.generation != before.generation + 1 {
            return Err("一次Cell词组删除没有只发布一次内容版本".into());
        }
        check(rig, expected, MarkdownTextSelection::caret(head))?;
        for (selector, stage, text, selected) in [
            (sel!(undo:), "cell_word_single_undo", text, before.selection),
            (
                sel!(redo:),
                "cell_word_single_redo",
                expected,
                after.selection,
            ),
            (
                sel!(undo:),
                "cell_word_restore_input",
                text,
                before.selection,
            ),
            (
                sel!(undo:),
                "cell_word_restore_seed",
                "seed",
                setup_selection,
            ),
        ] {
            let old = rig.view.input_state().map_err(|cause| cause.to_string())?;
            let frame = observe(&rig.body)?;
            command(rig, selector);
            finish(rig, stage, frame)?;
            if rig
                .view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .generation
                != old.generation + 1
            {
                return Err("Cell一次Undo/Redo没有只发布一次原内容变化".into());
            }
            check(rig, text, selected)?;
        }
    }
    move_words(rig)?;
    ordered_words(rig)?;
    ordered_typing(rig)?;
    selected_words(rig)?;
    ordered_selection_inputs(rig)?;
    let setup = observe(&rig.body)?;
    select(rig, 0..4)?;
    let original_selection = rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .selection;
    insert(rig, "hold word ", unspecified());
    finish(rig, "cell_word_pending_origin", setup)?;
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let successor = observe(&rig.body)?;
    command(rig, sel!(deleteWordBackward:));
    if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
        return Err("Cell旧Word尚未成为可撤权的真实pending".into());
    }
    insert(rig, "R", NSRange::new(0, 0));
    finish(rig, "cell_word_revoked_by_input", successor)?;
    if rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .generation
        != before.generation + 1
    {
        return Err("Cell旧Word在后继真实输入后仍发布了删除".into());
    }
    check(rig, "Rhold word ", MarkdownTextSelection::caret(1))?;
    for (stage, text, selection) in [
        (
            "cell_word_cancel_undo_input",
            "hold word ",
            before.selection,
        ),
        ("cell_word_cancel_restore_seed", "seed", original_selection),
    ] {
        let frame = observe(&rig.body)?;
        command(rig, sel!(undo:));
        finish(rig, stage, frame)?;
        check(rig, text, selection)?;
    }
    println!(
        "native_body_cell word_delete_native=true word_select_native=true double_click_native=true word_drag_native=true shift_word_drag_native=true same_word_drag_direction_matches_codemirror=true selection_no_text_history=true select_then_delete_order_verified=true select_typing_delete_order_verified=true exact_local_unicode=true canonical_unchanged=true single_undo_redo=true bof_eof_empty_history=true input_revoked_real_pending=true repeat_order_verified=true reverse_order_verified=true ordinary_nsnotfound_order_verified=true ime_order_verified=false visual_bidi_word_motion_verified=true original_two_mebibyte_workload_preserved=true formal=false"
    );
    Ok(())
}
