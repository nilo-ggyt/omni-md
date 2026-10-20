//! 真实父/子格 Word 手势与显示映射；未 Ready 快跨仍以原三秒门暴露，不能把 Pending 当完成。

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

/// 坐标只从固定原字面换算；中文组范围复用已执行 CM oracle 的中文分组=0..4、测试=5..7。
fn offset(text: &str) -> usize {
    SOURCE[..SOURCE.find(text).expect("固定 Word 格字面唯一存在")]
        .encode_utf16()
        .count()
}

/// 同一实际 Scene 的字符框须映回独立 canonical 字符；隐藏标记不能被拿来冒充显示文字。
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
        // 只在真实拒绝点借同Scene的公开空间结果；不读取选择/指针私态，也不改变固定点击侧来迁就候选。
        // 普通命中与Word命中的亲和、行列意图及两端caret可区分隐藏映射、跨part重合和视觉方向问题。
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

/// 事件继续交原 Down 接收者；child 已 detach 后仍借同一 Rig 活窗口，不重挂 View 或重播父 mouse。
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
    // SAFETY: 原主线程实际接收者、同窗口有限点；每个同步活事件只交一次既有 responder 语义。
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

/// 相同三秒从实际 Down 开始，只有真实有向首组和同 Input 的 CPU 完整帧都就绪才继续跨格。
/// 不检查 Word pending/cache/游标，也不以初始 child 可见帧替代首次选词完成。
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
        // SAFETY: 只等待原动作剩余期限；原 Source/字体/CPU 完成通知唤醒后核公开选择与帧。
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), true) };
    }
}

/// 固定短文逐片核 canonical 和原文件；真实草稿提交前后均无权自动保存到磁盘。
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

/// 选择只保当前公开方向；字符变化与历史是否存在由真实 Doc 事实决定，不读指针或格缓存私态。
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

/// Reopen 的父定位帧只供下一步真实挂载，不能把它当整次首组/末端交付已经完成。
/// 同一次 Down 的绝对三秒不重置；只等用户可观察的精确有向选择、格退出和其当前完整 CPU 帧。
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
            // 当前帧的原权限必须还活着；旧 mount 帧、只有 selection 变更或还在绘图都不算完成。
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
        // SAFETY: 仍消费原手势剩余期限；不另发事件、不重置截止时间，也不检查内部 pending/游标。
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), true) };
    }
}

/// 首个真实父 Down 进入 child Source 选词；随后仍把 Drag/Up 交原父接收者，目标依次经过强调/代码/管道投影。
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
        } // 原端点来自独立字面，空间只借真实格caret，不从映回后的范围反推期望。
        // 真实跨格选择会触发canonical目标reveal；后继数据格必须仍在原表头下方，
        // 不能让批量LineEnd补齐总高度后，旧Grid.top把同源正确字符框画回上一物理行。
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

/// 原父窗口的真实三击只选当前格的完整原段；格式标记仍属可编辑 raw，不能吞掉或扩到邻格。
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

        // 沿既有格取消动作结束这次只读选择；不造正文编辑，也不为测试添加退出快捷键。
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

/// 实际 child 新 Down 在已编辑 Source 内确定首组；跨到父 Grid 后用真实三点清洗 receipt 提交一次。
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
    // SAFETY: 原实际 child client 只替换固定七单元 raw；父 canonical 此时仍必须保持完整原文。
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

/// 此动作故意不等首组/child Ready；原三秒门必须证明接纳Up后仍完成同一次首组与目标。
/// 不以预期 Pending 分支返回成功，也不在发第二点之前驱动 runloop 帮产品规避快拖。
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

/// 已编辑child自己的首Down未Ready即跨到父Grid并Up；原局部三点只能经一次真实清洗提交。
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
    // SAFETY: 原实际child的固定七单元raw；正文在真正跨界发布前不得改变。
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

/// 旧Up已接纳而未Ready时，child新Down明确撤旧终点；迟到Drag不能重新发布或改写原磁盘。
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

/// 真实换raw必经Reopen：旧格未编辑，避免几何随正文变更，使独立终点仍对应同一物理字符。
/// 第二次在旧来源归槽前真实新Down，必须撤旧终点并保新Down，而不是以Pending丢掉用户动作。
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
        let retirement = release::close(rig, false)?;
        Ok((retirement, result))
    })?;
    retirement.verify(false)?;
    result?;
    println!(
        "native_body_grid_word actual_parent_down_child_group=true actual_bold_code_pipe_targets=true actual_edited_child_down=true same_commit_group_origin_receipt=true one_document_undo=true actual_pending_first_group_cross=true original_disk_unchanged=true strict_original_release=true triple_click_tested=true physical_ime_tested=false formal=false whole_product=false"
    );
    Ok(())
}
