//! Exercise actual click recovery, heading navigation, and cancellation in an old
//! sealed window of the same long document. Missing headings remain explicit failures.

use super::*;
use objc2_app_kit::{
    NSApplicationActivationPolicy, NSCursor, NSEvent, NSEventMask, NSEventModifierFlags,
    NSEventType, NSRunningApplication,
};
use std::io::Read;
#[path = "links/headings.rs"]
mod headings;

const PREFIX: &str = "outside\n\n[cached](#cached)\n\n";
const PARAGRAPH: &str =
    "bounded viewport paragraph text keeps the original source and stable native geometry. ";
const LABEL: usize = 10;

/// Write fragments smaller than 4KiB directly to the fixture file, without a
/// complete intermediate String. Production still opens the original mapped document.
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

/// Original Source targets only scroll. Both the actual three-second complete
/// frame and sole-reader visibility proof come from the same original Body.
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

/// Obtain a window point from the current actual grapheme geometry. Return the
/// Scene before event delivery instead of retaining another prefix reader.
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

/// Ordinary pointer events use the existing document responder for caret/drag
/// selection, without pre-resolving or opening a link target.
pub(super) fn mouse(rig: &Rig, point: NSPoint, kind: NSEventType) -> Result<(), String> {
    mouse_with_modifiers(rig, point, kind, NSEventModifierFlags::empty())
}

/// Link activation uses the product's existing Command-click convention. The
/// original responder receives actual event modifiers and window coordinates.
pub(super) fn command_mouse(rig: &Rig, point: NSPoint, kind: NSEventType) -> Result<(), String> {
    mouse_with_modifiers(rig, point, kind, NSEventModifierFlags::Command)
}

/// Deliver the three existing public mouse event kinds. Ordinary/link clicks
/// differ only in the event's actual modifier facts.
fn mouse_with_modifiers(
    rig: &Rig,
    point: NSPoint,
    kind: NSEventType,
    modifiers: NSEventModifierFlags,
) -> Result<(), String> {
    let window = rig.view.window().ok_or("原链接窗口已归还")?;
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, point, modifiers, 1.0, window.windowNumber(), None, 1, 1, 1.0,
    ).ok_or("无法构造原链接鼠标事件")?;
    // SAFETY: Use the original main-thread responder, real window point, and live
    // synchronous event; there is no fabricated selector or cross-thread object.
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

/// This operation preserves the existing complete frame, so no new paint event is
/// expected. Service actual preparation/font work within the same three-second limit.
pub(super) fn settle(rig: &Rig) -> Result<Option<ViewError>, String> {
    let started = Instant::now();
    while rig.body.is_preparing() {
        let remaining = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .ok_or("原链接回查超过三秒")?;
        // SAFETY: Service actual pending work in the original default mode, with
        // no sleep, background polling, or extension of the original deadline.
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), true) };
    }
    if let Some(cause) = rig.body.take_failure() {
        return Err(format!("原链接Body失败：{cause}"));
    }
    Ok(rig.view.take_failure()) // The caller classifies the actual typed result; anchor rejection is not successful navigation.
}

/// An unrecovered old cache must report Pending. Neither a LINK style bit nor an
/// old string directory may be used to guess its target.
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

