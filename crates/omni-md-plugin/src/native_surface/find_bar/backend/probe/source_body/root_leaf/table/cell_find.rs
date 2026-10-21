//! Find must consume the actual cell commit before revealing a match in the same
//! document. Independently verify original disk contents and one-entry history.

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
    // The actual client replaces only this fixed short draft, without writing
    // canonical text or claiming physical IME candidate confirmation.
    // SAFETY: The main-thread client/string are alive and the original cell has four UTF-16 units.
    unsafe {
        client.insertText_replacementRange(&NSString::from_str(UI_INSERTED), NSRange::new(0, 4));
    }
    frame::wait_complete_frame("table_ui_find_local_edit", &cell, first)?;
    ui_local(&child, UI_INSERTED)?;
    ui_document(rig, UI_SOURCE)?;

    // The parent background's context menu must route history to the actual active
    // cell rather than the parent document's still-uncommitted character history.
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
        menu.performActionForItemAtIndex(index); // Deliver through the parent view's actual NSMenu, not direct child/Doc mutation.
        drop((item, menu));
        frame::wait_complete_frame(stage, &cell, first)?;
        ui_local(&child, expected)?;
        ui_document(rig, UI_SOURCE)?; // Both menu actions affect only the original draft, without early canonical commit/undo.
    }

    // Production Body Find ends the actual draft. Require the new characters and
    // a complete visible frame, without directly committing the cell to assist the test.
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

    // Find selection adds no character history. One canonical Undo must restore
    // the original cell and every original CRLF byte together.
    let first = observe(&rig.body)?;
    rig.view
        .command(sel!(undo:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("table_ui_find_single_undo", &rig.body, first)?;
    ui_document(rig, UI_SOURCE)?;
    rig.remember_receipt()?;
    drop((child, cell)); // Strict close observes these same retired child graphs without local claims extending Input/font/mmap lifetime.
    println!(
        "native_body_table_cell_ui find_reads_actual_cell_commit=true visible_canonical_match=true one_document_undo=true original_disk_unchanged=true physical_ime_tested=false whole_product=false formal=false"
    );
    Ok(())
}
