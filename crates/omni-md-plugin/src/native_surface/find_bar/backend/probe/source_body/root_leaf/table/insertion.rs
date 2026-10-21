//! Verify the production insert-table entry's single commit, preview first-cell
//! focus, and literal Source behavior through the original Rig and strict close sequence.

use super::*;

const SOURCE: &str = "after\n";
const INSERTED: &str = concat!(
    "| 列 1 | 列 2 | 列 3 |\n",
    "| :--- | :---: | ---: |\n",
    "|  |  |  |\n",
    "|  |  |  |\n\n",
    "after\n",
);

/// Read the fixed short canonical fixture in chunks. Expected template/surrounding
/// text are independent literals, not output from the template function under test.
fn document(rig: &Rig, expected: &str) -> Result<(), String> {
    let original = rig.document.upgrade().ok_or("插表原 Doc 已归还")?;
    let document = original.borrow();
    if document.utf16_len() != expected.encode_utf16().count() {
        return Err("插表或撤销没有保持固定模板与邻文长度".into());
    }
    let mut units = [0_u16; 128];
    let mut expected = expected.encode_utf16();
    for offset in (0..document.utf16_len()).step_by(units.len()) {
        let used = units.len().min(document.utf16_len() - offset);
        document
            .copy_utf16_code_units(offset, &mut units[..used])
            .map_err(|cause| cause.to_string())?;
        if units[..used]
            .iter()
            .any(|unit| expected.next() != Some(*unit))
        {
            return Err("插表/局部输入/撤销改写了独立预期之外的正文".into());
        }
    }
    drop(document);
    drop(original);
    // No save occurred. Template transactions/local input affect document branches
    // only; the actual file must retain its original short contents.
    if std::fs::read(rig._file.path()).map_err(|cause| cause.to_string())? != SOURCE.as_bytes() {
        return Err("插表或局部格输入自动改写了原磁盘".into());
    }
    Ok(())
}

/// Establish the first cell through actual system focus and Native input protocol,
/// without creating a session or mutating source tickets/active slots from the test.
fn preview_cell(rig: &mut Rig, started: Instant) -> Result<(), String> {
    let child = ui_active(rig, None, started, "table_insert_actual_first_cell")?;
    let view = child.view().map_err(|cause| cause.to_string())?;
    let state = view.input_state().map_err(|cause| cause.to_string())?;
    if state.primary_range != NSRange::new(0, 3) || state.generation != 0 {
        return Err("插表没有进入原首格并全选列 1，或激活误改局部文字".into());
    }
    ui_local(&view, "列 1")?;
    document(rig, INSERTED)?;
    let frame = observe(&child)?;
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
    // SAFETY: The actual client inserts fixed Chinese at its selected range.
    // NSNotFound must consume actual selection; explicit 0..3 would conceal an
    // incorrect first-cell selection. This verifies no physical IME candidate UI
    // and changes no system input method.
    unsafe {
        client.insertText_replacementRange(
            &NSString::from_str("首格"),
            NSRange::new(objc2_foundation::NSNotFound as usize, 0),
        );
    }
    frame::wait_complete_frame("table_insert_first_cell_native_text", &child, frame)?;
    ui_local(&view, "首格")?;
    document(rig, INSERTED)?; // The first cell's local draft cannot become a second document-history entry early.
    if rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .generation
        != 1
    {
        return Err("首格本地输入提前提交了 canonical".into());
    }
    let frame = observe(&child)?;
    view.command(sel!(undo:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("table_insert_first_cell_local_undo", &child, frame)?;
    ui_local(&view, "列 1")?;
    document(rig, INSERTED)?;
    let frame = observe(&rig.body)?;
    // The existing Escape command cancels the sole draft without quitting the
    // app, manually dismantling its graph, or inventing an empty canonical commit.
    view.command(sel!(cancelOperation:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("table_insert_first_cell_original_escape", &rig.body, frame)?;
    if rig.body.active_cell_body().is_some() {
        return Err("插表首格原取消动作没有归还唯一 owner".into());
    }
    drop(view);
    drop(child); // Return temporary strong claims before product close; the Rig already retains weak child-resource observations.
    Ok(())
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    for mode in [
        MarkdownPresentationMode::InlinePreview,
        MarkdownPresentationMode::Source,
    ] {
        let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
            let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
            file.write_all(SOURCE.as_bytes())
                .map_err(|cause| cause.to_string())?;
            file.flush().map_err(|cause| cause.to_string())?;
            let mut rig = Rig::attach_mode(
                mtm,
                file,
                SOURCE.encode_utf16().count(),
                2,
                false,
                mode,
                true,
            )?;
            let result = (|| -> Result<(), String> {
                rig.panel.makeKeyWindow();
                let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
                if before.selection != MarkdownTextSelection::caret(0) {
                    return Err("固定插表夹具原选择并非真实 BOF".into());
                }
                let frame = observe(&rig.body)?;
                let started = Instant::now();
                rig.view
                    .insert_default_table()
                    .map_err(|cause| cause.to_string())?;
                // The same insert-button entry publishes one transaction. Preview
                // waits for the actual first cell/current frame; Source waits for
                // its literal frame. Both keep the original action-start deadline;
                // a first parent-location frame cannot certify preview editing readiness.
                if mode == MarkdownPresentationMode::InlinePreview {
                    preview_cell(&mut rig, started)?;
                } else {
                    frame::wait_complete_frame("table_insert_source_literal", &rig.body, frame)?;
                    if rig.body.active_cell_body().is_some()
                        || rig
                            .view
                            .input_state()
                            .map_err(|cause| cause.to_string())?
                            .selection
                            != MarkdownTextSelection::caret(2)
                    {
                        return Err("源码插表被自动切到 Cell，或丢失同笔首格 caret".into());
                    }
                }
                document(&rig, INSERTED)?;
                let original = rig.document.upgrade().ok_or("插表原 Doc 已归还")?;
                let canonical = original.borrow();
                if canonical.generation() != 1 || !canonical.can_undo() || canonical.can_redo() {
                    return Err("插表不再是一笔 canonical 事务，或局部历史污染正文".into());
                }
                drop(canonical);
                drop(original);
                let frame = observe(&rig.body)?;
                rig.view
                    .command(sel!(undo:))
                    .map_err(|cause| cause.to_string())?;
                frame::wait_complete_frame("table_insert_one_document_undo", &rig.body, frame)?;
                document(&rig, SOURCE)?;
                let original = rig.document.upgrade().ok_or("插表 Undo 原 Doc 已归还")?;
                let canonical = original.borrow();
                if canonical.can_undo()
                    || !canonical.can_redo()
                    || canonical.selection() != before.selection
                {
                    return Err("一笔正文 Undo 没有同时恢复原字节、方向与历史边界".into());
                }
                drop(canonical);
                drop(original);
                rig.remember_receipt()
            })();
            // Failure still returns actual resources before another mode/window
            // starts; process exit cannot substitute for this fixture's cleanup.
            let retirement = release::close(rig)?;
            Ok((retirement, result))
        })?;
        retirement.verify()?;
        result?;
    }
    println!(
        "native_body_table_insert actual_preview_first_cell_focus=true source_literal_unchanged=true same_single_document_commit=true local_native_input_and_undo=true one_document_undo=true original_disk_unchanged=true strict_original_release=true whole_product=false formal=false"
    );
    Ok(())
}
