//! Exercise Native cell word deletion, local history, and input revocation. Restore
//! the original seed afterward so the existing 2MiB draft workload remains intact.

use super::*;
use objc2::runtime::Sel;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};

/// Let actual NSTextInputClient interpret each action instead of supplying Core
/// with a deletion range calculated by the test.
fn command(rig: &Rig, selector: Sel) {
    // SAFETY: Use a fixed Cocoa editing selector and the original live main-thread
    // client; failures remain in the actual view's error channel.
    unsafe { client(rig).doCommandBySelector(selector) };
}

/// Send bounded new sample text through the original input protocol, without
/// reopening a shorter document or materializing the existing cell.
fn insert(rig: &Rig, text: &str, range: NSRange) {
    // SAFETY: The complete NSString and actual original cell client remain alive
    // synchronously; original Input still authenticates explicit replacement ranges.
    unsafe { client(rig).insertText_replacementRange(&NSString::from_str(text), range) };
}

/// Preserve the original three-second complete-frame gate and actual resource
/// observations for each action on this same cell Source.
fn finish(rig: &mut Rig, stage: &str, frame: Rc<frame::Frame>) -> Result<(), String> {
    frame::wait_complete_frame(stage, &rig.body, frame)?;
    rig.remember_receipt()
}

/// Compare short samples unit for unit with fixed 32-unit scratch. Parent canonical
/// text and neighboring cells must retain their original values throughout.
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

/// Queue actual adjacent same/reverse-direction commands; the final frame must
/// follow each original cell transaction, not a merged or reordered net edit.
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
        } // Verify each intermediate string/caret while preserving parent text, neighboring cells, and resource gates.
    }
    Ok(())
}

/// Ordinary typing between two word commands keeps NSNotFound. An explicit
/// replacement would instead revoke the queue and would not test this ordering.
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
    } // Check actual intermediate parent state/selection pixels, not merely the three actions' final net change.
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

/// Construct Native mouse events from actual visible cell grapheme geometry.
/// Return the Scene before dispatching the event.
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
        // SAFETY: The original main-thread view and complete event remain alive
        // synchronously; only existing public responder mouse entries are admitted.
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

/// Mouse-up ends the same gesture at its last actual point. Clearing pointer state
/// must not reacquire a Source/Scene already revoked by earlier input.
fn pointer_up(rig: &Rig, point: NSPoint, shift: bool) -> Result<(), String> {
    autoreleasepool(|_| {
        let window = rig.view.window().ok_or("原Cell窗口已释放")?;
        let flags = if shift {
            NSEventModifierFlags::Shift
        } else {
            NSEventModifierFlags::empty()
        };
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(NSEventType::LeftMouseUp, point, flags, 1.001, window.windowNumber(), None, 2, 2, 0.0).ok_or("无法构造原Cell鼠标抬起事件")?;
        // SAFETY: The original view lives on its main thread and the bounded point
        // belongs to this same gesture; public mouseUp synchronously ends it.
        unsafe {
            let _: () = objc2::msg_send![&**rig.view, mouseUp: &*event];
        }
        Ok(())
    })
}

/// Local word selection changes only actual cell selection. Chinese/non-BMP hits,
/// double-click, and drag must change neither parent text nor draft character history.
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
    } // The emoji's right half maps to its complete grapheme, never a low surrogate used as a Core query position.

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
    } // Same-word drags/return-to-origin follow actual CM6.43.9 results, checking direction even when ordered ranges match.
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
        // Original typing history fixes the commit-time caret; later pure
        // selection cannot replace that input's redo endpoint.
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

/// Cell word selection, typing, and deletion consume their actual predecessor
/// receipts in order, rather than deleting part of a word from an old caret.
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
        } // Undo typing restores the actual word selection; querying adds no character history and check preserves parent text.
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

/// Consume this action's actual error at the same real frame event, retaining the
/// original Frame's complete-key, three-second, and pixel requirements.
struct WordMoveFrame {
    frame: Rc<frame::Frame>,
    view: Rc<Retained<NativeMarkdownPagedTextView>>,
    error:
        RefCell<Option<crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError>>,
}

impl NativeMarkdownPagedBodyPaintObserver for WordMoveFrame {
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        *self.error.borrow_mut() = self.view.take_failure(); // Transfer the actual error without converting it to Pending or suppressing Source failure.
        self.frame.frame_published(key, paints);
    }
}

/// Reuse the original cell Body/draft. Word movement adds no character history;
/// a Source created by later typing must obtain its own actual UBA facts.
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

    // Verify logical forward/backward, RTL screen-left/right, nonempty collapse,
    // and anchored extension in the actual queue. Subsequent typing must consume
    // the exact resulting selection, not just a plausible nearby caret.
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
        drop((erased, observed)); // Return this frame observer's actual Native claims before Undo or closure.
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

/// Exercise actual endpoint no-ops, Unicode deletion, ordered input/history, and
/// revocation, restoring seed before the subsequent independent large-draft workload.
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
        command(rig, sel!(undo:)); // Initially empty history must stay empty; actual Undo may introduce no state change.
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
            // An existing nonempty selection is deleted directly; neither word
            // direction may expand it or leave an unmatched fragment behind.
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