/// Record public Native facts once on stage success or its original timeout, with
/// no text, URL, or cursor image. Read the global mouse only; hitTest uses content-
/// parent coordinates and returns all Native claims before later event delivery.
fn diagnose_hover(rig: &Rig, stage: &str, outcome: &str, point: NSPoint, expected: &NSCursor) {
    autoreleasepool(|_| {
        let application = NSApplication::sharedApplication(rig.view.mtm());
        let mouse = NSEvent::mouseLocation();
        let window_point = rig.panel.convertPointFromScreen(mouse);
        let own_hit = rig.panel.contentView().and_then(|content| {
            // SAFETY: The Rig owns the original main-thread window; borrow its
            // mounted parent and return every temporary claim within this pool.
            let parent = unsafe { content.superview() }?;
            let local = parent.convertPoint_fromView(window_point, None);
            let hit = content.hitTest(local)?;
            let view: &NSView = &rig.view;
            Some(std::ptr::eq(&*hit, view))
        });
        let current = NSCursor::currentCursor();
        println!(
            "native_link_hover_stage stage={stage} outcome={outcome} finished_launching={} running={} active={} hidden={} key={} policy={:?} visible={} level={} window={} frame={:?} view_frame={:?} view_visible={:?} requested_window_point={point:?} requested_screen_point={:?} mouse_screen={mouse:?} mouse_window={window_point:?} own_view_hit={own_hit:?} cursor={:p} expected={:p} cursor_matches={} cursor_ibeam={} cursor_hand={} formal=false",
            NSRunningApplication::currentApplication().isFinishedLaunching(),
            application.isRunning(),
            application.isActive(),
            application.isHidden(),
            rig.panel.isKeyWindow(),
            application.activationPolicy(),
            rig.panel.isVisible(),
            rig.panel.level(),
            rig.panel.windowNumber(),
            rig.panel.frame(),
            rig.view.frame(),
            rig.view.visibleRect(),
            rig.panel.convertPointToScreen(point),
            &*current,
            expected,
            std::ptr::eq(&*current, expected),
            std::ptr::eq(&*current, &*NSCursor::IBeamCursor()),
            std::ptr::eq(&*current, &*NSCursor::pointingHandCursor()),
        );
    }); // Return hitTest/window-getter claims before diagnostics end, without extending the close cohort.
}

