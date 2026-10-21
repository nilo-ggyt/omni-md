//! The Native input boundary has no TextKit text storage; the original Core model remains authoritative through PagedInput.

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
mod cursor;
mod find;
mod folding;
mod formatting;
mod ime_geometry;
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

/// Production input mounts only a bounded CPU viewport; direct drawing compiles only for explicit Native comparisons.
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
    #[error("当前笔记中没有找到该标题锚点")]
    LinkAnchorNotFound,
    #[error("标题锚点不是有效的UTF-8片段")]
    LinkAnchorInvalid,
    #[error("标题锚点查询失败：{0}")]
    HeadingAnchor(#[from] omni_markdown_core::MarkdownHeadingAnchorError),
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

/// Rc moves the Retained claim outside borrows; the context does not retain its client, so it creates no Native ownership cycle.
struct InputContextOwner(Retained<NSTextInputContext>);

pub struct PagedTextViewIvars {
    input: RefCell<Option<Rc<NativeMarkdownPagedInput>>>,
    delegate: RefCell<Option<Weak<dyn NativeMarkdownPagedTextViewDelegate>>>,
    context: RefCell<Option<Rc<InputContextOwner>>>,
    epoch: u64,
    geometry_epoch: Cell<u64>,
    geometry_failed: Cell<bool>,
    composition_anchor: Cell<Option<ime_geometry::CompositionAnchor>>,
    closed: Cell<bool>,
    depth: Cell<usize>,
    last_failure: RefCell<Option<NativeMarkdownPagedTextViewError>>,
    interpreting_key: Cell<bool>,
    input_callback_depth: Cell<usize>,
    interpretation_failure: RefCell<Option<NativeMarkdownPagedTextViewError>>,
    // One pointer slot retains the initial caret/word seed until mouse-up/close, without an asynchronous Input owner.
    drag_anchor: Cell<Option<commands::DragAnchor>>,
    // Physical Down location and actual Drag facts live only for that gesture; selection-driven layout cannot manufacture pointer movement.
    pointer_motion: Cell<Option<commands::PointerMotion>>,
    // Links commit on original mouse-up; these slots retain weak Scene/gesture identity, not NSEvent, document, or reader.
    pressed_link: RefCell<Option<commands::PressedLink>>,
    link_intent: RefCell<Option<Rc<()>>>,
    // One weak gesture spans Down to Up; asynchronous Cell preparation retains no event and cannot revive dragging after Up.
    cell_pointer_intent: RefCell<Option<Rc<()>>>,
    // After publication an old child weakly identifies its actual parent/gesture, without retaining Input, Scene, document, or retired Body.
    cell_pointer_forward: RefCell<Option<commands::CellPointerForward>>,
    // Autoscroll has one original gesture lifetime; its timer weakly holds View and is revoked by actual Up, new input, or close.
    drag_scroll: RefCell<Option<pointer_scroll::DragScroll>>,
    // Each client owns at most one Native edit menu; close revokes its actions before releasing Input/View resources.
    context_menu: RefCell<Option<Retained<NSMenu>>>,
    render_strategy: NativeMarkdownPagedRenderStrategy,
    cpu: RefCell<Option<Rc<cpu::CpuSurface>>>,
    close_receipt: RefCell<Option<Rc<NativeMarkdownPagedCloseReceipt>>>,
}

/// Complete input facts for this callback; the delegate strong claim lives only on the synchronous stack, not in View or successor events.
#[derive(Clone)]
struct Origin {
    input: Rc<NativeMarkdownPagedInput>,
    delegate: Rc<dyn NativeMarkdownPagedTextViewDelegate>,
    state: NativeMarkdownPagedInputState,
    geometry_epoch: u64,
    epoch: u64,
    /// Only actual NSTextInputClient callbacks or queued menu entry set this; external selection/mouse actions cannot claim receipts through interpretation depth.
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
        // current can synchronously edit/revoke; the final pure check prevents a newer selection from authorizing this original callback.
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

    // SAFETY: NSView implements NSObject behavior; every Native entry in this class is main-thread-only.
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
            self.stop_drag_scroll(); // Losing client focus revokes autoscroll from the original out-of-bounds drag.
            self.cancel_link_intent(); // Focus transfer revokes external-open authority from an unfinished old link gesture.
            if self.ivars().closed.get() {
                true // An empty closed View must not prevent its window from detaching first responder.
            } else {
                let result = (|| {
                let input = self.ivars().input.borrow().clone().ok_or(NativeMarkdownPagedTextViewError::Closed)?;
                let before = input.state()?;
                let context = self.ivars().context.borrow().clone().ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
                let delegate = self.ivars().delegate.borrow().as_ref().and_then(Weak::upgrade)
                    .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
                let epoch = self.ivars().epoch;
                if !self.same_input_context(&input, &delegate, &context)
                    || (before.marked.is_some() && !delegate.current(epoch))
                    || !self.same_input_context(&input, &delegate, &context)
                { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                // IMK deactivation may legitimately call insertText/unmarkText. Preserve original marked-range replacement authority first.
                // Unmarking locally first would insert confirmation at the caret and then misdiagnose the actual commit as revoked input.
                // Each callback validates its complete Input permit; this boundary checks client identity while allowing actual Input changes.
                let deactivated = self.input_context_step(|| context.0.deactivate());
                if self.ivars().closed.get() {
                    if let Err(cause) = deactivated { self.failure(cause); }
                    return Ok(()); // An empty closed client cannot block window detach; preserve actual callback failure in the original slot.
                }
                if self.ivars().epoch != epoch || !self.same_input_context(&input, &delegate, &context) {
                    return deactivated.and(Err(NativeMarkdownPagedTextViewError::Revoked));
                }
                if input.state()?.marked.is_some() {
                    // If the IME did not finish composition, original Input accepts the displayed candidate before ending that conversion session.
                    // A failed layout owner without marked text can still resign without acquiring new editing/Scene authority just to release its client.
                    let finished = self.finish_composition_in_context(&input, &delegate, &context, epoch);
                    deactivated?;
                    return finished;
                }
                if before.marked.is_some() && !delegate.current(epoch) {
                    return deactivated.and(Err(NativeMarkdownPagedTextViewError::Revoked));
                }
                match deactivated {
                    Err(cause @ NativeMarkdownPagedTextViewError::Revoked)
                        if before.marked.is_none() && !delegate.current(epoch) => {
                            // The failed Body has no composition; a late deactivation callback can only be revoked.
                            // Keep that failure without trapping the empty client in focus; Source and other failures still propagate.
                            self.failure(cause);
                            Ok(())
                        }
                    result => result,
                }
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
            self.cancel_link_intent(); // Even a key without text changes revokes the old click so it cannot open a link later.
            let result = (|| {
                let origin = self.origin()?;
                let current_input = |epoch| epoch == origin.epoch && origin.current(self);
                let permit = origin.input.capture(origin.epoch, &current_input)?;
                if origin.delegate.defer_key_down(event, &permit)? {
                    return Ok(()); // The original key has not reached the IME; this does not falsely complete its marked protocol asynchronously.
                }
                let (delegate, context) = self.foreground_context()?;
                let current = || !self.ivars().closed.get()
                    && delegate.current(self.ivars().epoch) && !self.ivars().closed.get();
                if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                // The custom NSView sends the original key to its system context; handleEvent owns IME and key-binding interpretation.
                // Callbacks may change text/marked/selection; check session lifetime afterward rather than require the old generation.
                let consumed = context.0.handleEvent(event);
                if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                if !consumed {
                    // Propagate once along the responder chain only when the system did not consume the key; never interpret it twice.
                    // SAFETY: The main-thread callback protects this NSView; keyDown: borrows its live NSEvent synchronously.
                    unsafe { let _: () = msg_send![super(self), keyDown: event]; }
                    if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                }
                Ok(())
            })();
            self.record(result);
            #[cfg(feature = "native-input-trace")]
            self.finish_input_trace_scope(trace_scope);
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let result = (|| {
                let input = self.ivars().input.borrow().clone()
                    .ok_or(NativeMarkdownPagedTextViewError::Closed)?;
                let (delegate, context) = self.foreground_context()?;
                let epoch = self.ivars().epoch;
                let current = || self.same_input_context(&input, &delegate, &context)
                    && delegate.current(epoch)
                    && self.same_input_context(&input, &delegate, &context);
                if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                // NSResponder normally forwards modifiers to nextResponder without notifying this client's input context.
                // The IME may use Shift press/release to finish or switch composition, so the original context must interpret it.
                // Callbacks can mutate original Input; check client lifetime before/after without requiring old marked text or selection.
                // flagsChanged has no character/repeat-key contract and does not borrow keyDown tracing, queuing, or character properties.
                let consumed = context.0.handleEvent(event);
                if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                if !consumed {
                    // Keep the responder chain only if unconsumed; do not replay or independently interpret this modifier event.
                    // SAFETY: The callback protects the main-thread View and synchronously borrows the original flagsChanged: event.
                    unsafe { let _: () = msg_send![super(self), flagsChanged: event]; }
                    if !current() { return Err(NativeMarkdownPagedTextViewError::Revoked); }
                }
                Ok(())
            })();
            self.record(result);
        }

        #[unsafe(method(scrollWheel:))]
        /// A real wheel event is new viewport intent: revoke old mode positioning even during layout reentry, then propagate scroll once.
        fn scroll_wheel(&self, event: &NSEvent) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let result = (|| {
                let delegate = self.foreground()?;
                delegate.user_scroll(self.ivars().epoch)?;
                // SAFETY: The original main-thread NSView/NSEvent are live; retain the parent responder's scrollWheel: semantics.
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
        /// Provide the current Body's Native context menu without moving selection or creating shortcuts/windows.
        fn menu_for_event(&self, _event: &NSEvent) -> Option<Retained<NSMenu>> {
            match self.edit_context_menu() {
                Ok(menu) => Some(menu),
                Err(cause) => { self.failure(cause); None }
            }
        }

        #[unsafe(method(draggingEntered:))]
        /// Native drag-and-drop advertises supported confined file-copy/plain-text input, not rich text or image copies.
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            self.accepted_drag_operation(sender)
        }

        #[unsafe(method(performDragOperation:))]
        /// At actual drop capture the same visible Scene hit; file work then follows the original input queue.
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let result = self.accept_drop(sender);
            self.boolean_result(result)
        }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            if self.ivars().render_strategy == NativeMarkdownPagedRenderStrategy::CpuViewport { true }
            else {
                // SAFETY: Use NSView's public zero-argument BOOL getter; the Direct comparison retains super semantics.
                unsafe { msg_send![super(self), wantsUpdateLayer] }
            }
        }
        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            if self.ivars().render_strategy != NativeMarkdownPagedRenderStrategy::CpuViewport {
                // SAFETY: Forward Direct to NSView once without mixing CPU publication and window CG drawing.
                unsafe { let _: () = msg_send![super(self), updateLayer]; }
                return;
            }
            let cpu = self.ivars().cpu.borrow().clone();
            let Some(surface) = cpu else {
                self.failure(NativeMarkdownPagedTextViewError::Closed);
                return;
            };
            // Display may abandon a request superseded by actual input or geometry during acquisition.
            // Input commands retain strict scene() checks; actual CPU paint/resource failures still enter the original error slot.
            self.record(surface.paint_current(self));
        }
        #[unsafe(method(resetCursorRects))]
        /// AppKit owns cursor regions; register link cursors only for the current complete CPU frame, without tracking continuations.
        fn reset_cursor_rects(&self) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.discardCursorRects();
            if self.ivars().closed.get() { return; }
            let result = self.install_link_cursor_rects();
            if result.is_err() { self.discardCursorRects(); }
            self.record(result);
        }
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if self.ivars().render_strategy == NativeMarkdownPagedRenderStrategy::CpuViewport { return; }

            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            let scene = match self.scene() {
                Ok(scene) => scene,
                Err(NativeMarkdownPagedTextViewError::Pending) => {
                    // Direct also consumes published output only; unfinished preparation preserves scheduling and the first input error.
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
            self.record(result); // After drawing starts preserve its cause; a waiting-for-frame branch must not hide Source/resource effects.
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: objc2_foundation::NSSize) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            if self.frame().size != size { self.invalidate_geometry_only(); }
            // SAFETY: Match NSView's public setter signature and forward the original argument once.
            unsafe { let _: () = msg_send![super(self), setFrameSize: size]; }
        }
        #[unsafe(method(setFrameOrigin:))]
        fn set_frame_origin(&self, origin: NSPoint) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            if self.frame().origin != origin { self.invalidate_geometry_only(); }
            // SAFETY: Forward the bounded View's position once; its original owner separately supplies virtual document offset.
            unsafe { let _: () = msg_send![super(self), setFrameOrigin: origin]; }
        }
        #[unsafe(method(setFrame:))]
        fn set_frame(&self, frame: NSRect) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            if self.frame() != frame { self.invalidate_geometry_only(); }
            // SAFETY: Match the public NSView setter without borrowing text or allocating a document-height surface.
            unsafe { let _: () = msg_send![super(self), setFrame: frame]; }
        }
        #[unsafe(method(setBounds:))]
        fn set_bounds(&self, bounds: NSRect) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            if self.bounds() != bounds { self.invalidate_geometry_only(); }
            // SAFETY: Forward original bounds to this NSView without publishing another Scene in the callback.
            unsafe { let _: () = msg_send![super(self), setBounds: bounds]; }
        }
        #[unsafe(method(viewDidMoveToWindow))]
        fn did_move_to_window(&self) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.invalidate_geometry_only();
            // SAFETY: Forward the NSView lifecycle callback synchronously once without creating windows/events.
            unsafe { let _: () = msg_send![super(self), viewDidMoveToWindow]; }
        }
        #[unsafe(method(viewDidChangeBackingProperties))]
        fn backing_changed(&self) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.invalidate_geometry_only();
            // SAFETY: Use NSView's public backing lifecycle callback without inspecting/mutating unrelated sublayers.
            unsafe { let _: () = msg_send![super(self), viewDidChangeBackingProperties]; }
        }
        #[unsafe(method(viewDidMoveToSuperview))]
        fn superview_changed(&self) {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            self.invalidate_geometry_only();
            // SAFETY: Forward the public mounting callback once; Root owns actual clip notifications and virtual offset.
            unsafe { let _: () = msg_send![super(self), viewDidMoveToSuperview]; }
        }

        // Menus/responder actions use the same typed command entry; the original NSTextInputContext interprets system keys.
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

    // SAFETY: Every required method matches the public NSTextInputClient signature; all source ranges come from the sole Core model.
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
            // Multiple selection retains its actual ordered primary range; history anchor/head does not identify current primary direction.
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
            NSArray::new() // Input consumes candidate characters/selection only, without claiming support for unrendered Native attributes.
        }
        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        unsafe fn attributed_substring(&self, range: NSRange, actual: *mut NSRange) -> Option<Retained<NSAttributedString>> {
            // SAFETY: Delegate writable/null output handling to the synchronous Rust helper without changing ABI or return ownership family.
            unsafe { self.native_substring(range, actual) }
        }
        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        unsafe fn first_rect(&self, range: NSRange, actual: *mut NSRange) -> NSRect {
            let _use = PayloadUseGuard::enter(&self.ivars().depth);
            // SAFETY: The protocol output is writable or null; ZERO plus NotFound never claims geometry Ready.
            unsafe { write_actual(actual, not_found()); }
            let result = match self.screen_rect(range) {
                Err(NativeMarkdownPagedTextViewError::Pending) => self.composition_screen_rect(range),
                result => result,
            };
            match result {
                Ok(value) => {
                    // SAFETY: Write only the current Scene's actual range or the current composition origin's empty caret range.
                    // The latter positions the candidate window only; it cannot stand in for unfinished new glyph layout.
                    unsafe { write_actual(actual, value.actual_range); }
                    value.rect
                }
                Err(NativeMarkdownPagedTextViewError::Pending) => {
                    // Report missing coordinates only if this composition has no actual insertion point; Core admission may still have succeeded.
                    NSRect::ZERO
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
                    // Before hit geometry is published return NotFound, without polluting the independent input error slot or authorizing old positions.
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
                self.command_from_input_context(selector) // Record even UnsupportedCommand; Scene ambiguity/missing functionality cannot silently fall through to another responder.
            } else {
                // SAFETY: Only the system NSTextInputClient action entry guarantees that an unknown selector has the sender signature.
                // Safe Rust command does not call arbitrary Sel with an object argument; forward this action once along the original chain.
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
        // SAFETY: Only public NSTextInputClient ABI entries supply this pointer; callers guarantee a complete writable NSRange.
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

/// An unmounted NSView may report NSInfiniteRect; actual drawing remains confined to that View's real bounds.
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
        failed.set(true); // The MAX authority is permanently invalid too; an early closed return cannot skip actual context/View cleanup.
        return Err(NativeMarkdownPagedTextViewError::CounterExhausted);
    };
    epoch.set(next);
    Ok(())
}

impl NativeMarkdownPagedTextView {
    /// Run two actual unpublished-installation revocation controls using short synthetic graphs under an explicit diagnostic feature.
    ///
    /// # Errors
    /// Reject missing revocation, rollback that takes a successor parent layer, or any Native/document resource not actually released.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub fn probe_cpu_installation_rollback(mtm: MainThreadMarker) -> Result<()> {
        cpu::probe_installation_rollback(mtm)
    }

    /// Explicit synthetic diagnostics verify actual mask/alpha-composite output and external relationships, without product/user documents.
    ///
    /// # Errors
    /// Reject incorrect RGBA, original Native identity, external relationships, or actual pixel/weak release.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub fn probe_cpu_mask_slots(mtm: MainThreadMarker) -> Result<()> {
        cpu::probe_mask_slots(mtm)
    }

    /// Create the real NSView and sole NSTextInputContext; module availability does not switch the default surface.
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

    /// The explicit backend supports same-Source mechanism acceptance, with no environment defaults or hidden product switch.
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
            composition_anchor: Cell::new(None),
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
        // SAFETY: A newly allocated main-thread NSView with complete ivars/bounded frame satisfies its designated initializer contract.
        let view: Retained<Self> = unsafe { msg_send![super(allocated), initWithFrame: frame] };
        if !owner.current(epoch) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // The context contract does not retain its client; create neither NSTextView nor a text mirror here.
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
        // A custom NSView lacks NSTextView drag registration; advertise only types handled by this bounded text/file entry.
        // SAFETY: AppKit's immutable public type constants have process lifetime and read no clipboard/document content.
        let drag_types = unsafe {
            [
                objc2_app_kit::NSPasteboardTypeFileURL,
                objc2_app_kit::NSPasteboardTypeString,
            ]
        };
        view.registerForDraggedTypes(&NSArray::from_slice(&drag_types));
        if render_strategy == NativeMarkdownPagedRenderStrategy::CpuViewport {
            // Return temporary view/layer claims from backing activation at the end of this assembly.
            // The sole CPU owner retains its layers explicitly, preventing an external long-lived pool from postponing closed View destruction.
            autoreleasepool(|_| -> Result<()> {
                let cpu = cpu::CpuSurface::install(&view)?;
                view.ivars().cpu.replace(Some(cpu)); // Publish the complete owner before pool drainage, so Native destruction callbacks see the same assembly.
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
        let retained = Retained::clone(&context.0); // Release the RefCell borrow before Native retain.
        (!self.ivars().closed.get() && delegate.current(self.ivars().epoch)).then_some(retained)
    }
    unsafe fn native_substring(
        &self,
        range: NSRange,
        actual: *mut NSRange,
    ) -> Option<Retained<NSAttributedString>> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        // SAFETY: The original protocol entry guarantees writable/null actual; failure explicitly leaves no actual range.
        unsafe {
            write_actual(actual, not_found());
        }
        match self.substring(range) {
            Ok(Some((value, range))) => {
                // SAFETY: Write the original output only after the bounded result is formed under the same authority.
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
        // The preceding getter's full exit check authorizes the adjacent call; no intervening reentry, borrow, or destruction occurs.
        // Check original Input/epoch/geometry after each Native return, not once after a group of operations.
        let bounds = self.bounds();
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let viewport =
            bounded_viewport(visible, bounds).ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        // bounded_viewport computes Copy rectangles only; use the same exit authority for parent coordinates without recapturing a successor.
        let frame_in_parent = self.frame();
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let virtual_viewport = origin
            .delegate
            .virtual_viewport(origin.epoch, bounds.size.width);
        // The delegate may reenter/fail; check original authority before unwrapping its result to preserve revocation priority.
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let virtual_viewport = virtual_viewport?;
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
        self.scene_for_origin(origin)
    }

    /// Scene acquisition follows the same original Input/geometry facts without recapturing a successor after Native reentry.
    fn scene_for_origin(&self, origin: Origin) -> Result<SceneUse> {
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

    /// The layout producer captures its actual request before publication, reading Native geometry without requiring or creating Ready output.
    pub fn capture_scene_request(&self) -> Result<NativeMarkdownPagedSceneRequest> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let origin = self.origin()?;
        let request = self.scene_request(&origin)?;
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(request)
    }

    /// The actual clip/width/backing owner calls this without queuing, laying out, or acquiring another Scene.
    pub fn invalidate_geometry(&self) -> Result<()> {
        if self.ivars().closed.get() {
            return Err(NativeMarkdownPagedTextViewError::Closed);
        }
        self.ivars().composition_anchor.set(None); // Actual geometry change ends the original composition-origin coordinate handoff.
        self.cancel_link_intent(); // Width/scroll/frame changes revoke the original hit; new coordinates cannot renew an old click.
        let result = advance_geometry(&self.ivars().geometry_epoch, &self.ivars().geometry_failed);
        self.discardCursorRects(); // Advance geometry before discarding regions so Native reentry cannot reinstall retired coordinates.
        let cpu = self.ivars().cpu.borrow().clone();
        if let Some(cpu) = cpu {
            cpu.cancel_candidate();
        }
        result
    }
    /// When Body retires Scene, synchronously revoke this View's sole unpublished candidate; Input/geometry/published pixels retain their owners.
    /// Acquire no successor Scene; discard cursor regions after Scene leaves its slot, and end CPU borrows before returning context/tiles.
    /// Queued closures hold only weak identity/serial and exit on revocation, without treating Scene retirement as Input failure.
    pub(in crate::native_surface) fn retire_scene_candidate(&self) {
        self.discardCursorRects(); // Retire cursor regions with Scene rather than wait for new Input/CPU to cover old link cursors.
        let cpu = self.ivars().cpu.borrow().clone();
        if let Some(cpu) = cpu {
            cpu.cancel_candidate();
        }
    }
    /// After Cell admission return unfinished paint's Source readers only, without advancing geometry, closing Input, or rewriting displayed pixels.
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

    /// A mode change ends pointer lifetime/revokes geometry without closing the input context or rebuilding document selection.
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
    /// The existing scheduler calls after actual Scene readiness, invalidating bounded display and notifying the original client to query coordinates.
    /// Consume published geometry only; do not force display/layout or certify earlier Pending coordinates as Ready.
    pub fn scene_ready(&self) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let scene = self.scene()?;
        self.refresh_composition_anchor(&scene);
        let cpu = self.ivars().cpu.borrow().clone();
        if let Some(cpu) = cpu {
            cpu.cancel_if_not_key(&scene.key);
        }
        scene.step(self, || {
            self.setNeedsDisplayInRect(scene.key.request.viewport)
        })?;
        // Scroll/resize/asynchronous layout may publish a successor outside handleEvent. If firstRect was Pending,
        // the system must query again here; invalidating pixels alone does not invalidate cached IME candidate coordinates.
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
            // Rust borrows have ended. The notification may call firstRect/selectedRange, requiring the original client and Scene
            // Input/geometry/publication identity before and after Native, without activating or switching input sources.
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
    /// Accept synchronous graph detachment and return its pixel receipt; acceptance is not actual provider release.
    pub fn begin_close(&self) -> Result<Rc<NativeMarkdownPagedCloseReceipt>> {
        if let Some(receipt) = self.ivars().close_receipt.borrow().clone() {
            return Ok(receipt); // Reentry/repetition shares the original receipt without repeating cleanup or certifying early success.
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
        self.discardCursorRects(); // A framework-retained empty shell must retain no old-document link hit regions.
        self.ivars().composition_anchor.set(None); // A system-cached empty View retains no original document position/input state.
        self.close_context_menu(); // End menu tracking and revoke target/action before close can route an old action into another note.
        self.unregisterDraggedTypes(); // Closed input accepts no paths/text; Body synchronously returns payload already in its queue.
        self.ivars().drag_anchor.set(None);
        self.cancel_cell_pointer_intent();
        self.cancel_link_intent();
        let context = self.ivars().context.borrow_mut().take();
        let cpu = self.ivars().cpu.borrow_mut().take();
        let delegate = self.ivars().delegate.borrow_mut().take();
        let input_slot = self.ivars().input.borrow_mut().take();
        if let Some(cpu) = &cpu {
            cpu.track_close(&receipt);
        } // Input is closed with no active paint; register actual pixel obligations before dropping images or Native layers.
        // This cleanup boundary owns Native temporaries: +0 getters such as contents and CA commit may create
        // autoreleased claims; dropping explicit Rust owners does not prove original image/provider release.
        // The local pool drains only this synchronous cleanup's temporary claims, without dispatching events or advancing a business runloop.
        let pixels_result = autoreleasepool(|_| {
            // Remove closed Body from the visible tree first so an ancestor display cycle cannot rebuild implicit backing.
            // Remove only this View; parent views, externally replaced backing, and relocated mounts retain their ownership.
            self.removeFromSuperview();
            let result = cpu.as_ref().map_or(Ok(()), |surface| surface.detach(self));
            let had_cpu = cpu.is_some();
            drop(cpu); // Return fixed layer strong claims before flushing this clearing transaction; do not defer pixels until next activation.
            if had_cpu {
                objc2_quartz_core::CATransaction::flush();
            }
            result // Native revocation retains its failure; draining temporary claims cannot certify external resources as released.
        });
        // deactivate/discard may call this client, but closed prevents reconstruction of document state.
        if let Some(context) = context {
            context.0.discardMarkedText();
            context.0.deactivate();
        }
        drop(delegate);
        drop(input_slot);
        if let Err(error) = pixels_result {
            receipt.record(NativeMarkdownPagedCloseFailure::View(error));
        } // After acceptance retain errors in the original receipt rather than pretend detachment has not started and may be retried.
        receipt.seal();
        Ok(receipt)
    }
    /// Return fixed atomic observation without retaining layers, images, providers, or text.
    pub fn pixel_observer(&self) -> Option<NativeMarkdownViewportPixelObserver> {
        self.ivars()
            .cpu
            .borrow()
            .as_ref()
            .map(|surface| surface.observer())
    }
    /// Acquire weak identities for the fixed mount/groups/slots after returning owner-slot borrows before Native weak registration.
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
    /// The published key identifies the actual current frame; pure Rust weak authority retains no drawing resources.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn probe_cpu_published_frame(
        &self,
    ) -> Option<(NativeMarkdownPagedSceneKey, usize)> {
        let cpu = self.ivars().cpu.borrow().clone();
        cpu.and_then(|surface| surface.published_frame())
    }

    /// Count reuse only for original Scene after actual health checks, without retaining request history/business payload.
    #[must_use]
    pub fn cpu_reuses(&self) -> Option<usize> {
        self.ivars()
            .cpu
            .borrow()
            .as_ref()
            .map(|surface| surface.reuses())
    }
    /// Explicit diagnostics inspect actual byte-identical frame reuse counts without retaining pixels, CGImage, or old frame owners.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub fn cpu_reused_images(&self) -> Option<usize> {
        let cpu = self.ivars().cpu.borrow().clone();
        cpu.and_then(|surface| surface.reused_images())
    }
    /// Explicit diagnostics deliver the main queue once during an actual Pending candidate, without another owner or product input path.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub fn probe_cpu_nested_delivery(&self, delivery: impl FnOnce()) -> Result<()> {
        cpu::probe_nested_delivery(self, delivery)
    }
    /// Read scalar state without running queues or waiting to renew authority; cpu_paints still observes actual publication.
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
    /// Report external file-use-case failure through original View without revoking healthy text, committed links, or successor queue authority.
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
    /// Toolbar queries actual local client history; Body returns None without creating a system Undo manager.
    pub(in crate::native_surface) fn cell_history_facts(
        &self,
    ) -> Result<Option<omni_markdown_core::MarkdownCellDraftHistoryFacts>> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        Ok(self.origin()?.input.cell_history_facts()?)
    }
    /// Viewport primitives reuse pure original-session foreground admission without capturing newer Input/Scene to renew an old request.
    #[must_use]
    pub fn session_is_current(&self) -> bool {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.foreground().is_ok()
    }
    #[cfg_attr(feature = "native-close-stage-diagnostic", track_caller)]
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
            #[cfg(feature = "native-close-stage-diagnostic")]
            let report_revoked = matches!(error, NativeMarkdownPagedTextViewError::Revoked);
            *slot = Some(error);
            drop(slot);
            #[cfg(feature = "native-close-stage-diagnostic")]
            if report_revoked {
                // Locate the first revocation actually stored after returning the slot borrow, without logging Input state, text, or Native objects.
                let caller = std::panic::Location::caller();
                eprintln!(
                    "native_view_first_failure cause=Revoked file={} line={} column={}",
                    caller.file(),
                    caller.line(),
                    caller.column(),
                );
            }
        } // Pending/file rejection cannot mask Source/resource/postcommit failures; later ordinary rejection cannot overwrite the first actual failure.
    }
    /// Prequeue file rejection and head-of-queue copy failure share feedback; status changes cannot revoke accepted word input.
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
        } // Present in the original current window only without an existing chain; close/note switch must not revive old feedback.
    }
    /// Only the top-level actual mutation/command void callback decides interpretation outcome; Pending candidate coordinates retain protocol negative values.
    #[cfg_attr(feature = "native-close-stage-diagnostic", track_caller)]
    fn record_input(&self, result: Result<()>) {
        if let Err(cause) = result {
            #[cfg(feature = "native-input-trace")]
            if !matches!(cause, NativeMarkdownPagedTextViewError::CommittedEffect(_)) {
                // Core success was recorded at original Input publication; later paint/revocation failure cannot report canonical rejection again.
                self.reject_input_trace(&cause);
            }
            if matches!(
                &cause,
                NativeMarkdownPagedTextViewError::AttachmentImport(_)
            ) {
                self.report_attachment_rejection(&cause);
                self.failure(cause);
                return; // File-use-case rejection is delivered; do not misclassify it as input-context/Core resource failure and revoke the entire chain.
            }
            if self.ivars().interpreting_key.get() && self.ivars().input_callback_depth.get() == 1 {
                let mut slot = self.ivars().interpretation_failure.borrow_mut();
                if slot.is_none() {
                    *slot = Some(cause);
                }
            } else {
                self.failure(cause); // External/reentrant callbacks neither claim the original event nor swallow their actual failure.
            }
        }
    }
    #[cfg_attr(feature = "native-close-stage-diagnostic", track_caller)]
    fn record(&self, result: Result<()>) {
        if let Err(error) = result {
            self.report_attachment_rejection(&error);
            self.failure(error);
        }
    }
    #[cfg_attr(feature = "native-close-stage-diagnostic", track_caller)]
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
