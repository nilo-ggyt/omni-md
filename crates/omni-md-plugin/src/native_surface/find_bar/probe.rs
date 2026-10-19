//! 同一Native9图上的真实Find接点；测试权限由原owner/root/epoch签发，不向全局前台槽安装替身。

use super::*;

/// 所有合成 Find 场景复用原 editor 的唯一栏 owner；不另建前台槽或第二份控件实现。
fn find_probe_bar(
    surface: &NativeMarkdownEditorSurface,
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
            // 栏仍登记在原editor的正常账本；局部构造完成且原票有效后才发布唯一owner。
            let created = native_markdown_table_intent_step(current, || {
                NativeMarkdownFindBar::new(
                    controller,
                    &surface.backend.owned_ledger,
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
    // 固定小查询只用于合成回归；保留生产Next允许的隐藏导航，不额外抢焦点或改变正文可见高度。
    step(&mut || bar.query.setStringValue(ns_string!("x")))?;
    step(&mut || bar.replacement.setStringValue(ns_string!("")))?;
    step(&mut || bar.case.setState(objc2_app_kit::NSControlStateValueOff))?;
    step(&mut || bar.regex.setState(objc2_app_kit::NSControlStateValueOff))?;
    step(&mut || bar.word.setState(objc2_app_kit::NSControlStateValueOff))?;
    Ok(())
}

pub(in crate::native_surface) fn begin_list_marker_find_probe(
    surface: &NativeMarkdownEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    intent: u64,
    expected: std::ops::Range<usize>,
    current: &impl Fn() -> bool,
) -> Result<Rc<NativeMarkdownFindRevealRequest>, String> {
    if !current() || controller.find_input_intent() != intent {
        return Err("任务框 Find 未持有原输入权限".into()); // 旧 intent 在任何栏构造或字段 setter 前拒绝。
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

/// 只复制原栏的纯 stamp；不读取 Native、不保控件，供外层原票跨 helper 与 pool 保持权限。
pub(in crate::native_surface) fn multi_selection_find_probe_bar_stamp(
    surface: &NativeMarkdownEditorSurface,
) -> Option<(u64, bool)> {
    surface.find_bar.borrow().as_ref().and_then(|bar| {
        (!bar.closed.get()).then_some((bar.layout_revision.get(), bar.visible.get()))
    })
}

/// 多匹配动作实际挂载生产栏；visible 与布局代次只由原 show 事务更新。
pub(in crate::native_surface) fn prepare_multi_selection_find_probe(
    surface: &NativeMarkdownEditorSurface,
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
            // SAFETY: 原产品 root/scroll 主线程存活；公开父视图 getter 不建立新的文本图。
            unsafe { bar.root.superview() }
        })
        .map_err(|_| "核对 Find 栏父视图时原权限已撤销")?;
        let scroll_parent = native_markdown_table_intent_step(&showing, || {
            // SAFETY: 同上，两个结果只在本调用栈比较，未保存外部父视图。
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
    if !showing() {
        return Err("合成 Find 栏临时对象释放后原权限已撤销".into());
    }
    result?;
    Ok(bar)
}

/// 只执行真实匹配器；多选没有 Reveal 收据，Next 则必须交还同一原 Rc 请求。
pub(in crate::native_surface) fn execute_multi_selection_find_probe(
    surface: &NativeMarkdownEditorSurface,
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
        return Ok(None); // SelectAll 必须由调用方核实际 Multiple，不能制造已定位的成功收据。
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
    drop(context); // 原生storage/controller claims不跨下一有界executor或后续模式转换。
    // context取得期间可能发生原生重入；坐标相同不能证明仍属于先前保存的request。
    // 此处只读原槽并比较Rc身份，中间不插入Native调用，后继请求不得替原收据作证。
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

pub(in crate::native_surface) fn finish_list_marker_find_probe(
    surface: &NativeMarkdownEditorSurface,
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
    drop(bar); // editor继续正常拥有同一栏；既有strict teardown负责其控件与查询释放。
    current()
        .then_some(())
        .ok_or_else(|| "任务框Find收尾后原权限已撤销".into())
}
