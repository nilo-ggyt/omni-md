//! 项目 modal 的栈权限、独占标准输入和公开清理；不实现文件或正文操作。

use super::*;
use objc2_app_kit::{
    NSComboBoxCell, NSComboBoxDelegate, NSResponder, NSText, NSTextFieldCell,
    NSUserInterfaceValidations, NSValidatedUserInterfaceItem, NSWindow,
};

mod routing;

/// native 字段先限 UTF-16，再限返回 UTF-8；超限拒绝整项，不截断合法前缀。
const MAX_MODAL_INPUT_UNITS: usize = 16_384;
const MAX_MODAL_INPUT_BYTES: usize = 16_384;

thread_local! {
    /// 只存有限代次；实际控制器活性由 guard/cell 的 weak owner 另外核对。
    static MODAL_ACTIVE: Cell<Option<u64>> = const { Cell::new(None) };
    static MODAL_GENERATION: Cell<u64> = const { Cell::new(0) };
}

#[derive(Debug, Error)]
/// 取消不是错误；未通过辞任的输入必须原位恢复，不能授权文件 mutation 或 accepted 清理。
pub(super) enum NativeMarkdownModalError {
    #[error("另一笔记对话框仍在完成中")]
    Busy,
    #[error("笔记对话框所属会话已结束")]
    StaleOwner,
    #[error("无法创建新的笔记对话框，请重新打开笔记窗口")]
    Exhausted,
    #[error("对话框输入超过长度上限")]
    InputTooLong,
    #[error("系统文件选择器无法打开")]
    Aborted,
    #[error("笔记对话框返回了无法识别的结果：{0}")]
    UnexpectedResponse(isize),
    #[error("{0}")]
    Boundary(String),
}

/// 纯标量门禁在任何 save/flush/take/admission 之前调用，绝不发送原生 getter。
pub(super) fn preflight_native_markdown_modal() -> Result<(), String> {
    if MODAL_ACTIVE.with(Cell::get).is_some() {
        // dialog 的输入、候选和局部 owner 仍在栈内；关闭不能先改变父草稿或权限。
        return Err(NativeMarkdownModalError::Busy.to_string());
    }
    Ok(())
}

/// 纯标量claim在任何native对象构造之前完成；耗尽与嵌套都不改变已接受代次。
fn claim_modal_generation() -> Result<u64, NativeMarkdownModalError> {
    if MODAL_ACTIVE.with(Cell::get).is_some() {
        // 原modal仍独占，失败不得增加代次或覆盖其claim。
        return Err(NativeMarkdownModalError::Busy);
    }
    let next = MODAL_GENERATION.with(|generation| {
        generation
            .get()
            .checked_add(1)
            .ok_or(NativeMarkdownModalError::Exhausted)
    })?;
    MODAL_GENERATION.with(|generation| generation.set(next));
    MODAL_ACTIVE.with(|active| active.set(Some(next)));
    Ok(next)
}

/// 旧guard或重复结束不能撤销另一个仍有效的claim。
fn release_modal_generation(generation: u64) {
    MODAL_ACTIVE.with(|active| {
        if active.get() == Some(generation) {
            // 只做纯标量恢复；native/业务owner必须已在调用方局部pool内结束。
            active.set(None);
        }
    });
}

/// 参数已经由原生weak实际升级为活controller；借出标量只补充同owner的栈权限。
fn modal_parent_is_current(parent: &NativeMarkdownController) -> bool {
    native_markdown_controller_is_current(parent)
        || BORROWED_NATIVE_MARKDOWN_OWNER.with(|owner| {
            // 父autosave/结果事务可暂借Active；不允许它使正在modal中编辑的自有cell退回未知provider。
            // 地址只比较已确认活parent的既有借出身份，本身不证明资源活性，也不授权Closing owner。
            owner.get() == Some(std::ptr::from_ref(parent).addr())
        })
}

/// 只在本次栈内持有 weak/scalar；不借出整份 Active surface 跨越嵌套 modal 循环。
pub(super) struct NativeMarkdownModalUseGuard {
    generation: u64,
    parent: Weak<NativeMarkdownController>,
    pub(super) ledger: Rc<NativeMarkdownOwnedLedger>,
    finished_input: RefCell<Option<NativeMarkdownBorrowedEditorSnapshot>>,
}

