//! 查找必须先读到真实单格提交，再完成同一正文的可见匹配；原磁盘与单笔历史独立核对。

use super::*;

pub(super) fn run(rig: &mut Rig) -> Result<(), String> {
    let seed = UI_SOURCE[..UI_SOURCE.find("seed").expect("固定格seed")]
        .encode_utf16()
        .count();
    let point = links::point(rig, seed)?;
    let started = Instant::now();
    links::mouse(rig, point, NSEventType::LeftMouseDown)?;
    links::mouse(rig, point, NSEventType::LeftMouseUp)?;
    let cell = ui_active(rig, None, started, "table_ui_find_mount")?;
    let child = cell.view().map_err(|cause| cause.to_string())?;
    let first = observe(&cell)?;
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**child);
    // 真实client只替换固定短格；这里不写canonical，也不代替物理输入法确认候选。
    // SAFETY: 当前主线程活client、有效字符串与原格四个真实UTF16单元。
    unsafe {
        client.insertText_replacementRange(&NSString::from_str(UI_INSERTED), NSRange::new(0, 4));
    }
    frame::wait_complete_frame("table_ui_find_local_edit", &cell, first)?;
    ui_local(&child, UI_INSERTED)?;
    ui_document(rig, UI_SOURCE)?;

    // 格外父背景的右键菜单也必须保留原活动格历史，不能碰尚未提交的父文稿 Undo。
    let window = rig.view.window().ok_or("活动格右键菜单的窗口已释放")?;
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::RightMouseDown,
        rig.view.convertPoint_toView(NSPoint::new(2.0, 2.0), None),
        NSEventModifierFlags::empty(),
        1.0,
        window.windowNumber(),
        None,
        1,
        1,
        1.0,
    ).ok_or("无法创建活动格父背景的右键事件")?;
    drop(window);
    for (index, expected, stage) in [
        (0, "seed", "table_parent_menu_local_undo"),
        (1, UI_INSERTED, "table_parent_menu_local_redo"),
    ] {
        let first = observe(&cell)?;
        let menu = rig
            .view
            .menuForEvent(&event)
            .ok_or("父背景未提供原活动格的编辑菜单")?;
        let item = menu
            .itemAtIndex(index)
            .ok_or("活动格编辑菜单没有历史动作")?;
        if !item.isEnabled() {
            return Err("右键菜单误用了父文稿历史，禁用了真实草稿历史".into());
        }
        menu.performActionForItemAtIndex(index); // 经父view实际返回的NSMenu交付，不直接调用child或改Doc。
        drop((item, menu));
        frame::wait_complete_frame(stage, &cell, first)?;
        ui_local(&child, expected)?;
        ui_document(rig, UI_SOURCE)?; // 两次菜单动作都只能影响原草稿，不可提前发布或撤销canonical。
    }

    // Find沿生产Body入口结束真实草稿；只有查到新字且显示完整帧才算完成，不能直接提交格帮助测试。
    rig.bar.query.setStringValue(&NSString::from_str("中文"));
    let first = observe(&rig.body)?;
    let pending = match rig
        .action(MarkdownFindAction::Next)
        .map_err(|cause| cause.to_string())?
    {
        NativeMarkdownFindExecution::Complete => None,
        NativeMarkdownFindExecution::Pending(request) => {
            rig.requests.push(request.clone());
            Some(request)
        }
    };
    frame::wait_complete_frame("table_ui_find_committed_cell", &rig.body, first)?;
    ui_document(rig, UI_COMMITTED)?;
    if rig.body.active_cell_body().is_some() {
        return Err("查找完成后仍持有已提交的原单元格".into());
    }
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let expected = NSRange::new(seed, 2);
    if key.request.input.primary_range != expected {
        return Err(format!(
            "查找没有使用原格提交后的真实正文：actual={:?}, expected={expected:?}",
            key.request.input.primary_range
        ));
    }
    rig.view
        .verify_find_target_visible(&key, expected)
        .map_err(|cause| cause.to_string())?;
    if let Some(request) = pending
        && (request.pending() || request.take_failure().is_some())
    {
        return Err("单格查找有画面但原匹配请求尚未成功结束".into());
    }

    // 仅一笔canonical修改；查找选择不制造历史，Undo必须一次恢复原格和整份原始CRLF。
    let first = observe(&rig.body)?;
    rig.view
        .command(sel!(undo:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("table_ui_find_single_undo", &rig.body, first)?;
    ui_document(rig, UI_SOURCE)?;
    rig.remember_receipt()?;
    drop((child, cell)); // 关闭门核同一批旧子图；测试不以强claim延长其输入、字体或映射寿命。
    println!(
        "native_body_table_cell_ui find_reads_actual_cell_commit=true visible_canonical_match=true one_document_undo=true original_disk_unchanged=true physical_ime_tested=false whole_product=false formal=false"
    );
    Ok(())
}
