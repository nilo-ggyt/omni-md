//! 原按钮的窗口命中与完整鼠标事件对；用例身份、前后态和异步终点仍归各自流程。

use super::*;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSWindow};

thread_local! {
    /// 只为本主线程构造的实际鼠标对编号；不保存事件或客户，耗尽时拒绝而不回绕复用。
    static EVENT_NUMBER: Cell<isize> = const { Cell::new(0) };
}

/// 只为已经可见的原按钮准备窗口坐标。调用方持自己的Doc/Input/意图守卫，
/// 本函数不显露控件、移动焦点或直接调用action，也不替原用例采集新的输入前态。
pub(super) fn prepare(
    button: &NSButton,
    window: &NSWindow,
    current: &impl Fn() -> bool,
) -> Result<[Retained<NSEvent>; 2], String> {
    MainThreadMarker::new().ok_or("原按钮鼠标事件必须在 AppKit 主线程构造")?;
    if !step(current, || button.isEnabled())?
        || step(current, || button.isHiddenOrHasHiddenAncestor())?
        || !step(current, || button.window())?.is_some_and(|actual| std::ptr::eq(&*actual, window))
    {
        return Err("原鼠标按钮不可用、已隐藏或脱离指定窗口".into());
    }
    let bounds = step(current, || button.bounds())?;
    let point = NSPoint::new(
        bounds.origin.x + bounds.size.width * 0.5,
        bounds.origin.y + bounds.size.height * 0.5,
    );
    if !step(current, || NSPointInRect(point, button.visibleRect()))? {
        return Err("原鼠标按钮中心没有实际可见区域".into());
    }
    // NSEvent使用窗口坐标，hitTest按公开合同接收contentView的父坐标，不能混用bounds。
    let point = step(current, || button.convertPoint_toView(point, None))?;
    let content = step(current, || window.contentView())?.ok_or("原鼠标窗口没有内容根")?;
    // SAFETY: 原窗口持有内容根，查询仅借当前主线程挂载关系；返回后仍核原用例权限。
    let parent = step(current, || unsafe { content.superview() })?
        .ok_or("原鼠标窗口内容根没有实际父视图")?;
    let local = step(current, || parent.convertPoint_fromView(point, None))?;
    let hit = step(current, || content.hitTest(local))?.ok_or("原按钮中心未命中窗口内容")?;
    if !std::ptr::eq::<NSView>(&*hit, button.as_super().as_super())
        && !step(current, || hit.isDescendantOf(button))?
    {
        return Err("原按钮中心被其他视图遮挡".into());
    }
    drop((hit, parent, content)); // Native临时claim在最后原票核验前归还，不进入事件或后继队列。
    if !current() {
        return Err("原鼠标按钮的文稿或完整Input已变化".into());
    }
    let number = step(current, || window.windowNumber())?;
    let event_number = EVENT_NUMBER.with(|number| {
        let next = number.get().checked_add(1).ok_or("原鼠标事件编号已耗尽")?;
        number.set(next);
        Ok::<_, String>(next)
    })?;
    let timestamp = step(current, || {
        objc2_foundation::NSProcessInfo::processInfo().systemUptime()
    })?;
    if !timestamp.is_finite() || timestamp < 0.0 {
        return Err("原鼠标事件没有有效系统运行时间".into());
    }
    let event = |kind, pressure| {
        step(current, || {
            NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                kind, point, NSEventModifierFlags::empty(), timestamp, number, None, event_number, 1, pressure,
            )
        })?
        .ok_or("无法创建原窗口鼠标事件".to_owned())
    };
    // 同一对事件使用唯一编号及同次真实系统时间，Down/Up均成功后才进入tracking。
    // 公开值身份允许后继识别AppKit事件副本，不再用固定1.0/1把不同按钮动作混在一起。
    let down = event(NSEventType::LeftMouseDown, 1.0)?;
    let up = event(NSEventType::LeftMouseUp, 0.0)?;
    Ok([down, up])
}

/// 两事件入同一原NSApp队列；调用方须已归还ACTIVE、Doc和控件强claim。
/// NSButton自己的mouseDown/tracking取出对应Up，动作结果仍由后继真实业务终点确认。
pub(super) fn post(events: [Retained<NSEvent>; 2], mtm: MainThreadMarker) {
    let application = NSApplication::sharedApplication(mtm);
    for event in events {
        application.postEvent_atStart(&event, false);
    }
}