impl NativeMarkdownModalUseGuard {
    /// 先拒绝嵌套和过期 owner，再取得当前 editor/root 的同 backing Modal handle。
    pub(super) fn enter(
        controller: &NativeMarkdownController,
    ) -> Result<Self, NativeMarkdownModalError> {
        MainThreadMarker::new().ok_or(NativeMarkdownModalError::StaleOwner)?;
        preflight_native_markdown_modal().map_err(|_| NativeMarkdownModalError::Busy)?;
        let ledger = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active
                .as_ref()
                .ok_or(NativeMarkdownModalError::StaleOwner)?;
            if !std::ptr::eq(&*surface.controller, controller) {
                // 实际 live controller 的借用身份不一致，不能把旧回调绑定到新会话。
                return Err(NativeMarkdownModalError::StaleOwner);
            }
            surface
                .preflight_modal_boundary()
                .map_err(NativeMarkdownModalError::Boundary)?;
            if surface.pending_note_switch.is_some() || surface.pending_file_mutation.is_some() {
                // 已接受的 note/file 交接拥有其原始资源边界，不能再分配 dialog。
                return Err(NativeMarkdownModalError::Busy);
            }
            let source = surface
                .editor
                .as_ref()
                .map_or(&*surface.owned_ledger, |editor| {
                    // 活动笔记的 Modal 角色同时进入 editor 局部清单和共同 root。
                    editor.owned_ledger()
                });
            Ok(Rc::new(source.modal_scope()))
        })?;
        let generation = claim_modal_generation()?;
        let mut guard = Self {
            generation,
            parent: Weak::default(),
            ledger,
            finished_input: RefCell::new(None),
        };
        // weak 初始化也进入 runtime；纯门禁已经生效，重入不能接受父窗口关闭。
        guard.parent = Weak::from(controller);
        if !guard.is_current() {
            // 初始化期间若 owner 已失效，guard 的 Drop 撤销本次纯标量权限。
            return Err(NativeMarkdownModalError::StaleOwner);
        }
        Ok(guard)
    }

    /// weak 只证明存活；同时核对真实 Active owner 和本次有限代次。
    fn is_current(&self) -> bool {
        MODAL_ACTIVE.with(Cell::get) == Some(self.generation)
            && self.parent.load().is_some_and(|parent| {
                // load 的临时强引用不跨越 ACTIVE 的纯身份借用。
                modal_parent_is_current(&parent)
            })
    }

    /// 清理失败保留父 session 首因；普通取消、Busy 或字段预算错误不毒化父历史。
    pub(super) fn record_cleanup_failure(&self, error: &str) {
        let Some(parent) = self.parent.load() else {
            // 没有旧 owner 可写，不能把首因转嫁给后来新建的 session。
            return;
        };
        ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow_mut(|active| {
            if let Some(surface) = active.as_mut()
                && std::ptr::eq(&*surface.controller, &*parent)
            {
                // 这里只更新有限 Rust 文本，不在 owner 借用内调用原生 API。
                surface
                    .modal_failure
                    .get_or_insert_with(|| error.to_owned());
            }
        });
    }

    /// 调用方已结束局部pool；现在仅由weak事实再次复核，不保留dialog原生owner。
    pub(super) fn complete<T>(
        &self,
        result: Result<T, NativeMarkdownModalError>,
    ) -> Result<T, NativeMarkdownModalError> {
        let borrowed = self.finished_input.borrow().clone();
        if let Some(borrowed) = borrowed
            && !borrowed.is_detached()
        {
            // pool内的名义清理不足以授权后继；末尾真实公开事实仍失败时保留首因。
            let error = borrowed
                .failure()
                .unwrap_or_else(|| "对话框输入尚未清空或解除关联".into());
            self.record_cleanup_failure(&error);
            return Err(NativeMarkdownModalError::Boundary(error));
        }
        result
    }
}

impl Drop for NativeMarkdownModalUseGuard {
    fn drop(&mut self) {
        // 只结束自己取得的claim，不retain/release native，不运行UI或保存。
        release_modal_generation(self.generation);
    }
}

/// 一个 dialog 的标准输入 owner；所有原生回边都是 weak。
pub(super) struct ModalInputIvars {
    ledger: Rc<NativeMarkdownOwnedLedger>,
    undo: Rc<NativeMarkdownSessionUndo>,
    provider: NativeMarkdownFieldEditorProvider,
    borrowed: NativeMarkdownBorrowedEditorSnapshot,
    parent: Weak<NativeMarkdownController>,
    field: OnceCell<Weak<NSTextField>>,
    window: OnceCell<Weak<NSWindow>>,
    generation: u64,
    active: Cell<bool>,
    failure: RefCell<Option<String>>,
    routing: routing::ModalUndoRoute,
}

define_class!(
    // SAFETY: 主线程NSResponder保留公开文本delegate；ivar完整初始化，链节点由本dialog owner保活。
    #[unsafe(super = NSResponder)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ModalInputIvars]
    pub(super) struct NativeMarkdownModalInput;

    // SAFETY: 初始化的NSResponder子类继承标准NSObject身份，并仅在AppKit主线程使用。
    unsafe impl NSObjectProtocol for NativeMarkdownModalInput {}
    // SAFETY: 声明的notification selector ABI匹配公开协议；清理先断开字段delegate。
    unsafe impl NSControlTextEditingDelegate for NativeMarkdownModalInput {
        #[unsafe(method(control:textShouldEndEditing:))]
        /// 真实辞任前只核验原客户端及历史；拒绝时不结束undo组、不清输入或backing。
        fn should_end(&self, control: &NSControl, editor: &NSText) -> bool {
            self.preflight_resignation(control, editor).is_ok()
        }

        #[unsafe(method(controlTextDidBeginEditing:))]
        /// 真实开始通知只观察本字段实际editor/history，不提前结束当前undo组。
        fn did_begin(&self, notification: &NSNotification) {
            self.observe(notification, false);
        }
        #[unsafe(method(controlTextDidChange:))]
        /// 实际输入变化继续核对归属；同一焦点客户端的Undo/Redo必须保持可用。
        fn did_change(&self, notification: &NSNotification) {
            self.observe(notification, false);
        }
        #[unsafe(method(controlTextDidEndEditing:))]
        /// 原生验证成功后才收到的结束通知复用统一焦点历史边界。
        fn did_end(&self, notification: &NSNotification) {
            // 只有真实成功结束通知能结束焦点内历史；provider getter与failed shouldEnd不清历史。
            self.observe(notification, true);
        }
    }
    // SAFETY: NSTextFieldDelegate继承上述公开控制文本通知，遵循同一main-thread生命周期。
    unsafe impl NSTextFieldDelegate for NativeMarkdownModalInput {}
    // SAFETY: combo保留AppKit标准建议/选项行为，本类只采用继承的文本通知。
    unsafe impl NSComboBoxDelegate for NativeMarkdownModalInput {}

    // SAFETY: 标准公开validation的参数/BOOL ABI一致；返回值不改变manager、输入或modal权限。
    unsafe impl NSUserInterfaceValidations for NativeMarkdownModalInput {
        #[unsafe(method(validateUserInterfaceItem:))]
        /// 只给本节点实现的两个历史动作授权；其他菜单动作沿原响应链寻找原有接收者。
        fn validate_item(&self, item: &ProtocolObject<dyn NSValidatedUserInterfaceItem>) -> bool {
            match item.action() {
                // 标准Undo菜单查询已存在的真实历史，不构造新manager。
                Some(action) if action == sel!(undo:) => routing::can_perform(self, false),
                // Redo独立查询原生redo栈，不以Undo是否可用推断。
                Some(action) if action == sel!(redo:) => routing::can_perform(self, true),
                // 本节点没有这些动作的实现，不能替其他responder宣称可执行。
                _ => false,
            }
        }
    }

    impl NativeMarkdownModalInput {
        #[unsafe(method(undo:))]
        /// 标准nil-target路由到本节点后，只执行该活输入当前已登记的历史。
        fn undo_input(&self, _sender: Option<&AnyObject>) {
            routing::perform(self, false);
        }

        #[unsafe(method(redo:))]
        /// 不另建业务Undo模型；恢复由实际NSTextView注册的原生redo。
        fn redo_input(&self, _sender: Option<&AnyObject>) {
            routing::perform(self, true);
        }
    }
);

