//! 显式诊断负控持真实PaintUse，让既有主队列事件实际命中Busy；产品路径没有嵌套runloop。

use super::*;

pub(in crate::native_surface::paged_text_view) fn nested_delivery(
    view: &NativeMarkdownPagedTextView,
    delivery: impl FnOnce(),
) -> Result<()> {
    let _use = PayloadUseGuard::enter(&view.ivars().depth);
    let surface = view
        .ivars()
        .cpu
        .borrow()
        .clone()
        .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
    let scene = view.scene()?;
    let serial = surface
        .active_serial
        .get()
        .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
    if surface.scheduled.get() != Some(serial) || surface.candidate.borrow().is_none() {
        return Err(NativeMarkdownPagedTextViewError::Pending);
    } // 只能观察真实Pending候选；不能强制owner current或伪造一份session。
    let before = surface.steps.get();
    let painting = PaintUse::enter(&surface.painting)?;
    delivery(); // 测试提供一次真实CFRunLoop交付；此回调不进入产品调度或保存为后续任务。
    // 在原绘图准入仍被同步栈持有时进入真实 Native 显示入口，覆盖 updateLayer 重入。
    // 观察公开失败结果，而非直接写内部状态；旧实现会把普通显示等待写成 Pending 首错。
    // SAFETY: 接收者是仍保活的原 NSView；公开无参 updateLayer 只在当前主线程调用。
    unsafe {
        let _: () = msg_send![view, updateLayer];
    }
    let reentrant = view.take_failure().map_or(Ok(()), Err);
    let validation = scene.scene.validate_ready(&|| scene.current(view));
    let deferred = surface.active_serial.get() == Some(serial)
        && surface.scheduled.get().is_none()
        && surface.deferred.get() == Some(serial)
        && surface.candidate.borrow().is_some()
        && surface.steps.get() == before;
    drop(painting);
    if let Err(error) = reentrant {
        surface.cancel_candidate();
        return Err(error); // 实际绘图/来源错误继续交付；不能靠验收读取清掉真实失败。
    }
    if let Err(error) = validation {
        surface.cancel_candidate();
        return Err(error);
    }
    surface.wake_deferred(view)?;
    if !deferred || surface.scheduled.get() != Some(serial) || surface.deferred.get().is_some() {
        return Err(NativeMarkdownPagedTextViewError::Raster(
            "真实嵌套交付没有保留同候选唯一延续".into(),
        ));
    }
    Ok(()) // 后继主队列事件仍未执行；actual paint/取消/close由原harness独立消费验证。
}