/// Enable this synthetic window's own MouseMoved events without moving the global
/// pointer, assigning NSCursor, or inventing cursor state. If local events do not
/// drive standard cursor rectangles, move only the owned window so the actual
/// pointer intersects the same label/ordinary-text geometry.
fn verify_hover(rig: &Rig) -> Result<(), String> {
    autoreleasepool(|_| {
        let application = NSApplication::sharedApplication(rig.view.mtm());
        let original_policy = application.activationPolicy();
        let original_frame = rig.panel.frame();
        let original_mouse_events = rig.panel.acceptsMouseMovedEvents();
        let original_key = rig.panel.isKeyWindow();
        let original = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let initial = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let started = Instant::now();
        let result = (|| {
            // Other headless controls may remain Prohibited. This actual cursor
            // control temporarily uses Accessory and restores the original policy afterward.
            if original_policy == NSApplicationActivationPolicy::Prohibited
                && !application.setActivationPolicy(NSApplicationActivationPolicy::Accessory)
            {
                return Err("链接悬停无法取得本应用可激活策略".into());
            }
            rig.panel.setAcceptsMouseMovedEvents(true);
            application.activate(); // Activate only this synthetic app; do not manipulate other apps, Spaces, or accessibility permission.
            rig.panel.orderFrontRegardless();
            rig.panel.makeKeyWindow();
            for (stage, index, expected) in [
                ("text_before_link", 0, NSCursor::IBeamCursor()),
                ("link_label", LABEL, NSCursor::pointingHandCursor()),
                ("text_after_link", 0, NSCursor::IBeamCursor()),
            ] {
                let point = point(rig, index)?;
                let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                NSEventType::MouseMoved, point, NSEventModifierFlags::empty(),
                objc2_foundation::NSProcessInfo::processInfo().systemUptime(),
                rig.panel.windowNumber(), None, 0, 0, 0.0,
            ).ok_or("无法构造原窗口悬停MouseMoved事件")?;
                application.sendEvent(&event); // Let actual NSApplication dispatch; call no cursor setter/private tracking method.
                let observed = NSCursor::currentCursor();
                if !application.isActive()
                    || !rig.panel.isKeyWindow()
                    || !std::ptr::eq(&*observed, &*expected)
                {
                    // currentCursor describes this application only. Move the
                    // owned window under the read-only pointer location so actual
                    // WindowServer geometry intersects its text, without moving
                    // the pointer or manipulating another window.
                    let mouse = NSEvent::mouseLocation();
                    let screen = rig.panel.convertPointToScreen(point);
                    let frame = rig.panel.frame();
                    rig.panel.setFrameOrigin(NSPoint::new(
                        frame.origin.x + mouse.x - screen.x,
                        frame.origin.y + mouse.y - screen.y,
                    ));
                    rig.panel.orderFrontRegardless();
                    rig.panel.makeKeyWindow();
                    rig.panel.invalidateCursorRectsForView(&rig.view);
                }
                loop {
                    if started.elapsed() > Duration::from_secs(3) {
                        diagnose_hover(rig, stage, "deadline", point, &expected);
                        return Err(format!("标准链接悬停超过原三秒期限：stage={stage}"));
                    }
                    // This component does not enter NSApplication.run, so service
                    // its normal window updates and all events, including activation/
                    // window-state AppKitDefined events. distantPast reads only
                    // already queued events rather than blocking a completed cursor
                    // transition behind a restricted event wait until deadline.
                    let dispatched = autoreleasepool(|_| {
                        application.updateWindows();
                        // SAFETY: AppKit's default mode is process-lifetime storage;
                        // dispatch this application's events only, without global input posting.
                        let mode = unsafe { objc2_foundation::NSDefaultRunLoopMode };
                        let event = application.nextEventMatchingMask_untilDate_inMode_dequeue(
                            NSEventMask::Any,
                            Some(&objc2_foundation::NSDate::distantPast()),
                            mode,
                            true,
                        );
                        let dispatched = event.is_some();
                        if let Some(event) = event {
                            application.sendEvent(&event);
                        }
                        dispatched // Return only a bool; event/window dispatch claims must not survive until Rig closure.
                    });
                    {
                        // Geometry changes may temporarily lack Ready. Check
                        // Source health through the original document's zero endpoint
                        // before waiting, without masking file errors as Pending or
                        // retaining a document borrow across Native event delivery.
                        let owner = rig.document.upgrade().ok_or("悬停原文稿已释放")?;
                        let document = owner.try_borrow().map_err(|_| "悬停原文稿仍在借用")?;
                        document
                            .byte_offset_for_utf16(0)
                            .map_err(|cause| cause.to_string())?;
                    }
                    if rig.view.input_state().map_err(|cause| cause.to_string())? != original {
                        return Err("真实链接悬停改变了原完整Input".into());
                    }
                    if let Some(cause) = rig.body.take_failure() {
                        return Err(format!("悬停正文失败：{cause}"));
                    }
                    if let Some(cause) = rig.view.take_failure() {
                        return Err(format!("悬停输入失败：{cause}"));
                    }
                    // Cursor authority requires the same actual Scene/CPU frame.
                    // Finish Source/font checks before calling the Native cursor getter.
                    let complete = autoreleasepool(|_| -> Result<bool, String> {
                        let key = match rig.view.capture_ready_scene_key() {
                            Ok(key) => key,
                            Err(ViewError::Pending) => return Ok(false),
                            Err(cause) => return Err(cause.to_string()),
                        };
                        if key.source != initial.source || key.request.input != original {
                            return Err("悬停换成另一Source或Input".into());
                        }
                        let scene = match NativeMarkdownPagedTextViewDelegate::ready_scene(
                            &*rig.body,
                            &key.request,
                        ) {
                            Ok(scene) => scene,
                            Err(ViewError::Pending) => return Ok(false),
                            Err(cause) => return Err(cause.to_string()),
                        };
                        scene
                            .validate_ready(&|| {
                                NativeMarkdownPagedTextViewDelegate::scene_current(&*rig.body, &key)
                            })
                            .map_err(|cause| cause.to_string())?;
                        drop(scene); // Native getters/event delivery must not retain old readers/fonts and conceal retirement defects.
                        Ok(rig.view.probe_cpu_published_frame().is_some_and(
                            |(published, paints)| {
                                paints != 0
                                    && published.same(&key)
                                    && rig.view.cpu_paint_pending() == Some(false)
                            },
                        ))
                    })?;
                    if complete
                        && application.isActive()
                        && rig.panel.isKeyWindow()
                        && std::ptr::eq(&*NSCursor::currentCursor(), &*expected)
                        && started.elapsed() <= Duration::from_secs(3)
                    {
                        diagnose_hover(rig, stage, "verified", point, &expected);
                        break;
                    }
                    let remaining = Duration::from_secs(3)
                        .checked_sub(started.elapsed())
                        .ok_or_else(|| {
                            diagnose_hover(rig, stage, "deadline", point, &expected);
                            format!(
                                "标准链接悬停三秒未交实际光标：stage={stage} active={} key={}",
                                application.isActive(),
                                rig.panel.isKeyWindow()
                            )
                        })?;
                    if !dispatched {
                        // Only an empty event queue waits for an actual main-loop/
                        // WindowServer source, then rechecks Source, complete CPU
                        // frame, and cursor. No sleep, periodic timer, or fabricated
                        // notification renews the original deadline; late final
                        // cursor state cannot retroactively certify timely completion.
                        autoreleasepool(|_| {
                            // SAFETY: Use the actual AppKit main thread/default mode
                            // and remaining deadline, without a document borrow across iterations.
                            unsafe {
                                CFRunLoop::run_in_mode(
                                    kCFRunLoopDefaultMode,
                                    remaining.as_secs_f64(),
                                    true,
                                )
                            };
                        });
                    }
                }
            }
            Ok(())
        })();
        // Restore the owned window and original activation policy on success or
        // failure; cached/unobserved hand-cursor state cannot establish acceptance.
        rig.panel.setFrameOrigin(original_frame.origin);
        rig.panel.setAcceptsMouseMovedEvents(original_mouse_events);
        // An already activatable app never changed policy. Setting that same value
        // can return false, so restore only an actually changed policy and then
        // verify its public final value together with the original key/Input facts.
        let restored = if application.activationPolicy() != original_policy {
            application.setActivationPolicy(original_policy)
        } else {
            true
        };
        if original_key {
            rig.panel.makeKeyWindow();
        }
        result?;
        let actual_policy = application.activationPolicy();
        let actual_key = rig.panel.isKeyWindow();
        let input_unchanged =
            rig.view.input_state().map_err(|cause| cause.to_string())? == original;
        if !restored
            || actual_policy != original_policy
            || original_key && !actual_key
            || !input_unchanged
        {
            eprintln!(
                "native_link_hover_restore accepted={restored} original_policy={original_policy:?} actual_policy={actual_policy:?} original_key={original_key} actual_key={actual_key} input_unchanged={input_unchanged} formal=false"
            );
            return Err("悬停结束未恢复原策略或完整Input".into());
        }
        println!(
            "native_link_hover actual_appkit_mouse_moved=true active_key=true text_link_text_cursor_verified=true global_mouse_moved=false original_input_unchanged=true formal=false"
        );
        Ok(())
    }) // Drain this control's autoreleased claims after restoration, not at later outer-Rig closure.
}