impl NativeMarkdownModalInput {
    /// 委托只拥有本dialog历史/provider，不共享父查询或文档的undo。
    fn new(guard: &NativeMarkdownModalUseGuard, mtm: MainThreadMarker) -> Retained<Self> {
        let undo = Rc::new(NativeMarkdownSessionUndo::default());
        let borrowed = NativeMarkdownBorrowedEditorSnapshot::default();
        let ledger = Rc::clone(&guard.ledger);
        let provider = NativeMarkdownFieldEditorProvider::new(&undo, &ledger, &borrowed);
        let allocated = Self::alloc(mtm).set_ivars(ModalInputIvars {
            ledger,
            undo,
            provider,
            borrowed,
            parent: guard.parent.clone(),
            field: OnceCell::new(),
            window: OnceCell::new(),
            generation: guard.generation,
            active: Cell::new(true),
            failure: RefCell::new(None),
            routing: routing::ModalUndoRoute::default(),
        });
        // SAFETY: 公开NSResponder initializer消费已经完整初始化ivar的主线程子类allocation。
        unsafe { msg_send![super(allocated), init] }
    }

    /// 安装固定weak身份，不在RefCell借用内发送native消息或替换第二套字段。
    fn bind(&self, field: &NSTextField, window: &NSWindow) -> Result<(), NativeMarkdownModalError> {
        self.ivars().field.set(Weak::from(field)).map_err(|_| {
            // 重复绑定不是合法modal复用，拒绝让旧cell指向另一客户端。
            NativeMarkdownModalError::Boundary("对话框输入被重复关联".into())
        })?;
        self.ivars().window.set(Weak::from(window)).map_err(|_| {
            // 窗口身份只能构造时安装一次，不能借此复活已结束的provider。
            NativeMarkdownModalError::Boundary("对话框窗口被重复关联".into())
        })?;
        // SAFETY: 字段弱delegate由本次Retained输入owner保活，公开协议ABI一致并在finish中先断开。
        unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
        Ok(())
    }

    /// actual owner与epoch共同授权；不以曾经存储的地址或单个active标志证明活性。
    fn is_current(&self) -> bool {
        self.ivars().active.get()
            && MODAL_ACTIVE.with(Cell::get) == Some(self.ivars().generation)
            && self.ivars().parent.load().is_some_and(|parent| {
                // 弱升级的临时claim仅包围纯身份核对，不进入父surface的可变借用。
                modal_parent_is_current(&parent)
            })
    }

    /// cell公开扩展点只为exact字段/exact窗口提供本dialog按需editor。
    fn provide(&self, control: &NSView) -> Option<Retained<NSTextView>> {
        if !self.is_current() {
            // late cell查询不分配新的editor/history，不接管未知框架客户端。
            return None;
        }
        let field = self.ivars().field.get()?.load()?;
        let window = self.ivars().window.get()?.load()?;
        let expected_view: &NSView = &field;
        if !std::ptr::eq(expected_view, control)
            || !control.window().is_some_and(|actual| {
                // 两个window都是活owner；不把父panel或另一alert的输入混入本provider。
                std::ptr::eq(&*actual, &*window)
            })
        {
            // 不匹配的controlView只能由其自己的标准AppKit路径处理。
            return None;
        }
        let editor = self.ivars().provider.provide(Some(&field), self.mtm())?;
        match editor.downcast::<NSTextView>() {
            Ok(editor) if self.is_current() => {
                // provider内部完成closed/creating/ledger校验后，返回标准实际field editor。
                Some(editor)
            }
            Ok(_) => {
                // provider返回期间本dialog已封闭，局部claim在此丢弃，不再发布。
                None
            }
            Err(_) => {
                // 具体provider承诺NSTextView；类型不符保留首因，不能把未知对象当输入editor。
                self.record_failure("对话框输入控件类型无效".into());
                None
            }
        }
    }

