//! 实际菜单返回后立刻用公开全选撤销原队列，只观察原取消终点且不推进产品 executor。

use super::*;

/// 原票必须保持失效；恢复动作只保留预先声明的选择和输入代次，不补签旧菜单权限。
pub(super) struct CancelledMenuSelection {
    original: NativeMarkdownSelectionCapture,
    input_epoch: u64,
}

/// 只借原 Weak 正文核对有限状态，不读取或复制整篇内容。
fn body_is_unchanged(
    state: &NativeMarkdownToolbarProbe,
    view: &PieceBackedMarkdownTextView,
) -> bool {
    if view.ivars().textkit_graph_teardown.get() || !view.ivars().folds.owner_is_current() {
        return false; // 关闭或已失去原 owner 的 view 不能再声明选择或恢复权限。
    }
    state.document.upgrade().is_some_and(|document| {
        if !view.ivars().folds.matches_document(&document) {
            return false; // 同长度或同内容的替换文档不能承担原菜单负例。
        }
        let document = document.borrow();
        document.generation() == state.generation
            && document
                .current_revision()
                .is_ok_and(|actual| actual == state.revision)
            && document.byte_len() == state.bytes
            && document.utf16_len() == state.units
            && document.is_dirty() == state.dirty
            && document.can_undo() == state.undo
            && document.can_redo() == state.redo
            && !document.has_prepared_text_input()
    })
}

/// 只接纳本次公开选择动作预先声明的完整范围与输入代次，不拼接来自不同阶段的条件。
fn selection_is(
    state: &NativeMarkdownToolbarProbe,
    view: &PieceBackedMarkdownTextView,
    selection: MarkdownTextSelection,
    input: u64,
) -> bool {
    body_is_unchanged(state, view)
        && view.ivars().table_input_intent_epoch.get() == input
        && state
            .document
            .upgrade()
            .is_some_and(|document| document.borrow().selection() == selection)
}

/// 同一明确目标的真实 nullable 范围、count、raw 与 Known 必须同时一致。
fn require_native_selection(
    state: &NativeMarkdownToolbarProbe,
    view: &PieceBackedMarkdownTextView,
    selection: MarkdownTextSelection,
    input: u64,
) -> Result<(), String> {
    let current = || selection_is(state, view, selection, input);
    let actual = native_markdown_table_intent_step(&current, || {
        autoreleasepool(|_| {
            let ranges: Option<Retained<NSArray<objc2_foundation::NSValue>>> =
                native_markdown_table_intent_step(&current, || {
                    // SAFETY: 同一主线程活正文的公开 superclass getter；所有 claim 在本池内结束。
                    unsafe { msg_send![super(view), selectedRanges] }
                })
                .map_err(|_| "取消负例的实际选择权限已改变")?;
            let count = match &ranges {
                Some(ranges) => native_markdown_table_intent_step(&current, || ranges.count())
                    .map_err(|_| "取消负例的范围数量权限已改变")?,
                None => 0, // nil 不能冒充单一全选或恢复成功。
            };
            let raw = native_markdown_table_intent_step(&current, || view.super_selected_range())
                .map_err(|_| "取消负例的原生范围权限已改变")?;
            let present = ranges.is_some();
            drop(ranges);
            Ok::<_, String>((present, count, raw))
        })
    })
    .map_err(|_| "取消负例的选择或池清理已失权")??;
    let expected = NSRange::new(
        selection.anchor_utf16.min(selection.head_utf16),
        selection.anchor_utf16.abs_diff(selection.head_utf16),
    );
    if actual != (true, 1, expected) || view.ivars().folds.known_selection() != Some(selection) {
        return Err(format!(
            "取消负例未保持明确新选择：actual={actual:?} expected={expected:?}"
        ));
    }
    Ok(())
}

