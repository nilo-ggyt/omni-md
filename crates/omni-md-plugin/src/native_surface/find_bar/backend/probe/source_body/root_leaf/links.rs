//! 同一长文旧sealed窗口的真实单击回查与取消；片段目标只核解析/消费，不冒充内部anchor导航。

use super::*;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};

const PREFIX: &str = "outside\n\n[cached](#cached)\n\n";
const PARAGRAPH: &str =
    "bounded viewport paragraph text keeps the original source and stable native geometry. ";
const LABEL: usize = 10;

/// 每段不足4KiB且逐片写文件；夹具不先拼出全文String，生产仍只打开原mmap文档。
fn fixture() -> Result<(tempfile::NamedTempFile, usize, usize), String> {
    let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
    file.write_all(PREFIX.as_bytes())
        .map_err(|cause| cause.to_string())?;
    for _ in 0..48 {
        for _ in 0..6 {
            file.write_all(PARAGRAPH.as_bytes())
                .map_err(|cause| cause.to_string())?;
        }
        file.write_all(b"\n\n").map_err(|cause| cause.to_string())?;
    }
    file.flush().map_err(|cause| cause.to_string())?;
    Ok((file, PREFIX.len() + 48 * (PARAGRAPH.len() * 6 + 2), 101))
}

/// 原Source目标只滚动，不改选择；真实三秒完整帧与唯一Reader证明都来自原Body。
fn reveal(rig: &mut Rig, stage: &str, index: usize) -> Result<(), String> {
    let frame = observe(&rig.body)?;
    let target = rig
        .body
        .request_source_target(NSRange::new(index, 1))
        .map_err(|cause| cause.to_string())?;
    wait_frame(stage, &rig.body, frame)?;
    rig.body
        .take_source_target_visible(&target)
        .map_err(|cause| cause.to_string())?
        .ok_or("原链接夹具Source目标没有真实可见证明")?;
    rig.remember_receipt()
}

/// 只取当前真实GCB的窗口点；旧Scene在系统事件交付前归还，不占第二份prefix读取。
pub(super) fn point(rig: &Rig, index: usize) -> Result<NSPoint, String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let first = scene
        .first_rect(NSRange::new(index, 1))
        .map_err(|cause| cause.to_string())?;
    if first.actual_range != NSRange::new(index, 1) || first.rect.size.width <= 0.0 {
        return Err("原链接字符没有真实单元几何".into());
    }
    let local = NSPoint::new(
        first.rect.origin.x + first.rect.size.width * 0.25,
        first.rect.origin.y + first.rect.size.height * 0.5,
    );
    Ok(rig.view.convertPoint_toView(local, None))
}

/// 交付既有三种公开鼠标语义；不调用测试专用点击入口，不提前解析target。
pub(super) fn mouse(rig: &Rig, point: NSPoint, kind: NSEventType) -> Result<(), String> {
    let window = rig.view.window().ok_or("原链接窗口已归还")?;
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, point, NSEventModifierFlags::empty(), 1.0, window.windowNumber(), None, 1, 1, 1.0,
    ).ok_or("无法构造原链接鼠标事件")?;
    // SAFETY: 原主线程NSResponder、真实窗口点和同步活事件；没有虚构selector或跨线程对象。
    unsafe {
        if kind == NSEventType::LeftMouseDown {
            let _: () = objc2::msg_send![&**rig.view, mouseDown: &*event];
        } else if kind == NSEventType::LeftMouseDragged {
            let _: () = objc2::msg_send![&**rig.view, mouseDragged: &*event];
        } else if kind == NSEventType::LeftMouseUp {
            let _: () = objc2::msg_send![&**rig.view, mouseUp: &*event];
        } else {
            unreachable!("本夹具只交Down/Drag/Up")
        }
    }
    Ok(())
}

/// 此操作保持原完整帧，不会再签新paint通知；按真实调度/Font状态消费主runloop，仍受同一三秒限制。
pub(super) fn settle(rig: &Rig) -> Result<Option<ViewError>, String> {
    let started = Instant::now();
    while rig.body.is_preparing() {
        let remaining = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .ok_or("原链接回查超过三秒")?;
        // SAFETY: 只处理原default mode的实际待办；没有sleep、后台轮询或延长原异步操作期限。
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), true) };
    }
    if let Some(cause) = rig.body.take_failure() {
        return Err(format!("原链接Body失败：{cause}"));
    }
    Ok(rig.view.take_failure()) // 原typed结果交调用者逐种核，不能把预期的anchor明确拒绝当成导航成功。
}

/// 未恢复的旧cache必须明确Pending；不能凭LINK位或旧字符串目录猜target。
fn pending_target(rig: &Rig) -> Result<(), String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let window_point = point(rig, LABEL)?;
    let local = rig.view.convertPoint_fromView(window_point, None);
    if scene
        .link_hit(local)
        .map_err(|cause| cause.to_string())?
        .is_none()
        || !matches!(scene.link_target(local), Err(ViewError::Pending))
    {
        return Err("旧sealed链接未复现真实可见命中与当前语义窗外Pending".into());
    }
    Ok(())
}