    /// notification来自本字段且owner仍有效时，观察实际历史或成功结束焦点历史。
    fn observe(&self, notification: &NSNotification, did_end: bool) {
        if !self.is_current() {
            // accepted cleanup后的迟到通知不能恢复业务或清后来会话的历史。
            return;
        }
        let Some(field) = notification
            .object()
            .and_then(|object| object.downcast::<NSTextField>().ok())
        else {
            // 外来notification不具备可猜测的文本字段身份。
            return;
        };
        if !self
            .ivars()
            .field
            .get()
            .and_then(Weak::load)
            .is_some_and(|expected| {
                // 只有本次绑定的实际客户端才可改变本dialog的历史边界。
                std::ptr::eq(&*expected, &*field)
            })
        {
            return;
        }
        let result = if did_end {
            // 复用统一焦点策略，不在本模块复制另一套Undo coalescing/clear实现。
            self.ivars().borrowed.finished_control_editing(
                &field,
                &self.ivars().undo,
                &self.ivars().ledger,
            )
        } else {
            // begin/change只核验真实editor/manager，不清当前客户端仍可用的历史。
            self.ivars()
                .borrowed
                .observe_editing(&field, &self.ivars().undo, &self.ivars().ledger)
        };
        if let Err(error) = result {
            // delegate没有Result通道；确认返回前会传播这项首因并拒绝文件操作。
            self.record_failure(error);
        } else if !did_end {
            // actual editor安装后才接入标准响应链；成功didEnd绝不能重接已结束输入。
            routing::install_after_editing(self);
        }
    }

    /// delegate无法返回Result时只记录本dialog首因，随后显式结果路径负责传播。
    fn record_failure(&self, error: String) {
        // 首因是有限错误文本，不拥有正文/字段/manager，也不覆盖更早的ownership失败。
        self.ivars().failure.borrow_mut().get_or_insert(error);
    }

    /// AppKit 的公开拒绝边界：这里只观察同一活客户端，不执行成功 didEnd 才有权执行的清理。
    fn preflight_resignation(
        &self,
        control: &NSControl,
        editor: &NSText,
    ) -> Result<(), NativeMarkdownModalError> {
        if !self.is_current() {
            // 已结束/过期的provider不能以一次迟到回调授权另一窗口辞任。
            return Err(NativeMarkdownModalError::StaleOwner);
        }
        let field = self
            .ivars()
            .field
            .get()
            .and_then(Weak::load)
            .ok_or(NativeMarkdownModalError::StaleOwner)?;
        let expected_control: &NSControl = &field;
        if !std::ptr::eq(expected_control, control)
            || !field
                .currentEditor()
                .is_some_and(|actual| std::ptr::eq(&*actual, editor))
        {
            // 不把其他control/editor的验证请求误当作本dialog输入；未知历史完整保留。
            return Err(NativeMarkdownModalError::Boundary(
                "对话框输入的结束请求与当前控件不匹配".into(),
            ));
        }
        self.ivars()
            .undo
            .preflight()
            .map_err(NativeMarkdownModalError::Boundary)?;
        self.ivars()
            .borrowed
            .observe_editing(&field, &self.ivars().undo, &self.ivars().ledger)
            .map_err(NativeMarkdownModalError::Boundary)?;
        self.ivars()
            .borrowed
            .preflight_unmarked()
            .map_err(NativeMarkdownModalError::Boundary)?;
        if let Some(error) = self.ivars().failure.borrow().clone() {
            // 已知归属失败不能靠焦点移动消失，返回NO保留原字段与其历史。
            return Err(NativeMarkdownModalError::Boundary(error));
        }
        Ok(())
    }

    /// 正常验证和辞任在清空候选之前运行，确认读取只能在该阶段通过后发生。
    pub(super) fn prepare_end(&self, window: &NSWindow) -> Result<(), NativeMarkdownModalError> {
        self.ivars()
            .undo
            .preflight()
            .map_err(NativeMarkdownModalError::Boundary)?;
        self.ivars()
            .borrowed
            .capture(window)
            .map_err(NativeMarkdownModalError::Boundary)?;
        if let Some(error) = self.ivars().failure.borrow().clone() {
            // 已记录失败必须早于正常辞任，否则成功didEnd可能先清掉仍需恢复的历史。
            return Err(NativeMarkdownModalError::Boundary(error));
        }
        if !window.makeFirstResponder(None) {
            // 验证拒绝不得被endEditing的强制能力反标成成功；父草稿尚无文件效果。
            return Err(NativeMarkdownModalError::Boundary(
                "对话框输入尚未结束，请完成输入后重试".into(),
            ));
        }
        self.ivars()
            .borrowed
            .preflight_unmarked()
            .map_err(NativeMarkdownModalError::Boundary)?;
        if let Some(error) = self.ivars().failure.borrow().clone() {
            // 实际begin/change/end发生的归属错误优先，不能在取消时丢失。
            return Err(NativeMarkdownModalError::Boundary(error));
        }
        Ok(())
    }

