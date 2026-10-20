//! 正式插表入口同一提交、预览首格焦点与源码字面行为；沿原 Rig 和原严格关闭核验。

use super::*;

const SOURCE: &str = "after\n";
const INSERTED: &str = concat!(
    "| 列 1 | 列 2 | 列 3 |\n",
    "| :--- | :---: | ---: |\n",
    "|  |  |  |\n",
    "|  |  |  |\n\n",
    "after\n",
);

/// 固定短例逐片读 canonical；原模板、邻文与历史由用户结果证明，不从待测模板函数生成期望。
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
    // 未执行保存，真实模板事务和局部输入都只能改变文档分支；磁盘仍应是最初 after。
    if std::fs::read(rig._file.path()).map_err(|cause| cause.to_string())? != SOURCE.as_bytes() {
        return Err("插表或局部格输入自动改写了原磁盘".into());
    }
    Ok(())
}

/// 首格只靠原系统焦点和 Native 输入协议证明；测试不创建会话、不修改来源票或活动槽。
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
    // SAFETY: 正式客户在当前全选范围键入固定中文；NSNotFound 沿真实选择替换，不能用显式
    // 0..3 帮产品绕开错误的首格选区。这不是物理输入法候选验收，也不切换系统输入法。
    unsafe {
        client.insertText_replacementRange(
            &NSString::from_str("首格"),
            NSRange::new(objc2_foundation::NSNotFound as usize, 0),
        );
    }
    frame::wait_complete_frame("table_insert_first_cell_native_text", &child, frame)?;
    ui_local(&view, "首格")?;
    document(rig, INSERTED)?; // 首格本地草稿不能提前变成第二笔正文历史。
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
    // 原 Esc 取消唯一格草稿；不按应用退出、不手动拆图，也不制造一次空 canonical 提交。
    view.command(sel!(cancelOperation:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("table_insert_first_cell_original_escape", &rig.body, frame)?;
    if rig.body.active_cell_body().is_some() {
        return Err("插表首格原取消动作没有归还唯一 owner".into());
    }
    drop(view);
    drop(child); // 真实临时强引用在整产品关闭之前归还；原 Rig 已持子图弱资源观察。
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
                // 同一插表按钮入口只发布一笔原事务。Preview 等实际首格/当前帧，Source 等原字面帧；
                // 两者都从动作开始计原三秒，不能用首个父定位帧冒充预览编辑已经就绪。
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
            // 失败同样先归还实际资源；不让后一个模式、后一个窗口或进程结束替本例清理。
            let retirement = release::close(rig, false)?;
            Ok((retirement, result))
        })?;
        retirement.verify(false)?;
        result?;
    }
    println!(
        "native_body_table_insert actual_preview_first_cell_focus=true source_literal_unchanged=true same_single_document_commit=true local_native_input_and_undo=true one_document_undo=true original_disk_unchanged=true strict_original_release=true whole_product=false formal=false"
    );
    Ok(())
}
