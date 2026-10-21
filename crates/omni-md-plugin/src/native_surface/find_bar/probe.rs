//! Exercise actual Find entry points on the same Native9 graph. Original owner,
//! root, and epoch issue authority without installing a replacement foreground slot.

use super::*;
use crate::native_surface::NativeMarkdownTextKitEditorSurface;

/// Synthetic Find scenarios share the original editor's sole bar owner, never a
/// second foreground slot or another control implementation.
fn find_probe_bar(
    surface: &NativeMarkdownTextKitEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    current: &impl Fn() -> bool,
) -> Result<Rc<NativeMarkdownFindBar>, String> {
    if !current() {
        return Err("合成 Find 栏未持有原窗口权限".into());
    }
    let original = surface.find_bar.borrow().clone();
    match original {
        Some(bar) => Ok(bar),
        None => {
            // Clone the same Rust ledger owner. Native construction holds no
            // RefCell borrow, and actual controls share the existing resource budget.
            let ledger = surface
                .find_ledger
                .borrow()
                .as_ref()
                .expect("实际Find对照已装配会话账本")
                .clone();
            let created = native_markdown_table_intent_step(current, || {
                NativeMarkdownFindBar::new(
                    controller,
                    &ledger,
                    Rc::downgrade(surface.document()),
                    surface.backend.text_view.mtm(),
                )
            })
            .map_err(|_| "任务框Find栏构造期间已撤权")??;
            let bar = created;
            if !current() || surface.find_bar.borrow().is_some() {
                return Err("任务框Find栏已被后继owner替换".into());
            }
            *surface.find_bar.borrow_mut() = Some(Rc::clone(&bar));
            native_markdown_table_intent_step(current, || surface.capture_find_resources())
                .map_err(|_| "任务框Find资源登记期间已撤权")??;
            Ok(bar)
        }
    }
}

fn configure_find_probe_x(
    bar: &NativeMarkdownFindBar,
    current: &impl Fn() -> bool,
) -> Result<(), String> {
    let step = |work: &mut dyn FnMut()| {
        native_markdown_table_intent_step(current, work)
            .map_err(|_| "合成 Find 的原窗口/正文/输入已变更".to_owned())
    };
    // This fixed small query belongs only to synthetic regression. Preserve
    // production Next's hidden navigation without taking focus or changing body height.
    step(&mut || bar.query.setStringValue(ns_string!("x")))?;
    step(&mut || bar.replacement.setStringValue(ns_string!("")))?;
    step(&mut || bar.case.setState(objc2_app_kit::NSControlStateValueOff))?;
    step(&mut || bar.regex.setState(objc2_app_kit::NSControlStateValueOff))?;
    step(&mut || bar.word.setState(objc2_app_kit::NSControlStateValueOff))?;
    Ok(())
}

/// Begin the actual task-marker Find under its original input intent and return
/// that same request for later endpoint observation, never a status-text inference.
pub(in crate::native_surface) fn begin_list_marker_find_probe(
    surface: &NativeMarkdownTextKitEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    intent: u64,
    expected: std::ops::Range<usize>,
    current: &impl Fn() -> bool,
) -> Result<Rc<NativeMarkdownFindRevealRequest>, String> {
    if !current() || controller.find_input_intent() != intent {
        return Err("任务框 Find 未持有原输入权限".into()); // Reject stale intent before bar creation or field setters.
    }
    let bar = find_probe_bar(surface, controller, current)?;
    configure_find_probe_x(&bar, current)?;
    execute_multi_selection_find_probe(
        surface,
        controller,
        &bar,
        intent,
        MarkdownFindAction::Next,
        Some(expected),
        current,
    )?
    .ok_or_else(|| "任务框真实 Find 缺少原定位请求".into())
}

/// Copy original scalar bar facts only, with no Native getters or retained controls,
/// so the outer ticket can preserve authority across helpers and autorelease pools.
pub(in crate::native_surface) fn multi_selection_find_probe_bar_stamp(
    surface: &NativeMarkdownTextKitEditorSurface,
) -> Option<(u64, bool)> {
    surface.find_bar.borrow().as_ref().and_then(|bar| {
        (!bar.closed.get()).then_some((bar.layout_revision.get(), bar.visible.get()))
    })
}