    /// 只在正常辞任已通过后结束业务权限；候选读取失败仍可清理已接受的已知owner。
    pub(super) fn finish(
        &self,
        window: &NSWindow,
        field: &NSTextField,
    ) -> Result<(), NativeMarkdownModalError> {
        self.ivars().active.set(false);
        // accepted权限已经封闭；先撤销自己安装的链边，再让既有provider/输入清理运行native消息。
        routing::detach(self);
        self.ivars().provider.freeze();
        self.ivars().borrowed.finish_accepted(
            window,
            &[field],
            &self.ivars().undo,
            &self.ivars().provider,
            true,
        );
        field.setStringValue(ns_string!(""));
        field.setPlaceholderString(None);
        field.setEditable(false);
        field.setSelectable(false);
        // SAFETY: 只有本dialog自有accessory的nullable callback被移除，不触碰alert按钮reserved actions。
        unsafe {
            field.setDelegate(None);
            field.setTarget(None);
            field.setAction(None);
        }
        if !self.ivars().borrowed.is_detached() {
            // 清理完成不保证事实正确；失败必须阻止后继，不能把当前输入恢复为已通过状态。
            return Err(NativeMarkdownModalError::Boundary(
                self.ivars()
                    .borrowed
                    .failure()
                    .unwrap_or_else(|| "对话框输入尚未清空或解除关联".into()),
            ));
        }
        if let Some(error) = self.ivars().failure.borrow().clone() {
            // native delegate首因始终保留，即使后来的公开empty事实已经成立。
            return Err(NativeMarkdownModalError::Boundary(error));
        }
        Ok(())
    }
}

/// cell仅弱指向独占输入owner，保留AppKit原有文本/IME行为。
struct ModalCellIvars {
    owner: Weak<NativeMarkdownModalInput>,
}

define_class!(
    // SAFETY: NSTextFieldCell的标准行为保持；仅覆写公开fieldEditorForView，ivar初始化先于super init。
    #[unsafe(super = NSTextFieldCell)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ModalCellIvars]
    struct NativeMarkdownModalNameCell;
    impl NativeMarkdownModalNameCell {
        #[unsafe(method_id(fieldEditorForView:))]
        fn field_editor(&self, view: &NSView) -> Option<Retained<NSTextView>> {
            // weak升级只包围一次同步查询，cell不能保活dialog或父session。
            self.ivars().owner.load().and_then(|owner| {
                // method_id保持最终Option的原生返回ABI；无owner时不取得输入权限。
                owner.provide(view)
            })
        }
    }
);

define_class!(
    // SAFETY: NSComboBoxCell保留标准候选列表/编辑行为，仅替换公开指定的field editor提供入口。
    #[unsafe(super = NSComboBoxCell)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ModalCellIvars]
    struct NativeMarkdownModalMoveCell;
    impl NativeMarkdownModalMoveCell {
        #[unsafe(method_id(fieldEditorForView:))]
        fn field_editor(&self, view: &NSView) -> Option<Retained<NSTextView>> {
            // control/window/代次由独占owner核对，不向其他combo借出权限。
            self.ivars().owner.load().and_then(|owner| {
                // 只有实际仍活的本dialog owner能提供editor，未升级不会分配替代对象。
                owner.provide(view)
            })
        }
    }
);

/// 构造自有name cell并先登记，随后才允许写入用户候选与进入modal循环。
pub(super) fn install_native_markdown_modal_name_input(
    guard: &NativeMarkdownModalUseGuard,
    input: &NSTextField,
    window: &NSWindow,
    mtm: MainThreadMarker,
) -> Result<Retained<NativeMarkdownModalInput>, NativeMarkdownModalError> {
    let owner = NativeMarkdownModalInput::new(guard, mtm);
    let allocated = NativeMarkdownModalNameCell::alloc(mtm).set_ivars(ModalCellIvars {
        owner: Weak::from_retained(&owner),
    });
    // SAFETY: 已初始化weak ivar的主线程NSTextFieldCell子类使用公开文本initializer及正确NSString参数。
    let cell: Retained<NativeMarkdownModalNameCell> =
        unsafe { msg_send![super(allocated), initTextCell: ns_string!("")] };
    input.setCell(Some(&cell));
    guard
        .ledger
        .include("modal.input-controller", &owner)
        .map_err(NativeMarkdownModalError::Boundary)?;
    guard
        .ledger
        .include_control("modal.name-input", input)
        .map_err(NativeMarkdownModalError::Boundary)?;
    owner.bind(input, window)?;
    Ok(owner)
}

/// move使用自己的标准combo cell，不替换NSAlert窗口delegate或其私有子控件。
pub(super) fn install_native_markdown_modal_move_input(
    guard: &NativeMarkdownModalUseGuard,
    input: &objc2_app_kit::NSComboBox,
    window: &NSWindow,
    mtm: MainThreadMarker,
) -> Result<Retained<NativeMarkdownModalInput>, NativeMarkdownModalError> {
    let owner = NativeMarkdownModalInput::new(guard, mtm);
    let allocated = NativeMarkdownModalMoveCell::alloc(mtm).set_ivars(ModalCellIvars {
        owner: Weak::from_retained(&owner),
    });
    // SAFETY: NSComboBoxCell公开initTextCell消费已初始化weak ivar的正确子类allocation。
    let cell: Retained<NativeMarkdownModalMoveCell> =
        unsafe { msg_send![super(allocated), initTextCell: ns_string!("")] };
    input.setCell(Some(&cell));
    guard
        .ledger
        .include("modal.input-controller", &owner)
        .map_err(NativeMarkdownModalError::Boundary)?;
    guard
        .ledger
        .include_control("modal.move-input", input)
        .map_err(NativeMarkdownModalError::Boundary)?;
    owner.bind(input, window)?;
    Ok(owner)
}

