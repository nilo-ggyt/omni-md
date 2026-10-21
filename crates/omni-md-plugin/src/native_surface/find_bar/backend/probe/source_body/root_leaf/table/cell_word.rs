//! Exercise actual parent/child-cell word gestures and display mapping. Fast
//! crossings before Ready retain the original three-second gate; Pending is not completion.

use super::*;

const SOURCE: &str = concat!(
    "outside\r\n\r\n",
    "| 首列 | 目标列 |\r\n",
    "| --- | --- |\r\n",
    "| 中文分组 测试 | **中文分组** `测试` a\\|b |\r\n",
    "\r\nafter\r\n",
);
const EDITED: &str = concat!(
    "outside\r\n\r\n",
    "| 首列 | 目标列 |\r\n",
    "| --- | --- |\r\n",
    "| 前缀 中文分组 测试 | **中文分组** `测试` a\\|b |\r\n",
    "\r\nafter\r\n",
);
const DRAFT: &str = "前缀 中文分组 测试";

/// Derive coordinates from fixed original literals. Chinese groups retain the
/// actual CodeMirror oracle ranges 0..4 and 5..7, independent of product hit results.
fn offset(text: &str) -> usize {
    SOURCE[..SOURCE.find(text).expect("固定 Word 格字面唯一存在")]
        .encode_utf16()
        .count()
}

/// Actual Scene rectangles must map back to independent canonical characters.
/// Hidden markers cannot stand in for displayed text.
fn point(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    index: usize,
) -> Result<NSPoint, String> {
    let key = view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let first = scene
        .first_rect(NSRange::new(index, 1))
        .map_err(|cause| format!("格Word字符框失败：expected={index}, cause={cause}"))?;
    if first.actual_range != NSRange::new(index, 1) || first.rect.size.width <= 0.0 {
        return Err(format!(
            "格Word点击没有真实canonical字符框：expected={index}, actual_range={:?}, rect={:?}",
            first.actual_range, first.rect
        ));
    }
    let local = NSPoint::new(
        first.rect.origin.x + first.rect.size.width * 0.125,
        first.rect.origin.y + first.rect.size.height * 0.5,
    );
    let actual = scene.hit(local).map_err(|cause| {
        format!(
            "格Word原点击失败：expected={index}, actual_range={:?}, rect={:?}, local={local:?}, cause={cause}",
            first.actual_range, first.rect
        )
    })?;
    if actual != index {
        // Diagnose the actual rejected point through the same Scene's public
        // geometry, with no private pointer/selection inspection or adjusted hit
        // point. Cell/word affinity, row identity, and both carets distinguish
        // hidden-source mapping, overlapping parts, and visual-direction failures.
        let cell = scene.cell_hit(local, false).map(|hit| {
            hit.map(|hit| {
                (
                    hit.head_utf16,
                    hit.head_affinity,
                    hit.row.source_range().clone(),
                    hit.content_frame,
                )
            })
        });
        let word = scene.word_hit(local);
        let start_caret = scene
            .first_rect(NSRange::new(index, 0))
            .map(|value| (value.actual_range, value.rect));
        let end_caret = scene
            .first_rect(NSRange::new(index + 1, 0))
            .map(|value| (value.actual_range, value.rect));
        return Err(format!(
            "格Word原点击没有命中独立canonical字符：actual={actual}, expected={index}, actual_range={:?}, rect={:?}, local={local:?}, cell={cell:?}, word={word:?}, start_caret={start_caret:?}, end_caret={end_caret:?}",
            first.actual_range, first.rect
        ));
    }
    Ok(view.convertPoint_toView(local, None))
}

/// Keep delivering to the original mouse-down receiver. A detached child uses the
/// same Rig's live window; do not remount a view or replay a parent mouse event.
fn mouse(
    rig: &Rig,
    view: &NativeMarkdownPagedTextView,
    point: NSPoint,
    kind: NSEventType,
    clicks: isize,
) -> Result<(), String> {
    let window = rig.view.window().ok_or("格 Word 原窗口已归还")?;
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, point, NSEventModifierFlags::empty(), 1.0, window.windowNumber(), None, 1, clicks, 1.0,
    ).ok_or("无法构造格 Word 实际鼠标事件")?;
    // SAFETY: The actual main-thread receiver and bounded same-window point are
    // valid; deliver each live event once through its existing responder entry.
    unsafe {
        if kind == NSEventType::LeftMouseDown {
            let _: () = objc2::msg_send![view, mouseDown: &*event];
        } else if kind == NSEventType::LeftMouseDragged {
            let _: () = objc2::msg_send![view, mouseDragged: &*event];
        } else if kind == NSEventType::LeftMouseUp {
            let _: () = objc2::msg_send![view, mouseUp: &*event];
        } else {
            unreachable!("格 Word 只交原 Down/Drag/Up");
        }
    }
    Ok(())
}

