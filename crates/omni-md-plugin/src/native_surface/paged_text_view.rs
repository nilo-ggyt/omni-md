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
use objc2_app_kit::{NSEvent, NSGraphicsContext, NSTextInputClient, NSTextInputContext, NSView};
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

mod close;
mod commands;
mod cpu;
mod find;
mod formatting;
mod input;
mod scene;
pub use super::viewport_pixels::{
    NativeMarkdownViewportPixelObserver, NativeMarkdownViewportPixelSnapshot,
};
pub use close::{NativeMarkdownPagedCloseFailure, NativeMarkdownPagedCloseReceipt};
pub use find::NativeMarkdownPagedFindVisibleProof;
pub use scene::*;

/// Direct保留真实窗口绘制对照；CpuViewport只改变同scene的像素后端，默认产品不切换。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownPagedRenderStrategy {
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
    // 同一指针槽保存首击caret或word种子；鼠标抬起/关闭清空它，不持异步输入owner。
    drag_anchor: Cell<Option<commands::DragAnchor>>,
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
            if self.ivars().closed.get() {
                true // 已封闭的空view不能阻止原窗口断开firstResponder。
            } else {
                let result = (|| {
                self.apply_unmark()?;
                let (delegate, context) = self.foreground_context()?;
                context.0.deactivate();
                if !delegate.current(self.ivars().epoch) { return Err(NativeMarkdownPagedTextViewError::Revoked); }
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
            let result = (|| {
                let (delegate, _context) = self.foreground_context()?;
                let current = || !self.ivars().closed.get()
                    && delegate.current(self.ivars().epoch) && !self.ivars().closed.get();
                if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                // 系统key binding与IME只解释一次；不再先handleEvent再重复interpret同一按键。
                let events = NSArray::from_slice(&[event]);
                if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                self.interpretKeyEvents(&events);
                if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                Ok(())
            })();
            self.record(result);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) { self.mouse(event, false); }
        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) { self.mouse(event, true); }
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) { self.ivars().drag_anchor.set(None); }

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
            let result = (|| {
                let surface = cpu.ok_or(NativeMarkdownPagedTextViewError::Closed)?;
                let scene = self.scene()?;
                surface.paint(self, &scene)
            })();
            self.record(result); // Pending没有新image；实际canvas未成功绝不增加paint或签Ready。
        }
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if self.ivars().render_strategy == NativeMarkdownPagedRenderStrategy::CpuViewport { return; }

            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let result = (|| {
                let scene = self.scene()?;
                let context = scene.step(self, NSGraphicsContext::currentContext)?.ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
                let cg = scene.step(self, || context.CGContext())?;
                scene.scene.paint(&cg, &|| scene.current(self))?;
                if !scene.current(self) { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                Ok(())
            })();
            self.record(result); // Pending不画旧image，不触发layout/重试，也不把零坐标签成Ready。
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

        // 菜单和responder目标沿相同typed命令入口，系统物理按键仍由interpretKeyEvents解释。
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
            self.record(self.apply_native_input(text, replacement, None));
        }
        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        unsafe fn set_marked_text(&self, text: &AnyObject, selected: NSRange, replacement: NSRange) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.record(self.apply_native_input(text, replacement, Some(selected)));
        }
        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.record(self.apply_unmark());
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
                Err(error) => { self.failure(error); NSRect::ZERO }
            }
        }
        #[unsafe(method(characterIndexForPoint:))]
        fn character_index(&self, point: NSPoint) -> usize {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            match self.screen_hit(point) { Ok(value) => value, Err(error) => { self.failure(error); NSNotFound as usize } }
        }
        #[unsafe(method(doCommandBySelector:))]
        unsafe fn do_command(&self, selector: Sel) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let result = if commands::recognizes_selector(selector) {
                self.command(selector) // 原命令即使返回UnsupportedCommand也须记录；Scene歧义/功能缺失不能静默交给别的responder。
            } else {
                // SAFETY: 只有系统原NSTextInputClient action入口可保证未知selector仍为sender签名。
                // 安全Rust command不会把任意Sel当对象参数函数调用；转发仍使用原链和同一action一次。
                unsafe { self.forward_native_action(selector) }
            };
            self.record(result);
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
            NativeMarkdownPagedRenderStrategy::Direct,
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
            drag_anchor: Cell::new(None),
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
        if render_strategy == NativeMarkdownPagedRenderStrategy::CpuViewport {
            let cpu = cpu::CpuSurface::install(&view)?;
            view.ivars().cpu.replace(Some(cpu)); // 只在Native构造全部返回后发布唯一owner槽。
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
        let virtual_viewport =
            origin.step(self, || origin.delegate.virtual_viewport(origin.epoch))??;
        if !finite_rect(viewport, true)
            || !bounds.size.width.is_finite()
            || bounds.size.width <= 0.0
        {
            return Err(NativeMarkdownPagedTextViewError::Pending);
        }
        if !virtual_viewport.document_origin.x.is_finite()
            || !virtual_viewport.document_origin.y.is_finite()
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
    fn invalidate_geometry_only(&self) {
        if !self.ivars().closed.get()
            && let Err(error) = self.invalidate_geometry()
        {
            self.failure(error);
        }
    }
    /// 原scene真正Ready后由既有调度者调用；只使实际有限可见区标脏，不强制display/layout。
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
        self.ivars().drag_anchor.set(None);
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
    /// 异步能力结论只交付原动作失败；不撤销健康Body或后续已接纳输入。
    pub(in crate::native_surface) fn reject_word_move(&self) {
        self.failure(NativeMarkdownPagedTextViewError::UnsupportedCommand);
    }
    pub fn input_state(&self) -> Result<NativeMarkdownPagedInputState> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        Ok(self.origin()?.state)
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
        {
            *slot = Some(error);
        } // Pending只是可用性状态；不能挡住后来的真实源/输入错误，真实首错也不被嵌套Pending覆盖。
    }
    fn record(&self, result: Result<()>) {
        if let Err(error) = result {
            self.failure(error);
        }
    }
    fn boolean_result(&self, result: Result<()>) -> bool {
        match result {
            Ok(()) => true,
            Err(error) => {
                self.failure(error);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests;
