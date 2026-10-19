//! 显式诊断只读取四个已有视图的公开标量，不把几何面积换算成实际图形内存。

use super::*;

/// 原生 getter 的内部成本没有零分配保证；诊断结果不能替代未插桩的内存验收。
pub(super) fn trace_memory_view_geometry(
    editor: &NativeMarkdownEditorSurface,
    current: &impl Fn() -> bool,
) {
    // 三个项目已拥有的视图只借用本同步栈，不能为诊断建立额外持久 owner。
    let roles: [(&str, &NSView); 3] = [
        ("document", &editor.backend.text_view),
        ("scroll", &editor.backend.scroll_view),
        ("gutter", &editor.backend.gutter),
    ];
    for (role, view) in roles {
        if !observe_view(role, view, current) {
            return; // 原票失效立即停止，不借后继输入继续观察其他角色。
        }
    }
    let clip =
        native_markdown_table_intent_step(current, || editor.backend.scroll_view.contentView());
    match clip {
        Ok(clip) => {
            let clip_observed = observe_view("clip", &clip, current);
            drop(clip); // 临时 Native claim 析构可能重入，结束后再报告该批是否仍授权。
            println!(
                "native_memory_views completed={} scope=fixed_roles",
                clip_observed && current()
            );
        }
        Err(_) => {
            // 没有同票取得 clip 时不再查询其他对象，批次保持未完成。
            println!("native_memory_views completed=false scope=fixed_roles");
        }
    }
}

/// 每个 getter 前后都消费同一原票；不调用 layout、display、viewport range 或图像读取。
fn observe_view(role: &str, view: &NSView, current: &impl Fn() -> bool) -> bool {
    let observed = (|| -> Result<_, NativeMarkdownStableTableFailure> {
        let frame = native_markdown_table_intent_step(current, || view.frame())?;
        let bounds = native_markdown_table_intent_step(current, || view.bounds())?;
        let visible = native_markdown_table_intent_step(current, || view.visibleRect())?;
        let explicit_layer = native_markdown_table_intent_step(current, || view.wantsLayer())?;
        let update_layer = native_markdown_table_intent_step(current, || view.wantsUpdateLayer())?;
        // 公开策略只作为原视图标量观察；不能由尺寸或 wantsLayer 猜测系统是否整面重绘。
        let redraw_policy =
            native_markdown_table_intent_step(current, || view.layerContentsRedrawPolicy())?;
        let merge_children =
            native_markdown_table_intent_step(current, || view.canDrawSubviewsIntoLayer())?;
        let opaque = native_markdown_table_intent_step(current, || view.isOpaque())?;
        let clips = native_markdown_table_intent_step(current, || view.clipsToBounds())?;
        // 隐藏状态也必须在原票下读取；仅有 frame 不证明这一角色当前实际参与显示。
        let hidden = native_markdown_table_intent_step(current, || view.isHidden())?;
        Ok((
            frame,
            bounds,
            visible,
            explicit_layer,
            update_layer,
            redraw_policy,
            merge_children,
            opaque,
            clips,
            hidden,
        ))
    })();
    match observed {
        Ok((
            frame,
            bounds,
            visible,
            explicit_layer,
            update_layer,
            redraw_policy,
            merge_children,
            opaque,
            clips,
            hidden,
        )) => {
            // wantsLayer=false 仍可能继承祖先 backing；此字段不能解释为没有 layer。
            println!(
                "native_memory_view role={role} frame={frame:?} bounds={bounds:?} visible={visible:?} wants_layer={explicit_layer} wants_update_layer={update_layer} redraw_policy={} merge_children={merge_children} opaque={opaque} clips_to_bounds={clips} hidden={hidden}",
                redraw_policy.0
            );
            current()
        }
        Err(_) => {
            // 任一步撤权都舍弃该角色的部分读数，不把跨输入的矩形拼成有效记录。
            println!("native_memory_view role={role} stale=true");
            false
        }
    }
}