/// Keep three seconds from the actual down. Cross cells only after the actual
/// directed first group and its same-Input complete CPU frame arrive. Inspect no
/// private word cache/pending/cursor, and do not substitute the initial child mount frame.
fn group_ready(
    rig: &mut Rig,
    child: &NativeMarkdownPagedBody,
    expected: MarkdownTextSelection,
    started: Instant,
    stage: &str,
) -> Result<(), String> {
    let view = child.view().map_err(|cause| cause.to_string())?;
    loop {
        if let Some(cause) = rig.body.take_failure() {
            return Err(format!("{stage}: 父Body失败：{cause}"));
        }
        if let Some(cause) = child.take_failure() {
            return Err(format!("{stage}: 子Body失败：{cause}"));
        }
        if let Some(cause) = view.take_failure()
            && !matches!(cause, ViewError::Pending)
        {
            return Err(format!("{stage}: 子View失败：{cause}"));
        }
        if view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .selection
            == expected
            && let Ok(key) = view.capture_ready_scene_key()
            && let Some((published, paints)) = view.probe_cpu_published_frame()
            && published.same(&key)
            && view.cpu_paints() == Some(paints)
            && view.cpu_paint_pending() == Some(false)
        {
            rig.receipts.push(
                child
                    .resource_receipts()
                    .map_err(|cause| cause.to_string())?,
            );
            return Ok(());
        }
        let remaining = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .ok_or_else(|| format!("{stage}: 三秒内没有真实首词组和当前完整帧"))?;
        // SAFETY: Wait only within the original action's remaining deadline;
        // actual Source/font/CPU events permit checking public selection/frame state.
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), true) };
    }
}

/// Compare the fixed short canonical text and original file in chunks. Draft
/// submission grants no authority to save automatically before or after commit.
fn document(rig: &Rig, expected: &str) -> Result<(), String> {
    let original = rig.document.upgrade().ok_or("格 Word 原 Doc 已归还")?;
    let document = original.borrow();
    if document.utf16_len() != expected.encode_utf16().count() {
        return Err("格 Word 原文长度不符".into());
    }
    let mut units = [0_u16; 128];
    let mut expected_units = expected.encode_utf16();
    for start in (0..document.utf16_len()).step_by(units.len()) {
        let used = units.len().min(document.utf16_len() - start);
        document
            .copy_utf16_code_units(start, &mut units[..used])
            .map_err(|cause| cause.to_string())?;
        if units[..used]
            .iter()
            .any(|unit| expected_units.next() != Some(*unit))
        {
            return Err("格 Word 改变了独立预期之外的原文/标记/CRLF".into());
        }
    }
    drop(document);
    drop(original);
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 128];
    for original in SOURCE.as_bytes().chunks(bytes.len()) {
        file.read_exact(&mut bytes[..original.len()])
            .map_err(|cause| cause.to_string())?;
        if &bytes[..original.len()] != original {
            return Err("格 Word 未保存操作改变了原磁盘".into());
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("格 Word 原磁盘出现新尾部".into());
    }
    Ok(())
}

/// Check public directed selection only. Actual document facts establish text/
/// history changes, without private pointer or cell-cache inspection.
fn selected(rig: &Rig, expected: MarkdownTextSelection) -> Result<(), String> {
    let actual = rig.view.input_state().map_err(|cause| cause.to_string())?;
    if actual.selection != expected || rig.body.active_cell_body().is_some() {
        return Err(format!(
            "格 Word 跨界没有最终有向选区或仍持旧格：actual={:?}, expected={expected:?}",
            actual.selection
        ));
    }
    Ok(())
}

