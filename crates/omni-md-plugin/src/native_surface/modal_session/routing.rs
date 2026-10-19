//! 弹窗标准Undo/Redo路由：既有dialog owner加入实际field editor的公开响应链，只连接原生历史。

use super::*;
use objc2_foundation::NSUndoManager;

/// weak保留首次接链的实际角色；响应链的unsafe_unretained边不能成为第二强owner。
struct RouteBinding {
    editor: Weak<NSTextView>,
    original_next: Option<Weak<NSResponder>>,
}

#[derive(Default)]
/// 一次接链记录与有限重入门禁；不拥有editor、manager或正文。
pub(super) struct ModalUndoRoute {
    binding: RefCell<Option<Rc<RouteBinding>>>,
    installing: Cell<bool>,
    validating: Cell<bool>,
    performing: Cell<bool>,
}

/// 原生调用返回后只撤销本层重入权限；Drop不触碰输入或历史。
struct BusyGuard<'a>(&'a Cell<bool>);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        // guard只由成功取得标量权限的外层创建，嵌套拒绝没有资格解除外层Busy。
        self.0.set(false);
    }
}

/// 最后一个native调用之后用纯Rust权限复核，防止getter/retain重入已接受清理却返回旧快照。
fn local_permission(owner: &NativeMarkdownModalInput) -> bool {
    owner.ivars().active.get()
        && MODAL_ACTIVE.with(Cell::get) == Some(owner.ivars().generation)
        && owner.ivars().provider.permits_modal_command()
        && owner.ivars().undo.preflight().is_ok()
        && owner.ivars().borrowed.failure().is_none()
        && owner.ivars().failure.borrow().is_none()
}

/// 只允许本dialog仍活动的actual client；所有Rust借用在native getter前结束。
fn active_editor(owner: &NativeMarkdownModalInput) -> Option<Retained<NSTextView>> {
    if !owner.is_current() || !local_permission(owner) {
        // stale、closing、构造/结束Busy或ownership首错都保留原输入，不借出执行权限。
        return None;
    }
    let field = owner.ivars().field.get()?.load()?;
    let window = owner.ivars().window.get()?.load()?;
    let editor = field.currentEditor()?.downcast::<NSTextView>().ok()?;
    let concrete = editor.downcast_ref::<NativeMarkdownSessionFieldEditor>()?;
    if !concrete.belongs_to(&owner.ivars().undo, &owner.ivars().ledger)
        || !field
            .window()
            .is_some_and(|actual| std::ptr::eq(&*actual, &*window))
        || !editor
            .window()
            .is_some_and(|actual| std::ptr::eq(&*actual, &*window))
        || !window.firstResponder().is_some_and(|actual| {
            // 活对象只比较身份；字段currentEditor与firstResponder必须指向同一具体输入editor。
            std::ptr::eq(
                AsRef::<AnyObject>::as_ref(&*actual),
                AsRef::<AnyObject>::as_ref(&*editor),
            )
        })
    {
        // 默认/其他客户端editor、失焦字段或另一window不能操作这个dialog的历史。
        return None;
    }
    let application = NSApplication::sharedApplication(owner.mtm());
    if !application
        .keyWindow()
        .is_some_and(|actual| std::ptr::eq(&*actual, &*window))
        || !application
            .modalWindow()
            .is_some_and(|actual| std::ptr::eq(&*actual, &*window))
    {
        // 主MD或后台窗口的菜单/迟到动作不能借此跨modal执行历史。
        return None;
    }
    // SAFETY: 实际NSTextView是主线程活对象，公开输入协议getter只读取marked状态，不提交/取消IME。
    let marked: bool = unsafe { msg_send![&*editor, hasMarkedText] };
    if marked || !local_permission(owner) {
        // 组合输入先由AppKit/IME自己的提交或取消语义处理，不能同时执行整字段历史。
        return None;
    }
    Some(editor)
}

/// 两个原生setter各自返回后重新验证；补偿始终使用外层仍持有的weak记录，不依赖共享槽尚未被take。
fn install_steps(
    attach_owner: impl FnOnce(),
    still_current: impl Fn() -> bool,
    attach_editor: impl FnOnce(),
    rollback: impl FnOnce(),
) {
    attach_owner();
    if !still_current() {
        // 第一个setter可能重入accepted finish；此时绝不能再把editor接到已结束的owner。
        rollback();
        return;
    }
    attach_editor();
    if !still_current() {
        // 第二个setter也可能在finish已解链之后才写回自己的边；局部记录仍可撤销本层留下的边。
        rollback();
    }
}