/// Source-prefix recovery preserves original selection/font graphs. One mouse-up
/// drives actual recovery; earlier successor input or focus loss revokes the old click.
fn verify(rig: &mut Rig) -> Result<(), String> {
    // Direct responder delivery bypasses NSApplication.sendEvent; establish the
    // actual key-window condition a user click would have before testing the responder.
    rig.panel.makeKeyWindow();
    if !rig.panel.isKeyWindow() {
        return Err("链接真实鼠标夹具没有取得原keyWindow，不能冒充已获点击焦点".into());
    }
    let initial = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    verify_hover(rig)?;
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
    // Both an ordinary subsequent command and focus transfer must cancel queued
    // link recovery before it can advance the old Source's semantic work.
    for focus in [false, true] {
        command_mouse(rig, press, NSEventType::LeftMouseDown)?;
        command_mouse(rig, press, NSEventType::LeftMouseUp)?;
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
    // Releasing Command before mouse-up cancels activation, preserving canonical
    // selection and the old cache's actual Pending target state.
    command_mouse(rig, press, NSEventType::LeftMouseDown)?;
    mouse(rig, press, NSEventType::LeftMouseUp)?;
    if let Some(cause) = settle(rig)? {
        return Err(format!("已松开 Command 的链接点击仍报告失败：{cause}"));
    }
    pending_target(rig)?;
    if rig.view.input_state().map_err(|cause| cause.to_string())? != cached.request.input {
        return Err("取消链接跟随改变了原 canonical 输入".into());
    }
    command_mouse(rig, press, NSEventType::LeftMouseDown)?;
    if rig.view.input_state().map_err(|cause| cause.to_string())? != cached.request.input {
        return Err("原链接mousedown在确认单击前改写选择或抢走拖选".into());
    }
    command_mouse(rig, press, NSEventType::LeftMouseUp)?;
    if !matches!(settle(rig)?, Some(ViewError::LinkAnchorNotFound)) {
        return Err("原片段目标不存在时没有明确报告标题未找到".into());
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
    // An ordinary label click positions the actual caret without requesting link
    // navigation or producing an anchor-rejection error.
    let frame = observe(&rig.body)?;
    mouse(rig, press, NSEventType::LeftMouseDown)?;
    mouse(rig, press, NSEventType::LeftMouseUp)?;
    wait_frame("root_leaf_link_plain_click_caret", &rig.body, frame)?;
    if let Some(cause) = settle(rig)? {
        return Err(format!("普通标签单击不应触发链接：{cause}"));
    }
    let positioned = rig.view.input_state().map_err(|cause| cause.to_string())?;
    if positioned.generation != cached.request.input.generation
        || positioned.primary_range != NSRange::new(LABEL, 0)
        || positioned.marked.is_some()
    {
        return Err("普通标签单击没有定位到真实字符，或改变了正文与组合输入".into());
    }
    // Selection can reveal markers and reflow locally. Obtain the label's current
    // actual point for dragging instead of borrowing stale hit geometry.
    let press = point(rig, LABEL)?;
    // Dragging still follows ordinary caret selection; starting on a link cannot
    // activate an external target prematurely at mouse-down.
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
        "native_body_root_leaf_link cached_scene_real_pending=true first_mouseup_restored_target=true missing_anchor_rejected=true command_click=true released_command_cancelled=true plain_click_caret=true successor_menu_and_focus_revoked=true original_drag_selection=true original_source=true external_url_open_tested=false whole_product=false formal=false"
    );
    Ok(())
}

const DIRECT_HEAD: &str = "outside `code` **strong** *italic* ***both*** ";
const DIRECT_PADDING: &str = "plain paragraph keeps the same original source and native viewport. ";
const DIRECT_PREFIX_REPEATS: usize = 1_024;
const DIRECT_LABEL: &str = "native link label ";
const DIRECT_LABEL_REPEATS: usize = 192;
// The independent URI expectation follows fixed Markdown escapes/entities:
// remove parenthesis escapes, decode amp, and preserve the entire fragment.
// Never populate it from Core's parsed target, a click result, or the current Scene.
const DIRECT_CLOSE: &str = "](https://example.com/a\\(b\\)?first=1&amp;last=2#native-long) ";
const DIRECT_URI: &str = "https://example.com/a(b)?first=1&last=2#native-long";
const DIRECT_TAIL_REPEATS: usize = 256;
const DIRECT_LABEL_START: usize =
    DIRECT_HEAD.len() + DIRECT_PADDING.len() * DIRECT_PREFIX_REPEATS + 1;
const DIRECT_LABEL_UNITS: usize = DIRECT_LABEL.len() * DIRECT_LABEL_REPEATS;
const DIRECT_UNITS: usize = DIRECT_LABEL_START
    + DIRECT_LABEL_UNITS
    + DIRECT_CLOSE.len()
    + DIRECT_PADDING.len() * DIRECT_TAIL_REPEATS;

/// Fixed ASCII makes literal bytes independent canonical UTF-16 units. With no
/// newline this remains one Root paragraph larger than 64KiB. Borrow fragments
/// incrementally without assembling complete text or an attributed graph.
fn direct_segments() -> impl Iterator<Item = &'static str> {
    std::iter::once(DIRECT_HEAD)
        .chain(std::iter::repeat_n(DIRECT_PADDING, DIRECT_PREFIX_REPEATS))
        .chain(std::iter::once("["))
        .chain(std::iter::repeat_n(DIRECT_LABEL, DIRECT_LABEL_REPEATS))
        .chain(std::iter::once(DIRECT_CLOSE))
        .chain(std::iter::repeat_n(DIRECT_PADDING, DIRECT_TAIL_REPEATS))
}