/// alert、实际window和已配置按钮/cell逐项登记，不遍历私有view树或清reserved action。
pub(super) fn register_native_markdown_modal_alert(
    guard: &NativeMarkdownModalUseGuard,
    alert: &NSAlert,
) -> Result<Retained<NSWindow>, NativeMarkdownModalError> {
    guard
        .ledger
        .include("modal.alert", alert)
        .map_err(NativeMarkdownModalError::Boundary)?;
    let window = alert.window();
    guard
        .ledger
        .include("modal.window", &window)
        .map_err(NativeMarkdownModalError::Boundary)?;
    let buttons = alert.buttons();
    if buttons.count() > 3 {
        // 项目只创建最多三个响应按钮；异常数量不允许截断登记后继续展示。
        return Err(NativeMarkdownModalError::Boundary(
            "对话框操作按钮超过数量上限".into(),
        ));
    }
    for index in 0..buttons.count() {
        // 返回的按钮与其真实cell是项目配置角色，target/action仍完全属于NSAlert。
        let button = buttons.objectAtIndex(index);
        guard
            .ledger
            .include_control("modal.response-button", &button)
            .map_err(NativeMarkdownModalError::Boundary)?;
    }
    Ok(window)
}

/// cleanup只清项目公开配置，不动NSAlert保留的按钮动作或私有window delegate。
pub(super) fn detach_native_markdown_modal_alert(alert: &NSAlert, window: &NSWindow) {
    window.setInitialFirstResponder(None);
    alert.setAccessoryView(None);
    alert.setMessageText(ns_string!(""));
    alert.setInformativeText(ns_string!(""));
    let buttons = alert.buttons();
    for index in 0..buttons.count() {
        // title是Apple明确允许定制的公开属性；移除路径/标题业务文本，不动reserved callback。
        // 同一alert的按钮已在展示前按最多三个完整登记，项目不在modal期间追加按钮。
        let button = buttons.objectAtIndex(index);
        button.setTitle(ns_string!(""));
    }
}

/// 无自有文本输入的确认/信息框共用显式登记和pool边界，不隐藏任何业务动作。
pub(super) fn run_native_markdown_modal_confirmation(
    controller: &NativeMarkdownController,
    message: &str,
    information: &str,
    action: &str,
    cancel: Option<&str>,
    mtm: MainThreadMarker,
) -> Result<bool, NativeMarkdownModalError> {
    let guard = NativeMarkdownModalUseGuard::enter(controller)?;
    let result = autoreleasepool(|_| {
        let alert = NSAlert::new(mtm);
        alert.addButtonWithTitle(&NSString::from_str(action));
        if let Some(cancel) = cancel {
            // 只有明确的取消按钮获得Escape；不覆盖框架保留的button target/action。
            assign_escape_to_native_markdown_alert_cancel(
                &alert.addButtonWithTitle(&NSString::from_str(cancel)),
            );
        }
        let window = register_native_markdown_modal_alert(&guard, &alert)?;
        alert.setMessageText(&NSString::from_str(message));
        alert.setInformativeText(&NSString::from_str(information));
        #[cfg(all(
            feature = "native-markdown-runtime-probe",
            feature = "native-table-row-append"
        ))]
        NativeMarkdownHostedTableProbe::observe_conflict_modal_ready(&alert, &window, None);
        let confirmed = native_markdown_alert_response(alert.runModal(), cancel.is_some());
        detach_native_markdown_modal_alert(&alert, &window);
        confirmed
    });
    guard.complete(result)
}

/// 返回有限、自有的候选；native输入过大或返回UTF-8超限均明确拒绝。
pub(super) fn native_markdown_modal_candidate(
    input: &NSTextField,
) -> Result<String, NativeMarkdownModalError> {
    autoreleasepool(|pool| {
        let value = input.stringValue();
        if value.length() > MAX_MODAL_INPUT_UNITS {
            // 不先构造无界Rust String，也不截断可能改变文件操作含义的路径/标题。
            return Err(NativeMarkdownModalError::InputTooLong);
        }
        // SAFETY: 当前最内层pool与value均覆盖同步长度/trim/copy，期间不调用native setter或外来回调。
        let value = unsafe { value.to_str(pool) };
        if value.len() > MAX_MODAL_INPUT_BYTES {
            // UTF-16上限不能替代UTF-8字节预算，两个边界独立验证。
            return Err(NativeMarkdownModalError::InputTooLong);
        }
        Ok(value.trim().to_owned())
    })
}

/// 在同一alert保留失败输入，直到某次明确用户动作后正常辞任通过。
pub(super) fn run_native_markdown_modal_input_response(
    alert: &NSAlert,
    window: &NSWindow,
    field: &NSTextField,
    owner: &NativeMarkdownModalInput,
) -> Result<bool, NativeMarkdownModalError> {
    #[cfg(all(
        feature = "native-markdown-runtime-probe",
        feature = "native-table-row-append"
    ))]
    NativeMarkdownHostedTableProbe::observe_conflict_modal_ready(alert, window, Some(field));
    await_prepared_modal_response(
        || {
            // 同一显式owner留在外层；每次用户动作的框架临时值在独立局部pool结束，不随失败次数积累。
            autoreleasepool(|_| native_markdown_alert_response(alert.runModal(), true))
        },
        || autoreleasepool(|_| owner.prepare_end(window)),
        |_| {
            autoreleasepool(|_| {
                // runModal不承诺accessory已正常辞任；原alert/field/editor/undo均留在本栈。
                // 只更新固定提示和公开初始焦点，不读候选、不清值、不封provider或增加另一事务。
                let message = if owner.ivars().undo.has_failure()
                    || owner.ivars().borrowed.failure().is_some()
                    || owner.ivars().failure.borrow().is_some()
                {
                    // 永久归属首错不能靠再点Cancel修复；保持资源失败状态，不承诺自动恢复。
                    ns_string!("当前输入无法安全结束，内容将保留在此窗口中，本次操作尚未执行。")
                } else {
                    // 尚未完成composition或正常辞任拒绝保留原输入，下一次用户动作可以重新验证。
                    ns_string!("请完成当前输入后重试。文字与撤销记录均已保留，本次操作尚未执行。")
                };
                alert.setInformativeText(message);
                window.setInitialFirstResponder(Some(field));
                let _ = window.makeFirstResponder(Some(field));
            });
        },
    )
}

