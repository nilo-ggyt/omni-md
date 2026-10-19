//! 显式 trace 工作负载通过真实产品动作删除 EOF 的一个 scalar，再 Undo 恢复合成原文。

use super::*;

fn counts(surface: &NativeMarkdownEditorSurface) -> [u64; 4] {
    let storage = surface.backend.text_storage.ivars();
    [
        storage.character_process_count.get(),
        storage.input_commit_count.get(),
        storage.input_callback_success_count.get(),
        storage.autosave_enqueue_count.get(),
    ]
}

/// 方向来自产品当前 Known/原 capture；真实单范围也须一致，不能用协议适配值假装 Native 已安装。
fn require_selection(
    surface: &NativeMarkdownEditorSurface,
    expected: MarkdownTextSelection,
) -> Result<(), String> {
    let captured = surface
        .backend
        .text_view
        .primary_text_selection()
        .map_err(|error| error.to_string())?;
    let actual = surface
        .backend
        .text_view
        .native_ordered_selection_state_for_capture(captured.native_capture())
        .map_err(|error| error.to_string())?;
    let range = NSRange::new(
        expected.anchor_utf16.min(expected.head_utf16),
        expected.anchor_utf16.abs_diff(expected.head_utf16),
    );
    if captured.selection != expected
        || !captured.current()
        || surface.backend.text_view.ivars().folds.known_selection() != Some(expected)
        || actual != NativeMarkdownOrderedSelection::Single(range)
    {
        return Err("EOF 测量动作没有真实安装原完整选择".into());
    }
    Ok(())
}

pub(super) fn verify(surface: &NativeMarkdownEditorSurface) -> Result<(), String> {
    let original = surface
        .backend
        .text_view
        .primary_text_selection()
        .map_err(|error| error.to_string())?;
    let saved_selection = original.selection;
    let identity = Rc::downgrade(surface.document());
    let (generation, revision, bytes, length, dirty, could_undo, removed_units, removed_bytes) = {
        let document = surface.document().borrow();
        let length = document.utf16_len();
        let final_unit = document
            .utf16_code_unit_at(length.checked_sub(1).ok_or("EOF 测量夹具为空")?)
            .map_err(|error| error.to_string())?;
        // EOF 恰在 scalar 后，低 surrogate 说明整个最后 scalar 占两单元；其他 scalar 占一单元。
        let units = if (0xdc00..=0xdfff).contains(&final_unit) {
            2
        } else {
            1
        };
        let start = length.checked_sub(units).ok_or("EOF scalar 起点溢出")?;
        let byte_start = document
            .byte_offset_for_utf16(start)
            .map_err(|error| error.to_string())?;
        (
            document.generation(),
            document
                .current_revision()
                .map_err(|error| format!("无法读取原文档revision：{error}"))?,
            document.byte_len(),
            length,
            document.is_dirty(),
            document.can_undo(),
            units,
            document.byte_len() - byte_start,
        )
    };
    if !original.current() {
        return Err("EOF 测量的原选择在动作前已失效".into());
    }
    drop(original); // 后续每个实际产品动作有自己的原票；保存的只是恢复目标，不能续用旧 capture。
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(surface.backend.text_view());
    // SAFETY: 这是 view 已注册的标准产品导航 selector；没有物理按键、新快捷键或裸选择写入。
    unsafe {
        client.doCommandBySelector(sel!(moveToEndOfDocument:));
    }
    require_selection(surface, MarkdownTextSelection::caret(length))?;
    if surface.document().borrow().generation() != generation {
        return Err("EOF 导航错误地修改了正文代次".into());
    }
    let before = counts(surface);
    let reads_before = surface
        .backend
        .text_storage
        .ivars()
        .pending_selection_reads
        .get();
    let empty = NSString::from_str("");
    let start = length - removed_units;
    // SAFETY: 明确范围覆盖最后一个完整 scalar；空 incoming 是实际删除，仍由产品原输入事务准备/发布。
    unsafe {
        client.insertText_replacementRange(&empty, NSRange::new(start, removed_units));
    }
    drop(empty);
    require_selection(surface, MarkdownTextSelection::caret(start))?;
    let deleted = {
        let document = surface.document().borrow();
        document.generation() == generation + 1
            && document.utf16_len() == start
            && document.byte_len() == bytes - removed_bytes
            && document.can_undo()
            && !document.can_redo()
            && !document.has_prepared_text_input()
    };
    let after_delete = counts(surface);
    let reads_after = surface
        .backend
        .text_storage
        .ivars()
        .pending_selection_reads
        .get();
    if !deleted
        || after_delete[0] <= before[0]
        || after_delete[1] != before[1] + 1
        || after_delete[2] != before[2] + 1
        || after_delete[3] != before[3] + 1
        || reads_after <= reads_before
    {
        return Err(format!(
            "EOF 删除没有完成真实事务或 raw 测量：deleted={deleted} counts={before:?}/{after_delete:?} raw_reads={reads_before}/{reads_after}"
        ));
    }
    // SAFETY: 同一真实产品 undo: 路径恢复刚才的一步删除；旧 EOF capture 不会被重新授权。
    unsafe {
        client.doCommandBySelector(sel!(undo:));
    }
    require_selection(surface, MarkdownTextSelection::caret(length))?;
    let after_undo = counts(surface);
    let restored = {
        let document = surface.document().borrow();
        document.generation() == generation + 2
            && document
                .current_revision()
                .is_ok_and(|actual| actual == revision)
            && document.utf16_len() == length
            && document.byte_len() == bytes
            && document.is_dirty() == dirty
            && document.can_undo() == could_undo
            && document.can_redo()
            && !document.has_prepared_text_input()
    };
    if !restored
        || !WeakMarkdownDocument::ptr_eq(&identity, &Rc::downgrade(surface.document()))
        || after_undo[0] <= after_delete[0]
        || after_undo[1] != after_delete[1]
        || after_undo[2] != after_delete[2]
        || after_undo[3] != after_delete[3] + 1
    {
        return Err(format!(
            "EOF Undo 未恢复原合成正文：restored={restored} counts={after_delete:?}/{after_undo:?}"
        ));
    }
    // 已经证明原 revision 恢复，下面是新的合法产品选择意图；只复用保存的目标值，不复活旧收据。
    surface
        .backend
        .text_view
        .set_semantic_native_selection(saved_selection);
    require_selection(surface, saved_selection)?;
    if surface.document().borrow().generation() != generation + 2
        || surface
            .backend
            .text_storage
            .ivars()
            .pending_selection
            .borrow()
            .is_some()
        || surface
            .backend
            .text_storage
            .ivars()
            .input_transaction
            .borrow()
            .is_some()
    {
        return Err("EOF 测量结束后仍有 pending owner 或额外字符提交".into());
    }
    eprintln!(
        "omni_pending_eof restored=true old_before={length} new_eof={start} removed_units={removed_units} raw_reads={} generation_steps=2 redo_branch=true counts_before={before:?} counts_delete={after_delete:?} counts_undo={after_undo:?}",
        reads_after - reads_before
    );
    Ok(()) // 真实 redo 分支如实保留；后续合成工作负载的新编辑沿正常 Core 路径清除它。
}