/// 返回原 menu action 后同步发送 selectAll；此处必须仍在同一 handler，不能先让队列消费。
pub(in crate::native_surface) fn supersede_menu_with_select_all(
    view: &PieceBackedMarkdownTextView,
    state: &mut NativeMarkdownToolbarProbe,
    original: NativeMarkdownSelectionCapture,
) -> Result<(), String> {
    if state.cancelled_menu.is_some()
        || !original.current()
        || !body_is_unchanged(state, view)
        || view.ivars().cell_editor.borrow().is_some()
    {
        return Err("取消负例缺少原菜单前态或已存在实际字段".into());
    }
    let before = view.ivars().table_input_intent_epoch.get();
    let after = before.checked_add(1).ok_or("取消负例输入代次溢出")?;
    let all = MarkdownTextSelection {
        anchor_utf16: 0,
        head_utf16: state.units,
    };
    let current = || {
        // 标准全选先推进输入、再发布完整范围；只允许这次调用预声明的三个有限阶段。
        selection_is(state, view, MarkdownTextSelection::caret(0), before)
            || selection_is(state, view, MarkdownTextSelection::caret(0), after)
            || selection_is(state, view, all, after)
    };
    native_markdown_table_intent_step(&current, || {
        // SAFETY: 固定公开选择 selector 在原主线程正文同步执行，不派生第二个输入请求。
        unsafe {
            let _: () = msg_send![view, selectAll: Option::<&AnyObject>::None];
        }
    })
    .map_err(|_| "菜单返回后的公开全选未保持原正文")?;
    require_native_selection(state, view, all, after)?;
    if original.current() {
        return Err("新的明确全选没有撤销原菜单选择票".into());
    }
    state.cancelled_menu = Some(CancelledMenuSelection {
        original,
        input_epoch: after,
    });
    Ok(())
}

/// 只消费原 once 取消终点；新全选的 Known 不得被旧队列清除，恢复也只能是新的公开动作。
pub(in crate::native_surface) fn finish_cancelled_menu(
    surface: &NativeMarkdownEditorSurface,
    state: &NativeMarkdownToolbarProbe,
) -> Result<(), String> {
    if state.finished.replace(true) {
        return Err("取消负例的原终点已消费".into());
    }
    let cancelled = state
        .cancelled_menu
        .as_ref()
        .ok_or("取消负例缺少原全选结果")?;
    let observation = state.activation.as_ref().ok_or("取消负例缺少原菜单观察")?;
    let view = &*surface.backend.text_view;
    let all = MarkdownTextSelection {
        anchor_utf16: 0,
        head_utf16: state.units,
    };
    if observation.outcome() != Some(NativeMarkdownTableCellOutcome::Cancelled { committed: false })
        || cancelled.original.current()
        || !body_is_unchanged(state, view)
        || counts(surface) != state.counts
        || !view.ivars().pending_table_cell_activation.is_empty()
        || view.ivars().active_table_cell.borrow().is_some()
        || view.ivars().cell_editor.borrow().is_some()
    {
        return Err(format!(
            "旧菜单没有无提交取消：outcome={:?} counts={:?}/{:?}",
            observation.outcome(),
            state.counts,
            counts(surface)
        ));
    }
    require_native_selection(state, view, all, cancelled.input_epoch)?;
    let removed = {
        let mut slot = view.ivars().table_insertion_observation.borrow_mut();
        if !slot
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, observation))
        {
            return Err("取消负例不能清除非原分配的观察槽".into());
        }
        slot.take() // 只清本次已完成的 probe 元数据，不取消任何产品请求或后继观察。
    };
    drop(removed);
    let mut input_epoch = cancelled.input_epoch;
    for (selector, expected) in [
        (sel!(selectAll:), all),
        (
            sel!(moveToBeginningOfDocument:),
            MarkdownTextSelection::caret(0),
        ),
    ] {
        // 每一步都是新的公开明确意图；不恢复旧 capture，也不通过 Undo 改写历史。
        input_epoch = input_epoch.checked_add(1).ok_or("取消恢复的输入代次溢出")?;
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(view.as_super());
        // SAFETY: 固定标准选择 selector 同步作用于原活正文；所有资源仍由 surface 唯一持有。
        unsafe {
            input.doCommandBySelector(selector);
        }
        require_native_selection(state, view, expected, input_epoch)?;
        if cancelled.original.current() {
            return Err("新的恢复意图复活了旧菜单票".into());
        }
    }
    if counts(surface) != state.counts
        || !view.ivars().pending_table_cell_activation.is_empty()
        || view.ivars().active_table_cell.borrow().is_some()
        || view.ivars().cell_editor.borrow().is_some()
    {
        return Err("公开选择恢复意外提交正文、autosave 或创建表格请求/字段".into());
    }
    eprintln!(
        "omni_toolbar_menu_cancelled source_unchanged=true generation_steps=0 history_steps=0 autosave_delta=0 field=false pending=false restored_caret=0"
    );
    Ok(())
}