/// 一次失败只等待下一次显式交互；不会轮询、设timer、递归或替换dialog owner。
fn await_prepared_modal_response<R, E>(
    mut present: impl FnMut() -> R,
    mut prepare: impl FnMut() -> Result<(), E>,
    mut restore: impl FnMut(&E),
) -> R {
    loop {
        // present由真实modal事件循环阻塞；旧按钮结果不能越过本次失败辞任取得文件权限。
        let response = present();
        match prepare() {
            Ok(()) => {
                // 只有正常辞任成功才向caller发布原始typed响应；后续candidate/cleanup各执行一次。
                return response;
            }
            Err(error) => {
                // 保留同一可编辑输入与历史；不把失败压成Cancel或调用accepted清理。
                restore(&error);
            }
        }
    }
}

/// 已通过正常辞任的结果分支都调用此结束入口；候选错误不跳过已接受owner清理。
pub(super) fn finish_native_markdown_modal_input(
    guard: &NativeMarkdownModalUseGuard,
    owner: &NativeMarkdownModalInput,
    window: &NSWindow,
    field: &NSTextField,
) -> Result<(), NativeMarkdownModalError> {
    let result = owner.finish(window, field);
    // 仅借出纯Rust tracker；稍后pool结束时复核，不把editor/manager强owner带出本栈。
    *guard.finished_input.borrow_mut() = Some(owner.ivars().borrowed.clone());
    if let Err(error) = &result {
        // 独占输入清理首因参与父session的后续预检，取消不能抹掉它。
        guard.record_cleanup_failure(&error.to_string());
    }
    result
}

/// 系统file chooser的Cancel是正常no-op，Abort或未知响应不能伪装用户取消。
pub(super) fn native_markdown_open_panel_response(
    response: isize,
) -> Result<bool, NativeMarkdownModalError> {
    match response {
        1 => {
            // NSModalResponseOK明确确认，调用方还须校验实际file URL/path。
            Ok(true)
        }
        0 => {
            // NSModalResponseCancel没有候选和文件效果。
            Ok(false)
        }
        -1001 => {
            // NSModalResponseAbort表示系统展示失败，不是用户取消。
            Err(NativeMarkdownModalError::Aborted)
        }
        other => {
            // 未声明响应不授予任何选择权限，保留有界数值便于诊断。
            Err(NativeMarkdownModalError::UnexpectedResponse(other))
        }
    }
}