/// Reopen's parent-location frame supports later actual mounting; it cannot prove
/// first-group/final-endpoint completion. Preserve the same down's absolute three
/// seconds while waiting for exact public directed selection, cell exit, and current complete CPU pixels.
fn wait_selected(
    rig: &Rig,
    expected: MarkdownTextSelection,
    started: Instant,
    stage: &str,
) -> Result<(), String> {
    loop {
        if let Some(cause) = rig.body.take_failure() {
            return Err(format!("{stage}: 原父 Body 失败：{cause}"));
        }
        if let Some(cause) = rig.view.take_failure()
            && !matches!(cause, ViewError::Pending)
        {
            return Err(format!("{stage}: 原父 View 失败：{cause}"));
        }
        let state = rig.view.input_state().map_err(|cause| cause.to_string())?;
        if state.selection == expected
            && rig.body.active_cell_body().is_none()
            && let Ok(key) = rig.view.capture_ready_scene_key()
            && let Some((published, paints)) = rig.view.probe_cpu_published_frame()
            && published.same(&key)
            && rig.view.cpu_paints() == Some(paints)
            && rig.view.cpu_paint_pending() == Some(false)
        {
            let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
                .map_err(|cause| cause.to_string())?;
            // Current-frame authority must remain live. An old mount frame,
            // selection-only change, or in-progress paint is insufficient.
            scene
                .validate_ready(&|| {
                    rig.view
                        .capture_ready_scene_key()
                        .is_ok_and(|actual| actual.same(&key))
                })
                .map_err(|cause| cause.to_string())?;
            return selected(rig, expected);
        }
        let remaining = Duration::from_secs(3).checked_sub(started.elapsed()).ok_or_else(|| {
            format!("{stage}: 原 Down 三秒内未完成最终有向选择与当前帧：actual={:?}, expected={expected:?}, active_cell={}", state.selection, rig.body.active_cell_body().is_some())
        })?;
        // SAFETY: Use the original gesture's remaining deadline with no additional
        // events, renewed deadline, or private pending/cursor inspection.
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), true) };
    }
}

/// Original parent down enters child-Source word selection. Keep drag/up on that
/// same parent receiver while targets cross bold, code, and escaped-pipe projections.
fn parent_drag(rig: &mut Rig) -> Result<(), String> {
    let start = offset("中文分组 测试");
    for (target, end, stage) in [
        (
            offset("**中文分组**") + 3,
            offset("**中文分组**") + 6,
            "grid_word_bold_target",
        ),
        (
            offset("`测试`") + 2,
            offset("`测试`") + 3,
            "grid_word_code_target",
        ),
        (
            offset("a\\|b") + 2,
            offset("a\\|b") + 3,
            "grid_word_pipe_target",
        ),
    ] {
        let down = point(&rig.body, &rig.view, start + 1)?;
        let end_point = point(&rig.body, &rig.view, target)?;
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let started = Instant::now();
        mouse(rig, &rig.view, down, NSEventType::LeftMouseDown, 2)?;
        let child = ui_active(rig, None, started, stage)?;
        group_ready(
            rig,
            &child,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 4,
            },
            started,
            stage,
        )?;
        document(rig, SOURCE)?;
        let frame = observe(&rig.body)?;
        mouse(rig, &rig.view, end_point, NSEventType::LeftMouseDragged, 2)?;
        mouse(rig, &rig.view, end_point, NSEventType::LeftMouseUp, 2)?;
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        selected(
            rig,
            MarkdownTextSelection {
                anchor_utf16: start,
                head_utf16: end,
            },
        )?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
            .map_err(|cause| cause.to_string())?;
        let canonical_caret = NSRange::new(end, 0);
        let actual = scene
            .first_rect(canonical_caret)
            .map_err(|cause| cause.to_string())?;
        if actual.actual_range != canonical_caret || actual.rect.size.height <= 0.0 {
            return Err(format!(
                "{stage}: 真实Word终点在隐藏标记处丢失canonical位置：actual={:?}, expected={canonical_caret:?}",
                actual.actual_range
            ));
        } // Independent literals define endpoints; actual cell caret geometry supplies no expected source range.
        // Cross-cell selection reveals its canonical target. Data must stay below
        // the header after accumulated LineEnd heights update; a stale Grid top
        // cannot paint otherwise correct source geometry onto the preceding physical row.
        let header = scene
            .first_rect(NSRange::new(offset("首列") + 1, 1))
            .map_err(|cause| cause.to_string())?;
        let data = scene
            .first_rect(NSRange::new(start + 1, 1))
            .map_err(|cause| cause.to_string())?;
        if data.actual_range != NSRange::new(start + 1, 1)
            || header.actual_range != NSRange::new(offset("首列") + 1, 1)
            || data.rect.origin.y + data.rect.size.height > header.rect.origin.y
        {
            return Err(format!(
                "{stage}: 真实选择reveal后数据格字符框与表头重叠或来源不符：data_range={:?}, data={:?}, header_range={:?}, header={:?}",
                data.actual_range, data.rect, header.actual_range, header.rect
            ));
        }
        drop(scene);
        document(rig, SOURCE)?;
        let original = rig.document.upgrade().ok_or("格 Word 原 Doc 已归还")?;
        let original = original.borrow();
        if original.generation() != before.generation
            || original.is_dirty()
            || original.can_undo()
            || original.can_redo()
        {
            return Err("只读 Grid Word 拖选产生了正文或历史".into());
        }
        drop(original);
        rig.remember_receipt()?;
        drop(child);
    }
    Ok(())
}