/// 真实begin/change通知之后仅安装一次；AppKit后续重接只禁用此路由，不叠加fallback。
pub(super) fn install_after_editing(owner: &NativeMarkdownModalInput) {
    if owner.ivars().routing.installing.replace(true) {
        // setter可能重入delegate，外层的第一次接链拥有唯一发布权。
        return;
    }
    let _installing = BusyGuard(&owner.ivars().routing.installing);
    if owner.ivars().routing.binding.borrow().is_some() {
        // 一旦安装过就不再次改链；若AppKit改写，后续动作门禁拒绝，不覆盖新的框架后继。
        return;
    }
    let Some(editor) = active_editor(owner) else {
        // begin通知可能早于最终firstResponder安装；后续真实change可再尝试，绝不计时重试。
        return;
    };
    // SAFETY: 两个responder都是本栈活对象，原后继由AppKit现有window/control图保活；getter不转移claim。
    let original = unsafe { editor.nextResponder() };
    let self_responder: &NSResponder = owner;
    if original
        .as_ref()
        .is_some_and(|next| std::ptr::eq(&**next, self_responder))
    {
        // 没有项目记录却已指向自己意味着未知链状态，拒绝制造self循环或猜测原后继。
        return;
    }
    let binding = Rc::new(RouteBinding {
        editor: Weak::from_retained(&editor),
        original_next: original.as_ref().map(Weak::from_retained),
    });
    if !owner.is_current() || !local_permission(owner) {
        // weak构造与原后继getter均已返回；重入结束不能发布新的安装记录或触碰链。
        return;
    }
    // 在native setter前发布纯Rust weak记录，重入由installing阻止；不持RefCell借用跨消息。
    let previous = owner
        .ivars()
        .routing
        .binding
        .replace(Some(Rc::clone(&binding)));
    drop(previous);
    install_steps(
        || {
            // SAFETY: owner由本dialog既有Retained保持，原后继由局部claim及原window图保持；边不retain。
            unsafe { owner.setNextResponder(original.as_deref()) };
        },
        || {
            // 先完成可能重入的parent弱升级，再以纯Rust权限和本层Rc身份复核，不把空槽当作仍已发布。
            owner.is_current()
                && local_permission(owner)
                && owner
                    .ivars()
                    .routing
                    .binding
                    .borrow()
                    .as_ref()
                    .is_some_and(|current| {
                        // Rc身份只决定Rust安装记录的发布权；native角色仍由各自真实weak证明。
                        Rc::ptr_eq(current, &binding)
                    })
        },
        || {
            // SAFETY: 第一个setter返回后本层仍拥有活动权限；owner的Retained覆盖此unretained链边。
            unsafe { editor.setNextResponder(Some(owner)) };
        },
        || {
            // 即使重入finish已经take共享槽，局部weak记录仍完整，能够撤掉本层晚写的边。
            restore_binding(owner, &binding);
            let remains_ours =
                owner
                    .ivars()
                    .routing
                    .binding
                    .borrow()
                    .as_ref()
                    .is_some_and(|current| {
                        // 不能take其他较晚发布的安装记录。
                        Rc::ptr_eq(current, &binding)
                    });
            if remains_ours {
                // 只撤销本层纯Rust记录；drop发生在借用外，不制造新的native owner。
                drop(owner.ivars().routing.binding.take());
            }
        },
    );
}

