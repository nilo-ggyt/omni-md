//! Exercise actual Source word double-click, fast drag, reversed direction, and
//! offscreen continuation using the original Rig and public Native responder entries.

use super::*;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use std::io::Read;

// Chinese group ranges come from Core's actual CM6.11/6.43.9 oracle: 0..4 and
// 5..7, rather than ASCII whitespace splitting. Write/verify the 4MiB tail one
// fixed fragment at a time, without assembling the complete expected document.
const LINE: &str = "中文分组 测试\r\n\r\n";
const FILLER: &str = "bounded paragraph keeps original selection stable.\r\n\r\n";
const ROWS: usize = 80;
const TAIL: &str = "unread original tail\r\n\r\n";
const TAIL_REPEATS: usize = (4 * 1024 * 1024) / TAIL.len();

fn far() -> usize {
    LINE.encode_utf16().count() + ROWS * FILLER.len()
}

/// Own only this case's temporary file, with no whole-document String, pre-parsed
/// directory, or second application instance.
fn fixture() -> Result<tempfile::NamedTempFile, String> {
    let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
    file.write_all(LINE.as_bytes())
        .map_err(|cause| cause.to_string())?;
    for _ in 0..ROWS {
        file.write_all(FILLER.as_bytes())
            .map_err(|cause| cause.to_string())?;
    }
    file.write_all(LINE.as_bytes())
        .map_err(|cause| cause.to_string())?;
    for _ in 0..TAIL_REPEATS {
        file.write_all(TAIL.as_bytes())
            .map_err(|cause| cause.to_string())?;
    }
    file.flush().map_err(|cause| cause.to_string())?;
    Ok(file)
}

/// Map actual character geometry to window coordinates and return the Scene before
/// event delivery, so a strong reader claim cannot prevent Source retirement.
fn point(rig: &Rig, index: usize) -> Result<NSPoint, String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let first = scene
        .first_rect(NSRange::new(index, 1))
        .map_err(|cause| cause.to_string())?;
    if first.actual_range != NSRange::new(index, 1) || first.rect.size.width <= 0.0 {
        return Err("Source Word 原点击没有真实 canonical 字符框".into());
    }
    let local = NSPoint::new(
        first.rect.origin.x + first.rect.size.width * 0.125,
        first.rect.origin.y + first.rect.size.height * 0.5,
    );
    let actual = scene.hit(local).map_err(|cause| cause.to_string())?;
    if actual != index {
        return Err(format!("Source Word 原点击映射到{actual}而非{index}"));
    }
    Ok(rig.view.convertPoint_toView(local, None))
}

/// Deliver each synchronous live event once to the original responder. clickCount
/// expresses double-click semantics without calling business selection methods directly.
fn mouse(rig: &Rig, point: NSPoint, kind: NSEventType, clicks: isize) -> Result<(), String> {
    let window = rig.view.window().ok_or("Source Word 原窗口已归还")?;
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, point, NSEventModifierFlags::empty(), 1.0, window.windowNumber(), None, 1, clicks, 1.0,
    ).ok_or("无法构造 Source Word 原 Native 鼠标事件")?;
    // SAFETY: The actual window/view live on the main thread. Use only existing
    // NSResponder mouse-down/drag/up entries.
    unsafe {
        if kind == NSEventType::LeftMouseDown {
            let _: () = objc2::msg_send![&**rig.view, mouseDown: &*event];
        } else if kind == NSEventType::LeftMouseDragged {
            let _: () = objc2::msg_send![&**rig.view, mouseDragged: &*event];
        } else if kind == NSEventType::LeftMouseUp {
            let _: () = objc2::msg_send![&**rig.view, mouseUp: &*event];
        } else {
            unreachable!("Source Word 只交真实 Down/Drag/Up");
        }
    }
    Ok(())
}