/// Actual parent triple-click selects this cell's complete raw paragraph only.
/// Editable formatting markers remain intact and selection excludes neighboring cells.
fn paragraph_click(rig: &mut Rig) -> Result<(), String> {
    for (target, raw, stage) in [
        (
            offset("中文分组 测试") + 1,
            "中文分组 测试",
            "grid_paragraph_original_plain_cell",
        ),
        (
            offset("**中文分组**") + 3,
            "**中文分组** `测试` a\\|b",
            "grid_paragraph_original_formatted_cell",
        ),
    ] {
        let down = point(&rig.body, &rig.view, target)?;
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let started = Instant::now();
        mouse(rig, &rig.view, down, NSEventType::LeftMouseDown, 3)?;
        mouse(rig, &rig.view, down, NSEventType::LeftMouseUp, 3)?;
        let child = ui_active(rig, None, started, stage)?;
        group_ready(
            rig,
            &child,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: raw.encode_utf16().count(),
            },
            started,
            stage,
        )?;
        let view = child.view().map_err(|cause| cause.to_string())?;
        ui_local(&view, raw)?;
        document(rig, SOURCE)?;

        // End read-only selection through the existing cell cancellation command,
        // without a document edit or test-specific quit shortcut.
        let frame = observe(&rig.body)?;
        view.command(sel!(cancelOperation:))
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        if rig.body.active_cell_body().is_some() {
            return Err(format!("{stage}: 原格取消后仍持活动正文客户"));
        }
        let original = rig.document.upgrade().ok_or("三击原 Doc 已归还")?;
        let original = original.borrow();
        if original.generation() != before.generation
            || original.is_dirty()
            || original.can_undo()
            || original.can_redo()
        {
            return Err(format!("{stage}: 三击选择或原格取消改写了正文/历史"));
        }
        drop(original);
        document(rig, SOURCE)?;
        rig.remember_receipt()?;
        drop((view, child));
    }
    Ok(())
}