/// 一次真实weak观察；不以原地址推断原后继仍活，也不覆盖AppKit后来安装的新链。
fn binding_is_current(owner: &NativeMarkdownModalInput, editor: &NSTextView) -> bool {
    let binding = owner
        .ivars()
        .routing
        .binding
        .borrow()
        .as_ref()
        .map(Rc::clone);
    let Some(binding) = binding else {
        // 尚未完成实际接链时没有历史动作权限。
        return false;
    };
    if !binding
        .editor
        .load()
        .is_some_and(|expected| std::ptr::eq(&*expected, editor))
    {
        // 共享editor已换client/对象时，旧记录不能授权当前字段。
        return false;
    }
    let original = match &binding.original_next {
        Some(next) => match next.load() {
            // 原后继临时claim覆盖下面的unsafe链getter；弱失效不能被假装成最初nil。
            Some(next) => Some(next),
            // 原后继已释放，此次链身份无法安全证明，保留输入并拒绝动作。
            None => return false,
        },
        // 最初nil与后来weak失效有不同语义；只有最初nil允许当前nil。
        None => None,
    };
    // SAFETY: editor/owner均活，原后继已在上述分支升级或原本nil；只比较当前公开链角色。
    let (actual_next, owner_next) = unsafe { (editor.nextResponder(), owner.nextResponder()) };
    actual_next.as_ref().is_some_and(|next| {
        // 两边都是真实活responder，比较不泄漏对象地址。
        std::ptr::eq(&**next, AsRef::<NSResponder>::as_ref(owner))
    }) && match (owner_next, original) {
        // 原有命令后继必须完整保持，而非接到父窗口或另一个panel。
        (Some(actual), Some(expected)) => std::ptr::eq(&*actual, &*expected),
        // 最初链终点仍然是终点。
        (None, None) => true,
        // AppKit或其他逻辑改写后继后不再执行本候选历史，也不再次抢接。
        _ => false,
    }
}

/// 返回一个只在本动作栈存活的cached历史claim；无cached输入时不创建manager。
fn current_manager(owner: &NativeMarkdownModalInput) -> Option<Retained<NSUndoManager>> {
    if owner.ivars().routing.installing.get() || owner.ivars().routing.performing.get() {
        // 安装或另一个历史动作执行期只拒绝嵌套请求，不清历史也不毒化正常owner。
        return None;
    }
    let editor = active_editor(owner)?;
    if !binding_is_current(owner, &editor) {
        // 只读菜单验证不能因无历史而创建新的manager；失效链也不能通过另一条路径补救。
        return None;
    }
    // cached入口内部先做同主线程的纯槽门禁，再进入原take/retain/恢复流程；本次不构造缺席历史。
    let manager = owner.ivars().undo.cached_manager(&owner.ivars().ledger)?;
    let actual = editor.undoManager()?;
    if !std::ptr::eq(&*manager, &*actual)
        || !owner.ivars().undo.owns_observed_manager(&manager)
        || manager.isUndoing()
        || manager.isRedoing()
        || manager.groupingLevel() > 1
        || active_editor(owner).is_none()
        || !binding_is_current(owner, &editor)
        || !local_permission(owner)
    {
        // 原生getter可能重入；最终再核对权限。嵌套group不是undo()可安全关闭的顶层group。
        return None;
    }
    Some(manager)
}

/// 标准菜单validation只回答真实栈是否可执行；不更新菜单、不结束group、不发送动作。
pub(super) fn can_perform(owner: &NativeMarkdownModalInput, redo: bool) -> bool {
    if owner.ivars().routing.validating.replace(true) {
        // canRedo会发checkpoint，菜单验证可能同步重入；内层只拒绝，不能复位外层验证权限。
        return false;
    }
    let _validating = BusyGuard(&owner.ivars().routing.validating);
    let Some(manager) = current_manager(owner) else {
        // 无合法当前输入或history时禁用本动作，不能回退到alert默认manager。
        return false;
    };
    if redo {
        // 原生canRedo可能发checkpoint；返回前再校验session/链仍授权同一输入。
        manager.canRedo() && current_manager(owner).is_some()
    } else {
        // canUndo=true仅说明存在历史，上方group/执行期/marked门禁才决定是否允许真正调用。
        manager.canUndo() && current_manager(owner).is_some()
    }
}

/// 键盘与菜单共用同一公开action入口；直接执行已经验证归属的原生manager，不伪造输入或历史。
pub(super) fn perform(owner: &NativeMarkdownModalInput, redo: bool) {
    if owner.ivars().routing.validating.get() {
        // 菜单只读checkpoint期间不能重入历史mutation；外层验证返回后用户可重新执行。
        return;
    }
    let Some(manager) = current_manager(owner) else {
        // 迟到/失焦/IME/Busy请求是无操作，保留用户值与原有Undo记录。
        return;
    };
    let available = if redo {
        // Redo查询与Undo分离，不改变原分组配置。
        manager.canRedo()
    } else {
        // Foundation允许undo()自己结束groupingLevel=1的顶层event组；这里不手工endUndoGrouping。
        manager.canUndo()
    };
    if !available
        || current_manager(owner).is_none()
        || owner.ivars().routing.performing.replace(true)
    {
        // checkpoint/getter重入后权限可能变化；拒绝时尚未进入任何历史mutation。
        return;
    }
    let _performing = BusyGuard(&owner.ivars().routing.performing);
    if !local_permission(owner) {
        // current_manager返回会释放其native局部claims；drop若重入封闭，此处不再查询native就拒绝最后动作。
        return;
    }
    if redo {
        // 唯一已登记的实际manager恢复NSTextView真实注册的操作；此处没有Rust借用覆盖原生回调。
        manager.redo();
    } else {
        // 标准undo负责其公开顶层组规则，真实didChange仍走原有输入与ownership观察。
        manager.undo();
    }
}