/// Check user-visible directed selection and original text history, without
/// reading/mutating private pending state, cursors, or word caches.
fn selected(rig: &Rig, expected: MarkdownTextSelection) -> Result<(), String> {
    let actual = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let original = rig.document.upgrade().ok_or("Source Word 原 Doc 已归还")?;
    let document = original.borrow();
    if actual.selection != expected
        || actual.generation != 0
        || document.generation() != 0
        || document.is_dirty()
        || document.can_undo()
        || document.can_redo()
    {
        return Err(format!(
            "Source Word 改变原字或选择方向：actual={:?}, expected={expected:?}, generation={}",
            actual.selection, actual.generation
        ));
    }
    Ok(())
}

/// Deliver drag/up without running the loop. Before Input publishes its first word
/// group, it must accept the latest point and finish within the original three-second frame limit.
fn fast_groups(rig: &mut Rig) -> Result<(), String> {
    for (origin, target, expected, stage) in [
        (
            2,
            6,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 7,
            },
            "source_word_first_pending_forward",
        ),
        (
            6,
            2,
            MarkdownTextSelection {
                anchor_utf16: 7,
                head_utf16: 0,
            },
            "source_word_first_pending_backward",
        ),
    ] {
        let down = point(rig, origin)?;
        let end = point(rig, target)?;
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let frame = observe(&rig.body)?;
        mouse(rig, down, NSEventType::LeftMouseDown, 2)?;
        if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
            return Err("Source Word 首组在实际查询完成前提前发布".into());
        }
        mouse(rig, end, NSEventType::LeftMouseDragged, 2)?;
        mouse(rig, end, NSEventType::LeftMouseUp, 2)?;
        wait_frame(stage, &rig.body, frame)?;
        selected(rig, expected)?;
        rig.remember_receipt()?;
    }
    Ok(())
}

/// Preserve actual CodeMirror range.extend direction: returning to the initial hit
/// is forward; another point within the group is backward. Obtain the actual first
/// group before dragging so fast-drag scenarios cannot conceal ready-group direction defects.
fn same_group(rig: &mut Rig) -> Result<(), String> {
    let origin = point(rig, 1)?;
    let frame = observe(&rig.body)?;
    mouse(rig, origin, NSEventType::LeftMouseDown, 2)?;
    wait_frame("source_word_ready_group", &rig.body, frame)?;
    selected(
        rig,
        MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 4,
        },
    )?;
    for (target, expected, stage) in [
        (
            2,
            MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 0,
            },
            "source_word_same_group_reverse",
        ),
        (
            1,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 4,
            },
            "source_word_return_origin",
        ),
    ] {
        let end = point(rig, target)?;
        let frame = observe(&rig.body)?;
        mouse(rig, end, NSEventType::LeftMouseDragged, 2)?;
        wait_frame(stage, &rig.body, frame)?;
        selected(rig, expected)?;
        rig.remember_receipt()?;
    }
    // Keep the actual mouse-down active. Scroll only through original SourceTarget
    // until the initial word group actually leaves the viewport.
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let frame = observe(&rig.body)?;
    let target = rig
        .body
        .request_source_target(NSRange::new(far() + 6, 1))
        .map_err(|cause| cause.to_string())?;
    wait_frame("source_word_origin_offscreen", &rig.body, frame)?;
    rig.body
        .take_source_target_visible(&target)
        .map_err(|cause| cause.to_string())?
        .ok_or("Source Word 远端目标没有真实可见收据")?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if key.request.virtual_viewport.document_origin.y <= key.request.viewport.size.height
        || key.request.input != before
    {
        return Err("Source Word 纯滚动没有使原首组真实离屏或改变了 Input".into());
    }
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    match scene.first_rect(NSRange::new(0, 1)) {
        Ok(first) => {
            let low = first.rect.origin.y.max(key.request.viewport.origin.y);
            let high = (first.rect.origin.y + first.rect.size.height)
                .min(key.request.viewport.origin.y + key.request.viewport.size.height);
            if high > low {
                return Err("Source Word 首组还在实际可见窗口内".into());
            }
        }
        Err(ViewError::Pending | ViewError::InvalidRange) => {}
        Err(cause) => return Err(cause.to_string()),
    } // Offscreen geometry may be unavailable; actual Source/Native errors cannot be downgraded to invisibility.
    drop((scene, key));
    let end = point(rig, far() + 6)?;
    let frame = observe(&rig.body)?;
    mouse(rig, end, NSEventType::LeftMouseDragged, 2)?;
    mouse(rig, end, NSEventType::LeftMouseUp, 2)?;
    wait_frame("source_word_offscreen_anchor_drag", &rig.body, frame)?;
    selected(
        rig,
        MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: far() + 7,
        },
    )?;
    rig.remember_receipt()
}