/// A new actual child down identifies its first group in edited Source. Crossing
/// to the parent Grid consumes the real three-point sanitization receipt in one commit.
fn edited_child(rig: &mut Rig) -> Result<(), String> {
    let start = offset("中文分组 测试");
    let down = point(&rig.body, &rig.view, start + 1)?;
    let end = point(&rig.body, &rig.view, offset("**中文分组**") + 3)?;
    let started = Instant::now();
    mouse(rig, &rig.view, down, NSEventType::LeftMouseDown, 1)?;
    mouse(rig, &rig.view, down, NSEventType::LeftMouseUp, 1)?;
    let child = ui_active(rig, None, started, "grid_word_edited_child_mount")?;
    let view = child.view().map_err(|cause| cause.to_string())?;
    let frame = observe(&child)?;
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
    // SAFETY: The original child client replaces its fixed seven-unit raw draft;
    // parent canonical text must still retain the complete original document.
    unsafe {
        client.insertText_replacementRange(&NSString::from_str(DRAFT), NSRange::new(0, 7));
    }
    frame::wait_complete_frame("grid_word_edited_child_source", &child, frame)?;
    ui_local(&view, DRAFT)?;
    document(rig, SOURCE)?;
    let down = point(&child, &view, 4)?;
    let started = Instant::now();
    mouse(rig, &view, down, NSEventType::LeftMouseDown, 2)?;
    group_ready(
        rig,
        &child,
        MarkdownTextSelection {
            anchor_utf16: 3,
            head_utf16: 7,
        },
        started,
        "grid_word_edited_child_group",
    )?;
    let frame = observe(&rig.body)?;
    mouse(rig, &view, end, NSEventType::LeftMouseDragged, 2)?;
    mouse(rig, &view, end, NSEventType::LeftMouseUp, 2)?;
    frame::wait_complete_frame("grid_word_child_cross_actual_commit", &rig.body, frame)?;
    selected(
        rig,
        MarkdownTextSelection {
            anchor_utf16: start + 3,
            head_utf16: offset("**中文分组**") + 9,
        },
    )?;
    document(rig, EDITED)?;
    let generation = rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .generation;
    if generation != 1 {
        return Err("已编辑格跨 Word 不是同一笔 canonical 提交".into());
    }
    let frame = observe(&rig.body)?;
    rig.view
        .command(sel!(undo:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("grid_word_one_actual_document_undo", &rig.body, frame)?;
    document(rig, SOURCE)?;
    let original = rig.document.upgrade().ok_or("格 Word 原 Doc 已归还")?;
    let original = original.borrow();
    if original.is_dirty() || original.can_undo() || !original.can_redo() {
        return Err("一次真实 Undo 没有恢复唯一原格事务".into());
    }
    drop(original);
    rig.remember_receipt()?;
    drop((view, child));
    Ok(())
}

/// Deliver without waiting for the first group/child Ready. The original three
/// seconds must prove that accepted mouse-up still completes this same group and
/// target. Pending cannot return success, and pre-delivering the loop before the
/// second point would evade the actual fast-drag scenario.
fn pending_first_group(rig: &mut Rig) -> Result<(), String> {
    let start = offset("中文分组 测试");
    let down = point(&rig.body, &rig.view, start + 1)?;
    let end = point(&rig.body, &rig.view, offset("`测试`") + 2)?;
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let frame = observe(&rig.body)?;
    mouse(rig, &rig.view, down, NSEventType::LeftMouseDown, 2)?;
    if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
        return Err("Grid Word 快拖例未留下真实未发布首组".into());
    }
    mouse(rig, &rig.view, end, NSEventType::LeftMouseDragged, 2)?;
    mouse(rig, &rig.view, end, NSEventType::LeftMouseUp, 2)?;
    frame::wait_complete_frame("grid_word_first_pending_cross_and_up", &rig.body, frame)?;
    selected(
        rig,
        MarkdownTextSelection {
            anchor_utf16: start,
            head_utf16: offset("`测试`") + 3,
        },
    )?;
    document(rig, SOURCE)?;
    rig.remember_receipt()
}

/// Cross from an edited child's own unready first down to the parent Grid and up.
/// Its original three local points must pass one actual sanitizing commit only.
fn child_pending_group(rig: &mut Rig) -> Result<(), String> {
    let start = offset("中文分组 测试");
    let mount = point(&rig.body, &rig.view, start + 1)?;
    let end = point(&rig.body, &rig.view, offset("**中文分组**") + 3)?;
    let started = Instant::now();
    mouse(rig, &rig.view, mount, NSEventType::LeftMouseDown, 1)?;
    mouse(rig, &rig.view, mount, NSEventType::LeftMouseUp, 1)?;
    let child = ui_active(rig, None, started, "grid_word_child_pending_mount")?;
    let view = child.view().map_err(|cause| cause.to_string())?;
    let frame = observe(&child)?;
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
    // SAFETY: Replace the original actual child's fixed seven-unit raw draft;
    // parent text must not change before the actual cross-boundary publication.
    unsafe {
        client.insertText_replacementRange(&NSString::from_str(DRAFT), NSRange::new(0, 7));
    }
    frame::wait_complete_frame("grid_word_child_pending_draft", &child, frame)?;
    document(rig, SOURCE)?;
    let down = point(&child, &view, 4)?;
    let before = view.input_state().map_err(|cause| cause.to_string())?;
    let parent_before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let frame = observe(&rig.body)?;
    mouse(rig, &view, down, NSEventType::LeftMouseDown, 2)?;
    if view.input_state().map_err(|cause| cause.to_string())? != before {
        return Err("child快拖没有留下真实未发布首词组".into());
    }
    mouse(rig, &view, end, NSEventType::LeftMouseDragged, 2)?;
    mouse(rig, &view, end, NSEventType::LeftMouseUp, 2)?;
    frame::wait_complete_frame(
        "grid_word_child_first_pending_cross_and_up",
        &rig.body,
        frame,
    )?;
    selected(
        rig,
        MarkdownTextSelection {
            anchor_utf16: start + 3,
            head_utf16: offset("**中文分组**") + 9,
        },
    )?;
    document(rig, EDITED)?;
    if rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .generation
        != parent_before.generation + 1
    {
        return Err("child快Word跨界没有保唯一正文提交".into());
    }
    let frame = observe(&rig.body)?;
    rig.view
        .command(sel!(undo:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("grid_word_child_pending_one_undo", &rig.body, frame)?;
    document(rig, SOURCE)?;
    rig.remember_receipt()?;
    drop((view, child));
    Ok(())
}

/// A new child down revokes an accepted but unready old mouse-up endpoint. Late
/// drag cannot republish it or change original disk contents.
fn cancelled_child_group(rig: &mut Rig) -> Result<(), String> {
    let start = offset("中文分组 测试");
    let mount = point(&rig.body, &rig.view, start + 1)?;
    let end = point(&rig.body, &rig.view, offset("`测试`") + 2)?;
    let started = Instant::now();
    mouse(rig, &rig.view, mount, NSEventType::LeftMouseDown, 1)?;
    mouse(rig, &rig.view, mount, NSEventType::LeftMouseUp, 1)?;
    let child = ui_active(rig, None, started, "grid_word_cancelled_child_mount")?;
    let view = child.view().map_err(|cause| cause.to_string())?;
    let down = point(&child, &view, 1)?;
    let successor = point(&child, &view, 6)?;
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let frame = observe(&child)?;
    mouse(rig, &view, down, NSEventType::LeftMouseDown, 2)?;
    mouse(rig, &view, end, NSEventType::LeftMouseDragged, 2)?;
    mouse(rig, &view, end, NSEventType::LeftMouseUp, 2)?;
    mouse(rig, &view, successor, NSEventType::LeftMouseDown, 1)?;
    mouse(rig, &view, successor, NSEventType::LeftMouseUp, 1)?;
    frame::wait_complete_frame(
        "grid_word_child_successor_down_revoked_old_up",
        &child,
        frame,
    )?;
    let after = view.input_state().map_err(|cause| cause.to_string())?;
    if after.selection != MarkdownTextSelection::caret(6)
        || rig.view.input_state().map_err(|cause| cause.to_string())? != before
    {
        return Err("child新Down没有撤掉旧Word终点或错误发布父正文".into());
    }
    mouse(rig, &view, end, NSEventType::LeftMouseDragged, 2)?;
    if view.input_state().map_err(|cause| cause.to_string())? != after {
        return Err("child迟到Drag复活了已取消Word终点".into());
    }
    document(rig, SOURCE)?;
    rig.receipts.push(
        child
            .resource_receipts()
            .map_err(|cause| cause.to_string())?,
    );
    let frame = observe(&rig.body)?;
    view.command(sel!(cancelOperation:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame(
        "grid_word_cancelled_child_original_escape",
        &rig.body,
        frame,
    )?;
    if rig.body.active_cell_body().is_some() {
        return Err("原Esc没有归还取消后的活动格".into());
    }
    document(rig, SOURCE)?;
    rig.remember_receipt()?;
    drop((view, child));
    Ok(())
}

/// Changing actual raw cells requires Reopen. Keep the old cell unedited so
/// independent endpoint coordinates still identify the same physical characters.
/// A successor down before the old Source returns to its slot must revoke the old
/// endpoint and preserve the new action, never discard user input as Pending.
fn reopen_groups(rig: &mut Rig) -> Result<(), String> {
    let first = offset("中文分组 测试");
    let bold = offset("**中文分组**");
    let code = offset("`测试`");
    for cancel_with_down in [false, true] {
        let mount = point(&rig.body, &rig.view, first + 1)?;
        let down = point(&rig.body, &rig.view, bold + 3)?;
        let end = point(&rig.body, &rig.view, code + 2)?;
        let successor = point(&rig.body, &rig.view, first + 6)?;
        let started = Instant::now();
        mouse(rig, &rig.view, mount, NSEventType::LeftMouseDown, 1)?;
        mouse(rig, &rig.view, mount, NSEventType::LeftMouseUp, 1)?;
        let previous = ui_active(rig, None, started, "grid_word_reopen_original_cell")?;
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let frame = observe(&rig.body)?;
        let started = Instant::now();
        mouse(rig, &rig.view, down, NSEventType::LeftMouseDown, 2)?;
        mouse(rig, &rig.view, end, NSEventType::LeftMouseDragged, 2)?;
        mouse(rig, &rig.view, end, NSEventType::LeftMouseUp, 2)?;
        if cancel_with_down {
            mouse(rig, &rig.view, successor, NSEventType::LeftMouseDown, 1)?;
            mouse(rig, &rig.view, successor, NSEventType::LeftMouseUp, 1)?;
            let child = ui_active(
                rig,
                Some(&previous),
                started,
                "grid_word_reopen_successor_down",
            )?;
            let view = child.view().map_err(|cause| cause.to_string())?;
            let current = view.input_state().map_err(|cause| cause.to_string())?;
            if current.selection != MarkdownTextSelection::caret(6) {
                return Err("Reopen归槽后没有接住最新真实Down的原格局部位置".into());
            }
            ui_local(&view, "中文分组 测试")?;
            mouse(rig, &rig.view, end, NSEventType::LeftMouseDragged, 2)?;
            if view.input_state().map_err(|cause| cause.to_string())? != current {
                return Err("Reopen旧Up的迟到Drag覆盖了新格Down".into());
            }
            rig.receipts.push(
                child
                    .resource_receipts()
                    .map_err(|cause| cause.to_string())?,
            );
            let close = observe(&rig.body)?;
            view.command(sel!(cancelOperation:))
                .map_err(|cause| cause.to_string())?;
            frame::wait_complete_frame(
                "grid_word_reopen_successor_original_escape",
                &rig.body,
                close,
            )?;
            drop((view, child));
        } else {
            frame::wait_complete_frame(
                "grid_word_reopen_original_down_fast_drag_up",
                &rig.body,
                frame,
            )?;
            wait_selected(
                rig,
                MarkdownTextSelection {
                    anchor_utf16: bold + 2,
                    head_utf16: code + 3,
                },
                started,
                "grid_word_reopen_original_down_fast_drag_up",
            )?;
        }
        document(rig, SOURCE)?;
        if rig
            .view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .generation
            != before.generation
        {
            return Err("Reopen只读Word或取消新Down产生了正文编辑".into());
        }
        rig.remember_receipt()?;
        drop(previous);
    }
    Ok(())
}

/// Run actual cross-cell gestures on the original preview graph, then complete
/// strict resource closure before reporting interaction results.
pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(SOURCE.as_bytes())
            .map_err(|cause| cause.to_string())?;
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            SOURCE.encode_utf16().count(),
            SOURCE.bytes().filter(|byte| *byte == b'\n').count() + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            true,
        )?;
        let result = (|| {
            rig.panel.makeKeyWindow();
            if !rig.panel.isKeyWindow() {
                return Err("Grid Word 原窗口未取得键盘焦点".into());
            }
            paragraph_click(&mut rig)?;
            parent_drag(&mut rig)?;
            edited_child(&mut rig)?;
            pending_first_group(&mut rig)?;
            child_pending_group(&mut rig)?;
            cancelled_child_group(&mut rig)?;
            reopen_groups(&mut rig)
        })();
        let retirement = release::close(rig)?;
        Ok((retirement, result))
    })?;
    retirement.verify()?;
    result?;
    println!(
        "native_body_grid_word actual_parent_down_child_group=true actual_bold_code_pipe_targets=true actual_edited_child_down=true same_commit_group_origin_receipt=true one_document_undo=true actual_pending_first_group_cross=true original_disk_unchanged=true strict_original_release=true triple_click_tested=true physical_ime_tested=false formal=false whole_product=false"
    );
    Ok(())
}
