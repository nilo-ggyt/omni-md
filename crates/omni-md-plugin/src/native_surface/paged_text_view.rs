//! 无TextKit正文的原生输入边界；文稿只由原PagedInput弱借的Core模型管理。

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::{AnyObject, ProtocolObject, Sel},
};
use objc2_app_kit::{
    NSDragOperation, NSDraggingInfo, NSEvent, NSGraphicsContext, NSMenu, NSTextInputClient,
    NSTextInputContext, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound,
    NSObjectProtocol, NSPoint, NSRange, NSRect,
};
use omni_markdown_core::MarkdownDocumentBufferError;

use super::paged_input::{
    NativeMarkdownPagedInput, NativeMarkdownPagedInputCommit, NativeMarkdownPagedInputError,
    NativeMarkdownPagedInputHistory, NativeMarkdownPagedInputOutcome,
    NativeMarkdownPagedInputState,
};
use crate::payload_use::PayloadUseGuard;

mod cell_word;
mod close;
mod commands;
mod context_menu;
mod cpu;
mod find;
mod folding;
mod formatting;
mod input;
mod paste;
mod pointer_scroll;
mod scene;
pub use super::viewport_pixels::{
    NativeMarkdownViewportPixelObserver, NativeMarkdownViewportPixelSnapshot,
};
pub use cell_word::NativeMarkdownPagedCellWordTarget;
pub use close::{NativeMarkdownPagedCloseFailure, NativeMarkdownPagedCloseReceipt};
pub use find::NativeMarkdownPagedFindVisibleProof;
pub use folding::NativeMarkdownPagedSourceFoldAction;
pub use scene::*;