/// A new down immediately after up revokes the old pending word group. Its late
/// completion cannot overwrite the new caret, and late drag has no new down authority.
fn cancelled_group(rig: &mut Rig) -> Result<(), String> {
    let frame = observe(&rig.body)?;
    let target = rig
        .body
        .request_source_target(NSRange::new(2, 1))
        .map_err(|cause| cause.to_string())?;
    wait_frame("source_word_cancel_return_top", &rig.body, frame)?;
    rig.body
        .take_source_target_visible(&target)
        .map_err(|cause| cause.to_string())?
        .ok_or("Source Word 撤权例没有回到真实原首屏")?;
    let down = point(rig, 6)?;
    let end = point(rig, 2)?;
    let successor = point(rig, 3)?;
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let frame = observe(&rig.body)?;
    mouse(rig, down, NSEventType::LeftMouseDown, 2)?;
    mouse(rig, end, NSEventType::LeftMouseDragged, 2)?;
    mouse(rig, end, NSEventType::LeftMouseUp, 2)?;
    if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
        return Err("Source Word 撤权例没有留下真实在途首组".into());
    }
    mouse(rig, successor, NSEventType::LeftMouseDown, 1)?;
    mouse(rig, successor, NSEventType::LeftMouseUp, 1)?;
    wait_frame("source_word_up_then_new_down", &rig.body, frame)?;
    selected(rig, MarkdownTextSelection::caret(3))?;
    let after = rig.view.input_state().map_err(|cause| cause.to_string())?;
    mouse(rig, end, NSEventType::LeftMouseDragged, 2)?;
    if rig.view.input_state().map_err(|cause| cause.to_string())? != after {
        return Err("Source Word Up 后迟到 Drag 复活了旧首组".into());
    }
    // Same-point up keeps the unready group's revocable identity. A successor down
    // at the existing caret must revoke it even when InputState values do not change.
    let started = Instant::now();
    mouse(rig, down, NSEventType::LeftMouseDown, 2)?;
    mouse(rig, down, NSEventType::LeftMouseUp, 2)?;
    if rig.view.input_state().map_err(|cause| cause.to_string())? != after {
        return Err("同点Up首词组没有保留真实在途输入".into());
    }
    mouse(rig, successor, NSEventType::LeftMouseDown, 1)?;
    mouse(rig, successor, NSEventType::LeftMouseUp, 1)?;
    // SAFETY: The short query still uses its actual asynchronous producer. Service
    // the original loop without private-state mutation or fabricated cancellation results.
    unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 0.2, false) };
    selected(rig, MarkdownTextSelection::caret(3))?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?
        .validate_ready(&|| true)
        .map_err(|cause| cause.to_string())?;
    if rig.body.has_pending_input()
        || rig.view.cpu_paint_pending() != Some(false)
        || started.elapsed() > Duration::from_secs(3)
    {
        return Err("同caret后继Down未在原三秒内撤销同点Up的旧首词组".into());
    }
    rig.remember_receipt()
}