/// Navigation/URI lookup read the original source only. Compare canonical text and
/// an independent disk descriptor through fixed stack chunks, retaining empty
/// history, original generation, and file identity. Syntax removal, truncated tails,
/// or a newly saved replacement file cannot satisfy integrity verification.
fn direct_unchanged(rig: &Rig) -> Result<(), String> {
    let owner = rig.document.upgrade().ok_or("长直接链接原 Doc 已归还")?;
    let document = owner.borrow();
    if document.generation() != 0
        || document.utf16_len() != DIRECT_UNITS
        || document.byte_len() != DIRECT_UNITS
        || document.selection() != MarkdownTextSelection::caret(0)
        || document.can_undo()
        || document.can_redo()
        || document.is_dirty()
        || !document.original_file_identity_is_unchanged()
    {
        return Err("长直接链接只读导航改变了原文稿、选择、历史或磁盘身份".into());
    }
    let mut expected = direct_segments().flat_map(str::bytes);
    let mut units = [0_u16; 256];
    for start in (0..DIRECT_UNITS).step_by(units.len()) {
        let used = units.len().min(DIRECT_UNITS - start);
        document
            .copy_utf16_code_units(start, &mut units[..used])
            .map_err(|cause| cause.to_string())?;
        if units[..used]
            .iter()
            .any(|actual| expected.next().map(u16::from) != Some(*actual))
        {
            return Err("长直接链接显示改变了原 canonical 字符".into());
        }
    }
    if expected.next().is_some() {
        return Err("长直接链接原 canonical 尾部未完整保留".into());
    }
    drop(document);
    drop(owner); // Return document borrows/claims before disk checks and any later Native/run-loop operation.
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 128];
    for segment in direct_segments() {
        file.read_exact(&mut bytes[..segment.len()])
            .map_err(|cause| cause.to_string())?;
        if &bytes[..segment.len()] != segment.as_bytes() {
            return Err("长直接链接只读流程改变了原磁盘字节".into());
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("长直接链接原磁盘出现额外尾部".into());
    }
    Ok(())
}