/// 所有项目alert都显式添加按钮；只有实际添加的第二按钮可以表示取消。
pub(super) fn native_markdown_alert_response(
    response: isize,
    has_cancel: bool,
) -> Result<bool, NativeMarkdownModalError> {
    match response {
        1000 => {
            // NSAlertFirstButtonReturn是本次明确确认，不依赖视觉按钮顺序。
            Ok(true)
        }
        1001 if has_cancel => {
            // 仅第二个已添加的Cancel响应是普通no-op。
            Ok(false)
        }
        -1001 => {
            // 系统中止不是用户点击取消，不能授权后继或隐藏展示错误。
            Err(NativeMarkdownModalError::Aborted)
        }
        other => {
            // 无取消按钮的信息框、未知额外按钮或stop响应均不伪装正常确认。
            Err(NativeMarkdownModalError::UnexpectedResponse(other))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_preflight_keeps_input_and_history_unpublished_until_success() {
        // 用实际生产策略模拟同一活输入：首次按钮响应不能越过失败辞任触发候选读取/accepted清理。
        let input = RefCell::new(String::from("unchanged input"));
        let history = RefCell::new(vec![String::from("original edit")]);
        let presentations = Cell::new(0);
        let preparations = Cell::new(0);
        let candidate_reads = Cell::new(0);
        let cleanups = Cell::new(0);
        let response = await_prepared_modal_response(
            || {
                presentations.set(presentations.get() + 1);
                assert!(presentations.get() <= 2);
                // 首次Cancel同样不能让失败输入走强制accepted；下次明确确认才是最终响应。
                presentations.get() == 2
            },
            || {
                preparations.set(preparations.get() + 1);
                if preparations.get() == 1 {
                    // 可失败验证拒绝后仍需保留同一字段、history和全部业务权限边界。
                    Err("refused resignation")
                } else {
                    Ok(())
                }
            },
            |_| {
                assert_eq!(candidate_reads.get(), 0);
                assert_eq!(cleanups.get(), 0);
                assert_eq!(&*input.borrow(), "unchanged input");
                assert_eq!(&*history.borrow(), &[String::from("original edit")]);
            },
        );
        // 生产caller仅在策略发布后读取候选并清理；验证成功后的单次执行，不为失败补一次清理。
        if response {
            candidate_reads.set(candidate_reads.get() + 1);
            assert_eq!(&*input.borrow(), "unchanged input");
        }
        cleanups.set(cleanups.get() + 1);
        input.borrow_mut().clear();
        history.borrow_mut().clear();
        assert_eq!(presentations.get(), 2);
        assert_eq!(preparations.get(), 2);
        assert_eq!(candidate_reads.get(), 1);
        assert_eq!(cleanups.get(), 1);
    }

    #[test]
    fn prepared_modal_preserves_typed_abort_and_unknown_responses() {
        // 正常辞任只授权释放，不把展示错误改称取消或确认。
        for raw in [-1001, 77] {
            let response = await_prepared_modal_response(
                || native_markdown_alert_response(raw, true),
                || Ok::<(), ()>(()),
                |_| panic!("successful resignation must not restore input"),
            );
            match (raw, response) {
                (-1001, Err(NativeMarkdownModalError::Aborted))
                | (77, Err(NativeMarkdownModalError::UnexpectedResponse(77))) => {}
                _ => panic!("typed response was changed after normal resignation"),
            }
        }
    }

    #[test]
    fn sticky_failure_requires_another_real_event_and_never_publishes_or_cleans() {
        // 有限假事件源耗尽即停止测试；真实present会等待用户，不允许失败后仅重复prepare忙循环。
        let mut events = [1000, 1001, -1001].into_iter();
        let preparations = Cell::new(0);
        let restorations = Cell::new(0);
        let candidate_reads = Cell::new(0);
        let cleanups = Cell::new(0);
        let file_mutations = Cell::new(0);
        let input = String::from("retained fault input");
        let history = vec![String::from("retained edit")];
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _response = await_prepared_modal_response(
                || {
                    let event = events.next().expect("fake user event source is exhausted");
                    native_markdown_alert_response(event, true)
                },
                || {
                    preparations.set(preparations.get() + 1);
                    Err::<(), _>("permanent ownership failure")
                },
                |_| {
                    restorations.set(restorations.get() + 1);
                    assert_eq!(&input, "retained fault input");
                    assert_eq!(&history, &[String::from("retained edit")]);
                },
            );
            // 与生产caller相同的授权位置：策略从未发布响应，这些有后果动作全不可达。
            candidate_reads.set(candidate_reads.get() + 1);
            cleanups.set(cleanups.get() + 1);
            file_mutations.set(file_mutations.get() + 1);
        }));
        assert!(stopped.is_err());
        assert_eq!(preparations.get(), 3);
        assert_eq!(restorations.get(), 3);
        assert_eq!(candidate_reads.get(), 0);
        assert_eq!(cleanups.get(), 0);
        assert_eq!(file_mutations.get(), 0);
        assert_eq!(&input, "retained fault input");
        assert_eq!(&history, &[String::from("retained edit")]);
    }

    #[test]
    fn chooser_cancel_is_distinct_from_abort_and_unknown() {
        // 纯响应策略不构造AppKit；系统错误不能丢成普通取消。
        assert!(native_markdown_open_panel_response(1).unwrap());
        assert!(!native_markdown_open_panel_response(0).unwrap());
        assert!(matches!(
            native_markdown_open_panel_response(-1001),
            Err(NativeMarkdownModalError::Aborted)
        ));
        assert!(matches!(
            native_markdown_open_panel_response(1000),
            Err(NativeMarkdownModalError::UnexpectedResponse(1000))
        ));
    }

    #[test]
    fn nested_modal_preflight_refuses_without_native_owner() {
        // 测试只设置纯标量，拒绝必须发生在native构造或Active owner借出之前。
        MODAL_ACTIVE.with(|active| active.set(Some(7)));
        assert!(preflight_native_markdown_modal().is_err());
        MODAL_ACTIVE.with(|active| active.set(None));
        assert!(preflight_native_markdown_modal().is_ok());
    }

    #[test]
    fn nested_claim_and_stale_release_preserve_the_original_modal() {
        // 实际pure claim路径保证拒绝先于任何native构造，失败也不消费新generation。
        MODAL_ACTIVE.with(|active| active.set(None));
        MODAL_GENERATION.with(|generation| generation.set(10));
        assert_eq!(claim_modal_generation().unwrap(), 11);
        assert!(matches!(
            claim_modal_generation(),
            Err(NativeMarkdownModalError::Busy)
        ));
        assert_eq!(MODAL_GENERATION.with(Cell::get), 11);
        release_modal_generation(10);
        assert_eq!(MODAL_ACTIVE.with(Cell::get), Some(11));
        release_modal_generation(11);
        assert!(preflight_native_markdown_modal().is_ok());
    }

    #[test]
    fn exhausted_modal_epoch_is_a_nonmutating_failure() {
        // 不回绕到旧epoch，拒绝后仍无Active claim和任何native副作用。
        MODAL_ACTIVE.with(|active| active.set(None));
        MODAL_GENERATION.with(|generation| generation.set(u64::MAX));
        assert!(matches!(
            claim_modal_generation(),
            Err(NativeMarkdownModalError::Exhausted)
        ));
        assert_eq!(MODAL_GENERATION.with(Cell::get), u64::MAX);
        assert_eq!(MODAL_ACTIVE.with(Cell::get), None);
        MODAL_GENERATION.with(|generation| generation.set(0));
    }

    #[test]
    fn alert_cancellation_requires_an_actual_cancel_button() {
        // 信息框没有隐式第二按钮；额外响应不能压成正常取消。
        assert!(native_markdown_alert_response(1000, false).unwrap());
        assert!(!native_markdown_alert_response(1001, true).unwrap());
        assert!(matches!(
            native_markdown_alert_response(1001, false),
            Err(NativeMarkdownModalError::UnexpectedResponse(1001))
        ));
        assert!(matches!(
            native_markdown_alert_response(-1001, true),
            Err(NativeMarkdownModalError::Aborted)
        ));
    }
}