/// One actual drag remains outside the viewport. Without additional events, its
/// existing timer must keep scrolling and extending the same mouse-down selection.
fn autoscroll(rig: &mut Rig) -> Result<NativeWeak<objc2::runtime::AnyObject>, String> {
    let down = point(rig, 2)?;
    let event = observe(&rig.body)?;
    mouse(rig, down, NSEventType::LeftMouseDown, 1)?;
    wait_frame("source_drag_scroll_down", &rig.body, event)?;
    let before = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let bounds = rig.view.bounds();
    let below = if rig.view.isFlipped() {
        bounds.origin.y + bounds.size.height + 20.0
    } else {
        bounds.origin.y - 20.0
    };
    let outside = rig
        .view
        .convertPoint_toView(NSPoint::new(bounds.origin.x + 24.0, below), None);
    let paints = rig.view.cpu_paints().ok_or("拖选没有原CPU画布")?;
    let started = Instant::now();
    mouse(rig, outside, NSEventType::LeftMouseDragged, 1)?;
    let timer = rig
        .view
        .drag_scroll_weak()
        .ok_or("真实越界拖选没有原期限")?;
    loop {
        if let Some(cause) = rig.body.take_failure() {
            return Err(cause.to_string());
        }
        if let Some(cause) = rig.view.take_failure()
            && !matches!(cause, ViewError::Pending)
        {
            return Err(cause.to_string());
        }
        if let Ok(key) = rig.view.capture_ready_scene_key()
            && key.request.virtual_viewport.document_origin.y
                >= before.request.virtual_viewport.document_origin.y + 40.0
            && key.request.input.selection.anchor_utf16 == 2
            && key.request.input.selection.head_utf16 > 2
            && rig.view.cpu_paint_pending() == Some(false)
            && rig
                .view
                .cpu_paints()
                .is_some_and(|actual| actual >= paints + 2)
        {
            NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
                .map_err(|cause| cause.to_string())?
                .validate_ready(&|| true)
                .map_err(|cause| cause.to_string())?;
            break; // Two actual scroll/frame completions occurred without direct business scrolling/selection calls.
        }
        let remaining = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .ok_or("同一个真实Drag三秒内没有连续滚动与最终有向选区")?;
        // SAFETY: Service only the original main-thread event loop, with each slice
        // contained in the same absolute three-second action deadline.
        unsafe {
            CFRunLoop::run_in_mode(
                kCFRunLoopDefaultMode,
                remaining.as_secs_f64().min(0.02),
                false,
            )
        };
    }
    mouse(rig, outside, NSEventType::LeftMouseUp, 1)?;
    // Mouse-up may arrive during actual reflow. Wait for its accepted endpoint;
    // an intermediate frame cannot stand in for the stopped final selection.
    let stopped = loop {
        if let Ok(key) = rig.view.capture_ready_scene_key()
            && rig.view.cpu_paint_pending() == Some(false)
        {
            break key;
        }
        let remaining = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .ok_or("原拖选Up没有在同一次三秒期限内完成")?;
        // SAFETY: Wait only for original up/CPU-frame delivery, with no replayed
        // mouse-down or extension of the total deadline.
        unsafe {
            CFRunLoop::run_in_mode(
                kCFRunLoopDefaultMode,
                remaining.as_secs_f64().min(0.02),
                false,
            )
        };
    };
    // SAFETY: Service more than three old timer intervals to verify mouse-up
    // stopped scrolling, without posting another event or changing input methods.
    unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 0.2, false) };
    let after = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if after.request.virtual_viewport.document_origin
        != stopped.request.virtual_viewport.document_origin
        || after.request.input != stopped.request.input
        || after.request.input.selection.anchor_utf16 != 2
        || rig.view.drag_scroll_weak().is_some()
    {
        return Err("原Up后旧期限仍改变视口、选择或保留timer owner".into());
    }
    rig.remember_receipt()?;
    Ok(timer) // After outer-pool drainage, verify actual timer destruction using this nonowning weak identity.
}