/// 清理和安装补偿共用同一实际边恢复；共享槽已被重入take也不丢掉局部weak身份。
fn restore_binding(owner: &NativeMarkdownModalInput, binding: &RouteBinding) {
    // 所有RefCell借用已结束；这些短暂claims只包围一次解链，不保活至后继会话。
    let editor = binding.editor.load();
    let original = binding.original_next.as_ref().and_then(Weak::load);
    if let Some(editor) = editor {
        // SAFETY: editor与本owner均由本栈保持；读取当前链只用于避免覆盖别人后来安装的新边。
        let current = unsafe { editor.nextResponder() };
        if current.as_ref().is_some_and(|next| {
            // 仅本节点仍是actual next时才拥有恢复这条边的权限。
            std::ptr::eq(&**next, AsRef::<NSResponder>::as_ref(owner))
        }) {
            // SAFETY: 有效原后继由局部claim保持；若原后继已nil则断开，绝不复用过期地址。
            unsafe { editor.setNextResponder(original.as_deref()) };
        }
    }
    // SAFETY: 本节点是dialog自己的owner；退出时撤销它的unretained后继，不能留下指向旧window的能力。
    unsafe { owner.setNextResponder(None) };
}

/// accepted cleanup先断开自己的节点；只有实际边仍属于本次安装时才恢复原editor后继。
pub(super) fn detach(owner: &NativeMarkdownModalInput) {
    let binding = owner.ivars().routing.binding.take();
    if let Some(binding) = binding {
        // 共享槽提前清空使重入无法继续命令，但本层Rc仍保留恢复所需weak记录。
        restore_binding(owner, &binding);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 第一个setter重入清理并take记录后，绝不能继续给editor安装已结束的owner。
    fn first_setter_finishes_before_editor_is_attached() {
        let active = Cell::new(true);
        let record_published = Cell::new(true);
        let editor_writes = Cell::new(0);
        let compensations = Cell::new(0);
        install_steps(
            || {
                // 模拟native setter重入已接受的finish；共享槽和活动权限都已撤销。
                active.set(false);
                record_published.set(false);
            },
            || active.get() && record_published.get(),
            || editor_writes.set(editor_writes.get() + 1),
            || compensations.set(compensations.get() + 1),
        );
        assert_eq!(editor_writes.get(), 0);
        assert_eq!(compensations.get(), 1);
    }

    #[test]
    /// 第二个setter在finish解链后晚写自己的边，补偿仍须用局部记录运行，不能依赖已空共享槽。
    fn second_setter_finishes_then_returns_a_late_link() {
        let active = Cell::new(true);
        let record_published = Cell::new(true);
        let editor_is_ours = Cell::new(false);
        let compensations = Cell::new(0);
        install_steps(
            || {},
            || active.get() && record_published.get(),
            || {
                // native内部先重入finish，随后完成原setter的晚写，这是原实现遗漏的反例。
                active.set(false);
                record_published.set(false);
                editor_is_ours.set(true);
            },
            || {
                // 外层记录仍在局部；即使共享槽已空也撤销自己的晚写边。
                compensations.set(compensations.get() + 1);
                editor_is_ours.set(false);
            },
        );
        assert!(!editor_is_ours.get());
        assert_eq!(compensations.get(), 1);
    }

    #[test]
    /// 两个setter均正常返回时只完成一次安装，不能执行accepted补偿或清输入历史。
    fn normal_installation_keeps_both_links() {
        let writes = Cell::new(0);
        install_steps(
            || writes.set(writes.get() + 1),
            || true,
            || writes.set(writes.get() + 1),
            || panic!("a current input must keep its installed route"),
        );
        assert_eq!(writes.get(), 2);
    }
}