/// Mount the actual production bar for multiple-match actions. Only its original
/// show transaction changes visibility and layout revision.
pub(in crate::native_surface) fn prepare_multi_selection_find_probe(
    surface: &NativeMarkdownTextKitEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    current: &impl Fn() -> bool,
) -> Result<Rc<NativeMarkdownFindBar>, String> {
    let bar = find_probe_bar(surface, controller, current)?;
    let same_bar = || {
        current()
            && !bar.closed.get()
            && surface
                .find_bar
                .borrow()
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, &bar))
    };
    configure_find_probe_x(&bar, &same_bar)?;
    // Declare this show's exact revision transition before Native setters. Reentry
    // may observe only these states, never acquire arbitrary successor layout authority.
    let before = (bar.layout_revision.get(), bar.visible.get());
    let revision = before.0.checked_add(1).ok_or("合成 Find 布局代次耗尽")?;
    let entered = (revision, before.1);
    let after = (revision, true);
    let showing = || {
        let actual = (bar.layout_revision.get(), bar.visible.get());
        same_bar() && (actual == before || actual == entered || actual == after)
    };
    let result: Result<(), String> = autoreleasepool(|_| {
        native_markdown_table_intent_step(&showing, || {
            bar.show(&surface.backend.scroll_view, false, controller)
        })
        .map_err(|_| "实际显示合成 Find 栏期间原权限已撤销")??;
        if !showing()
            || bar.layout_revision.get() != revision
            || !bar.is_visible()
            || bar.replace_visible.get()
        {
            return Err("合成 Find 栏未达到预声明的实际显示状态".into());
        }
        let root_parent = native_markdown_table_intent_step(&showing, || {
            // SAFETY: The original product root/scroll are alive on the main
            // thread; the public parent getter creates no document graph.
            unsafe { bar.root.superview() }
        })
        .map_err(|_| "核对 Find 栏父视图时原权限已撤销")?;
        let scroll_parent = native_markdown_table_intent_step(&showing, || {
            // SAFETY: The same live-main-thread condition holds. Compare both
            // parents on this stack only; retain no external parent afterward.
            unsafe { surface.backend.scroll_view.superview() }
        })
        .map_err(|_| "核对正文父视图时原权限已撤销")?;
        let mounted = root_parent
            .as_ref()
            .zip(scroll_parent.as_ref())
            .is_some_and(|(root_parent, scroll_parent)| {
                std::ptr::eq(&**root_parent, &**scroll_parent)
            });
        let root_window = native_markdown_table_intent_step(&showing, || bar.root.window())
            .map_err(|_| "核对 Find 栏窗口时已撤权")?
            .ok_or("合成 Find 栏没有挂入实际窗口")?;
        let scroll_window =
            native_markdown_table_intent_step(&showing, || surface.backend.scroll_view.window())
                .map_err(|_| "核对正文窗口时已撤权")?
                .ok_or("合成正文已经离开原窗口")?;
        let hidden = native_markdown_table_intent_step(&showing, || bar.root.isHidden())
            .map_err(|_| "核对 Find 栏隐藏状态时已撤权")?;
        if !mounted || hidden || !std::ptr::eq(&*root_window, &*scroll_window) {
            return Err("合成 Find 栏没有实际挂载到原产品正文旁".into());
        }
        Ok(())
    });
    // Autorelease-pool drainage also releases Native claims and can reenter. The
    // successful mounting observation remains valid only under the original ticket.
    if !showing() {
        return Err("合成 Find 栏临时对象释放后原权限已撤销".into());
    }
    result?;
    Ok(bar)
}

/// Execute the actual matcher. SelectAll has no reveal receipt; Next must return
/// the same original Rc request created by the real action.
pub(in crate::native_surface) fn execute_multi_selection_find_probe(
    surface: &NativeMarkdownTextKitEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    bar: &Rc<NativeMarkdownFindBar>,
    intent: u64,
    action: MarkdownFindAction,
    expected: Option<std::ops::Range<usize>>,
    current: &impl Fn() -> bool,
) -> Result<Option<Rc<NativeMarkdownFindRevealRequest>>, String> {
    if !matches!(
        action,
        MarkdownFindAction::Next | MarkdownFindAction::SelectAll
    ) || (action == MarkdownFindAction::Next) != expected.is_some()
    {
        return Err("合成 Find 只允许已声明的 Next 或 SelectAll".into());
    }
    let revision = bar.layout_revision.get();
    let visible = bar.visible.get();
    let original = || {
        current()
            && controller.find_input_intent() == intent
            && !bar.closed.get()
            && bar.layout_revision.get() == revision
            && bar.visible.get() == visible
            && surface
                .find_bar
                .borrow()
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, bar))
    };
    if !original() || (action == MarkdownFindAction::SelectAll && !visible) {
        return Err("合成 Find 没有原栏或可见多选权限".into());
    }
    native_markdown_table_intent_step(&original, || {
        matching::execute_find_action(bar, &surface.backend.text_view, controller, intent, action)
    })
    .map_err(|_| "任务框真实Find执行期间已撤权")??;
    let Some(expected) = expected else {
        return Ok(None); // The caller verifies actual Multiple selection; invent no successful reveal receipt.
    };
    let request = surface
        .backend
        .text_view
        .observe_find_reveal_for_probe()
        .ok_or("真实Find未发布原定位请求")?;
    let context = native_markdown_table_intent_step(&original, || {
        surface.backend.text_view.pending_find_reveal_context()
    })
    .map_err(|_| "任务框Find定位上下文已撤权")?
    .ok_or("真实Find缺少原可执行上下文")?;
    if context.probe() != expected || !context.is_current(&surface.backend.text_view) || !original()
    {
        return Err("真实Find选中的不是预声明任务框内部x".into());
    }
    drop(context); // Native storage/controller claims cannot cross the next executor or mode transition.
    // Context acquisition can reenter. Equal coordinates do not prove identity
    // with the saved request; compare the original slot's Rc without Native calls
    // so a successor request cannot certify the original receipt.
    if !surface
        .backend
        .text_view
        .observe_find_reveal_for_probe()
        .is_some_and(|pending| Rc::ptr_eq(&pending, &request))
    {
        return Err("任务框Find上下文已由后继请求替换".into());
    }
    if !original() {
        return Err("任务框Find上下文释放后已撤权".into());
    }
    Ok(Some(request))
}

/// Hide through the actual bar transaction while keeping the editor's unique bar
/// owner; existing strict teardown remains responsible for its eventual release.
pub(in crate::native_surface) fn finish_list_marker_find_probe(
    surface: &NativeMarkdownTextKitEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    current: &impl Fn() -> bool,
) -> Result<(), String> {
    let bar = surface
        .find_bar
        .borrow()
        .clone()
        .ok_or("任务框Find失去原栏owner")?;
    native_markdown_table_intent_step(current, || {
        bar.hide(
            &surface.backend.scroll_view,
            &surface.backend.text_view,
            controller,
        )
    })
    .map_err(|_| "任务框Find收尾期间已撤权")??;
    drop(bar); // The editor still owns the same bar; strict teardown releases controls and query state.
    current()
        .then_some(())
        .ok_or_else(|| "任务框Find收尾后原权限已撤销".into())
}
