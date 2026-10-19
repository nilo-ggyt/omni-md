//! 实际原生属性窗口换页后，单格与 delimiter 仍只读原不可变发布；不假设 Native 自行重入。

use super::*;

pub(super) fn verify(
    surface: &NativeMarkdownEditorSurface,
    controller: &NativeMarkdownController,
    fixture: &str,
) -> Result<(), String> {
    let storage = &surface.backend.text_storage;
    let view = &surface.backend.text_view;
    let first_marker = markdown_probe_utf16_offset(
        fixture,
        fixture
            .find("**用户鉴权中心**")
            .ok_or("快照回归缺少原粗体单格")?,
    )?;
    let other_window = markdown_probe_utf16_offset(
        fixture,
        fixture
            .rfind("| 阶段 | 交付核心物料 |")
            .ok_or("快照回归缺少远端表")?,
    )?;
    let snapshot = storage
        .table_presentation_snapshot(first_marker..first_marker + 1)
        .ok_or("快照回归无法取得原表格目录")?;
    let epochs = [
        controller.find_input_intent(),
        controller.workspace_query_intent(),
        controller.ivars().header_layout_revision.get(),
        view.ivars().table_input_intent_epoch.get(),
    ];
    let revision = surface
        .document()
        .borrow()
        .current_revision()
        .map_err(|error| format!("无法捕获原文档revision：{error}"))?;
    let fold_revision = view.ivars().folds.revision();
    let current = || {
        snapshot.is_current(storage)
            && native_markdown_controller_has_foreground_authority(controller)
            && !view.ivars().textkit_graph_teardown.get()
            && controller.find_input_intent() == epochs[0]
            && controller.workspace_query_intent() == epochs[1]
            && controller.ivars().header_layout_revision.get() == epochs[2]
            && view.ivars().table_input_intent_epoch.get() == epochs[3]
            && view.ivars().folds.revision() == fold_revision
            && surface
                .document()
                .borrow()
                .current_revision()
                .is_ok_and(|actual| actual == revision)
    };
    let (table_index, cell_index) = snapshot
        .tables()
        .iter()
        .enumerate()
        .find_map(|(index, table)| {
            // 用原目录给出 O(1) cell 身份；不以裸范围或当前后继缓存建立样式权限。
            table
                .cells
                .iter()
                .position(|cell| cell.utf16_range.contains(&first_marker))
                .map(|cell| (index, cell))
        })
        .ok_or("快照回归的粗体源不在原表 cell 中")?;
    let delimiter = snapshot.tables()[table_index].delimiter_utf16_range.clone();
    let before = storage
        .table_cell_display_text(&snapshot, table_index, cell_index)
        .ok_or("快照回归无法转换原单格")?;
    if before.text.contains("**")
        || !before.text.contains("用户鉴权中心")
        || !before
            .style_spans
            .iter()
            .any(|span| span.styles.contains(MarkdownPresentationStyles::STRONG))
    {
        return Err("快照回归缺少实际粗体、隐藏标记或预期显示内容".into());
    }
    let before_native = native_markdown_table_intent_step(&current, || {
        storage.table_cell_attributed_text(&snapshot, table_index, cell_index)
    })
    .map_err(|_| "原单格 Native 构造已撤权")?
    .ok_or("原单格 Native 属性构造失败")?
    .0;

    // 显式调用生产属性 accessor，确定把缓存换到远端；不把潜在 AppKit 重入说成已经观测到。
    native_markdown_table_intent_step(&current, || {
        let mut effective = NSRange::new(0, 0);
        // SAFETY: 远端位置来自同一合成文档的完整 scalar，栈范围在同步调用内可写。
        let attributes = unsafe {
            NSAttributedString::attributesAtIndex_effectiveRange(
                surface.backend.text_storage(),
                other_window,
                &mut effective,
            )
        };
        drop(attributes);
    })
    .map_err(|_| "远端属性换页已撤权")?;
    if storage
        .ivars()
        .presentation
        .borrow()
        .query(first_marker)
        .is_some()
    {
        return Err("快照回归没有实际形成互不覆盖的 A/B 属性窗口".into());
    }

    // 当前窗口已是 B；A 的 cell 样式、隐藏标记和 Native 值仍须完全相同。
    let after = storage
        .table_cell_display_text(&snapshot, table_index, cell_index)
        .ok_or("缓存换页使原单格丢失其真实语义")?;
    if after != before {
        return Err("缓存换页改变了原单格文本或样式".into());
    }
    let after_native = native_markdown_table_intent_step(&current, || {
        storage.table_cell_attributed_text(&snapshot, table_index, cell_index)
    })
    .map_err(|_| "换页后原单格 Native 构造已撤权")?
    .ok_or("换页后原单格 Native 构造失败")?
    .0;
    let same_native = native_markdown_table_intent_step(&current, || {
        before_native.isEqualToAttributedString(&after_native)
    })
    .map_err(|_| "原生单格比较已撤权")?;
    let hidden = snapshot.semantics.as_ref().is_some_and(|semantics| {
        native_markdown_table_delimiter_semantics_are_hidden(
            semantics,
            snapshot.document_generation,
            &delimiter,
        )
    });
    if !same_native || !hidden || !current() {
        return Err("原快照的 Native 单格或完整 delimiter 认证在换页后失效".into());
    }
    drop((before_native, after_native)); // 强 Native 临时值在原权限检查之前结束，不跨关闭屏障保活。
    if !current() {
        return Err("快照回归在 Native 临时值析构后失去原权限".into());
    }
    println!(
        "omni_semantic_snapshot verified=true cache_replaced=true cell_styles_equal=true delimiter_hidden=true source_unchanged=true"
    );
    Ok(())
}