/// Publish actual code/bold/italic styles in this long physical line, then scroll
/// through the original SourceTarget to the link's start/middle/end. Borrow only
/// current Scene geometry, hit testing, and target lookup; do not open external
/// links or use the known URI to replace missing semantic authority. Unexpected
/// Pending is a real failure of this scenario.
fn verify_direct(rig: &mut Rig) -> Result<(), String> {
    let initial = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if rig.backing.mapped_bytes() == 0 {
        return Err("长直接链接没有保持原真实源映射".into());
    }
    let parts = rig
        .body
        .probe_root_leaf_parts()
        .map_err(|cause| cause.to_string())?;
    // Inspect only font/style facts actually sealed for the first viewport, never
    // all paragraph parts merely to inspect a remote link. Fixed head literals
    // provide independent expectations for real glyph roles, not parser tokens alone.
    for (literal, styles) in [
        ("code", MarkdownPresentationStyles::INLINE_CODE),
        ("strong", MarkdownPresentationStyles::STRONG),
        ("italic", MarkdownPresentationStyles::EMPHASIS),
        ("both", MarkdownPresentationStyles::STRONG),
        ("both", MarkdownPresentationStyles::EMPHASIS),
    ] {
        let start = DIRECT_HEAD.find(literal).expect("固定长段落保原样式字面");
        for index in start..start + literal.len() {
            if parts
                .iter()
                .find(|part| part.source.contains(&index))
                .is_none_or(|part| !part.presentation.contains(styles))
            {
                return Err(format!(
                    "长直接链接前缀的真实封存样式缺失：source={index} expected={styles:?}"
                ));
            }
        }
    }
    drop(parts); // End this frame's font observations; later navigation retains no paragraph font/Source graph or duplicate line directory.
    direct_unchanged(rig)?;
    let mut previous: Option<NativeMarkdownPagedSceneKey> = None;
    for (stage, index) in [
        ("root_leaf_long_direct_link_start", DIRECT_LABEL_START),
        (
            "root_leaf_long_direct_link_middle",
            DIRECT_LABEL_START + DIRECT_LABEL_UNITS / 2,
        ),
        (
            "root_leaf_long_direct_link_end",
            DIRECT_LABEL_START + DIRECT_LABEL_UNITS - 2,
        ),
    ] {
        // The label crosses real viewports. Fixed start/middle/end literals are
        // independent of candidate runs/visible ranges. Register the actual CPU
        // event before scrolling and consume the same target's proof within three seconds.
        reveal(rig, stage, index)?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        // SourceTarget preserves selection and changes only actual viewport
        // geometry. Even a surviving old weak key must lose current Body authority;
        // equal document/URI identity cannot reuse its frame as this target's success.
        if key.source != initial.source
            || key.request.input != initial.request.input
            || previous.as_ref().is_some_and(|previous| {
                key.same(previous)
                    || key.request.virtual_viewport.document_origin
                        == previous.request.virtual_viewport.document_origin
                    || NativeMarkdownPagedTextViewDelegate::scene_current(&*rig.body, previous)
            })
        {
            return Err("长链接跨窗口导航没有保原输入、Source或撤销旧几何".into());
        }
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
            .map_err(|cause| cause.to_string())?;
        let first = scene
            .first_rect(NSRange::new(index, 1))
            .map_err(|cause| cause.to_string())?;
        if first.actual_range != NSRange::new(index, 1)
            || first.rect.size.width <= 0.0
            || first.rect.size.height <= 1.0
        {
            return Err(format!("{stage}: 原标签字符没有完整非空 canonical 框"));
        }
        let local = NSPoint::new(
            first.rect.origin.x + first.rect.size.width * 0.25,
            first.rect.origin.y + first.rect.size.height * 0.5,
        );
        // The fixed ASCII label's quarter-width point must hit its original left
        // canonical unit. Source/publication/full-Input authority belongs to this
        // Scene; direct Core lookup cannot substitute for actual Native behavior.
        if scene.hit(local).map_err(|cause| cause.to_string())? != index
            || scene
                .link_target(local)
                .map_err(|cause| cause.to_string())?
                .as_deref()
                != Some(DIRECT_URI)
        {
            return Err(format!(
                "{stage}: 长标签真实命中没有返回原转义和实体规范化后的完整 URI"
            ));
        }
        drop(scene); // End synchronous reader/font/Source claims before the next navigation and strict close.
        previous = Some(key);
        direct_unchanged(rig)?;
    }
    rig.remember_receipt()?;
    println!(
        "native_root_long_direct_link original_root_bytes={DIRECT_UNITS} original_label_units={DIRECT_LABEL_UNITS} mapped_source=true code_strong_emphasis_combined=true actual_scene_hit_uri_start_middle_end=true native_viewport_navigation=true normalized_complete_uri=true canonical_disk_history_unchanged=true external_url_open_tested=false whole_product=false formal=false"
    );
    Ok(())
}