/// 正式输入视图只装配有限CPU视口；旧直接绘制仅在显式原生对照中编译。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownPagedRenderStrategy {
    #[cfg(feature = "native-markdown-runtime-probe")]
    Direct,
    CpuViewport,
}

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownPagedTextViewError {
    #[error("原正文视图已经封闭")]
    Closed,
    #[error("原正文视图仍在同步回调，不能接受释放")]
    Busy,
    #[error("原正文视图或scene权限已撤销")]
    Revoked,
    #[error("原正文可见scene尚未Ready")]
    Pending,
    #[error("原可见几何无效或超界")]
    InvalidGeometry,
    #[error("原文本范围越界或切入Unicode标量")]
    InvalidRange,
    #[error("原视图计数已经耗尽")]
    CounterExhausted,
    #[error("原生文本输入上下文不可用")]
    ContextUnavailable,
    #[error("原生剪贴板没有可读取的文字")]
    ClipboardUnavailable,
    #[error("无法复制选中文本：{0}")]
    ClipboardWrite(String),
    #[error("无法导入附件：{0}")]
    AttachmentImport(String),
    #[error("无法执行折叠：{0}")]
    FoldAction(String),
    #[error("插入附件链接失败：{cause}；清理临时附件失败：{cleanup}")]
    AttachmentRollback { cause: Box<Self>, cleanup: String },
    #[error("系统未能打开该链接")]
    LinkOpenFailed,
    #[error("当前编辑器尚未提供稳定的标题锚点导航，无法跳转到该锚点")]
    LinkAnchorUnsupported,
    #[error("无法打开笔记链接：{0}")]
    RelativeNoteLink(String),
    #[error("原链接来源查询失败")]
    LinkSource(omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError),
    #[error("该selector不是本正文支持的typed编辑动作")]
    UnsupportedCommand,
    #[error("原输入事务失败：{0}")]
    Input(#[from] NativeMarkdownPagedInputError),
    #[error("原文字范围读取失败：{0}")]
    Document(#[from] MarkdownDocumentBufferError),
    #[error("原封存字形输出失败：{0}")]
    ShapedSpool(omni_markdown_core::MarkdownShapedSpoolError),
    #[error("整个存活像素图组和候选超过32MiB预算，当前后端尚不支持该窗口范围")]
    PixelBudgetExceeded,
    #[error("有界像素后端失败：{0}")]
    Raster(String),
    #[error("正文已提交，但原显示或坐标后果失效：{0}")]
    CommittedEffect(Box<Self>),
}
type Result<T> = std::result::Result<T, NativeMarkdownPagedTextViewError>;
impl From<super::viewport_pixels::PixelError> for NativeMarkdownPagedTextViewError {
    fn from(error: super::viewport_pixels::PixelError) -> Self {
        match error {
            super::viewport_pixels::PixelError::BudgetExceeded => Self::PixelBudgetExceeded,
            super::viewport_pixels::PixelError::InvalidGeometry => Self::InvalidGeometry,
            other => Self::Raster(other.to_string()),
        }
    }
}

/// Rc只用于把原Retained复制移到借用外；context公开合同不retain client，因此没有Native强环。
struct InputContextOwner(Retained<NSTextInputContext>);

pub struct PagedTextViewIvars {
    input: RefCell<Option<Rc<NativeMarkdownPagedInput>>>,
    delegate: RefCell<Option<Weak<dyn NativeMarkdownPagedTextViewDelegate>>>,
    context: RefCell<Option<Rc<InputContextOwner>>>,
    epoch: u64,
    geometry_epoch: Cell<u64>,
    geometry_failed: Cell<bool>,
    closed: Cell<bool>,
    depth: Cell<usize>,
    last_failure: RefCell<Option<NativeMarkdownPagedTextViewError>>,
    interpreting_key: Cell<bool>,
    input_callback_depth: Cell<usize>,
    interpretation_failure: RefCell<Option<NativeMarkdownPagedTextViewError>>,
    // 同一指针槽保存首击caret或word种子；鼠标抬起/关闭清空它，不持异步输入owner。
    drag_anchor: Cell<Option<commands::DragAnchor>>,
    // 原Down物理位置与真实Drag事实只随该手势存活；选择激活引起的排版不能伪造鼠标移动。
    pointer_motion: Cell<Option<commands::PointerMotion>>,
    // 链接在原mouseup才提交；两槽只保弱Scene身份及单手势寿命，不保NSEvent、Doc或Reader。
    pressed_link: RefCell<Option<commands::PressedLink>>,
    link_intent: RefCell<Option<Rc<()>>>,
    // 原Down到Up只有一张弱手势票；活动格异步准备不保系统事件，也不能在Up后复活拖选。
    cell_pointer_intent: RefCell<Option<Rc<()>>>,
    // 已发布后旧child只弱认真正父view及原手势；没有Input/Scene/Doc或retired Body强边。
    cell_pointer_forward: RefCell<Option<commands::CellPointerForward>>,
    // 拖选越过视口时只有一份原手势期限；定时器仅持弱view，真实Up/新输入/关闭都会撤销。
    drag_scroll: RefCell<Option<pointer_scroll::DragScroll>>,
    // 每个客户最多保留一份原生编辑菜单；关闭先撤其动作，再归还输入及视图资源。
    context_menu: RefCell<Option<Retained<NSMenu>>>,
    render_strategy: NativeMarkdownPagedRenderStrategy,
    cpu: RefCell<Option<Rc<cpu::CpuSurface>>>,
    close_receipt: RefCell<Option<Rc<NativeMarkdownPagedCloseReceipt>>>,
}

/// 原callback栈的完整输入事实；delegate强claim只活同步栈，不保存到View或后继事件。
struct Origin {
    input: Rc<NativeMarkdownPagedInput>,
    delegate: Rc<dyn NativeMarkdownPagedTextViewDelegate>,
    state: NativeMarkdownPagedInputState,
    geometry_epoch: u64,
    epoch: u64,
    /// 只由实际NSTextInputClient回调或已排队菜单入口设置；外部选择/鼠标不能借解释depth认领收据。
    interpreted: bool,
}
impl Origin {
    fn session_current(&self, view: &NativeMarkdownPagedTextView, epoch: u64) -> bool {
        epoch == self.epoch
            && !view.ivars().closed.get()
            && view.ivars().epoch == epoch
            && self.delegate.current(epoch)
            && !view.ivars().closed.get()
            && view.ivars().epoch == epoch
    }
    fn current(&self, view: &NativeMarkdownPagedTextView) -> bool {
        let pure = || {
            !view.ivars().closed.get()
                && view.ivars().epoch == self.epoch
                && geometry_matches(
                    &view.ivars().geometry_epoch,
                    &view.ivars().geometry_failed,
                    self.geometry_epoch,
                )
                && self.input.state().is_ok_and(|state| state == self.state)
        };
        // current可同步输入或撤权；后置纯核保证它不能把较新的选择补签给原callback。
        pure() && self.delegate.current(self.epoch) && pure()
    }
    fn step<T>(
        &self,
        view: &NativeMarkdownPagedTextView,
        operation: impl FnOnce() -> T,
    ) -> Result<T> {
        if !self.current(view) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let value = operation();
        if !self.current(view) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(value)
    }
}

struct SceneUse {
    origin: Origin,
    scene: Rc<dyn NativeMarkdownPagedTextScene>,
    key: NativeMarkdownPagedSceneKey,
}
impl SceneUse {
    fn current(&self, view: &NativeMarkdownPagedTextView) -> bool {
        self.origin.current(view)
            && self.scene.key().same(&self.key)
            && self.origin.delegate.scene_current(&self.key)
            && self.origin.current(view)
            && self.scene.key().same(&self.key)
    }
    fn step<T>(
        &self,
        view: &NativeMarkdownPagedTextView,
        operation: impl FnOnce() -> T,
    ) -> Result<T> {
        if !self.current(view) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let value = operation();
        if !self.current(view) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(value)
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PagedTextViewIvars]
    pub struct NativeMarkdownPagedTextView;

    // SAFETY: NSObject基础行为由NSView实现；本类所有Native入口都限制在主线程。
    unsafe impl NSObjectProtocol for NativeMarkdownPagedTextView {}

    impl NativeMarkdownPagedTextView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.foreground().is_ok()
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let result = (|| { let (delegate, context) = self.foreground_context()?;
                context.0.activate();
                if self.ivars().closed.get() || !delegate.current(self.ivars().epoch) { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                Ok(())
            })();
            self.boolean_result(result)
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.stop_drag_scroll(); // 已失去键盘/鼠标客户资格，不能让原越界拖选继续滚动。
            self.cancel_link_intent(); // 焦点已开始移交，未完成的旧链接不再拥有外部打开权限。
            if self.ivars().closed.get() {
                true // 已封闭的空view不能阻止原窗口断开firstResponder。
            } else {
                let result = (|| {
                let input = self.ivars().input.borrow().clone().ok_or(NativeMarkdownPagedTextViewError::Closed)?;
                if input.state()?.marked.is_some() {
                    self.apply_unmark()?; // 仍有真实候选时必须同步提交，不能因排版失败跳过或丢弃组合。
                }
                let before = input.state()?;
                let context = self.ivars().context.borrow().clone().ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
                // 辞任只需结束本客户的真实context；无marked时不向已failed的排版owner申请编辑权限。
                context.0.deactivate();
                if self.ivars().closed.get() { return Ok(()); } // Native重入已封闭的空客户不能再阻止原窗口detach。
                if input.state()? != before { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                Ok(())
                })();
                self.boolean_result(result)
            }
        }

        #[unsafe(method_id(inputContext))]
        fn input_context(&self) -> Option<Retained<NSTextInputContext>> {
            self.retain_input_context()
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            #[cfg(feature = "native-input-trace")]
            let trace_scope = self.begin_input_trace_event(event);
            self.cancel_cell_pointer_intent();
            self.cancel_link_intent(); // 连无文本变化的后继按键也撤旧click，不能在稍后突然打开旧链接。
            let result = (|| {
                let origin = self.origin()?;
                let current_input = |epoch| epoch == origin.epoch && origin.current(self);
                let permit = origin.input.capture(origin.epoch, &current_input)?;
                if origin.delegate.defer_key_down(event, &permit)? {
                    return Ok(()); // 原key尚未交给IME，marked协议没有被异步假完成。
                }
                let (delegate, context) = self.foreground_context()?;
                let current = || !self.ivars().closed.get()
                    && delegate.current(self.ivars().epoch) && !self.ivars().closed.get();
                if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                // 自写NSView把原key交给本客户的系统context，IME与key binding由handleEvent统一解释；
                // 回调允许实际改变文字、marked或选择，事后只核会话存活，不要求旧generation不变。
                let consumed = context.0.handleEvent(event);
                if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                if !consumed {
                    // 系统明确未消费才沿原responder链传播一次，不再interpret同key造成二次解释。
                    // SAFETY: 原主线程NSView仍由当前回调保护，公开keyDown:签名只借同步活NSEvent。
                    unsafe { let _: () = msg_send![super(self), keyDown: event]; }
                    if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                }
                Ok(())
            })();
            self.record(result);
            #[cfg(feature = "native-input-trace")]
            self.finish_input_trace_scope(trace_scope);
        }

        #[unsafe(method(scrollWheel:))]
        /// 实际滚轮是新视口意图，即使重入原排版栈也先撤旧模式定位；系统滚动仍只传播一次。
        fn scroll_wheel(&self, event: &NSEvent) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let result = (|| {
                let delegate = self.foreground()?;
                delegate.user_scroll(self.ivars().epoch)?;
                // SAFETY: 原主线程NSView及本同步NSEvent仍存活，保持原父responder的公开scrollWheel:语义。
                unsafe { let _: () = msg_send![super(self), scrollWheel: event]; }
                Ok(())
            })();
            self.record(result);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) { self.mouse(event, false); }
        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) { self.mouse(event, true); }
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) { self.finish_link_mouse(event); }

        #[unsafe(method_id(menuForEvent:))]
        /// 仅恢复当前正文的原生右键编辑菜单；不移动原选区、不创建新的快捷键或窗口。
        fn menu_for_event(&self, _event: &NSEvent) -> Option<Retained<NSMenu>> {
            match self.edit_context_menu() {
                Ok(menu) => Some(menu),
                Err(cause) => { self.failure(cause); None }
            }
        }

        #[unsafe(method(draggingEntered:))]
        /// 原生拖放仅声明实际支持的受限文件复制与纯文本，不接收富文本或图形副本。
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            self.accepted_drag_operation(sender)
        }

        #[unsafe(method(performDragOperation:))]
        /// 实际放下时固定同一可见Scene命中；文件工作随后沿原输入队列执行。
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let result = self.accept_drop(sender);
            self.boolean_result(result)
        }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            if self.ivars().render_strategy == NativeMarkdownPagedRenderStrategy::CpuViewport { true }
            else {
                // SAFETY: 原NSView公开无参BOOL getter；Direct对照保持原super语义。
                unsafe { msg_send![super(self), wantsUpdateLayer] }
            }
        }
        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            if self.ivars().render_strategy != NativeMarkdownPagedRenderStrategy::CpuViewport {
                // SAFETY: Direct路径仍转发原NSView一次，不混用CPU发布和窗口CG绘图。
                unsafe { let _: () = msg_send![super(self), updateLayer]; }
                return;
            }
            let cpu = self.ivars().cpu.borrow().clone();
            let Some(surface) = cpu else {
                self.failure(NativeMarkdownPagedTextViewError::Closed);
                return;
            };
            let scene = match self.scene() {
                Ok(scene) => scene,
                Err(NativeMarkdownPagedTextViewError::Pending) => {
                    // 原ready_scene已安排唯一schedule；本次没有可画图，不能把正常等帧写成输入失败。
                    return; // 不发布image、不增加paint，也不把旧图冒充本次Ready。
                }
                Err(cause) => { self.failure(cause); return; }
            };
            self.record(surface.paint(self, &scene)); // 实际绘图/Source/资源错误照常保留，不能被上面的未Ready分支吞掉。
        }
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if self.ivars().render_strategy == NativeMarkdownPagedRenderStrategy::CpuViewport { return; }

            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let scene = match self.scene() {
                Ok(scene) => scene,
                Err(NativeMarkdownPagedTextViewError::Pending) => {
                    // Direct入口同样只消费已经发布的图；原准备尚未完成时保留原调度与输入首错。
                    return;
                }
                Err(cause) => { self.failure(cause); return; }
            };
            let result = (|| {
                let context = scene.step(self, NSGraphicsContext::currentContext)?.ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
                let cg = scene.step(self, || context.CGContext())?;
                scene.scene.paint(&cg, &|| scene.current(self))?;
                if !scene.current(self) { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                Ok(())
            })();
            self.record(result); // 真正开始绘图后的任何失败均保原cause，不用等帧分支掩盖Source或资源后果。
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: objc2_foundation::NSSize) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            if self.frame().size != size { self.invalidate_geometry_only(); }
            // SAFETY: 与当前真实NSView公开setter完全同签名，只转发原参数一次。
            unsafe { let _: () = msg_send![super(self), setFrameSize: size]; }
        }
        #[unsafe(method(setFrameOrigin:))]
        fn set_frame_origin(&self, origin: NSPoint) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            if self.frame().origin != origin { self.invalidate_geometry_only(); }
            // SAFETY: 原有限viewport视图的位置变化只转发原参数一次，虚拟文档offset另由原owner给出。
            unsafe { let _: () = msg_send![super(self), setFrameOrigin: origin]; }
        }
        #[unsafe(method(setFrame:))]
        fn set_frame(&self, frame: NSRect) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            if self.frame() != frame { self.invalidate_geometry_only(); }
            // SAFETY: 与公开NSView setter同签名，不借正文或创建整篇高的绘图surface。
            unsafe { let _: () = msg_send![super(self), setFrame: frame]; }
        }
        #[unsafe(method(setBounds:))]
        fn set_bounds(&self, bounds: NSRect) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            if self.bounds() != bounds { self.invalidate_geometry_only(); }
            // SAFETY: 原bounds原样交回同一NSView，不在回调内发布新的scene。
            unsafe { let _: () = msg_send![super(self), setBounds: bounds]; }
        }
        #[unsafe(method(viewDidMoveToWindow))]
        fn did_move_to_window(&self) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.invalidate_geometry_only();
            // SAFETY: 原NSView生命周期回调同步转发一次，不创建窗口或额外事件。
            unsafe { let _: () = msg_send![super(self), viewDidMoveToWindow]; }
        }
        #[unsafe(method(viewDidChangeBackingProperties))]
        fn backing_changed(&self) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.invalidate_geometry_only();
            // SAFETY: 公开NSView backing生命周期回调；不在其中读取或调整任意子图层。
            unsafe { let _: () = msg_send![super(self), viewDidChangeBackingProperties]; }
        }
        #[unsafe(method(viewDidMoveToSuperview))]
        fn superview_changed(&self) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.invalidate_geometry_only();
            // SAFETY: 公开NSView挂载回调只转发一次，Root负责原clip真实通知与virtual offset。
            unsafe { let _: () = msg_send![super(self), viewDidMoveToSuperview]; }
        }

        // 菜单和responder目标沿相同typed命令入口，系统按键由原NSTextInputContext统一解释。
        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) { self.record(self.command(objc2::sel!(copy:))); }
        #[unsafe(method(cut:))]
        fn cut(&self, _sender: Option<&AnyObject>) { self.record(self.command(objc2::sel!(cut:))); }
        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) { self.record(self.command(objc2::sel!(paste:))); }
        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) { self.record(self.command(objc2::sel!(selectAll:))); }
        #[unsafe(method(undo:))]
        fn undo(&self, _sender: Option<&AnyObject>) { self.record(self.command(objc2::sel!(undo:))); }
        #[unsafe(method(redo:))]
        fn redo(&self, _sender: Option<&AnyObject>) { self.record(self.command(objc2::sel!(redo:))); }
    }

    // SAFETY: 全部required入口按公开NSTextInputClient签名实现；所有源范围来自唯一Core模型。
    unsafe impl NSTextInputClient for NativeMarkdownPagedTextView {
        #[unsafe(method(insertText:replacementRange:))]
        unsafe fn insert_text(&self, text: &AnyObject, replacement: NSRange) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let _callback = PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            #[cfg(feature = "native-input-trace")]
            let trace_scope = self.begin_input_trace_selector(crate::NativeInputTraceSelector::InsertText);
            self.record_input(self.apply_native_input(text, replacement, None));
            #[cfg(feature = "native-input-trace")]
            self.finish_input_trace_scope(trace_scope);
        }
        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        unsafe fn set_marked_text(&self, text: &AnyObject, selected: NSRange, replacement: NSRange) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let _callback = PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            #[cfg(feature = "native-input-trace")]
            let trace_scope = self.begin_input_trace_selector(crate::NativeInputTraceSelector::SetMarkedText);
            self.record_input(self.apply_native_input(text, replacement, Some(selected)));
            #[cfg(feature = "native-input-trace")]
            self.finish_input_trace_scope(trace_scope);
        }
        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let _callback = PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            #[cfg(feature = "native-input-trace")]
            let trace_scope = self.begin_input_trace_selector(crate::NativeInputTraceSelector::UnmarkText);
            self.record_input(self.apply_unmark());
            #[cfg(feature = "native-input-trace")]
            self.finish_input_trace_scope(trace_scope);
        }
        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            // 多选保留真实有序主范围；历史memento的anchor/head不代表当前主方向。
            self.origin().map_or_else(|_| not_found(), |origin| origin.state.primary_range)
        }
        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.origin().ok().and_then(|origin| origin.state.marked).unwrap_or_else(not_found)
        }
        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.origin().is_ok_and(|origin| origin.state.marked.is_some())
        }
        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_marked_attributes(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new() // 原输入只消费实际候选字符和选择；不谎称支持尚未呈现的任意Native属性。
        }
        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        unsafe fn attributed_substring(&self, range: NSRange, actual: *mut NSRange) -> Option<Retained<NSAttributedString>> {
            // SAFETY: 协议out参数的可写/null合同交同一同步Rust helper，不修改ABI或返回家族。
            unsafe { self.native_substring(range, actual) }
        }
        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        unsafe fn first_rect(&self, range: NSRange, actual: *mut NSRange) -> NSRect {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            // SAFETY: out参数按协议可写或null，零rect同时带NotFound，从不表示几何Ready。
            unsafe { write_actual(actual, not_found()); }
            match self.screen_rect(range) {
                Ok(value) => {
                    // SAFETY: 仅成功的同源scene结果写出实际范围，不伪造输入候选坐标。
                    unsafe { write_actual(actual, value.actual_range); }
                    value.rect
                }
                Err(NativeMarkdownPagedTextViewError::Pending) => {
                    // 原提交到新帧之间系统可同步询问候选坐标；负坐标协议不代表已接纳输入失败。
                    NSRect::ZERO // actualRange保持上方NotFound，不借旧scene伪造候选框位置。
                }
                Err(error) => { self.failure(error); NSRect::ZERO }
            }
        }
        #[unsafe(method(characterIndexForPoint:))]
        fn character_index(&self, point: NSPoint) -> usize {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            match self.screen_hit(point) {
                Ok(value) => value,
                Err(NativeMarkdownPagedTextViewError::Pending) => {
                    // 尚未发布真实命中几何时只交原NotFound，不能污染独立的输入失败槽或签发旧位置。
                    NSNotFound as usize
                }
                Err(error) => { self.failure(error); NSNotFound as usize }
            }
        }
        #[unsafe(method(doCommandBySelector:))]
        unsafe fn do_command(&self, selector: Sel) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let _callback = PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            let result = if commands::recognizes_selector(selector) {
                self.command_from_input_context(selector) // 原命令即使返回UnsupportedCommand也须记录；Scene歧义/功能缺失不能静默交给别的responder。
            } else {
                // SAFETY: 只有系统原NSTextInputClient action入口可保证未知selector仍为sender签名。
                // 安全Rust command不会把任意Sel当对象参数函数调用；转发仍使用原链和同一action一次。
                unsafe { self.forward_native_action(selector) }
            };
            self.record_input(result);
        }
    }
);