/// Source前缀回溯不改原选择/字体图；一次mouseup即可驱动真恢复，后继输入和失焦先到则撤旧。
fn verify(rig: &mut Rig) -> Result<(), String> {
    rig.panel.makeKeyWindow(); // 直接NSResponder夹具不会走NSApplication.sendEvent，明确恢复真实用户点击前的键盘窗口条件。
    if !rig.panel.isKeyWindow() {
        return Err("链接真实鼠标夹具没有取得原keyWindow，不能冒充已获点击焦点".into());
    }
    let initial = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    reveal(rig, "root_leaf_link_far_prefix", 20_001)?;
    reveal(rig, "root_leaf_link_cached_top", LABEL)?;
    pending_target(rig)?;
    let cached = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let before = rig.body.progress().map_err(|cause| cause.to_string())?;
    if cached.source != initial.source || cached.request.input != initial.request.input {
        return Err("链接旧cache准备改变原Source或canonical选择".into());
    }
    let press = point(rig, LABEL)?;
    for focus in [false, true] {
        mouse(rig, press, NSEventType::LeftMouseDown)?;
        mouse(rig, press, NSEventType::LeftMouseUp)?;
        if focus {
            rig.panel.makeFirstResponder(Some(&*rig.bar.query));
        } else {
            rig.view
                .command(sel!(copy:))
                .map_err(|cause| cause.to_string())?;
        }
        if let Some(cause) = settle(rig)? {
            return Err(format!("已取消的click仍报告失败：{cause}"));
        }
        pending_target(rig)?;
        if rig
            .body
            .progress()
            .map_err(|cause| cause.to_string())?
            .semantic_work
            != before.semantic_work
        {
            return Err("后继菜单输入或失焦未撤旧click，仍推进了原链接来源".into());
        }
        rig.panel.makeFirstResponder(Some(&**rig.view));
    }
    mouse(rig, press, NSEventType::LeftMouseDown)?;
    if rig.view.input_state().map_err(|cause| cause.to_string())? != cached.request.input {
        return Err("原链接mousedown在确认单击前改写选择或抢走拖选".into());
    }
    mouse(rig, press, NSEventType::LeftMouseUp)?;
    if !matches!(settle(rig)?, Some(ViewError::LinkAnchorUnsupported)) {
        return Err("真实片段链接没有明确报告尚无稳定anchor导航".into());
    }
    let recovered = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if !recovered.same(&cached) {
        return Err("旧cache链接回查另建了Scene/Source/排版图".into());
    }
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &cached.request)
        .map_err(|cause| cause.to_string())?;
    let local = rig.view.convertPoint_fromView(press, None);
    if scene
        .link_target(local)
        .map_err(|cause| cause.to_string())?
        .as_deref()
        != Some("#cached")
    {
        return Err("第一次真实mouseup未恢复原Source解析的片段target".into());
    }
    drop(scene);
    // 随后真实拖动仍走原caret选择；即使从链接开始，也不能在mousedown提前激活外部目标。
    let frame = observe(&rig.body)?;
    let end = point(rig, PREFIX.len() + 12)?;
    mouse(rig, press, NSEventType::LeftMouseDown)?;
    mouse(rig, end, NSEventType::LeftMouseDragged)?;
    mouse(rig, end, NSEventType::LeftMouseUp)?;
    wait_frame("root_leaf_link_original_drag_selection", &rig.body, frame)?;
    let dragged = rig.view.input_state().map_err(|cause| cause.to_string())?;
    if dragged.generation != cached.request.input.generation
        || dragged.primary_range.length == 0
        || dragged.primary_range.location > LABEL
    {
        return Err("链接开始的原拖动未保留真实连续选区及原正文generation".into());
    }
    rig.remember_receipt()?;
    println!(
        "native_body_root_leaf_link cached_scene_real_pending=true first_mouseup_restored_target=true anchor_typed_rejection=true successor_menu_and_focus_revoked=true original_drag_selection=true original_source=true external_url_open_tested=false internal_anchor_navigation_tested=false whole_product=false formal=false"
    );
    Ok(())
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, publication, result) = autoreleasepool(|_| -> Result<_, String> {
        let (file, units, lines) = fixture()?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            units,
            lines,
            false,
            MarkdownPresentationMode::InlinePreview,
            false,
        )?;
        let result = verify(&mut rig);
        let publication = rig
            .body
            .probe_root_leaf_publication()
            .map_err(|cause| cause.to_string());
        let retirement = release::close(rig, false)?;
        Ok((retirement, publication, result)) // 即使点击/帧断言失败，先按原完整资源图清理合成窗口和Source。
    })?;
    retirement.verify(false)?;
    if publication?.upgrade().is_some() {
        return Err("旧cache链接关闭后原有限Ready仍被持有".into());
    }
    result
}