/// Assemble this long paragraph only after the preceding click/cancellation graph
/// is released. Failures still complete strict teardown before reporting, checking
/// Native objects, document/Input, fonts, workers, FDs, mappings, and pixels.
fn direct_case(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, publication, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        for segment in direct_segments() {
            file.write_all(segment.as_bytes())
                .map_err(|cause| cause.to_string())?;
        }
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            DIRECT_UNITS,
            1,
            false,
            MarkdownPresentationMode::InlinePreview,
            false,
        )?;
        let result = verify_direct(&mut rig);
        let publication = rig
            .body
            .probe_root_leaf_publication()
            .map_err(|cause| cause.to_string());
        let retirement = release::close(rig)?;
        Ok((retirement, publication, result))
    })?;
    retirement.verify()?;
    if publication?.upgrade().is_some() {
        return Err("长直接链接关闭后原语义 publication 仍被强持有".into());
    }
    result
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
        if let Err(cause) = &result {
            eprintln!("native_root_link_primary outcome=failed reason={cause}");
        } // Still execute the full close gate, but do not hide the primary interaction error behind a cohort timeout.
        let publication = rig
            .body
            .probe_root_leaf_publication()
            .map_err(|cause| cause.to_string());
        let retirement = release::close(rig).map_err(|cleanup| match &result {
            Err(cause) => format!("{cause}；原链接关闭同时失败：{cleanup}"),
            Ok(()) => cleanup,
        })?;
        Ok((retirement, publication, result)) // Even failed click/frame checks clean the actual synthetic window/Source graph first.
    })?;
    let cleanup = retirement.verify().and_then(|()| {
        if publication?.upgrade().is_some() {
            return Err("旧cache链接关闭后原有限Ready仍被持有".into());
        }
        Ok(())
    });
    match (result, cleanup) {
        (Err(cause), Err(cleanup)) => {
            return Err(format!("{cause}；原链接关闭同时失败：{cleanup}"));
        }
        (Err(cause), Ok(())) => return Err(cause),
        (Ok(()), cleanup) => cleanup?,
    }
    direct_case(mtm)?;
    headings::run(mtm)
}
