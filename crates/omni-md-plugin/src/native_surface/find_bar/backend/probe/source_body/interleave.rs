//! Verify cancellation through ordinary sequential input, hide, and Find actions
//! before the original queued request binds its Source. No production callback injects work.

use super::*;

/// Change the query through its actual session field editor. A synthetic
/// notification without that editor cannot establish committed user input.
fn query_changed(
    bar: &NativeMarkdownFindBar,
    controller: &NativeMarkdownController,
) -> Result<(), String> {
    let intent = controller.find_input_intent();
    // SAFETY: This synchronous control retains the original bar and field. AppKit
    // performs normal focus without a replacement delegate or input context.
    unsafe { bar.query.selectText(None) };
    let editor = bar
        .query
        .currentEditor()
        .ok_or("原查询没有实际字段编辑器")?
        .downcast::<objc2_app_kit::NSTextView>()
        .map_err(|_| "原查询输入不是NSTextView")?;
    let session = editor
        .downcast_ref::<crate::native_surface::NativeMarkdownSessionFieldEditor>()
        .ok_or("原查询没有使用会话字段编辑器")?;
    if !session.belongs_to(&controller.ivars().undo, &controller.ivars().owned)
        || editor.hasMarkedText()
        || controller.find_input_intent() != intent
    {
        return Err("真实查询输入前原会话或意图已经变化".into());
    }
    let length = bar.query.stringValue().length();
    if length > 32 {
        // Only the existing short synthetic query is authorized, never document
        // contents or an arbitrarily large field value.
        return Err("本回归的固定短查询已变化".into());
    }
    let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
    // SAFETY: The actual editor and fixed NSString live through this protocol
    // call; the explicit range replaces only its original bounded query.
    unsafe {
        input.insertText_replacementRange(ns_string!("新查询"), NSRange::new(0, length));
    }
    if controller.find_input_intent() != intent.checked_add(1).ok_or("查询意图溢出")?
        || !bar
            .query
            .stringValue()
            .isEqualToString(ns_string!("新查询"))
        || editor.hasMarkedText()
    {
        return Err("真实查询输入没有产生一次准确的原意图变化".into());
    }
    Ok(())
}

/// Issue actual input/hide before returning to the queue, then require the old
/// request to end cancelled. Finally prove that a new explicit Find preserves its
/// successor request rather than borrowing that successor's Visible endpoint.
pub(super) fn before_binding(rig: &mut Rig) -> Result<(), String> {
    for hide in [false, true] {
        rig.bar.query.setStringValue(ns_string!("目标"));
        let frame = observe(&rig.body)?;
        let request = cases::pending(rig, MarkdownFindAction::Next)?;
        if rig.body.probe_bound_find_source().is_some() {
            return Err("begin尚未返回主队列就已偷捕Source或提前绑定".into());
        }
        if hide {
            rig.bar.hide(&rig.scroll, &rig.view, &rig.controller)?;
        } else {
            query_changed(&rig.bar, &rig.controller)?;
        }
        wait_frame("cancel_before_binding", &rig.body, frame)?;
        if !request.cancelled()
            || request.pending()
            || request.completed_visible()
            || request.take_failure().is_some()
        {
            return Err("未绑定Find没有准确取消为原终态".into());
        }
        if hide {
            // Restore the actual bar through its normal show transaction before
            // the next case; wait for the resulting real frame, not a queue turn.
            let shown = observe(&rig.body)?;
            rig.bar.show(&rig.scroll, true, &rig.controller)?;
            rig.panel.makeFirstResponder(Some(&**rig.view));
            wait_frame("show_after_cancel", &rig.body, shown)?;
        }
    }
    rig.bar.query.setStringValue(ns_string!("目标"));
    let frame = observe(&rig.body)?;
    let old = cases::pending(rig, MarkdownFindAction::Next)?;
    rig.bar.query.setStringValue(ns_string!("局部"));
    let current = cases::pending(rig, MarkdownFindAction::Next)?;
    wait_frame("new_action_keeps_successor", &rig.body, frame)?;
    if !old.cancelled()
        || old.completed_visible()
        || !current.completed_visible()
        || current.cancelled()
    {
        return Err("旧Find取消夺走了后继动作或借后继Visible补签".into());
    }
    Ok(())
}