/// Compare canonical/disk contents against independent fixed generation rules,
/// using stack chunks rather than deriving expected text from product output.
fn unchanged(rig: &Rig) -> Result<(), String> {
    let original = rig.document.upgrade().ok_or("Source Word 原 Doc 已归还")?;
    let document = original.borrow();
    let units = 2 * LINE.encode_utf16().count() + ROWS * FILLER.len() + TAIL_REPEATS * TAIL.len();
    let bytes = 2 * LINE.len() + ROWS * FILLER.len() + TAIL_REPEATS * TAIL.len();
    if document.utf16_len() != units || document.byte_len() != bytes {
        return Err("Source Word 原文长度改变".into());
    }
    let line_units = LINE.encode_utf16().count();
    let far_start = far();
    let mut scratch = [0_u16; 2048];
    for start in (0..units).step_by(scratch.len()) {
        let used = scratch.len().min(units - start);
        document
            .copy_utf16_code_units(start, &mut scratch[..used])
            .map_err(|cause| cause.to_string())?;
        for (offset, actual) in scratch[..used].iter().enumerate() {
            let index = start + offset;
            let expected = if index < line_units {
                LINE.encode_utf16().nth(index).expect("固定首行")
            } else if index < far_start {
                u16::from(FILLER.as_bytes()[(index - line_units) % FILLER.len()])
            } else if index < far_start + line_units {
                LINE.encode_utf16()
                    .nth(index - far_start)
                    .expect("固定远端行")
            } else {
                u16::from(TAIL.as_bytes()[(index - far_start - line_units) % TAIL.len()])
            };
            if *actual != expected {
                return Err(format!("Source Word 在原 UTF16 位置{index}改变正文"));
            }
        }
    }
    drop(document);
    drop(original);
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut scratch = [0_u8; 128];
    let mut segment = |expected: &str| -> Result<(), String> {
        file.read_exact(&mut scratch[..expected.len()])
            .map_err(|cause| cause.to_string())?;
        if &scratch[..expected.len()] != expected.as_bytes() {
            return Err("Source Word 选择流程改写了原磁盘字节".into());
        }
        Ok(())
    };
    segment(LINE)?;
    for _ in 0..ROWS {
        segment(FILLER)?;
    }
    segment(LINE)?;
    for _ in 0..TAIL_REPEATS {
        segment(TAIL)?;
    }
    if file
        .read(&mut scratch[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("Source Word 原磁盘末尾出现新增字节".into());
    }
    Ok(())
}

/// Complete actual word/drag scenarios, verify the unread layout tail, then close
/// the original graph and inspect the retained weak timer only after release.
pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result, timer) = autoreleasepool(|_| -> Result<_, String> {
        let file = fixture()?;
        let units =
            2 * LINE.encode_utf16().count() + ROWS * FILLER.len() + TAIL_REPEATS * TAIL.len();
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            units,
            1 + 2 * (ROWS + 2 + TAIL_REPEATS),
            false,
            MarkdownPresentationMode::Source,
            false,
        )?;
        let mut timer = None;
        let result = (|| -> Result<(), String> {
            rig.panel.makeKeyWindow();
            if !rig.panel.isKeyWindow() {
                return Err("Source Word 原窗口未取得键盘焦点".into());
            }
            fast_groups(&mut rig)?;
            same_group(&mut rig)?;
            cancelled_group(&mut rig)?;
            timer = Some(autoscroll(&mut rig)?);
            let progress = rig.body.progress().map_err(|cause| cause.to_string())?;
            if progress.complete
                || progress.frontier_source_bytes >= progress.source_total_bytes
                || progress.semantic_work.copied_source_bytes != 0
                || progress.semantic_work.parser_input_bytes != 0
            {
                return Err("Source Word 未保视口前缀或错误物化了原长文".into());
            }
            unchanged(&rig)?;
            Ok(())
        })();
        let retirement = release::close(rig)?;
        Ok((retirement, result, timer))
    })?;
    retirement.verify()?;
    if timer.is_some_and(|timer| timer.load().is_some()) {
        return Err("原拖选期限在关闭与原池归还后仍未真实销毁".into());
    }
    result?;
    println!(
        "native_body_source_word actual_native_double_click_and_fast_drag=true actual_first_query_pending=true original_cm_chinese_groups=true directional_same_group=true actual_offscreen_first_group=true mouseup_then_successor_down_revoked=true no_text_history=true canonical_and_disk_unchanged=true original_four_mib_unread_tail=true strict_original_release=true grid_word_tested=false physical_ime_tested=false formal=false whole_product=false"
    );
    Ok(())
}