fn not_found() -> NSRange {
    NSRange::new(NSNotFound as usize, 0)
}
unsafe fn write_actual(output: *mut NSRange, value: NSRange) {
    if !output.is_null() {
        // SAFETY: 仅由公开NSTextInputClient ABI入口传入，调用者保证完整可写NSRange。
        unsafe {
            output.write(value);
        }
    }
}
fn finite_rect(rect: NSRect, positive: bool) -> bool {
    [
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    ]
    .into_iter()
    .all(f64::is_finite)
        && (!positive || (rect.size.width > 0.0 && rect.size.height > 0.0))
}

/// 未挂载NSView的visibleRect可为NSInfiniteRect；实际绘制域始终还受同一view的真实bounds限制。
fn bounded_viewport(visible: NSRect, bounds: NSRect) -> Option<NSRect> {
    if !finite_rect(visible, true) || !finite_rect(bounds, true) {
        return None;
    }
    let intersection = objc2_foundation::NSIntersectionRect(visible, bounds);
    finite_rect(intersection, true).then_some(intersection)
}

fn geometry_matches(epoch: &Cell<u64>, failed: &Cell<bool>, expected: u64) -> bool {
    !failed.get() && epoch.get() == expected
}
fn advance_geometry(epoch: &Cell<u64>, failed: &Cell<bool>) -> Result<()> {
    if failed.get() {
        return Err(NativeMarkdownPagedTextViewError::Revoked);
    }
    let Some(next) = epoch.get().checked_add(1) else {
        failed.set(true); // 原MAX票也永久失效；不能借closed早返而跳过context和View真实清理。
        return Err(NativeMarkdownPagedTextViewError::CounterExhausted);
    };
    epoch.set(next);
    Ok(())
}

impl NativeMarkdownPagedTextView {
    /// 运行两个真实未发布安装撤权控制；只在显式诊断feature下自建短合成图，不操作产品实例。
    ///
    /// # Errors
    /// 原撤权未发生、回滚夺走后继父层或任一原生/文档资源未实际释放时拒绝。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub fn probe_cpu_installation_rollback(mtm: MainThreadMarker) -> Result<()> {
        cpu::probe_installation_rollback(mtm)
    }

    /// 仅显式合成诊断验证真实遮罩/透明混合图与外方关系；不操作产品或用户文稿。
    ///
    /// # Errors
    /// 精确RGBA、原Native身份、外方关系或实际像素/Weak释放任一未通过时拒绝。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub fn probe_cpu_mask_slots(mtm: MainThreadMarker) -> Result<()> {
        cpu::probe_mask_slots(mtm)
    }

    /// 构造真实NSView和唯一NSInputContext；默认surface不会因模块存在而自动切换。
    pub fn new(
        input: Rc<NativeMarkdownPagedInput>,
        delegate: Weak<dyn NativeMarkdownPagedTextViewDelegate>,
        epoch: u64,
        frame: NSRect,
        mtm: MainThreadMarker,
    ) -> Result<Retained<Self>> {
        Self::new_with_render_strategy(
            input,
            delegate,
            epoch,
            frame,
            NativeMarkdownPagedRenderStrategy::CpuViewport,
            mtm,
        )
    }

    /// 显式后端仅供同源机制验收；没有环境默认值或隐藏的产品切换。
    pub fn new_with_render_strategy(
        input: Rc<NativeMarkdownPagedInput>,
        delegate: Weak<dyn NativeMarkdownPagedTextViewDelegate>,
        epoch: u64,
        frame: NSRect,
        render_strategy: NativeMarkdownPagedRenderStrategy,
        mtm: MainThreadMarker,
    ) -> Result<Retained<Self>> {
        if !finite_rect(frame, true) {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        input.state()?;
        let owner = delegate
            .upgrade()
            .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
        if !owner.current(epoch) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let allocated = Self::alloc(mtm).set_ivars(PagedTextViewIvars {
            input: RefCell::new(Some(input)),
            delegate: RefCell::new(Some(delegate)),
            context: RefCell::new(None),
            epoch,
            geometry_epoch: Cell::new(0),
            geometry_failed: Cell::new(false),
            closed: Cell::new(false),
            depth: Cell::new(0),
            last_failure: RefCell::new(None),
            interpreting_key: Cell::new(false),
            input_callback_depth: Cell::new(0),
            interpretation_failure: RefCell::new(None),
            drag_anchor: Cell::new(None),
            pointer_motion: Cell::new(None),
            pressed_link: RefCell::new(None),
            link_intent: RefCell::new(None),
            cell_pointer_intent: RefCell::new(None),
            cell_pointer_forward: RefCell::new(None),
            drag_scroll: RefCell::new(None),
            context_menu: RefCell::new(None),
            render_strategy,
            cpu: RefCell::new(None),
            close_receipt: RefCell::new(None),
        });
        // SAFETY: 主线程新分配NSView、完整ivars及有限frame满足原designated initializer合同。
        let view: Retained<Self> = unsafe { msg_send![super(allocated), initWithFrame: frame] };
        if !owner.current(epoch) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // Apple公开合同明确context不retain client；此处不创建NSTextView或任何正文镜像。
        let context = NSTextInputContext::initWithClient(
            NSTextInputContext::alloc(mtm),
            ProtocolObject::from_ref(&*view),
        );
        if !owner.current(epoch) {
            context.deactivate();
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        view.ivars()
            .context
            .replace(Some(Rc::new(InputContextOwner(context))));
        // 自写NSView没有NSTextView的拖放注册；仅声明同一有限纯文本/文件入口真实处理的类型。
        // SAFETY: AppKit公开不可变类型常量具有进程寿命，不涉及读取任何剪贴板或正文内容。
        let drag_types = unsafe {
            [
                objc2_app_kit::NSPasteboardTypeFileURL,
                objc2_app_kit::NSPasteboardTypeString,
            ]
        };
        view.registerForDraggedTypes(&NSArray::from_slice(&drag_types));
        if render_strategy == NativeMarkdownPagedRenderStrategy::CpuViewport {
            // AppKit启用backing时产生的临时view/layer claim必须在本次装配结束归还；
            // 唯一Cpu owner只带显式Retained层，不让外方长寿命池推迟已关闭view的析构。
            autoreleasepool(|_| -> Result<()> {
                let cpu = cpu::CpuSurface::install(&view)?;
                view.ivars().cpu.replace(Some(cpu)); // 先完整发布唯一owner，池末Native析构回调才能看到同一装配。
                Ok(())
            })?;
        }
        Ok(view)
    }

    fn foreground(&self) -> Result<Rc<dyn NativeMarkdownPagedTextViewDelegate>> {
        if self.ivars().closed.get() {
            return Err(NativeMarkdownPagedTextViewError::Closed);
        }
        let weak = self
            .ivars()
            .delegate
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedTextViewError::Closed)?;
        let delegate = weak
            .upgrade()
            .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
        if !delegate.current(self.ivars().epoch) || self.ivars().closed.get() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(delegate)
    }
    fn foreground_context(
        &self,
    ) -> Result<(
        Rc<dyn NativeMarkdownPagedTextViewDelegate>,
        Rc<InputContextOwner>,
    )> {
        let delegate = self.foreground()?;
        let context = self
            .ivars()
            .context
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
        Ok((delegate, context))
    }
    fn retain_input_context(&self) -> Option<Retained<NSTextInputContext>> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let (delegate, context) = self.foreground_context().ok()?;
        let retained = Retained::clone(&context.0); // 不持RefCell借用跨原生retain。
        (!self.ivars().closed.get() && delegate.current(self.ivars().epoch)).then_some(retained)
    }
    unsafe fn native_substring(
        &self,
        range: NSRange,
        actual: *mut NSRange,
    ) -> Option<Retained<NSAttributedString>> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        // SAFETY: 原协议入口保证actual可写或null；失败明确表示没有实际range。
        unsafe {
            write_actual(actual, not_found());
        }
        match self.substring(range) {
            Ok(Some((value, range))) => {
                // SAFETY: 原out参数，在同票有界结果已形成后写实际范围。
                unsafe {
                    write_actual(actual, range);
                }
                Some(value)
            }
            Ok(None) => None,
            Err(error) => {
                self.failure(error);
                None
            }
        }
    }
    fn origin(&self) -> Result<Origin> {
        let delegate = self.foreground()?;
        let input = self
            .ivars()
            .input
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedTextViewError::Closed)?;
        let origin = Origin {
            state: input.state()?,
            input,
            delegate,
            geometry_epoch: self.ivars().geometry_epoch.get(),
            epoch: self.ivars().epoch,
            interpreted: false,
        };
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(origin)
    }
    fn scene_request(&self, origin: &Origin) -> Result<NativeMarkdownPagedSceneRequest> {
        let visible = origin.step(self, || self.visibleRect())?;
        let bounds = origin.step(self, || self.bounds())?;
        let viewport =
            bounded_viewport(visible, bounds).ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let frame_in_parent = origin.step(self, || self.frame())?;
        let virtual_viewport = origin.step(self, || {
            origin
                .delegate
                .virtual_viewport(origin.epoch, bounds.size.width)
        })??;
        if !finite_rect(viewport, true)
            || !bounds.size.width.is_finite()
            || bounds.size.width <= 0.0
        {
            return Err(NativeMarkdownPagedTextViewError::Pending);
        }
        if !virtual_viewport.document_origin.x.is_finite()
            || !virtual_viewport.document_origin.y.is_finite()
            || !virtual_viewport.layout_origin_x.is_finite()
            || !virtual_viewport.layout_width.is_finite()
            || virtual_viewport.layout_width <= 0.0
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        Ok(NativeMarkdownPagedSceneRequest {
            document: origin.input.document_identity(),
            target: origin.input.target_domain()?,
            input: origin.state,
            viewport,
            bounds_width: bounds.size.width,
            frame_in_parent,
            geometry_epoch: origin.geometry_epoch,
            virtual_viewport,
        })
    }
    fn scene(&self) -> Result<SceneUse> {
        let origin = self.origin()?;
        let request = self.scene_request(&origin)?;
        let scene = origin.delegate.ready_scene(&request)?;
        let key = scene.key().clone();
        if !origin.current(self) || !key.request.same(&request) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let ready = SceneUse { origin, scene, key };
        if !ready.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(ready)
    }

    /// 原布局producer在发布前捕获真实请求；只读原Native几何，不要求现成Ready也不创建它。
    pub fn capture_scene_request(&self) -> Result<NativeMarkdownPagedSceneRequest> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let origin = self.origin()?;
        let request = self.scene_request(&origin)?;
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(request)
    }

    /// 原clip通知、宽度或backing变更的真实owner调用；不排队、不layout、不领取新scene。
    pub fn invalidate_geometry(&self) -> Result<()> {
        if self.ivars().closed.get() {
            return Err(NativeMarkdownPagedTextViewError::Closed);
        }
        self.cancel_link_intent(); // 宽度、滚动或frame换代使原鼠标命中失效，不能给旧click补签新坐标。
        let result = advance_geometry(&self.ivars().geometry_epoch, &self.ivars().geometry_failed);
        let cpu = self.ivars().cpu.borrow().clone();
        if let Some(cpu) = cpu {
            cpu.cancel_candidate();
        }
        result
    }
    /// 单格提交准入后只归还未完成paint的Source；不推进几何、不关闭Input或改写已显示像素。
    pub(in crate::native_surface) fn release_scene_readers(&self) -> Result<()> {
        if self.ivars().closed.get() {
            return Err(NativeMarkdownPagedTextViewError::Closed);
        }
        let cpu = self.ivars().cpu.borrow().clone();
        if let Some(cpu) = cpu {
            cpu.release_source_readers()?;
        }
        Ok(())
    }

    /// 模式动作结束原指针寿命并撤旧几何；不关闭输入上下文或重建文稿选择。
    pub(in crate::native_surface) fn prepare_presentation_change(&self) -> Result<()> {
        if self.is_busy() {
            return Err(NativeMarkdownPagedTextViewError::Busy);
        }
        if self.cancel_cell_pointer_intent() {
            return Err(NativeMarkdownPagedTextViewError::Pending);
        }
        self.cancel_link_intent();
        self.invalidate_geometry()
    }
    fn invalidate_geometry_only(&self) {
        if !self.ivars().closed.get()
            && let Err(error) = self.invalidate_geometry()
        {
            self.failure(error);
        }
    }
    /// 原scene真正Ready后由既有调度者调用；标脏有限可见区并通知原输入客户重查坐标。
    /// 通知只消费本次已发布几何，不强制display/layout，也不把先前Pending的坐标签成Ready。
    pub fn scene_ready(&self) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let scene = self.scene()?;
        let cpu = self.ivars().cpu.borrow().clone();
        if let Some(cpu) = cpu {
            cpu.cancel_if_not_key(&scene.key);
        }
        scene.step(self, || {
            self.setNeedsDisplayInRect(scene.key.request.viewport)
        })?;
        // 滚动、缩放和异步排版可在handleEvent之外发布真实后继帧。此前firstRect若返回Pending，
        // 系统仍需在此重新查询；仅标脏像素不会使输入法缓存的候选坐标失效。
        let context = self
            .ivars()
            .context
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
        let context_current = || {
            self.ivars()
                .context
                .borrow()
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, &context))
        };
        scene.step(self, || -> Result<()> {
            if !context_current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            // Rust槽借用已结束；公开通知可能同步回调firstRect/selectedRange，只允许原客户
            // 和原Scene的Input、geometry及发布身份在Native调用前后均仍有效，不激活或切换输入源。
            context.0.invalidateCharacterCoordinates();
            if !context_current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            Ok(())
        })??;
        Ok(())
    }
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.ivars().depth.get() != 0
            || self
                .ivars()
                .input
                .borrow()
                .as_ref()
                .is_some_and(|input| input.is_busy())
    }
    /// 接受并同步拆图，返回原像素完成收据；不能以接受成功代替provider实际归还。
    pub fn begin_close(&self) -> Result<Rc<NativeMarkdownPagedCloseReceipt>> {
        if let Some(receipt) = self.ivars().close_receipt.borrow().clone() {
            return Ok(receipt); // 同次重入或重复请求共享原收据，不重新清理或提前签成功。
        }
        if self.is_busy() {
            return Err(NativeMarkdownPagedTextViewError::Busy);
        }
        let input = self
            .ivars()
            .input
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedTextViewError::Closed)?;
        input.close()?;
        let receipt = NativeMarkdownPagedCloseReceipt::new(self.mtm());
        *self.ivars().close_receipt.borrow_mut() = Some(Rc::clone(&receipt));
        self.ivars().closed.set(true);
        self.close_context_menu(); // 跟踪中的原菜单先结束且失去target/action，不能在关闭后路由到另一篇笔记。
        self.unregisterDraggedTypes(); // 封闭后不再接收新路径或文本，原队列的已有载荷由Body同步归还。
        self.ivars().drag_anchor.set(None);
        self.cancel_cell_pointer_intent();
        self.cancel_link_intent();
        let context = self.ivars().context.borrow_mut().take();
        let cpu = self.ivars().cpu.borrow_mut().take();
        let delegate = self.ivars().delegate.borrow_mut().take();
        let input_slot = self.ivars().input.borrow_mut().take();
        if let Some(cpu) = &cpu {
            cpu.track_close(&receipt);
        } // 输入已封闭、没有在途paint；登记真实像素义务先于任何图像或Native层Drop。
        // 此清理边自行管理 Native 临时对象：contents 等 +0 getter 和 CA 提交可产生
        // autorelease claim，Rust 显式 owner 已 drop 仍不代表原 image/provider 已归还。
        // 局部池只结束本次同步清理的临时 claim，不投递事件或推进业务 runloop。
        let pixels_result = autoreleasepool(|_| {
            // 已封闭正文先退出可见层树，避免祖先显示周期为其重建隐式 backing。
            // 只移除本 view；父视图、外方后来替换的 backing 和迁出的 mount 保持原归属。
            self.removeFromSuperview();
            let result = cpu.as_ref().map_or(Ok(()), |surface| surface.detach(self));
            let had_cpu = cpu.is_some();
            drop(cpu); // 先归还固定 layer 强边，再提交同一次清空事务，不把像素留给下次激活。
            if had_cpu {
                objc2_quartz_core::CATransaction::flush();
            }
            result // Native 撤权仍返回原失败；排空临时 claim 不能把外方资源洗成已释放。
        });
        // deactivate/discard可回调本client，但closed使它们无法重建正文状态。
        if let Some(context) = context {
            context.0.discardMarkedText();
            context.0.deactivate();
        }
        drop(delegate);
        drop(input_slot);
        if let Err(error) = pixels_result {
            receipt.record(NativeMarkdownPagedCloseFailure::View(error));
        } // 接受后错误留在原收据；不伪装为尚未拆图的可重试Err。
        receipt.seal();
        Ok(receipt)
    }
    /// 只返回固定原子收据，不延长layer、image、provider或正文生命期。
    pub fn pixel_observer(&self) -> Option<NativeMarkdownViewportPixelObserver> {
        self.ivars()
            .cpu
            .borrow()
            .as_ref()
            .map(|surface| surface.observer())
    }
    /// 取得固定mount、两组和所有slot的弱收据；先结束owner槽借用再登记Native Weak。
    pub fn cpu_layer_weaks(&self) -> Option<Vec<objc2::rc::Weak<AnyObject>>> {
        let cpu = self.ivars().cpu.borrow().clone();
        cpu.map(|surface| surface.layer_weaks())
    }
    #[must_use]
    pub fn cpu_paints(&self) -> Option<usize> {
        self.ivars()
            .cpu
            .borrow()
            .as_ref()
            .map(|surface| surface.paints())
    }
    /// 同一已发布key证明正式窗口所验的是当前画面；纯Rust弱票不延长实际绘图资源。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn probe_cpu_published_frame(
        &self,
    ) -> Option<(NativeMarkdownPagedSceneKey, usize)> {
        let cpu = self.ivars().cpu.borrow().clone();
        cpu.and_then(|surface| surface.published_frame())
    }

    /// 只计同原Scene且全部实际健康门通过的复用次数，不保留请求历史或业务载荷。
    #[must_use]
    pub fn cpu_reuses(&self) -> Option<usize> {
        self.ivars()
            .cpu
            .borrow()
            .as_ref()
            .map(|surface| surface.reuses())
    }
    /// 显式诊断只读当前实际帧的逐byte图像复用数；不保像素、CGImage或旧帧owner。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub fn cpu_reused_images(&self) -> Option<usize> {
        let cpu = self.ivars().cpu.borrow().clone();
        cpu.and_then(|surface| surface.reused_images())
    }
    /// 仅显式诊断在真实Pending候选中交付一次主队列；不建立新owner或产品输入行为。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub fn probe_cpu_nested_delivery(&self, delivery: impl FnOnce()) -> Result<()> {
        cpu::probe_nested_delivery(self, delivery)
    }
    /// 只读固定标量，不执行队列或用等待补签；实际publish仍由cpu_paints观察。
    #[must_use]
    pub fn cpu_paint_pending(&self) -> Option<bool> {
        self.ivars()
            .cpu
            .borrow()
            .as_ref()
            .map(|surface| surface.pending())
    }
    #[must_use]
    pub fn cpu_paint_steps(&self) -> Option<usize> {
        self.ivars()
            .cpu
            .borrow()
            .as_ref()
            .map(|surface| surface.steps())
    }

    pub fn take_failure(&self) -> Option<NativeMarkdownPagedTextViewError> {
        self.ivars().last_failure.borrow_mut().take()
    }
    /// 外部文件业务失败沿原View反馈；它不撤销健康正文、已提交链接或后继队列权限。
    pub(in crate::native_surface) fn reject_attachment_import(
        &self,
        cause: NativeMarkdownPagedTextViewError,
    ) {
        super::workspace_actions::present_native_markdown_paged_attachment_error(
            self,
            &cause.to_string(),
        );
        self.failure(cause);
    }
    pub fn input_state(&self) -> Result<NativeMarkdownPagedInputState> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        Ok(self.origin()?.state)
    }
    /// 工具栏只查询此刻真实输入客户的局部历史；正文返回None，不创建系统Undo manager。
    pub(in crate::native_surface) fn cell_history_facts(
        &self,
    ) -> Result<Option<omni_markdown_core::MarkdownCellDraftHistoryFacts>> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        Ok(self.origin()?.input.cell_history_facts()?)
    }
    /// 原session纯前台准入供viewport同步原语复用；不捕获较新输入或scene替旧请求续权。
    #[must_use]
    pub fn session_is_current(&self) -> bool {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.foreground().is_ok()
    }
    fn failure(&self, error: NativeMarkdownPagedTextViewError) {
        let mut slot = self.ivars().last_failure.borrow_mut();
        if slot.is_none()
            || (matches!(*slot, Some(NativeMarkdownPagedTextViewError::Pending))
                && !matches!(error, NativeMarkdownPagedTextViewError::Pending))
            || (matches!(
                *slot,
                Some(
                    NativeMarkdownPagedTextViewError::AttachmentImport(_)
                        | NativeMarkdownPagedTextViewError::FoldAction(_)
                )
            ) && !matches!(
                error,
                NativeMarkdownPagedTextViewError::Pending
                    | NativeMarkdownPagedTextViewError::AttachmentImport(_)
                    | NativeMarkdownPagedTextViewError::FoldAction(_)
            ))
        {
            *slot = Some(error);
        } // Pending/外部文件拒绝不能挡住真实源、资源或已提交后果首错；真实首错不被后续普通拒绝覆盖。
    }
    /// 入队前的外部文件拒绝与队首复制失败共用原反馈边；状态栏改变不能撤权已接纳的Word输入。
    fn report_attachment_rejection(&self, cause: &NativeMarkdownPagedTextViewError) {
        let NativeMarkdownPagedTextViewError::AttachmentImport(message) = cause else {
            return;
        };
        let Ok(delegate) = self.foreground() else {
            return;
        };
        if !delegate.defer_attachment_rejection(message) {
            super::workspace_actions::present_native_markdown_paged_attachment_error(
                self,
                &cause.to_string(),
            );
        } // 没有原链时才显示当前原窗口；关闭或换笔记后不复活旧反馈。
    }
    /// 只有顶层真实mutation/command的void回调决定原解释结果；候选坐标Pending保协议负值。
    fn record_input(&self, result: Result<()>) {
        if let Err(cause) = result {
            #[cfg(feature = "native-input-trace")]
            if !matches!(cause, NativeMarkdownPagedTextViewError::CommittedEffect(_)) {
                // 真实Core成功已在原Input发布点记录；后续绘图/撤权失败不能再报canonical拒绝。
                self.reject_input_trace(&cause);
            }
            if matches!(
                &cause,
                NativeMarkdownPagedTextViewError::AttachmentImport(_)
            ) {
                self.report_attachment_rejection(&cause);
                self.failure(cause);
                return; // 实际文件业务拒绝已交付，不伪装成NSTextInputContext/Core资源失败而撤销整条输入链。
            }
            if self.ivars().interpreting_key.get() && self.ivars().input_callback_depth.get() == 1 {
                let mut slot = self.ivars().interpretation_failure.borrow_mut();
                if slot.is_none() {
                    *slot = Some(cause);
                }
            } else {
                self.failure(cause); // 外部/重入回调不认领原event，也不吞掉其真实失败。
            }
        }
    }
    fn record(&self, result: Result<()>) {
        if let Err(error) = result {
            self.report_attachment_rejection(&error);
            self.failure(error);
        }
    }
    fn boolean_result(&self, result: Result<()>) -> bool {
        match result {
            Ok(()) => true,
            Err(error) => {
                self.report_attachment_rejection(&error);
                self.failure(error);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests;
