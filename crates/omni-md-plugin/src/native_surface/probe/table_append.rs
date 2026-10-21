//! 正式分页表格的真实窗口、单格输入、格式、保存和冲突验收。
//! 所有等待都观察原输入客户已经发布的 CPU 帧；不定义测试端口或替代正文。

use super::*;
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewDelegate,
};

/// 仅等待原鼠标或 Tab 实际创建的下一张格正文；不激活格、不推进执行器，也不补发输入。
/// 每轮只借一次正式窗口，核同一父 Body、文稿和完整 Input，退租后才进入原主 runloop。
/// 返回弱票避免测试把换走的格保活；真实 Ready 与 CPU 像素同 key 才能结束三秒期限。
pub(in crate::native_surface) fn wait_for_paged_cell(
    parent: std::rc::Weak<NativeMarkdownPagedBody>,
    document: WeakMarkdownDocument,
    input: NativeMarkdownPagedInputState,
    previous: Option<std::rc::Weak<NativeMarkdownPagedBody>>,
    started: Instant,
) -> Result<std::rc::Weak<NativeMarkdownPagedBody>, String> {
    loop {
        if started.elapsed() >= Duration::from_secs(3) {
            return Err("原单格动作三秒内没有当前焦点中的真实 CPU 帧".into());
        }
        let ready = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("原单格父正文已释放")?;
            let same_parent = || {
                native_markdown_controller_has_foreground_authority(&surface.controller)
                    && WeakMarkdownDocument::ptr_eq(&document, &Rc::downgrade(editor.document()))
                    && std::rc::Weak::ptr_eq(&parent, &Rc::downgrade(&editor.body))
                    && editor
                        .text_view
                        .input_state()
                        .is_ok_and(|state| state == input)
            };
            if !same_parent() {
                return Err("原单格动作的父 Body、文稿或完整 Input 已撤销".into());
            }
            if let Some(cause) = editor.body.take_failure() {
                return Err(format!("原单格父排版失败：{cause}"));
            }
            if let Some(cause) = editor.text_view.take_failure()
                && !matches!(cause, crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending)
            {
                return Err(format!("原单格父输入失败：{cause}"));
            }
            let Some(child) = editor.body.active_cell_body() else {
                return Ok(None);
            };
            let identity = Rc::downgrade(&child);
            if previous
                .as_ref()
                .is_some_and(|old| std::rc::Weak::ptr_eq(old, &identity))
            {
                return Ok(None); // Tab 尚未交接时，旧客户即使有完整帧也不是后继格。
            }
            if let Some(cause) = child.take_failure() {
                return Err(format!("原单格子排版失败：{cause}"));
            }
            let view = child.view().map_err(|cause| cause.to_string())?;
            if let Some(cause) = view.take_failure()
                && !matches!(cause, crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending)
            {
                return Err(format!("原单格子输入失败：{cause}"));
            }
            let focused = surface.panel.firstResponder().is_some_and(|responder| {
                let actual: &objc2::runtime::AnyObject = responder.as_ref();
                actual.downcast_ref::<crate::native_surface::paged_text_view::NativeMarkdownPagedTextView>()
                    .is_some_and(|actual| std::ptr::eq(actual, &**view))
            });
            // Native 焦点 getter 可重入，返回后再次核原父票；不得给后来安装的格续权。
            if !same_parent() {
                return Err("原单格焦点查询后父动作已撤销".into());
            }
            if focused
                && !child.is_preparing()
                && let Ok(key) = view.capture_ready_scene_key()
                && let Some((published, paints)) = view.probe_cpu_published_frame()
                && paints != 0
                && published.same(&key)
                && key.request.input == view.input_state().map_err(|cause| cause.to_string())?
                && view.cpu_paints() == Some(paints)
                && view.cpu_paint_pending() == Some(false)
            {
                if key.request.target.is_body(&document)
                    || view
                        .cell_history_facts()
                        .map_err(|cause| cause.to_string())?
                        .is_none()
                    || !same_parent()
                {
                    return Err("原单格完整帧没有当前真实 Cell 输入域".into());
                }
                return Ok(Some(identity));
            }
            Ok(None)
        })?; // SurfaceLease、文稿/Native 临时引用和格强引用均在本轮结束前归还。
        if let Some(ready) = ready {
            if started.elapsed() > Duration::from_secs(3) {
                return Err("原单格 CPU 帧超过三秒期限".into());
            }
            return Ok(ready);
        }
        let remaining = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .ok_or("原单格观察耗尽三秒期限")?;
        // SAFETY: 唯一原主线程 runloop 只消费本动作剩余期限；没有 timer、sleep、重播或新票。
        unsafe {
            objc2_core_foundation::CFRunLoop::run_in_mode(
                objc2_core_foundation::kCFRunLoopDefaultMode,
                remaining.as_secs_f64(),
                true,
            )
        };
    }
}

const CONFLICT_DRAFT: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 甲乙 |\n";
const CONFLICT_EXTERNAL: &str = "# 外部版本甲\n\n磁盘内容由独立写入者修改，原本地草稿必须保留。\n";
const CONFLICT_RELOADED: &str = "# 外部版本乙\n\n确认重载后应显示这一实际磁盘版本。\n";
const CONFLICT_COPY_TITLE: &str = "冲突草稿副本";
const CONFLICT_COPY_PATH: &str = "冲突草稿副本.md";
const BEFORE: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 原值 |\n";
const SELECTED_BEFORE: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 前中😀后 |\n";
const SELECTED_AFTER: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 前**中😀**后 |\n";
const UNCHANGED: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | **甲** |\n";
const APPENDED: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 末格追加验收 |\n| | |\n";
const FIND_REPLACEMENT_BEFORE: &str = BEFORE;
const FIND_REPLACEMENT_AFTER: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 原中文 |\n";

thread_local! {
    /// 保留原真实 modal 和安装端点的弱观察；普通产品未武装时不产生动作。
    static CONFLICT_OPERATION: RefCell<std::rc::Weak<ConflictProbeOperation>> =
        const { RefCell::new(std::rc::Weak::new()) };
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ConflictProbeAction {
    CancelReload,
    CopyDraft,
    ReturnOriginal,
    ConfirmReload,
}

/// 一次明确实际操作的收据；不保活controller、原Doc、NSAlert、按钮或字段。
struct ConflictProbeOperation {
    controller: Weak<NativeMarkdownController>,
    original: NativeMarkdownCurrentNoteIntent,
    input: NativeMarkdownPagedInputState,
    action: ConflictProbeAction,
    destination: String,
    expected_terminal: NativeMarkdownHostedTableStep,
    signal: RefCell<Option<Box<dyn FnOnce()>>>,
    active: Cell<bool>,
    modal_seen: Cell<bool>,
    delivered: Cell<bool>,
    modal_result: RefCell<Option<Result<(), String>>>,
}

impl ConflictProbeOperation {
    /// 实际按钮返回后主队列才交付取消；复制/重载必须另外等真实新Doc安装。
    fn deliver(&self) {
        if self.active.get() && !self.delivered.replace(true) {
            let signal = self.signal.borrow_mut().take();
            if let Some(signal) = signal {
                signal();
            } // 已发生的真实终点先退槽，回调可以正常进入宿主队列。
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConflictDocumentSnapshot {
    revision: omni_markdown_core::MarkdownDocumentRevision,
    generation: u64,
    selection: MarkdownTextSelection,
    dirty: bool,
    undo: bool,
    redo: bool,
}

impl ConflictDocumentSnapshot {
    /// 仅取原根和历史标量；不请求全文、布局或另一份正文。
    fn capture(document: &MarkdownDocumentBuffer) -> Result<Self, String> {
        Ok(Self {
            revision: document
                .current_revision()
                .map_err(|cause| cause.to_string())?,
            generation: document.generation(),
            selection: document.selection(),
            dirty: document.is_dirty(),
            undo: document.can_undo(),
            redo: document.can_redo(),
        })
    }
}

struct ConflictProbeFacts {
    original: NativeMarkdownCurrentNoteIntent,
    publication: u64,
    snapshot: ConflictDocumentSnapshot,
    current_document: WeakMarkdownDocument,
    current_path: String,
    current_text: &'static str,
    original_disk_text: &'static str,
    bar: Weak<NSBox>,
    completed: bool,
}

/// 真实按钮已发送、正式主队列尚可未执行时，只保留这一次排队的精确前后意图。
/// 前态Input在点击前固定；等待端不能从后来状态重采，也不能借前态通过完成认证。
struct FindReplacementQueuedButton {
    before_intent: u64,
    after_intent: u64,
    before_input: NativeMarkdownPagedInputState,
    paint: Rc<FindButtonPaint>,
}

/// 原实际构造事件的公开派发身份；AppKit可复制事件，allocation地址不是其交付合同。
/// eventNumber由鼠标入口单调分配，其余字段从构造后的原NSEvent读取，不能从后继状态补采。
#[derive(Clone, Copy)]
struct FindButtonDownIdentity {
    kind: objc2_app_kit::NSEventType,
    number: isize,
    // Timestamp differences are diagnostic only; dispatch authority uses the
    // actual event number, target window, position, type, and modifiers below.
    #[cfg(feature = "native-close-stage-diagnostic")]
    timestamp: f64,
    window: isize,
    location: NSPoint,
    modifiers: objc2_app_kit::NSEventModifierFlags,
}

impl FindButtonDownIdentity {
    /// 实测AppKit派发副本会改timestamp，公开合同也未承诺保留该浮点值。
    /// 时间只作诊断；原动作期限由started约束，派发严格按唯一编号和原窗口/位置/类型/修饰键关联。
    fn same_dispatch(self, other: Self) -> bool {
        self.kind == other.kind
            && self.number == other.number
            && self.window == other.window
            && self.location == other.location
            && self.modifiers == other.modifiers
    }

    /// 只读本次仍存活的不可变鼠标元数据，不取得Window、Context、Doc或Source对象。
    fn capture(event: &objc2_app_kit::NSEvent) -> Self {
        Self {
            kind: event.r#type(),
            number: event.eventNumber(),
            #[cfg(feature = "native-close-stage-diagnostic")]
            timestamp: event.timestamp(),
            window: event.windowNumber(),
            location: event.locationInWindow(),
            modifiers: event.modifierFlags(),
        }
    }
}

/// 四次具体 Find 鼠标动作的完成收据；kind 和点击前后唯一 intent 标识原动作，
/// 不冒充生产 FindRevealRequest。业务拥有者保持弱边，仅原有限Down活到真实派发返回。
struct FindButtonPaint {
    weak: std::rc::Weak<Self>,
    controller: Weak<NativeMarkdownController>,
    button: Weak<NSButton>,
    view: Weak<NativeMarkdownPagedTextView>,
    facts: AppendFacts,
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    route: Option<(usize, u64)>,
    action: MarkdownFindAction,
    terminal_step: NativeMarkdownHostedTableStep,
    before_intent: u64,
    after_intent: u64,
    before_input: NativeMarkdownPagedInputState,
    generation: u64,
    selection: MarkdownTextSelection,
    utf16_len: usize,
    started: Instant,
    down: RefCell<Option<Retained<objc2_app_kit::NSEvent>>>,
    down_identity: FindButtonDownIdentity,
    dispatched: Cell<bool>,
    selector_returned: Cell<bool>,
    after_queued: Cell<bool>,
    action_returned: Cell<bool>,
    // 临时派发/帧诊断只限首个 Next；action 返回各记原四次动作的一条，均不重复刷早帧。
    #[cfg(feature = "native-close-stage-diagnostic")]
    dispatch_diagnosed: Cell<bool>,
    #[cfg(feature = "native-close-stage-diagnostic")]
    frame_diagnosed: Cell<bool>,
    terminal: RefCell<Option<Result<(NativeMarkdownPagedSceneKey, usize), String>>>,
    observer: terminal::ProbeTerminalObserver,
}

impl FindButtonPaint {
    /// 原 controller/bar/客户路由与原 Body/Doc/View 缺一即撤权；不从当前窗口重采新拥有者。
    fn current(&self, surface: &NativeMarkdownWindowSurface) -> bool {
        let Some(controller) = self.controller.load() else {
            return false;
        };
        std::ptr::eq(&*controller, &*surface.controller)
            && std::ptr::from_ref(&*controller).addr() == self.facts.owner
            && native_markdown_controller_has_foreground_authority(&controller)
            && controller
                .ivars()
                .field_editor_provider
                .current_input_client_identity()
                == self.route
            && surface.editor.as_ref().is_some_and(|editor| {
                std::rc::Weak::ptr_eq(&self.facts.body, &Rc::downgrade(&editor.body))
                    && WeakMarkdownDocument::ptr_eq(
                        &self.facts.document,
                        &Rc::downgrade(editor.document()),
                    )
                    && editor.presentation_mode() == MarkdownPresentationMode::InlinePreview
                    && self
                        .view
                        .load()
                        .is_some_and(|view| std::ptr::eq(&*view, &**editor.text_view))
                    && editor
                        .find_bar
                        .borrow()
                        .as_ref()
                        .is_some_and(|bar| std::rc::Weak::ptr_eq(&self.bar, &Rc::downgrade(bar)))
            })
    }

    /// Down/tracking 返回与原 selector 返回可任意先后；两张真实收据齐备才排一次观察。
    /// selector 返回证明其 enqueue_owned 已入同一主队列，不能再用 Down 返回猜这个先序。
    fn queue_after_action_if_ready(&self, mtm: MainThreadMarker) {
        if !self.dispatched.get()
            || !self.selector_returned.get()
            || self.after_queued.replace(true)
        {
            return;
        }
        let weak = dispatch2::MainThreadBound::new(self.weak.clone(), mtm);
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("Find action 终点只在原主队列观察");
            if let Some(paint) = weak.get(mtm).upgrade() {
                paint.after_action();
            }
        });
    }

    /// 早于原动作的真实帧只能等下一次真实发布；这里不交付旧帧、不触发绘制，也不重发输入。
    fn rearm(&self) -> Result<(), String> {
        let body = self.facts.body.upgrade().ok_or("Find 原 Body 已释放")?;
        let observer: Rc<dyn NativeMarkdownPagedBodyPaintObserver> =
            self.weak.upgrade().ok_or("Find 原帧观察已释放")?;
        body.observe_next_cpu_frame(Rc::downgrade(&observer))
            .map_err(|cause| cause.to_string())
    }

    /// 通知只授予下一验证事件排队权；该事件仍须消费当时保存的同一完整 CPU 帧。
    fn completed(&self) -> Result<(), String> {
        if !self.dispatched.get() || !self.selector_returned.get() || !self.action_returned.get() {
            return Err("原 Find 鼠标派发或正式 action 尚未返回".into());
        }
        let (key, paints) = self
            .terminal
            .borrow()
            .clone()
            .ok_or("原 Find 按钮尚未到达真实 CPU 终点")??;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface) || surface.controller.find_input_intent() != self.after_intent
            {
                return Err("原 Find 完成通知后的客户或意图已撤销".into());
            }
            let view = &surface
                .editor
                .as_ref()
                .ok_or("原 Find 正文已释放")?
                .text_view;
            if view.input_state().map_err(|cause| cause.to_string())? != key.request.input
                || !view
                    .capture_ready_scene_key()
                    .map_err(|cause| cause.to_string())?
                    .same(&key)
                || view
                    .probe_cpu_published_frame()
                    .is_none_or(|(actual, count)| !actual.same(&key) || count != paints)
                || view.cpu_paints() != Some(paints)
                || view.cpu_paint_pending() != Some(false)
                || !self.current(surface)
            {
                return Err("原 Find CPU 终点已被后继发布替换".into());
            }
            Ok(())
        })
    }

    /// 新帧和只读拒绝验证共用原后态契约；调用方决定是否必须有新的 CPU 发布。
    fn check_frame(
        &self,
        key: &NativeMarkdownPagedSceneKey,
        paints: usize,
    ) -> Result<bool, String> {
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface) {
                return Err("Find 帧发布时原客户、栏或文稿已撤销".into());
            }
            let editor = surface.editor.as_ref().ok_or("Find 原正文已释放")?;
            if let Some(cause) = editor.body.take_failure() {
                return Err(format!("Find 原正文失败：{cause}"));
            }
            if let Some(cause) = editor.text_view.take_failure()
                && !matches!(cause, crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending)
            {
                return Err(format!("Find 原输入失败：{cause}"));
            }
            // Source 健康错误先于等待分类；端点零只验证原存储，不读取正文或新建 Source。
            editor
                .document()
                .borrow()
                .byte_offset_for_utf16(0)
                .map_err(|cause| cause.to_string())?;
            let input = editor
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            let intent = surface.controller.find_input_intent();
            if !WeakMarkdownDocument::ptr_eq(&key.request.document, &self.facts.document)
                || !key.request.target.is_body(&self.facts.document)
                || key.presentation_mode != MarkdownPresentationMode::InlinePreview
                || key.request.input != input
            {
                return Err("Find 回调不是原 Doc 的当前完整 Input 帧".into());
            }
            if self.started.elapsed() > Duration::from_secs(3) {
                return Err("原 Find 按钮没有在原三秒期限内发布目标帧".into());
            }
            if (intent == self.before_intent || intent == self.after_intent)
                && input == self.before_input
                && self.terminal_step
                    != NativeMarkdownHostedTableStep::VerifyReplaceRejectedAndCommit
            {
                return Ok(false); // 本次动作尚未发布选择/编辑；只订阅下一真实帧，不把前态当完成。
            }
            if intent != self.after_intent {
                return Err("Find 帧不属于原按钮唯一后继意图".into());
            }
            let range = NSRange::new(
                self.selection.anchor_utf16,
                self.selection.head_utf16 - self.selection.anchor_utf16,
            );
            let replacing =
                self.terminal_step == NativeMarkdownHostedTableStep::VerifyReplacedAndUndo;
            let rejected =
                self.terminal_step == NativeMarkdownHostedTableStep::VerifyReplaceRejectedAndCommit;
            let fold_revision = self
                .before_input
                .fold_revision
                .checked_add(u64::from(replacing))
                .ok_or("Find 原 fold 代次溢出")?;
            if input.generation != self.generation
                || input.selection != self.selection
                || !input.selection_is_directed
                || input.selection_range_count != 1
                || input.primary_range != range
                || input.marked != self.before_input.marked
                || input.utf16_len != self.utf16_len
                || input.fold_revision != fold_revision
                || (rejected && input != self.before_input)
                || input.selection_serial
                    != self
                        .before_input
                        .selection_serial
                        .checked_add(u64::from(!rejected))
                        .ok_or("Find 原选择代次溢出")?
            {
                return Err("Find 完整 Input 不符合原按钮预声明的选择、编辑或拒绝后态".into());
            }
            {
                let document = editor.document().borrow();
                if document.generation() != self.generation
                    || document.selection() != self.selection
                    || document.utf16_len() != self.utf16_len
                {
                    return Err("Find CPU 输入与原 canonical 后态不一致".into());
                }
            }
            if rejected {
                if self.action != MarkdownFindAction::ReplaceNext {
                    return Err("组合拒绝不属于原 ReplaceNext 按钮".into());
                }
                let current = || {
                    self.current(surface)
                        && surface.controller.find_input_intent() == self.after_intent
                        && editor
                            .text_view
                            .input_state()
                            .is_ok_and(|actual| actual == input)
                };
                let status = surface
                    .controller
                    .ivars()
                    .status_label
                    .get()
                    .ok_or("原替换拒绝缺少状态控件")?;
                if !step(&current, || {
                    status
                        .stringValue()
                        .isEqualToString(ns_string!("请先完成当前输入法组合，再执行查找或替换"))
                })? {
                    return Ok(false); // 同一输入可有较早发布；仍须等真实拒绝反馈后的完整帧。
                }
            }
            let view = &editor.text_view;
            if editor.body.is_preparing()
                || !view
                    .capture_ready_scene_key()
                    .map_err(|cause| cause.to_string())?
                    .same(key)
                || paints == 0
                || view.cpu_paints() != Some(paints)
                || view.cpu_paint_pending() != Some(false)
                || view
                    .probe_cpu_published_frame()
                    .is_none_or(|(actual, count)| !actual.same(key) || count != paints)
            {
                return Err("Find 通知没有同一 Ready 和完整 CPU 发布终点".into());
            }
            view.verify_find_target_visible(key, range)
                .map_err(|cause| cause.to_string())?;
            if !self.current(surface)
                || surface.controller.find_input_intent() != self.after_intent
                || view.input_state().map_err(|cause| cause.to_string())? != input
            {
                return Err("Find 可见性 Native 查询后原完整输入已撤销".into());
            }
            Ok(true)
        })
    }

    /// 原 Down 与具体 selector 均已返回，才沿同主队列 FIFO 排在正式 action 之后执行一次。
    /// 组合拒绝不改正文：原完整帧仍健康时直接验证；反馈使 Scene/CPU 真 Pending
    /// 时才登记一次真实发布。拒绝没有新帧义务，不能预订阅后遗留无人消费的弱槽。
    fn after_action(&self) {
        #[cfg(feature = "native-close-stage-diagnostic")]
        let mut authority = None;
        #[cfg(feature = "native-close-stage-diagnostic")]
        let (
            mut route_same,
            mut controller_same,
            mut foreground_current,
            mut body_same,
            mut doc_same,
            mut view_same,
            mut bar_same,
            mut mode_same,
        ) = (false, false, false, false, false, false, false, false);
        let checked = with_active_native_markdown_surface(|surface| {
            let current = self.current(surface);
            let intent = surface.controller.find_input_intent();
            #[cfg(feature = "native-close-stage-diagnostic")]
            {
                authority = Some((current, intent));
                // 仅分解原 current 的身份事实，不读取字段正文、Native 焦点或重新取得业务票。
                // Surface 未取得时保持 false，配合 entered_surface 区分 Busy 与身份变化。
                let controller = self.controller.load();
                controller_same = controller.as_ref().is_some_and(|controller| {
                    std::ptr::eq(&**controller, &*surface.controller)
                        && std::ptr::from_ref(&**controller).addr() == self.facts.owner
                });
                foreground_current = controller.as_ref().is_some_and(|controller| {
                    native_markdown_controller_has_foreground_authority(controller)
                });
                route_same = controller.as_ref().is_some_and(|controller| {
                    controller
                        .ivars()
                        .field_editor_provider
                        .current_input_client_identity()
                        == self.route
                });
                if let Some(editor) = &surface.editor {
                    body_same =
                        std::rc::Weak::ptr_eq(&self.facts.body, &Rc::downgrade(&editor.body));
                    doc_same = WeakMarkdownDocument::ptr_eq(
                        &self.facts.document,
                        &Rc::downgrade(editor.document()),
                    );
                    view_same = self
                        .view
                        .load()
                        .is_some_and(|view| std::ptr::eq(&*view, &**editor.text_view));
                    bar_same =
                        editor.find_bar.borrow().as_ref().is_some_and(|bar| {
                            std::rc::Weak::ptr_eq(&self.bar, &Rc::downgrade(bar))
                        });
                    mode_same =
                        editor.presentation_mode() == MarkdownPresentationMode::InlinePreview;
                }
            }
            if !current
                || intent != self.after_intent
                || self.started.elapsed() > Duration::from_secs(3)
            {
                return Err("原 Find action 返回后客户、唯一意图或三秒期限不符".into());
            }
            if self.terminal_step != NativeMarkdownHostedTableStep::VerifyReplaceRejectedAndCommit {
                return Ok(None); // action 返回不等于像素完成，绝不能从当前旧图补一份新帧收据。
            }
            let editor = surface.editor.as_ref().ok_or("原 Find 拒绝正文已释放")?;
            // 等待只适用于真实 Pending；正文、输入及原文件的失败必须先沿本次原动作返回，
            // 不能把已失败的生产者留作“稍后有帧”或从后继 Source 取得替代权限。
            if let Some(cause) = editor.body.take_failure() {
                return Err(format!("Find 原正文失败：{cause}"));
            }
            if let Some(cause) = editor.text_view.take_failure()
                && !matches!(cause, crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending)
            {
                return Err(format!("Find 原输入失败：{cause}"));
            }
            editor
                .document()
                .borrow()
                .byte_offset_for_utf16(0)
                .map_err(|cause| cause.to_string())?;
            if editor
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?
                != self.before_input
            {
                return Err("原组合拒绝改变了完整正文 Input".into());
            }
            let key = match editor.text_view.capture_ready_scene_key() {
                Ok(key) => Some(key),
                Err(crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending) => None,
                Err(cause) => return Err(cause.to_string()),
            };
            if key.is_none() || editor.text_view.cpu_paint_pending() == Some(true) {
                // 拒绝反馈可以改变原窗口布局而保持完整 Input；只在这一确切未完成状态
                // 登记。登记只读原 Rust/Weak/CPU 计数，没有发布交付点或 Native 布局调用。
                let current = || {
                    self.current(surface)
                        && surface.controller.find_input_intent() == self.after_intent
                        && editor
                            .text_view
                            .input_state()
                            .is_ok_and(|actual| actual == self.before_input)
                        && self.started.elapsed() <= Duration::from_secs(3)
                };
                if !current() {
                    return Err("原组合拒绝等待前完整客户、输入或期限已撤销".into());
                }
                self.rearm()?;
                editor
                    .document()
                    .borrow()
                    .byte_offset_for_utf16(0)
                    .map_err(|cause| cause.to_string())?;
                if !current() {
                    return Err("原组合拒绝登记后完整客户、输入或期限已撤销".into());
                }
                return Ok(None); // 保原三秒与同一 Paint；只有后继真实发布才能交付准确拒绝终态。
            }
            let key = key.expect("非 Pending 拒绝保有原 Ready key");
            let (actual, paints) = editor
                .text_view
                .probe_cpu_published_frame()
                .ok_or("原组合拒绝缺少实际完整 CPU 帧")?;
            if !actual.same(&key) {
                return Err("原组合拒绝的 Ready 与实际 CPU 不同".into());
            }
            Ok(Some((key, paints)))
        });
        #[cfg(feature = "native-close-stage-diagnostic")]
        {
            eprintln!(
                "native_find_button_terminal phase=after_action kind={} entered_surface={} current={:?} actual_intent={:?} checked_ok={} before_intent={} after_intent={} dispatched={} frame_terminal={} deadline_current={} route_same={route_same} controller_same={controller_same} foreground_current={foreground_current} body_same={body_same} doc_same={doc_same} view_same={view_same} bar_same={bar_same} mode_same={mode_same}",
                self.action as isize,
                authority.is_some(),
                authority.map(|(current, _)| current),
                authority.map(|(_, intent)| intent),
                checked.is_ok(),
                self.before_intent,
                self.after_intent,
                self.dispatched.get(),
                self.terminal.borrow().is_some(),
                self.started.elapsed() <= Duration::from_secs(3),
            ); // 原租约即使 Busy 也记录到达；None明确没有取得Surface，不额外调用Native或重借。
        }
        // Active 租约结束后才复用真实帧验证，避免在一个 SurfaceLease 内递归借活窗口。
        let checked = match checked {
            Ok(Some((key, paints))) => match self.check_frame(&key, paints) {
                Ok(true) => Some(Ok((key, paints))),
                Ok(false) => Some(Err("原拒绝 action 返回后没有准确拒绝终态".into())),
                Err(cause) => Some(Err(cause)),
            },
            Ok(None) => None,
            Err(cause) => Some(Err(cause)),
        };
        if let Some(checked) = checked {
            let mut terminal = self.terminal.borrow_mut();
            // 原 CPU 首错优先保留；action 身份失败也不能被较早成功帧掩盖。
            if terminal.as_ref().is_none_or(Result::is_ok) {
                *terminal = Some(checked);
            }
        }
        self.action_returned.set(true); // 最后才打开通知门，所有 Native 查询与 Active 租约已经结束。
        self.notify_if_complete();
    }

    fn notify_if_complete(&self) {
        let completed = self.dispatched.get()
            && self.selector_returned.get()
            && self.action_returned.get()
            && self.terminal.borrow().is_some();
        if completed {
            self.observer.notify();
        } // 不在 NSButton tracking 里发 Host 事件；两个真实边界都结束后才交付一次。
    }
}

impl NativeMarkdownPagedBodyPaintObserver for FindButtonPaint {
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        if self.terminal.borrow().is_some() {
            return; // 已写入的成功或失败都只交付一次，后继帧不能改写原动作结论。
        }
        let checked = self.check_frame(key, paints);
        #[cfg(feature = "native-close-stage-diagnostic")]
        if self.action == MarkdownFindAction::Next && !self.frame_diagnosed.replace(true) {
            let outcome = match &checked {
                Ok(true) => "ready",
                Ok(false) => "earlier_frame",
                Err(_) => "failed",
            };
            eprintln!(
                "native_find_button_terminal phase=frame kind={} outcome={outcome} before_intent={} after_intent={} dispatched={} action_returned={} generation={} selection_serial={} fold_revision={} paints={paints}",
                self.action as isize,
                self.before_intent,
                self.after_intent,
                self.dispatched.get(),
                self.action_returned.get(),
                key.request.input.generation,
                key.request.input.selection_serial,
                key.request.input.fold_revision,
            ); // 首张实际交付帧只记 key 标量及原检查结果，不打印错误正文或源内容。
        }
        let checked = match checked {
            Ok(false) => match self.rearm() {
                Ok(()) => return,
                Err(cause) => Err(cause),
            },
            Ok(true) => Ok((key.clone(), paints)),
            Err(cause) => Err(cause),
        };
        *self.terminal.borrow_mut() = Some(checked);
        // 先归还 Active、Native claim 和全部 RefCell，再通知 Host；失败同样唤醒严格验证，
        // 不吞首错或丢弃原 Host 信号，也不提前解除尚无终点的原三秒 deadline。
        self.notify_if_complete();
    }
}

/// 查找组合回归只跨事件保弱身份和预声明坐标；不保活栏、字段或共享 editor。
struct FindReplacementProbeFacts {
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    field_editor: Weak<NSTextView>,
    route: Option<(usize, u64)>,
    intent: u64,
    queued_button: Option<FindReplacementQueuedButton>,
    first: MarkdownTextSelection,
    last: MarkdownTextSelection,
    /// 每个真实按钮或历史动作先固定起点；后继观察只能消费该次三秒期限。
    action_started: Instant,
}

/// 每个入口只验证一条已有用户操作，不读取测试参数改变产品能力或预算。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NativeMarkdownHostedTableCase {
    #[default]
    Positive,
    Cancel,
    FormatUnchanged,
    CommitResigned,
    FormatSelection,
    ToolbarFormatSelection,
    ToolbarFieldHistory,
    FindReplacementComposition,
}

/// 表格验收的真实业务阶段；宿主只按对应 case 的 slice 调用，不接受任意数字命令。
/// 四条冲突完成边界订阅已有 modal/安装通知，其余动作在原事件和三秒帧合同内结束。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownHostedTableStep {
    VerifyOpen,
    ActivateCell,
    SubmitLastCellAndTab,
    VerifyAppendAndUndo,
    VerifyUndoAndRedo,
    VerifyRedoAndUndo,
    VerifyRestored,
    VerifyCancelled,
    FormatActiveCell,
    BeginLocalHistory,
    VerifyFirstLocalAndTypeSecond,
    UndoSecondLocal,
    VerifyLocalUndoAndRedo,
    VerifyLocalRedoAndUndo,
    VerifyLocalUndoAndRedoAgain,
    CommitLocalDraft,
    VerifyToolbarCommitAndUndo,
    VerifyToolbarUndoAndRedo,
    VerifyToolbarRedoAndSave,
    BeginConflictAndCancel,
    VerifyConflictCancelAndCopy,
    VerifyConflictCopyAndReturn,
    VerifyConflictOriginalAndReload,
    VerifyConflictReload,
    BeginFindReplacement,
    VerifyFindNextAndPrevious,
    VerifyFindPreviousAndRejectReplace,
    VerifyReplaceRejectedAndCommit,
    VerifyReplacedAndUndo,
    VerifyReplacementUndoAndRedo,
    VerifyReplacementRedoAndSave,
    VerifyReplacementSaved,
    VerifyInformationClosedAndReopen,
    RefreshInformationCard,
}

impl NativeMarkdownHostedTableCase {
    /// 原短样本只有这一份权威字面值；Host流式生成自己的全新文件，不能镜像一份协议。
    #[must_use]
    pub fn fixture_source(self) -> &'static str {
        match self {
            Self::FormatUnchanged => UNCHANGED,
            Self::FormatSelection | Self::ToolbarFormatSelection | Self::ToolbarFieldHistory => {
                SELECTED_BEFORE
            }
            _ => BEFORE,
        }
    }

    /// 保留8条原业务流程；实际 Host 每次先 fresh activate，再逐真实事件消费整个 slice。
    #[must_use]
    pub fn steps(self) -> &'static [NativeMarkdownHostedTableStep] {
        use NativeMarkdownHostedTableStep::*;
        match self {
            Self::Positive | Self::CommitResigned => &[
                VerifyOpen,
                ActivateCell,
                SubmitLastCellAndTab,
                VerifyAppendAndUndo,
                VerifyUndoAndRedo,
                VerifyRedoAndUndo,
                VerifyRestored,
            ],
            Self::Cancel => &[
                VerifyOpen,
                ActivateCell,
                SubmitLastCellAndTab,
                VerifyCancelled,
            ],
            Self::FormatUnchanged | Self::FormatSelection => {
                &[VerifyOpen, ActivateCell, FormatActiveCell]
            }
            Self::ToolbarFormatSelection => &[
                VerifyOpen,
                ActivateCell,
                FormatActiveCell,
                VerifyToolbarCommitAndUndo,
                VerifyToolbarUndoAndRedo,
                VerifyToolbarRedoAndSave,
            ],
            Self::ToolbarFieldHistory => &[
                VerifyOpen,
                ActivateCell,
                BeginLocalHistory,
                VerifyFirstLocalAndTypeSecond,
                VerifyInformationClosedAndReopen,
                RefreshInformationCard,
                UndoSecondLocal,
                VerifyLocalUndoAndRedo,
                VerifyLocalRedoAndUndo,
                VerifyLocalUndoAndRedoAgain,
                CommitLocalDraft,
                VerifyToolbarCommitAndUndo,
                VerifyToolbarUndoAndRedo,
                VerifyToolbarRedoAndSave,
                BeginConflictAndCancel,
                VerifyConflictCancelAndCopy,
                VerifyConflictCopyAndReturn,
                VerifyConflictOriginalAndReload,
                VerifyConflictReload,
            ],
            Self::FindReplacementComposition => &[
                VerifyOpen,
                BeginFindReplacement,
                VerifyFindNextAndPrevious,
                VerifyFindPreviousAndRejectReplace,
                VerifyReplaceRejectedAndCommit,
                VerifyReplacedAndUndo,
                VerifyReplacementUndoAndRedo,
                VerifyReplacementRedoAndSave,
                VerifyReplacementSaved,
            ],
        }
    }
}

#[derive(Clone)]
struct AppendFacts {
    owner: usize,
    document: WeakMarkdownDocument,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    generation: u64,
    baseline: omni_markdown_core::MarkdownDocumentRevision,
    selection: MarkdownTextSelection,
    after: Option<omni_markdown_core::MarkdownDocumentRevision>,
    after_selection: Option<MarkdownTextSelection>,
}

/// 格客户只保 Weak；正常提交和关闭必须让这些实际子正文释放。
pub struct NativeMarkdownHostedTableProbe {
    case: NativeMarkdownHostedTableCase,
    path: std::path::PathBuf,
    relative_path: String,
    facts: RefCell<Option<AppendFacts>>,
    cell: RefCell<Option<std::rc::Weak<NativeMarkdownPagedBody>>>,
    previous_cells: RefCell<Vec<std::rc::Weak<NativeMarkdownPagedBody>>>,
    information_root: RefCell<Option<Weak<NSBox>>>,
    conflict: RefCell<Option<ConflictProbeFacts>>,
    conflict_operation: RefCell<Option<Rc<ConflictProbeOperation>>>,
    find_replacement: RefCell<Option<FindReplacementProbeFacts>>,
}

impl NativeMarkdownHostedTableProbe {
    /// 只接真实 Host 已激活的根目录合成笔记；不创建会话、Native 图或替代宿主端口。
    /// 调用者拥有这张隔离样本及其目录，8 个场景才有权保存或创建固定冲突副本。
    pub fn observe_active_document(
        path: &Path,
        case: NativeMarkdownHostedTableCase,
    ) -> Result<Self, String> {
        MainThreadMarker::new().ok_or("Hosted 表格观察必须在 AppKit 主线程")?;
        let path = path.canonicalize().map_err(|cause| cause.to_string())?;
        let (relative_path, facts) = with_active_native_markdown_surface(|surface| {
            let editor = surface
                .editor
                .as_ref()
                .ok_or("真实 Host 尚未安装原表格正文")?;
            if !native_markdown_controller_has_foreground_authority(&surface.controller)
                || editor
                    .document()
                    .borrow()
                    .path()
                    .canonicalize()
                    .ok()
                    .as_ref()
                    != Some(&path)
            {
                return Err("Hosted 表格路径或原前台身份不匹配".into());
            }
            let relative = surface
                .current_note_relative_path
                .clone()
                .ok_or("Hosted 表格没有原相对路径")?;
            if Path::new(&relative).components().count() != 1 {
                return Err("Hosted 表格夹具必须是本隔离库根目录笔记".into());
            }
            let document = editor.document().borrow();
            Self::verify_conflict_text(&document, case.fixture_source())?;
            if document.is_dirty() || document.can_undo() || document.can_redo() {
                return Err("原表格样本不是干净空历史".into());
            }
            let facts = AppendFacts {
                owner: std::ptr::from_ref(&*surface.controller).addr(),
                document: Rc::downgrade(editor.document()),
                body: Rc::downgrade(&editor.body),
                generation: document.generation(),
                baseline: document.current_revision().map_err(|e| e.to_string())?,
                selection: document.selection(),
                after: None,
                after_selection: None,
            };
            Ok((relative, facts))
        })?;
        let probe = Self {
            case,
            path,
            relative_path,
            facts: RefCell::new(None),
            cell: RefCell::new(None),
            previous_cells: RefCell::new(Vec::new()),
            information_root: RefCell::new(None),
            conflict: RefCell::new(None),
            conflict_operation: RefCell::new(None),
            find_replacement: RefCell::new(None),
        };
        *probe.facts.borrow_mut() = Some(facts);
        Ok(probe)
    }

    /// 原宿主事件只驱动该 case 的明确业务步骤；没有测试版 Host 或第二正文输入实现。
    pub fn step(&self, action: NativeMarkdownHostedTableStep) -> Result<(), String> {
        MainThreadMarker::new().ok_or("Hosted 表格步骤必须在 AppKit 主线程")?;
        if !self.case.steps().contains(&action) {
            return Err("该表格场景不拥有此业务步骤".into());
        }
        use NativeMarkdownHostedTableStep::*;
        match action {
            VerifyOpen => self.verify_open(),
            ActivateCell => self.begin_append(),
            SubmitLastCellAndTab => self.finish_cell_activation_and_begin_append(),
            VerifyAppendAndUndo => self.verify_append_and_undo(),
            VerifyUndoAndRedo => self.verify_history(true, false),
            VerifyRedoAndUndo => self.verify_history(false, false),
            VerifyRestored => self.verify_history(false, true),
            VerifyCancelled => self.verify_negative(),
            FormatActiveCell => self.verify_format(),
            BeginLocalHistory => self.advance_toolbar_field_history(0),
            VerifyFirstLocalAndTypeSecond => self.advance_toolbar_field_history(1),
            VerifyInformationClosedAndReopen => self.reopen_information_card(),
            RefreshInformationCard => self.refresh_information_card(),
            UndoSecondLocal => self.advance_toolbar_field_history(2),
            VerifyLocalUndoAndRedo => self.advance_toolbar_field_history(3),
            VerifyLocalRedoAndUndo => self.advance_toolbar_field_history(4),
            VerifyLocalUndoAndRedoAgain => self.advance_toolbar_field_history(5),
            CommitLocalDraft => self.advance_toolbar_field_history(6),
            VerifyToolbarCommitAndUndo => self.verify_toolbar_format_history(0),
            VerifyToolbarUndoAndRedo => self.verify_toolbar_format_history(1),
            VerifyToolbarRedoAndSave => self.verify_toolbar_format_history(2),
            BeginConflictAndCancel => self.begin_conflict_bar_case(),
            VerifyConflictCancelAndCopy => self.verify_conflict_cancel_and_copy(),
            VerifyConflictCopyAndReturn => self.verify_conflict_copy_and_return(),
            VerifyConflictOriginalAndReload => self.verify_conflict_aba_and_reload(),
            VerifyConflictReload => self.verify_conflict_reload(),
            BeginFindReplacement => self.begin_find_replacement_composition(),
            VerifyFindNextAndPrevious => self.advance_find_replacement_composition(1),
            VerifyFindPreviousAndRejectReplace => self.advance_find_replacement_composition(2),
            VerifyReplaceRejectedAndCommit => self.advance_find_replacement_composition(3),
            VerifyReplacedAndUndo => self.advance_find_replacement_composition(4),
            VerifyReplacementUndoAndRedo => self.advance_find_replacement_composition(5),
            VerifyReplacementRedoAndSave => self.advance_find_replacement_composition(6),
            VerifyReplacementSaved => self.advance_find_replacement_composition(7),
        }
    }

    /// 只订阅上一原动作的真实终点；查找按钮同时等待原 Down/action 返回和精确 CPU 后态。
    /// 已结束的成功或失败均可交回排队权，后继验证必须消费该结论，不能重新执行按钮。
    pub fn observe_step_terminal(
        &self,
        action: NativeMarkdownHostedTableStep,
        observer: impl FnOnce() + 'static,
    ) -> Result<bool, String> {
        MainThreadMarker::new().ok_or("Hosted 表格终点必须在 AppKit 主线程")?;
        if !self.case.steps().contains(&action) {
            return Err("该表格场景不拥有此终点".into());
        }
        if matches!(
            action,
            NativeMarkdownHostedTableStep::VerifyFindNextAndPrevious
                | NativeMarkdownHostedTableStep::VerifyFindPreviousAndRejectReplace
                | NativeMarkdownHostedTableStep::VerifyReplaceRejectedAndCommit
                | NativeMarkdownHostedTableStep::VerifyReplacedAndUndo
        ) {
            let paint = self
                .find_replacement
                .borrow()
                .as_ref()
                .and_then(|probe| probe.queued_button.as_ref())
                .map(|button| Rc::clone(&button.paint))
                .ok_or("Find 终点没有点击前固定的原动作")?;
            if paint.terminal_step != action {
                return Err("Find 终点不属于原按钮 kind 和唯一意图".into());
            }
            let completed =
                (paint.action_returned.get() && paint.terminal.borrow().is_some()).then_some(true);
            return paint.observer.observe(completed, observer);
        }
        if !matches!(
            action,
            NativeMarkdownHostedTableStep::VerifyConflictCancelAndCopy
                | NativeMarkdownHostedTableStep::VerifyConflictCopyAndReturn
                | NativeMarkdownHostedTableStep::VerifyConflictOriginalAndReload
                | NativeMarkdownHostedTableStep::VerifyConflictReload
        ) {
            drop(observer);
            return Ok(true);
        }
        let operation = self
            .conflict_operation
            .borrow()
            .clone()
            .ok_or("冲突终点没有原真实动作")?;
        if !operation.active.get() || operation.expected_terminal != action {
            return Err("冲突观察不属于原动作终点".into());
        }
        if operation.delivered.get() {
            drop(observer);
            return Ok(true);
        }
        if operation.signal.borrow().is_some() {
            return Err("冲突原终点已被订阅".into());
        }
        *operation.signal.borrow_mut() = Some(Box::new(observer));
        Ok(false)
    }

    /// 只由验收 NSApplication 的 super.sendEvent 原 Down 返回边界调用。
    /// 这只证明原鼠标 tracking 已返回，不推断 AppKit 已交付 selector；另一个真实边界
    /// 证明原 selector 返回，两者齐备才被动观察。不重发鼠标、执行查找或制造绘图。
    pub fn did_dispatch_hosted_event(&self, event: &objc2_app_kit::NSEvent) -> Result<(), String> {
        let mtm = MainThreadMarker::new().ok_or("Find 鼠标终点必须在 AppKit 主线程")?;
        if event.r#type() != objc2_app_kit::NSEventType::LeftMouseDown {
            return Ok(());
        }
        let paint = self
            .find_replacement
            .borrow()
            .as_ref()
            .and_then(|probe| probe.queued_button.as_ref())
            .map(|button| Rc::clone(&button.paint));
        let Some(paint) = paint else {
            return Ok(()); // 其它表格/工具栏事件没有这个具体 Find 动作的后继权限。
        };
        let identity = FindButtonDownIdentity::capture(event);
        #[cfg(feature = "native-close-stage-diagnostic")]
        if paint.action == MarkdownFindAction::Next && !paint.dispatch_diagnosed.replace(true) {
            let down = paint.down.borrow();
            eprintln!(
                "native_find_button_terminal phase=dispatch kind={} event_lease={} ptr_same={} value_same={} number_same={} timestamp_same={} window_same={} location_same={} modifiers_same={} before_intent={} after_intent={} dispatched={}",
                paint.action as isize,
                down.is_some(),
                down.as_ref()
                    .is_some_and(|down| std::ptr::eq(&**down, event)),
                identity.same_dispatch(paint.down_identity),
                identity.number == paint.down_identity.number,
                identity.timestamp == paint.down_identity.timestamp,
                identity.window == paint.down_identity.window,
                identity.location == paint.down_identity.location,
                identity.modifiers == paint.down_identity.modifiers,
                paint.before_intent,
                paint.after_intent,
                paint.dispatched.get(),
            ); // allocation和timestamp仅作诊断，接纳仍核其余精确派发值；没有裸地址或正文。
        }
        if !identity.same_dispatch(paint.down_identity) {
            return Ok(());
        }
        if paint.dispatched.replace(true) {
            return Err("原 Find Down 不能重复交付".into());
        }
        let down = paint
            .down
            .borrow_mut()
            .take()
            .ok_or("原 Find Down 的有限事件租约已丢失")?;
        drop(down); // 原派发已返回；先退槽再释放唯一事件租约，不跨FIFO观察保留Native事件。
        paint.queue_after_action_if_ready(mtm);
        Ok(())
    }

    /// 仅验收 App 的公开 sendAction:to:from: 完整返回后调用；参数就是原系统派发值。
    /// 只认事前原按钮、controller 与具体 selector，不读取 tag、字段或后继文稿来重签权限。
    pub fn did_dispatch_hosted_action(
        &self,
        action: objc2::runtime::Sel,
        target: Option<&AnyObject>,
        sender: Option<&AnyObject>,
        sent: bool,
    ) -> Result<(), String> {
        let mtm = MainThreadMarker::new().ok_or("Find selector 终点必须在 AppKit 主线程")?;
        if action != sel!(performMarkdownFindAction:) {
            return Ok(());
        }
        let paint = self
            .find_replacement
            .borrow()
            .as_ref()
            .and_then(|probe| probe.queued_button.as_ref())
            .map(|button| Rc::clone(&button.paint));
        let Some(paint) = paint else {
            return Ok(()); // 非这次原排队按钮的 action 没有具体终点订阅。
        };
        let Some((target, sender)) = target.zip(sender) else {
            return Ok(()); // 本按钮已显式绑定 controller，不从 responder 链猜目标。
        };
        let same_controller = paint.controller.load().is_some_and(|controller| {
            std::ptr::eq(AsRef::<AnyObject>::as_ref(&*controller), target)
        });
        let same_button = paint
            .button
            .load()
            .is_some_and(|button| std::ptr::eq(AsRef::<AnyObject>::as_ref(&*button), sender));
        if !same_controller || !same_button {
            return Ok(()); // kind 由事前那个具体按钮固定，外来 sender 不能借相同 selector 接续。
        }
        if !sent {
            return Err("原 Find 按钮的实际 selector 未成功交付".into());
        }
        if paint.selector_returned.replace(true) {
            return Err("原 Find 按钮的实际 selector 不能重复交付".into());
        }
        // 只保存系统派发已返回的标量；临时 Native weak claim 已结束，不保 sender/target。
        paint.queue_after_action_if_ready(mtm);
        Ok(())
    }

    fn facts(&self) -> Result<AppendFacts, String> {
        self.facts
            .borrow()
            .clone()
            .ok_or_else(|| "表格验收缺少原文稿基线".into())
    }

    /// 原 Body allocation、Doc 和字符代次共同撤权；不把旧 TextKit 宽度缓存当分页权限。
    fn current(
        &self,
        surface: &NativeMarkdownWindowSurface,
        facts: &AppendFacts,
        generation: u64,
    ) -> bool {
        native_markdown_controller_has_foreground_authority(&surface.controller)
            && std::ptr::from_ref(&*surface.controller).addr() == facts.owner
            && surface.editor.as_ref().is_some_and(|editor| {
                std::rc::Weak::ptr_eq(&facts.body, &Rc::downgrade(&editor.body))
                    && WeakMarkdownDocument::ptr_eq(
                        &facts.document,
                        &Rc::downgrade(editor.document()),
                    )
                    && editor.presentation_mode() == MarkdownPresentationMode::InlinePreview
                    && editor.document().borrow().generation() == generation
            })
    }

    fn initial_text(&self) -> &'static str {
        self.case.fixture_source()
    }

    pub(crate) fn verify_open(&self) -> Result<(), String> {
        let facts = self.facts()?;
        self.wait_parent(&facts, facts.generation, Instant::now(), false)?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, facts.generation) {
                return Err("Hosted 表格原首票已撤销".into());
            }
            let document = surface.editor.as_ref().unwrap().document().borrow();
            Self::verify_conflict_text(&document, self.case.fixture_source())?;
            if document.selection() != facts.selection
                || document.current_revision().map_err(|e| e.to_string())? != facts.baseline
                || document.is_dirty()
                || document.can_undo()
                || document.can_redo()
            {
                return Err("Hosted 表格首帧改变了原源、选择或历史".into());
            }
            Ok(())
        })?;
        if self.case == NativeMarkdownHostedTableCase::ToolbarFormatSelection {
            self.verify_toolbar_resize(&facts)?;
        }
        Ok(())
    }

    /// 只读原父正文已发布事实；无 SurfaceLease、RefCell 借用或 Native 强客户跨 runloop。
    fn wait_parent(
        &self,
        facts: &AppendFacts,
        generation: u64,
        started: Instant,
        no_cell: bool,
    ) -> Result<(), String> {
        loop {
            let ready = with_active_native_markdown_surface(|surface| {
                if !self.current(surface, facts, generation) {
                    return Err("等待父帧期间原文稿已撤销".into());
                }
                let editor = surface.editor.as_ref().unwrap();
                if let Some(cause) = editor.body.take_failure() {
                    return Err(format!("真实父正文失败：{cause}"));
                }
                if let Some(cause) = editor.text_view.take_failure()
                    && !matches!(cause, crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending)
                { return Err(format!("真实父输入失败：{cause}")); }
                if no_cell && editor.body.active_cell_body().is_some() {
                    return Ok(false);
                }
                Ok(!editor.body.is_preparing()
                    && editor.text_view.capture_ready_scene_key().is_ok_and(|key| {
                        editor
                            .text_view
                            .probe_cpu_published_frame()
                            .is_some_and(|(cpu, paints)| {
                                paints != 0
                                    && cpu.same(&key)
                                    && editor.text_view.cpu_paints() == Some(paints)
                                    && editor.text_view.cpu_paint_pending() == Some(false)
                            })
                    }))
            })?;
            if ready && started.elapsed() <= Duration::from_secs(3) {
                return Ok(());
            }
            let remaining = Duration::from_secs(3)
                .checked_sub(started.elapsed())
                .ok_or("父正文没有在三秒内发布原完整帧")?;
            // SAFETY: 原主线程仅消费本动作剩余期限；当轮所有窗口和正文借用已经结束。
            unsafe {
                objc2_core_foundation::CFRunLoop::run_in_mode(
                    objc2_core_foundation::kCFRunLoopDefaultMode,
                    remaining.as_secs_f64(),
                    true,
                )
            };
        }
    }

    /// 唯一活动格的每次短读取都来自实际客户端，不能把 canonical raw 源当当前草稿。
    fn local(view: &NativeMarkdownPagedTextView, expected: &str) -> Result<(), String> {
        let state = view.input_state().map_err(|e| e.to_string())?;
        let range = NSRange::new(0, expected.encode_utf16().count());
        if state.utf16_len != range.length || range.length > 256 {
            return Err("真实格草稿长度不同于固定短期望".into());
        }
        if range.length == 0 {
            return Ok(());
        }
        let mut actual = NSRange::new(objc2_foundation::NSNotFound as usize, 0);
        // SAFETY: 原实际 NSTextInputClient 和有效 out 参数，只领取固定小夹具明确的有限范围。
        let value = unsafe {
            ProtocolObject::<dyn NSTextInputClient>::from_ref(view)
                .attributedSubstringForProposedRange_actualRange(range, &mut actual)
        }
        .ok_or("实际格没有有限草稿")?;
        if actual != range || value.string().to_string() != expected {
            return Err("实际格内容或客户范围不同".into());
        }
        Ok(())
    }

    fn with_cell<T>(
        &self,
        work: impl FnOnce(&NativeMarkdownPagedBody, &NativeMarkdownPagedTextView) -> Result<T, String>,
    ) -> Result<T, String> {
        let facts = self.facts()?;
        let expected = self.cell.borrow().clone().ok_or("原活动格身份尚未安装")?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, facts.generation) {
                return Err("原格父文稿已撤销".into());
            }
            let editor = surface.editor.as_ref().unwrap();
            let child = editor.body.active_cell_body().ok_or("当前没有实际活动格")?;
            if !std::rc::Weak::ptr_eq(&expected, &Rc::downgrade(&child)) {
                return Err("当前客户不是原格正文".into());
            }
            let view = child.view().map_err(|e| e.to_string())?;
            work(&child, &view)
        })
    }

    fn settle_cell(&self, started: Instant) -> Result<(), String> {
        let facts = self.facts()?;
        let input = with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, facts.generation) {
                return Err("等待格帧前原父文稿已撤销".into());
            }
            surface
                .editor
                .as_ref()
                .unwrap()
                .text_view
                .input_state()
                .map_err(|e| e.to_string())
        })?;
        let child = wait_for_paged_cell(facts.body, facts.document, input, None, started)?;
        if !self
            .cell
            .borrow()
            .as_ref()
            .is_some_and(|old| std::rc::Weak::ptr_eq(old, &child))
        {
            return Err("等待原格绘制时客户已换走".into());
        }
        Ok(())
    }

    /// 从原 Scene 的真实字符框构造一次 Down/Up，交给 NSApplication 的正常窗口路由。
    pub(crate) fn begin_append(&self) -> Result<(), String> {
        let facts = self.facts()?;
        let started = Instant::now();
        let (down, up, input, mtm) = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            let editor = surface.editor.as_ref().ok_or("点按原格缺少父正文")?;
            let key = editor
                .text_view
                .capture_ready_scene_key()
                .map_err(|e| e.to_string())?;
            let scene =
                NativeMarkdownPagedTextViewDelegate::ready_scene(&*editor.body, &key.request)
                    .map_err(|e| e.to_string())?;
            let position = "| 编号 | 值 |\n| --- | --- |\n| 末项 | "
                .encode_utf16()
                .count()
                + if self.case == NativeMarkdownHostedTableCase::FormatUnchanged {
                    2
                } else {
                    0
                }; // 命中真实“甲”，不把隐藏 ** 当可见字符。
            let first = scene
                .first_rect(NSRange::new(position, 1))
                .map_err(|e| e.to_string())?;
            if first.actual_range != NSRange::new(position, 1) || first.rect.size.width <= 0.0 {
                return Err("原末格没有真实字符命中框".into());
            }
            let point = step(&current, || {
                editor.text_view.convertPoint_toView(
                    NSPoint::new(
                        first.rect.origin.x + first.rect.size.width * 0.25,
                        first.rect.origin.y + first.rect.size.height * 0.5,
                    ),
                    None,
                )
            })?;
            let number = step(&current, || surface.panel.windowNumber())?;
            let event = |kind, n, pressure| {
                NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind, point, NSEventModifierFlags::empty(), 1.0, number, None, n, 1, pressure).ok_or("无法创建原格真实鼠标事件".to_owned())
            };
            Ok((
                event(NSEventType::LeftMouseDown, 1, 1.0)?,
                event(NSEventType::LeftMouseUp, 2, 0.0)?,
                editor.text_view.input_state().map_err(|e| e.to_string())?,
                surface.panel.mtm(),
            ))
        })?;
        let app = NSApplication::sharedApplication(mtm);
        app.sendEvent(&down);
        app.sendEvent(&up);
        drop((app, down, up)); // 交付结束后才等待；不跨 ACTIVE 租约或 Native event claim 跑主循环。
        let child = wait_for_paged_cell(facts.body, facts.document, input, None, started)?;
        self.cell.replace(Some(child.clone()));
        self.previous_cells.borrow_mut().push(child);
        Ok(())
    }

    /// 同一实际客户接收一次有限草稿和 Tab；取消反例在没有让出原栈时立即 Esc。
    pub(crate) fn finish_cell_activation_and_begin_append(&self) -> Result<(), String> {
        let mut facts = self.facts()?;
        let started = Instant::now();
        let previous = self.cell.borrow().clone().ok_or("末格身份缺失")?;
        let before = with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, facts.generation) {
                return Err("Tab 前原父文稿已撤销".into());
            }
            surface
                .editor
                .as_ref()
                .unwrap()
                .text_view
                .input_state()
                .map_err(|e| e.to_string())
        })?;
        self.with_cell(|_, view| {
            Self::local(view, "原值")?;
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(view);
            // SAFETY: 原真实客户只替换已证明的固定两个 UTF16 单元；不触 canonical 私态。
            unsafe {
                input.insertText_replacementRange(ns_string!("末格追加验收"), NSRange::new(0, 2));
            }
            if let Some(cause) = view.take_failure() {
                return Err(format!("原末格输入失败：{cause}"));
            }
            view.command(sel!(insertTab:)).map_err(|e| e.to_string())?;
            if let Some(cause) = view.take_failure() {
                return Err(format!("原末格 Tab 失败：{cause}"));
            }
            if self.case == NativeMarkdownHostedTableCase::Cancel {
                view.command(sel!(cancelOperation:))
                    .map_err(|e| e.to_string())?;
                if let Some(cause) = view.take_failure() {
                    return Err(format!("原末格取消失败：{cause}"));
                }
            }
            Ok(())
        })?;
        if self.case == NativeMarkdownHostedTableCase::Cancel {
            self.wait_parent(&facts, facts.generation, started, true)?;
        } else {
            let child = self.wait_appended_cell(&facts, &previous, started, before)?;
            self.cell.replace(Some(child.clone()));
            self.previous_cells.borrow_mut().push(child);
            let selection = with_active_native_markdown_surface(|surface| {
                let editor = surface.editor.as_ref().ok_or("追加后原正文已释放")?;
                Ok(editor.document().borrow().selection())
            })?;
            facts.after_selection = Some(selection);
            self.facts.replace(Some(facts));
        }
        Ok(())
    }

    /// 末格追加会合法推进父 Input；原 g0/g1、同 Body/Doc 和真实新空客户约束整个移交。
    fn wait_appended_cell(
        &self,
        facts: &AppendFacts,
        previous: &std::rc::Weak<NativeMarkdownPagedBody>,
        started: Instant,
        before: NativeMarkdownPagedInputState,
    ) -> Result<std::rc::Weak<NativeMarkdownPagedBody>, String> {
        let generation = facts.generation.checked_add(1).ok_or("追加代次溢出")?;
        // 原Cell追加只发布一次canonical事务；Input::publish无论folds_changed是否为真，
        // 都为新Source重映射推进一次fold_revision。保精确后态，不能沿用旧源revision，
        // 也不能接受同generation上另一次折叠动作造成的额外变化。
        let expected_fold = before
            .fold_revision
            .checked_add(1)
            .ok_or("追加折叠映射代次溢出")?;
        let expected_units = APPENDED.encode_utf16().count();
        loop {
            let found = with_active_native_markdown_surface(|surface| {
                if !self.current(surface, facts, facts.generation)
                    && !self.current(surface, facts, generation)
                {
                    return Err("原追加事务越过唯一合法字符代次".into());
                }
                let editor = surface.editor.as_ref().unwrap();
                let actual = editor.text_view.input_state().map_err(|e| e.to_string())?;
                if actual.generation == facts.generation && actual != before {
                    return Err("未发布追加前原完整父 Input 被别的动作改变".into());
                }
                if actual.generation == generation
                    && (actual.utf16_len != expected_units
                        || actual.marked.is_some()
                        || actual.fold_revision != expected_fold)
                {
                    // 同一失败边界只记纯Input标量，不读源或输出文本；原严格拒绝保持不变。
                    eprintln!(
                        "native_table_append_parent_input_replaced before={before:?} actual={actual:?} expected_generation={generation} expected_utf16_len={expected_units} expected_fold_revision={expected_fold}"
                    );
                    return Err("追加发布的父 Input 不属于预声明新源".into());
                }
                if let Some(cause) = editor.body.take_failure() {
                    return Err(format!("实际追加失败：{cause}"));
                }
                if let Some(cause) = editor.text_view.take_failure()
                    && !matches!(cause, crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending)
                { return Err(format!("实际追加父输入失败：{cause}")); }
                // 原客户关闭过程中仍可能留下 void 回调首错；只观察同一 Weak，不据其存活补签。
                if let Some(old) = previous.upgrade() {
                    if let Some(cause) = old.take_failure() {
                        return Err(format!("原末格在途失败：{cause}"));
                    }
                    if let Ok(view) = old.view()
                        && let Some(cause) = view.take_failure()
                        && !matches!(cause, crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending)
                    { return Err(format!("原末格在途输入失败：{cause}")); }
                }
                let Some(child) = editor.body.active_cell_body() else {
                    return Ok(None);
                };
                if std::rc::Weak::ptr_eq(previous, &Rc::downgrade(&child)) {
                    return Ok(None);
                }
                if !self.current(surface, facts, generation) {
                    return Err("新格未由一次 canonical 追加产生".into());
                }
                let view = child.view().map_err(|e| e.to_string())?;
                if let Some(cause) = child.take_failure() {
                    return Err(format!("实际新格失败：{cause}"));
                }
                if let Some(cause) = view.take_failure()
                    && !matches!(cause, crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending)
                { return Err(format!("实际新格输入失败：{cause}")); }
                if !child.is_preparing()
                    && view.capture_ready_scene_key().is_ok_and(|key| {
                        view.probe_cpu_published_frame()
                            .is_some_and(|(cpu, paints)| {
                                cpu.same(&key)
                                    && paints != 0
                                    && view.cpu_paint_pending() == Some(false)
                            })
                    })
                {
                    Self::local(&view, "")?;
                    let focus = surface.panel.firstResponder().is_some_and(|r| {
                        let r: &AnyObject = r.as_ref();
                        r.downcast_ref::<NativeMarkdownPagedTextView>()
                            .is_some_and(|r| std::ptr::eq(r, &**view))
                    });
                    if !focus {
                        return Err("新空格没有取得真实键盘焦点".into());
                    }
                    Self::verify_conflict_text(&editor.document().borrow(), APPENDED)?;
                    return Ok(Some(Rc::downgrade(&child)));
                }
                Ok(None)
            })?;
            if let Some(found) = found
                && started.elapsed() <= Duration::from_secs(3)
            {
                return Ok(found);
            }
            let remaining = Duration::from_secs(3)
                .checked_sub(started.elapsed())
                .ok_or("末格 Tab 三秒内没有真实新行首格完整帧")?;
            // SAFETY: 只消费本动作的原 runloop 剩余期限，所有客户强引用和租约已经归还。
            unsafe {
                objc2_core_foundation::CFRunLoop::run_in_mode(
                    objc2_core_foundation::kCFRunLoopDefaultMode,
                    remaining.as_secs_f64(),
                    true,
                )
            };
        }
    }

    /// 真实待处理 Tab 的取消不得产生字符事务，也不能在后来的空闲回调复活草稿。
    pub(crate) fn verify_negative(&self) -> Result<(), String> {
        if self.case != NativeMarkdownHostedTableCase::Cancel {
            return Err("本入口只消费真实取消反例".into());
        }
        let facts = self.facts()?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, facts.generation) {
                return Err("取消改变了原字符代次".into());
            }
            let editor = surface.editor.as_ref().unwrap();
            let doc = editor.document().borrow();
            Self::verify_conflict_text(&doc, BEFORE)?;
            if doc.current_revision().map_err(|e| e.to_string())? != facts.baseline
                || doc.is_dirty()
                || doc.can_undo()
                || doc.can_redo()
                || editor.body.active_cell_body().is_some()
            {
                return Err("取消未保持原源、历史或真实关闭客户".into());
            }
            Ok(())
        })?;
        Self::verify_conflict_file(&self.path, BEFORE)?;
        println!(
            "native_table_append_negative case=cancel same_stack_tab_escape=true canonical_unchanged=true late_cpu_checked=true physical_tab=false"
        );
        Ok(())
    }

    /// 子格局部格式与父文档提交是两个明确动作；旧字段隐藏/NSUndoManager 不再是分页功能条件。
    pub(crate) fn verify_format(&self) -> Result<(), String> {
        let selected = self.case != NativeMarkdownHostedTableCase::FormatUnchanged;
        let started = Instant::now();
        self.with_cell(|_, view| {
            Self::local(view, if selected { "前中😀后" } else { "**甲**" })?;
            if !selected {
                // SAFETY: 原真实客户只接有限草稿替换，不把移除标记写入 canonical。
                unsafe {
                    ProtocolObject::<dyn NSTextInputClient>::from_ref(view)
                        .insertText_replacementRange(ns_string!("甲"), NSRange::new(0, 5));
                }
            }
            Ok(())
        })?;
        self.settle_cell(started)?;
        let selectors = if selected {
            &[
                sel!(moveToBeginningOfDocument:),
                sel!(moveRight:),
                sel!(moveRightAndModifySelection:),
                sel!(moveRightAndModifySelection:),
            ][..]
        } else {
            &[
                sel!(moveToBeginningOfDocument:),
                sel!(moveRightAndModifySelection:),
            ][..]
        };
        for &selector in selectors {
            // 每次原选择命令完成真实帧再消费下一次几何，整组仍使用本动作原三秒期限。
            self.with_cell(|_, view| view.command(selector).map_err(|e| e.to_string()))?;
            self.settle_cell(started)?;
        }
        self.with_cell(|_, view| {
            let expected = NSRange::new(if selected { 1 } else { 0 }, if selected { 3 } else { 1 });
            if ProtocolObject::<dyn NSTextInputClient>::from_ref(view).selectedRange() != expected {
                return Err("实际格没有选择预声明的完整中😀或甲".into());
            }
            Ok(())
        })?;
        if self.case == NativeMarkdownHostedTableCase::ToolbarFormatSelection {
            return self.click_toolbar_button(0, 103);
        }
        self.with_cell(|_, view| {
            view.apply_format(MarkdownFormatAction::Bold)
                .map_err(|e| e.to_string())
        })?;
        self.settle_cell(Instant::now())?;
        self.with_cell(|_, view| {
            Self::local(
                view,
                if selected {
                    "前**中😀**后"
                } else {
                    "**甲**"
                },
            )
        })?;
        self.commit_cell()?;
        if selected {
            self.verify_formatted_revision(1, SELECTED_AFTER, true, false)?;
            let mut facts = self.facts()?;
            with_active_native_markdown_surface(|surface| {
                let document = surface.editor.as_ref().unwrap().document().borrow();
                facts.after = Some(document.current_revision().map_err(|e| e.to_string())?);
                facts.after_selection = Some(document.selection());
                Ok(())
            })?;
            self.facts.replace(Some(facts));
            self.parent_history(sel!(undo:), 1)?;
            self.verify_formatted_revision(2, SELECTED_BEFORE, false, true)?;
            self.parent_history(sel!(redo:), 2)?;
            self.verify_formatted_revision(3, SELECTED_AFTER, true, false)?;
            self.save_exact(3, SELECTED_AFTER)?;
        } else {
            let facts = self.facts()?;
            self.verify_formatted_revision(0, UNCHANGED, false, false)?;
            with_active_native_markdown_surface(|surface| {
                let doc = surface.editor.as_ref().unwrap().document().borrow();
                if doc.current_revision().map_err(|e| e.to_string())? != facts.baseline
                    || doc.selection() != facts.selection
                {
                    return Err("无变化格提交错误移动 canonical 选择或历史".into());
                }
                Ok(())
            })?;
            Self::verify_conflict_file(&self.path, UNCHANGED)?;
        }
        Ok(())
    }

    /// 同原正式 Body 提交客户；这是已有保存/失焦用例真实入口，不重建 Cell source 或改文档。
    fn commit_cell(&self) -> Result<(), String> {
        let facts = self.facts()?;
        let started = Instant::now();
        let generation = with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, facts.generation) {
                return Err("提交前原父文稿已撤销".into());
            }
            let editor = surface.editor.as_ref().unwrap();
            editor
                .body
                .commit_active_cell()
                .map_err(|e| e.to_string())?;
            Ok(editor.document().borrow().generation())
        })?;
        if generation != facts.generation
            && generation != facts.generation.checked_add(1).ok_or("格提交代次溢出")?
        {
            return Err("格提交超过一次 canonical 字符事务".into());
        }
        self.wait_parent(&facts, generation, started, true)
    }

    fn verify_formatted_revision(
        &self,
        delta: u64,
        source: &str,
        undo: bool,
        redo: bool,
    ) -> Result<(), String> {
        let facts = self.facts()?;
        let generation = facts.generation.checked_add(delta).ok_or("源代次溢出")?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, generation) {
                return Err("真实字符结果不在预声明代次".into());
            }
            let editor = surface.editor.as_ref().unwrap();
            let doc = editor.document().borrow();
            Self::verify_conflict_text(&doc, source)?;
            if doc.can_undo() != undo
                || doc.can_redo() != redo
                || doc.is_dirty() != (source != self.initial_text())
            {
                return Err("实际一次 Undo/Redo 历史或 dirty 状态不符".into());
            }
            let selection = doc.selection();
            if source == self.initial_text() && selection != facts.selection {
                return Err("回到原源后完整 canonical 有向选择没有恢复".into());
            }
            if matches!(
                self.case,
                NativeMarkdownHostedTableCase::FormatSelection
                    | NativeMarkdownHostedTableCase::ToolbarFormatSelection
            ) && undo
            {
                let start = "| 编号 | 值 |\n| --- | --- |\n| 末项 | "
                    .encode_utf16()
                    .count();
                if selection
                    != (MarkdownTextSelection {
                        anchor_utf16: start + 3,
                        head_utf16: start + 6,
                    })
                {
                    return Err("格式提交没有映回准确的 canonical 中😀 有向选区".into());
                }
            }
            let low = selection.anchor_utf16.min(selection.head_utf16);
            let high = selection.anchor_utf16.max(selection.head_utf16);
            if ProtocolObject::<dyn NSTextInputClient>::from_ref(&**editor.text_view)
                .selectedRange()
                != NSRange::new(low, high - low)
            {
                return Err("父正文 Native 选择没有对应真实提交结果".into());
            }
            Ok(())
        })
    }

    fn parent_history(&self, selector: objc2::runtime::Sel, before: u64) -> Result<(), String> {
        let facts = self.facts()?;
        let generation = facts.generation.checked_add(before).ok_or("历史代次溢出")?;
        let started = Instant::now();
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, generation) {
                return Err("原正文历史动作已撤销".into());
            }
            surface
                .editor
                .as_ref()
                .unwrap()
                .text_view
                .command(selector)
                .map_err(|e| e.to_string())
        })?;
        self.wait_parent(
            &facts,
            generation.checked_add(1).ok_or("历史后态溢出")?,
            started,
            true,
        )
    }

    fn save_exact(&self, delta: u64, text: &str) -> Result<(), String> {
        let facts = self.facts()?;
        let generation = facts.generation.checked_add(delta).ok_or("保存代次溢出")?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, generation) {
                return Err("原文稿保存动作已撤销".into());
            }
            surface.save_current_document()?;
            if !self.current(surface, &facts, generation)
                || surface
                    .editor
                    .as_ref()
                    .unwrap()
                    .document()
                    .borrow()
                    .is_dirty()
            {
                return Err("真实保存改变字符代次或没有结算 dirty".into());
            }
            Ok(())
        })?;
        Self::verify_conflict_file(&self.path, text)
    }

    /// 追加后的首个空客户也经原失焦/提交或 Esc 结束；然后单次文档 Undo 移除草稿和唯一新行。
    pub(crate) fn verify_append_and_undo(&self) -> Result<(), String> {
        let mut facts = self.facts()?;
        let started = Instant::now();
        let generation = facts.generation.checked_add(1).ok_or("追加代次溢出")?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, generation) {
                return Err("真实追加不是一次 canonical 事务".into());
            }
            let editor = surface.editor.as_ref().unwrap();
            let child = editor
                .body
                .active_cell_body()
                .ok_or("真实新增行没有活动首格")?;
            let view = child.view().map_err(|e| e.to_string())?;
            Self::local(&view, "")?;
            if self.case == NativeMarkdownHostedTableCase::CommitResigned {
                if !surface.panel.makeFirstResponder(Some(&**editor.text_view)) {
                    return Err("新空格无法实际交回父正文焦点".into());
                }
                // 焦点已经实际离开原客户后，普通提交必须正常释放空格且不能创建第二个字符事务。
                editor
                    .body
                    .commit_active_cell()
                    .map_err(|e| e.to_string())?;
            } else {
                view.command(sel!(cancelOperation:))
                    .map_err(|e| e.to_string())?;
            }
            let document = editor.document().borrow();
            Self::verify_conflict_text(&document, APPENDED)?;
            facts.after = Some(document.current_revision().map_err(|e| e.to_string())?);
            facts.after_selection = Some(document.selection());
            Ok(())
        })?;
        self.facts.replace(Some(facts.clone()));
        self.wait_parent(&facts, generation, started, true)?;
        self.parent_history(sel!(undo:), 1)
    }

    pub(crate) fn verify_history(&self, redo: bool, final_check: bool) -> Result<(), String> {
        let facts = self.facts()?;
        let delta = if final_check {
            4
        } else if redo {
            2
        } else {
            3
        };
        self.verify_formatted_revision(
            delta,
            if redo || final_check {
                BEFORE
            } else {
                APPENDED
            },
            !redo && !final_check,
            redo || final_check,
        )?;
        with_active_native_markdown_surface(|surface| {
            let doc = surface.editor.as_ref().unwrap().document().borrow();
            let expected = if redo || final_check {
                facts.baseline
            } else {
                facts.after.ok_or("缺少真实追加 revision")?
            };
            if doc.current_revision().map_err(|e| e.to_string())? != expected {
                return Err("一次文档历史没有恢复完整 revision".into());
            }
            Ok(())
        })?;
        if final_check {
            self.save_exact(delta, BEFORE)
        } else {
            self.parent_history(if redo { sel!(redo:) } else { sel!(undo:) }, delta)
        }
    }

    /// 工具栏粗体先只改变实际格草稿；同一次正常提交后再走真实文档 Undo/Redo 按钮。
    pub(crate) fn verify_toolbar_format_history(&self, stage: u8) -> Result<(), String> {
        let field_history = self.case == NativeMarkdownHostedTableCase::ToolbarFieldHistory;
        let expected = if field_history {
            CONFLICT_DRAFT
        } else {
            SELECTED_AFTER
        };
        if stage == 0 && !field_history {
            self.settle_cell(Instant::now())?;
            self.with_cell(|_, view| Self::local(view, "前**中😀**后"))?;
            self.commit_cell()?;
        }
        let delta = u64::from(stage) + 1;
        self.verify_formatted_revision(
            delta,
            if stage == 1 {
                SELECTED_BEFORE
            } else {
                expected
            },
            stage != 1,
            stage == 1,
        )?;
        if stage == 0 {
            let mut facts = self.facts()?;
            with_active_native_markdown_surface(|surface| {
                let doc = surface.editor.as_ref().unwrap().document().borrow();
                facts.after = Some(doc.current_revision().map_err(|e| e.to_string())?);
                facts.after_selection = Some(doc.selection());
                Ok(())
            })?;
            self.facts.replace(Some(facts));
            self.click_toolbar_button(delta, 22)
        } else if stage == 1 {
            self.click_toolbar_button(delta, 23)
        } else {
            self.save_exact(delta, expected)
        }
    }

    /// 两次真实局部输入和工具栏历史都只作用于同一活动格；信息卡不得确认 marked。
    pub(crate) fn advance_toolbar_field_history(&self, stage: u8) -> Result<(), String> {
        let facts = self.facts()?;
        if stage != 0 {
            self.settle_cell(Instant::now())?;
            self.verify_information_card(&facts, stage != 4)?;
        }
        self.with_cell(|_, view| {
            let expected = match stage {
                0 => "前中😀后",
                1 | 3 | 5 => "甲",
                _ => "甲乙",
            };
            Self::local(view, expected)?;
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(view);
            let history = view
                .cell_history_facts()
                .map_err(|e| e.to_string())?
                .ok_or("原格没有局部历史")?;
            if history.can_undo != (stage != 0) || history.can_redo != matches!(stage, 3 | 5) {
                return Err("真实格局部撤销重做可用性不符".into());
            }
            if stage == 1 {
                if !input.hasMarkedText()
                    || input.markedRange() != NSRange::new(0, 1)
                    || input.selectedRange() != NSRange::new(1, 0)
                {
                    return Err("信息卡改变了真实 marked 范围或选择".into());
                }
                input.unmarkText();
            }
            if stage != 1 && input.hasMarkedText() {
                return Err("原格出现未预声明组合".into());
            }
            match stage {
                0 => {
                    // SAFETY: 原实际单格客户，固定有限输入；setMarked 只验证协议保留，不声称物理 IME。
                    unsafe {
                        input.insertText_replacementRange(ns_string!("甲"), NSRange::new(0, 5));
                        input.setMarkedText_selectedRange_replacementRange(
                            ns_string!("甲"),
                            NSRange::new(1, 0),
                            NSRange::new(0, 1),
                        );
                    }
                }
                1 => {
                    // SAFETY: 同一已核客户已经结束“甲”的组合，固定1..1位于这份短草稿的真实末端。
                    unsafe {
                        input.insertText_replacementRange(ns_string!("乙"), NSRange::new(1, 0));
                    }
                }
                2 => {
                    view.command(sel!(undo:)).map_err(|e| e.to_string())?;
                }
                _ => {}
            }
            Ok(())
        })?;
        // 所有本地输入与观察后仍须保持原 canonical root、完整选择和空字符历史。
        self.verify_formatted_revision(0, SELECTED_BEFORE, false, false)?;
        match stage {
            0 => self.click_toolbar_button(0, 6),
            1 => self.click_information_control(&facts, false),
            3 => {
                self.click_toolbar_button(0, 23)?;
                self.click_toolbar_button(0, 6)
            }
            4 => {
                self.click_toolbar_button(0, 22)?;
                self.click_toolbar_button(0, 6)
            }
            5 => self.click_toolbar_button(0, 23),
            6 => self.commit_cell(),
            _ => Ok(()),
        }
    }

    /// 关闭按钮必须先经过真实鼠标事件，再由下一Host步骤核移除和清空；本步骤才重开。
    fn reopen_information_card(&self) -> Result<(), String> {
        let facts = self.facts()?;
        self.verify_information_card(&facts, false)?;
        self.click_toolbar_button(0, 6)
    }

    /// 刷新也是独立用户动作；后继局部历史步骤核同一卡与canonical，不在排队后立即冒充完成。
    fn refresh_information_card(&self) -> Result<(), String> {
        let facts = self.facts()?;
        self.settle_cell(Instant::now())?;
        self.verify_information_card(&facts, true)?;
        self.click_information_control(&facts, true)
    }

    /// 独立小表场景预先固定两处真实源坐标，再沿原字段协议开始组合和真实按钮导航。
    pub(crate) fn begin_find_replacement_composition(&self) -> Result<(), String> {
        let facts = self.facts()?;
        if self.case != NativeMarkdownHostedTableCase::FindReplacementComposition
            || self.find_replacement.borrow().is_some()
            || facts.selection != MarkdownTextSelection::caret(0)
        {
            return Err("查找组合场景缺少唯一文首基线".into());
        }
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, facts.generation) {
                return Err("查找组合初始原票已失效".into());
            }
            let document = surface
                .editor
                .as_ref()
                .ok_or("原正文缺失")?
                .document()
                .borrow();
            if document.byte_len() != FIND_REPLACEMENT_BEFORE.len()
                || document.utf16_len() != FIND_REPLACEMENT_BEFORE.encode_utf16().count()
                || document.selection() != facts.selection
                || document.is_dirty()
                || document.can_undo()
                || document.can_redo()
            {
                return Err("查找组合的固定源或初始历史不符".into());
            }
            for (index, expected) in FIND_REPLACEMENT_BEFORE.encode_utf16().enumerate() {
                if document
                    .utf16_code_unit_at(index)
                    .map_err(|cause| cause.to_string())?
                    != expected
                {
                    return Err("查找组合初始字节不是原固定夹具".into());
                }
            } // 首个查找 Native 动作前核完原范围，后继不能用实际新选择替代它。
            Ok(())
        })?;
        // 坐标只从本用例固定字面量计算，绝不读实际全文或在动作结束后重采目标。
        let position = |byte: usize| {
            let start = FIND_REPLACEMENT_BEFORE[..byte].encode_utf16().count();
            MarkdownTextSelection {
                anchor_utf16: start,
                head_utf16: start + 1,
            }
        };
        let first = position(FIND_REPLACEMENT_BEFORE.find('值').unwrap());
        let last = position(FIND_REPLACEMENT_BEFORE.rfind('值').unwrap());
        show_native_markdown_find_bar(true)?; // 正式显示完成且 owner 已归还，才领取实际字段。
        let mut probe = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("查找组合缺少原正文")?;
            let bar = editor.find_bar.borrow().clone().ok_or("实际查找栏未显示")?;
            let intent = Cell::new(surface.controller.find_input_intent());
            let route = Cell::new(
                surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    .ok_or("查询缺少原会话路由")?,
            );
            let current = || {
                self.current(surface, &facts, facts.generation)
                    && surface.controller.find_input_intent() == intent.get()
                    && surface
                        .controller
                        .ivars()
                        .field_editor_provider
                        .current_input_client_identity()
                        == Some(route.get())
            };
            let fields = step(&current, || bar.fields())?;
            let query = &fields[0];
            let replacement = &fields[1];
            for field in &fields {
                let value = step(&current, || field.stringValue())?;
                if !step(&current, || value.isEqualToString(ns_string!("")))? {
                    return Err("新查找栏不是本用例的空输入基线".into());
                }
                drop(value); // 后继字段读取仍先核原票，临时原生值不跨输入动作。
            }
            let query_editor = step(&current, || query.currentEditor())?
                .ok_or("查询未获得真实字段编辑器")?
                .downcast::<NSTextView>()
                .map_err(|_| "查询字段编辑器类型错误")?;
            let value = step(&current, || NSString::from_str("值"))?;
            if std::ptr::from_ref(&*query_editor).addr() != route.get().0 {
                return Err("查询不是原会话当前输入客户".into());
            }
            let next = intent.get().checked_add(1).ok_or("查询输入意图溢出")?;
            // SAFETY: 原实际字段和固定一个 UTF16 单元存活，只替换已证明为空的查询。
            unsafe {
                ProtocolObject::<dyn NSTextInputClient>::from_ref(&*query_editor)
                    .insertText_replacementRange(&value, NSRange::new(0, 0));
            }
            if !self.current(surface, &facts, facts.generation)
                || surface.controller.find_input_intent() != next
                || surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    != Some(route.get())
            {
                return Err("真实查询输入未只推进原查询意图一次".into());
            }
            intent.set(next);
            drop((value, query_editor));
            if !current() {
                return Err("查询临时值归还后原输入已变化".into());
            }
            // SAFETY: 原替换字段仍属于同一栏；正常选择让 AppKit 交接输入、校验和本地历史。
            unsafe { replacement.selectText(None) };
            if !self.current(surface, &facts, facts.generation)
                || surface.controller.find_input_intent() != intent.get()
            {
                return Err("仅切换替换焦点改变了查询意图或原正文".into());
            }
            // selectText 是明确请求的真实客户切换；仅这次成功动作之后固定其后继路由。
            route.set(
                surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    .ok_or("替换缺少原会话路由")?,
            );
            let actual = step(&current, || replacement.currentEditor())?
                .ok_or("替换未获得真实字段编辑器")?
                .downcast::<NSTextView>()
                .map_err(|_| "替换字段编辑器类型错误")?;
            let session = actual
                .downcast_ref::<NativeMarkdownSessionFieldEditor>()
                .filter(|session| {
                    session.belongs_to(
                        &surface.controller.ivars().undo,
                        &surface.controller.ivars().owned,
                    )
                })
                .ok_or("替换输入不属于原会话")?;
            let actual_route = (
                std::ptr::from_ref(&*actual).addr(),
                session.input_client_revision(),
            );
            if actual_route != route.get() {
                return Err("替换字段不是预声明的实际交接客户".into());
            }
            let value = step(&current, || NSString::from_str("中"))?;
            let next = intent.get().checked_add(1).ok_or("替换组合意图溢出")?;
            // SAFETY: 实际替换 editor 接收固定候选；不发假通知，也不提前确认中文输入。
            unsafe {
                ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual)
                    .setMarkedText_selectedRange_replacementRange(
                        &value,
                        NSRange::new(1, 0),
                        NSRange::new(0, 0),
                    );
            }
            if !self.current(surface, &facts, facts.generation)
                || surface.controller.find_input_intent() != next
                || surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    != Some(route.get())
            {
                return Err("真实替换组合没有保持原客户或准确后态".into());
            }
            intent.set(next);
            let weak = Weak::from_retained(&actual);
            drop((value, actual, fields));
            if !current()
                || surface.controller.find_input_intent() != next
                || surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    != Some(route.get())
            {
                return Err("归还组合输入临时值后原客户已变化".into());
            }
            Ok(FindReplacementProbeFacts {
                bar: Rc::downgrade(&bar),
                field_editor: weak,
                route: Some(route.get()),
                intent: next,
                queued_button: None,
                first,
                last,
                action_started: Instant::now(),
            })
        })?;
        self.click_find_replacement_button(
            &facts,
            &mut probe,
            MarkdownFindAction::Next,
            NativeMarkdownHostedTableStep::VerifyFindNextAndPrevious,
        )?;
        self.find_replacement.replace(Some(probe));
        Ok(())
    }

    /// 原栏、Doc、源代次、输入意图及活客户共同撤权；后继事件不能重新签原操作票。
    fn find_replacement_current(
        &self,
        surface: &NativeMarkdownWindowSurface,
        facts: &AppendFacts,
        probe: &FindReplacementProbeFacts,
        generation: u64,
        intent: u64,
    ) -> bool {
        self.current(surface, facts, generation)
            && surface.controller.find_input_intent() == intent
            && probe.route.is_none_or(|route| {
                surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    == Some(route)
            })
            && surface.editor.as_ref().is_some_and(|editor| {
                editor
                    .find_bar
                    .borrow()
                    .as_ref()
                    .is_some_and(|bar| std::rc::Weak::ptr_eq(&probe.bar, &Rc::downgrade(bar)))
            })
    }

    /// 原实际按钮在Active借用结束后接收鼠标对；只预声明本次动作应推进的一次意图。
    fn click_find_replacement_button(
        &self,
        facts: &AppendFacts,
        probe: &mut FindReplacementProbeFacts,
        action: MarkdownFindAction,
        terminal_step: NativeMarkdownHostedTableStep,
    ) -> Result<(), String> {
        if probe.queued_button.is_some() {
            return Err("原查找按钮尚未消费真实终点".into());
        }
        let (generation, selection, utf16_len) = match (action, terminal_step) {
            (
                MarkdownFindAction::Next,
                NativeMarkdownHostedTableStep::VerifyFindNextAndPrevious,
            ) => (
                facts.generation,
                probe.first,
                FIND_REPLACEMENT_BEFORE.encode_utf16().count(),
            ),
            (
                MarkdownFindAction::Previous,
                NativeMarkdownHostedTableStep::VerifyFindPreviousAndRejectReplace,
            )
            | (
                MarkdownFindAction::ReplaceNext,
                NativeMarkdownHostedTableStep::VerifyReplaceRejectedAndCommit,
            ) => (
                facts.generation,
                probe.last,
                FIND_REPLACEMENT_BEFORE.encode_utf16().count(),
            ),
            (
                MarkdownFindAction::ReplaceNext,
                NativeMarkdownHostedTableStep::VerifyReplacedAndUndo,
            ) => (
                facts.generation.checked_add(1).ok_or("替换目标代次溢出")?,
                probe.first,
                FIND_REPLACEMENT_AFTER.encode_utf16().count(),
            ),
            _ => return Err("原 Find kind 没有该预声明后态".into()),
        };
        let next = probe.intent.checked_add(1).ok_or("查找按钮意图溢出")?;
        let started = Instant::now(); // 在任何排队和观察登记之前固定；后继不会重置原三秒。
        let (events, before_input, paint, mtm) = with_active_native_markdown_surface(|surface| {
            let original = || {
                self.find_replacement_current(surface, facts, probe, facts.generation, probe.intent)
            };
            let bar = probe.bar.upgrade().ok_or("原查找栏已释放")?;
            let input = step(&original, || {
                surface.editor.as_ref().unwrap().text_view.input_state()
            })?
            .map_err(|cause| cause.to_string())?;
            let current = || {
                original()
                    && surface
                        .editor
                        .as_ref()
                        .unwrap()
                        .text_view
                        .input_state()
                        .is_ok_and(|actual| actual == input)
            };
            let button = bar.probe_button(action);
            if !step(&current, || button.refusesFirstResponder())? {
                return Err("原查找按钮会夺取字段组合焦点".into());
            }
            let events = button_mouse::prepare(button, &surface.panel, &current)?;
            let button = step(&current, || Weak::from(button))?;
            let down_identity = step(&current, || FindButtonDownIdentity::capture(&events[0]))?;
            let down = events[0].clone(); // 只租原有限鼠标Down，不持正文/输入context，匹配返回即释放。
            let view = step(&current, || {
                Weak::from_retained(&surface.editor.as_ref().unwrap().text_view)
            })?;
            let controller = step(&current, || Weak::from_retained(&surface.controller))?;
            let paint = Rc::new_cyclic(|weak| FindButtonPaint {
                weak: weak.clone(),
                controller,
                button,
                view,
                facts: facts.clone(),
                bar: probe.bar.clone(),
                route: probe.route,
                action,
                terminal_step,
                before_intent: probe.intent,
                after_intent: next,
                before_input: input,
                generation,
                selection,
                utf16_len,
                started,
                down: RefCell::new(Some(down)),
                down_identity,
                dispatched: Cell::new(false),
                selector_returned: Cell::new(false),
                after_queued: Cell::new(false),
                action_returned: Cell::new(false),
                #[cfg(feature = "native-close-stage-diagnostic")]
                dispatch_diagnosed: Cell::new(false),
                #[cfg(feature = "native-close-stage-diagnostic")]
                frame_diagnosed: Cell::new(false),
                terminal: RefCell::new(None),
                observer: terminal::ProbeTerminalObserver::default(),
            });
            if terminal_step != NativeMarkdownHostedTableStep::VerifyReplaceRejectedAndCommit {
                // 真正选择/编辑在点击前订阅原下一完整帧；组合拒绝没有新像素义务，
                // 仅 after_action 证实原 Scene/CPU 真 Pending 才登记，不遗留无帧消费的观察槽。
                let erased: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = paint.clone();
                surface
                    .editor
                    .as_ref()
                    .unwrap()
                    .body
                    .observe_next_cpu_frame(Rc::downgrade(&erased))
                    .map_err(|cause| cause.to_string())?;
            }
            if !current() {
                return Err("登记 Find 原动作观察时完整 Input 已变化".into());
            }
            Ok((events, input, paint, surface.panel.mtm()))
        })?;
        probe.action_started = started;
        // 鼠标队列和正式controller工作都尚可未执行，下一Host事件与主GCD队列没有先后保证。
        // 保存原票允许精确前态暂等；预声明后态仍只有这次按钮应推进的一次意图。
        probe.queued_button = Some(FindReplacementQueuedButton {
            before_intent: probe.intent,
            after_intent: next,
            before_input,
            paint,
        });
        button_mouse::post(events, mtm); // 原栏/按钮/Doc强claim均已退还，实际窗口负责命中与tracking。
        probe.intent = next;
        Ok(())
    }

    /// 按钮回调只接纳动作，不等于 Find/reveal 已完成；只观察该原意图的真实 CPU 和目标。
    /// 原选择、世代与时间都在动作前已知。每轮结束 Active 租约和 Native claim 后才等主循环，
    /// 保留现控件协议断言；这里没有重发按钮、pump 执行器或重新签下一次输入的权限。
    fn wait_find_replacement(
        &self,
        facts: &AppendFacts,
        probe: &mut FindReplacementProbeFacts,
        stage: u8,
    ) -> Result<(), String> {
        if stage <= 4 {
            probe
                .queued_button
                .as_ref()
                .ok_or("Find 原按钮收据已丢失")?
                .paint
                .completed()?;
        } // Host 只有真实终点才能排此事件；这里不再用 runloop 等待补成一次按钮成功。
        let (before_delta, after_delta, expected) = match stage {
            1 => (0, 0, probe.first),
            2 | 3 => (0, 0, probe.last),
            4 => (0, 1, probe.first),
            5 => (1, 2, probe.last),
            6 => (2, 3, probe.first),
            7 => (3, 3, probe.first),
            _ => return Err("未知查找组合终点".into()),
        };
        let before = facts
            .generation
            .checked_add(before_delta)
            .ok_or("查找前态代次溢出")?;
        let after = facts
            .generation
            .checked_add(after_delta)
            .ok_or("查找后态代次溢出")?;
        let range = NSRange::new(
            expected.anchor_utf16,
            expected.head_utf16 - expected.anchor_utf16,
        );
        loop {
            let ready = with_active_native_markdown_surface(|surface| {
                // 只有四次实际Find按钮拥有排队前态；Undo/Redo/保存不能借旧intent继续等。
                let queued = probe.queued_button.as_ref().filter(|button| {
                    stage <= 4
                        && button.after_intent == probe.intent
                        && surface.controller.find_input_intent() == button.before_intent
                });
                let intent = queued.map_or(probe.intent, |button| button.before_intent);
                if !self.find_replacement_current(surface, facts, probe, before, intent)
                    && (queued.is_some()
                        || !self.find_replacement_current(surface, facts, probe, after, intent))
                {
                    return Err("等待查找组合结果时原意图或文稿已撤销".into());
                }
                let editor = surface.editor.as_ref().ok_or("等待查找结果缺少原正文")?;
                if let Some(cause) = editor.body.take_failure() {
                    return Err(format!("查找组合正文失败：{cause}"));
                }
                if let Some(cause) = editor.text_view.take_failure()
                    && !matches!(cause, crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending)
                { return Err(format!("查找组合输入失败：{cause}")); }
                if let Some(button) = queued {
                    // 正文/客户失败仍优先于Pending；原Source健康也须真实核验，零端点
                    // 不加载正文页。前态只接受点击前完整Input，任何额外输入立即撤权。
                    editor
                        .document()
                        .borrow()
                        .byte_offset_for_utf16(0)
                        .map_err(|cause| cause.to_string())?;
                    if editor
                        .text_view
                        .input_state()
                        .map_err(|cause| cause.to_string())?
                        != button.before_input
                    {
                        return Err("查找按钮排队期间原完整正文输入已改变".into());
                    }
                    return Ok(false); // 不看旧反馈/旧CPU，也不重发按钮或重新领取三秒期限。
                }
                {
                    let document = editor.document().borrow();
                    if document.generation() != after
                        || document.selection() != expected
                        || stage == 7 && document.is_dirty()
                    {
                        return Ok(false);
                    }
                } // 原源借用先结束，才能读取 Scene 或实际状态控件。
                if stage == 3 {
                    let status = surface
                        .controller
                        .ivars()
                        .status_label
                        .get()
                        .ok_or("原替换拒绝缺少状态控件")?;
                    if !status
                        .stringValue()
                        .isEqualToString(ns_string!("请先完成当前输入法组合，再执行查找或替换"))
                    {
                        return Ok(false);
                    }
                    if !self.find_replacement_current(surface, facts, probe, after, probe.intent) {
                        return Err("替换拒绝反馈后原票已撤销".into());
                    }
                }
                if editor.body.is_preparing() {
                    return Ok(false);
                }
                let key = match editor.text_view.capture_ready_scene_key() {
                    Ok(key) => key,
                    Err(crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending) => return Ok(false),
                    Err(cause) => return Err(cause.to_string()),
                };
                let Some((cpu, paints)) = editor.text_view.probe_cpu_published_frame() else {
                    return Ok(false);
                };
                if key.request.input.primary_range != range
                    || !cpu.same(&key)
                    || paints == 0
                    || editor.text_view.cpu_paints() != Some(paints)
                    || editor.text_view.cpu_paint_pending() != Some(false)
                {
                    return Ok(false);
                }
                match editor.text_view.verify_find_target_visible(&key, range) {
                    Ok(_) => {}
                    Err(crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending) => return Ok(false),
                    Err(cause) => return Err(cause.to_string()),
                }
                if !self.find_replacement_current(surface, facts, probe, after, probe.intent) {
                    return Err("真实查找可见证明返回后原票已撤销".into());
                }
                Ok(true)
            })?;
            if ready && probe.action_started.elapsed() <= Duration::from_secs(3) {
                let _ = probe.queued_button.take(); // 只有原严格终点通过才消费前态，后继历史动作没有排队宽限。
                return Ok(());
            }
            if stage <= 4 {
                return Err("Find 原真实终点在后继验证前已撤销".into());
            }
            let remaining = Duration::from_secs(3)
                .checked_sub(probe.action_started.elapsed())
                .ok_or("真实查找组合动作三秒内没有原目标完整 CPU 帧")?;
            // SAFETY: 只等待该次实际按钮动作剩余的原主循环期限；没有任何正文租约跨等待。
            unsafe {
                objc2_core_foundation::CFRunLoop::run_in_mode(
                    objc2_core_foundation::kCFRunLoopDefaultMode,
                    remaining.as_secs_f64(),
                    true,
                )
            };
        }
    }

    /// 每个后继事件核对预声明源和真实 Native 选择，组合候选只核范围及状态，不读取候选文字。
    fn verify_find_replacement_stage(
        &self,
        facts: &AppendFacts,
        probe: &FindReplacementProbeFacts,
        stage: u8,
    ) -> Result<(), String> {
        let (delta, source, selection, dirty, undo, redo) = match stage {
            1 => (0, FIND_REPLACEMENT_BEFORE, probe.first, false, false, false),
            2 | 3 => (0, FIND_REPLACEMENT_BEFORE, probe.last, false, false, false),
            4 => (1, FIND_REPLACEMENT_AFTER, probe.first, true, true, false),
            5 => (2, FIND_REPLACEMENT_BEFORE, probe.last, false, false, true),
            6 => (3, FIND_REPLACEMENT_AFTER, probe.first, true, true, false),
            7 => (3, FIND_REPLACEMENT_AFTER, probe.first, false, true, false),
            _ => return Err("未知查找组合认证阶段".into()),
        }; // 三次字符代次仅来自 Replace、Undo、Redo；前后导航和拒绝不允许字符变化。
        let generation = facts
            .generation
            .checked_add(delta)
            .ok_or("查找组合代次溢出")?;
        let revision = with_active_native_markdown_surface(|surface| {
            let current =
                || self.find_replacement_current(surface, facts, probe, generation, probe.intent);
            if !current() {
                return Err("查找组合原操作票已失效".into());
            }
            let editor = surface.editor.as_ref().ok_or("查找组合原正文缺失")?;
            let view = &editor.text_view;
            if step(&current, || {
                ProtocolObject::<dyn NSTextInputClient>::from_ref(&***view).selectedRange()
            })? != NSRange::new(
                selection.anchor_utf16,
                selection.head_utf16 - selection.anchor_utf16,
            ) {
                return Err("查找组合没有选择预声明的准确 Native 匹配".into());
            }
            let bar = probe.bar.upgrade().ok_or("原查找栏已释放")?;
            let fields = step(&current, || bar.fields())?;
            let actual = step(&current, || fields[1].currentEditor())?;
            if stage <= 4 {
                let actual = actual
                    .ok_or("导航或替换辞任了原替换字段")?
                    .downcast::<NSTextView>()
                    .map_err(|_| "实际替换 editor 类型错误")?;
                let original =
                    step(&current, || probe.field_editor.load())?.ok_or("原替换 editor 已释放")?;
                // 实际鼠标 tracking 返回后，字段仍声明 currentEditor 不等于窗口仍把键盘交给它。
                // 只核同一原 Input/Doc/意图下的公开 firstResponder，不补设焦点或重发按钮。
                let responder = step(&current, || surface.panel.firstResponder())?
                    .ok_or("查找按钮结束后窗口没有实际输入响应者")?;
                if std::ptr::from_ref(&*responder).addr() != std::ptr::from_ref(&*actual).addr() {
                    return Err("查找按钮结束后原替换字段失去真实键盘焦点".into());
                }
                let marked = stage <= 3;
                if !std::ptr::eq(&*actual, &*original)
                    || step(&current, || actual.hasMarkedText())? != marked
                    || step(&current, || actual.selectedRange())?
                        != NSRange::new(if marked { 1 } else { 2 }, 0)
                    || (marked && step(&current, || actual.markedRange())? != NSRange::new(0, 1))
                {
                    return Err("导航/拒绝改变了实际替换候选、局部选择或原客户".into());
                }
                if !marked {
                    let value = step(&current, || actual.string())?;
                    if !step(&current, || value.isEqualToString(ns_string!("中文")))? {
                        return Err("实际提交的替换词不准确".into());
                    }
                } // marked 阶段不读取 string 或 control.stringValue。
            } else if actual.is_some() {
                return Err("正文历史阶段没有正常辞任替换客户".into());
            }
            // 原表头和末格各有一个“值”；实际Next/Previous必须显示对应完整源次序及真实总数。
            if matches!(stage, 1 | 2)
                && !step(&current, || {
                    bar.probe_numbered_feedback(usize::from(stage), 2)
                })?
            {
                return Err("真实导航尚未完成原匹配可见定位".into());
            }
            if stage == 3 {
                let status = surface
                    .controller
                    .ivars()
                    .status_label
                    .get()
                    .ok_or("拒绝缺少真实状态控件")?;
                let value = step(&current, || status.stringValue())?;
                if !step(&current, || {
                    value.isEqualToString(ns_string!("请先完成当前输入法组合，再执行查找或替换"))
                })? {
                    return Err("实际替换按钮没有报告原组合输入拒绝".into());
                }
            }
            drop(fields);
            if !current() {
                return Err("归还字段临时值后原查找票已失效".into());
            }
            let document = editor.document().borrow();
            if document.byte_len() != source.len()
                || document.utf16_len() != source.encode_utf16().count()
                || document.selection() != selection
                || document.is_dirty() != dirty
                || document.can_undo() != undo
                || document.can_redo() != redo
            {
                return Err("查找组合的准确源、选择或历史状态不符".into());
            }
            for (index, expected) in source.encode_utf16().enumerate() {
                if document
                    .utf16_code_unit_at(index)
                    .map_err(|cause| cause.to_string())?
                    != expected
                {
                    return Err("查找替换改动了预声明范围之外的固定源".into());
                }
            } // 只核这张固定小表的每个真实 unit，不建立实际正文副本。
            document
                .current_revision()
                .map_err(|cause| cause.to_string())
        })?;
        if matches!(stage, 1..=3 | 5) && revision != facts.baseline
            || matches!(stage, 6 | 7) && Some(revision) != facts.after
        {
            return Err("查找组合 Undo/Redo 没有恢复原完整根".into());
        }
        Self::verify_conflict_file(
            &self.path,
            if stage == 7 {
                FIND_REPLACEMENT_AFTER
            } else {
                FIND_REPLACEMENT_BEFORE
            },
        )?;
        if stage == 4 {
            let mut updated = facts.clone();
            updated.after = Some(revision); // 准确字节、范围和唯一 Undo 全部已核后，才保存真实已发布根供 Redo/保存比较。
            updated.after_selection = Some(probe.first);
            self.facts.replace(Some(updated));
        }
        Ok(())
    }

    /// 后继阶段仅执行下一实际动作；没有成功返回值替代下一事件对正文、候选和历史的认证。
    pub(crate) fn advance_find_replacement_composition(&self, stage: u8) -> Result<(), String> {
        let facts = self.facts()?;
        let mut probe = self
            .find_replacement
            .borrow_mut()
            .take()
            .ok_or("查找组合原票缺失")?;
        let result = (|| {
            self.wait_find_replacement(&facts, &mut probe, stage)?;
            self.verify_find_replacement_stage(&facts, &probe, stage)?;
            match stage {
                1 => self.click_find_replacement_button(
                    &facts,
                    &mut probe,
                    MarkdownFindAction::Previous,
                    NativeMarkdownHostedTableStep::VerifyFindPreviousAndRejectReplace,
                ),
                2 => self.click_find_replacement_button(
                    &facts,
                    &mut probe,
                    MarkdownFindAction::ReplaceNext,
                    NativeMarkdownHostedTableStep::VerifyReplaceRejectedAndCommit,
                ),
                3 => {
                    with_active_native_markdown_surface(|surface| {
                        let current = || {
                            self.find_replacement_current(
                                surface,
                                &facts,
                                &probe,
                                facts.generation,
                                probe.intent,
                            )
                        };
                        let actual = step(&current, || probe.field_editor.load())?
                            .ok_or("原替换 editor 已释放")?;
                        let value = step(&current, || NSString::from_str("中文"))?;
                        let next = probe.intent.checked_add(1).ok_or("替换确认意图溢出")?;
                        // SAFETY: 原字段仍持有0..1真实marked；用公开insert确认，不手工unmark或改写control值。
                        unsafe {
                            ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual)
                                .insertText_replacementRange(&value, NSRange::new(0, 1));
                        }
                        if !self.current(surface, &facts, facts.generation)
                            || surface.controller.find_input_intent() != next
                        {
                            return Err("真实替换词确认没有产生预声明后态".into());
                        }
                        drop((value, actual));
                        probe.intent = next;
                        Ok(())
                    })?;
                    self.click_find_replacement_button(
                        &facts,
                        &mut probe,
                        MarkdownFindAction::ReplaceNext,
                        NativeMarkdownHostedTableStep::VerifyReplacedAndUndo,
                    )
                }
                4 => {
                    with_active_native_markdown_surface(|surface| {
                        let generation = facts.generation.checked_add(1).ok_or("替换代次溢出")?;
                        let current = || {
                            self.find_replacement_current(
                                surface,
                                &facts,
                                &probe,
                                generation,
                                probe.intent,
                            )
                        };
                        let view = &surface.editor.as_ref().ok_or("原正文缺失")?.text_view;
                        if !current() || !surface.panel.makeFirstResponder(Some(&**view)) {
                            return Err("提交后替换字段拒绝正常辞任".into());
                        }
                        let after = || {
                            self.current(surface, &facts, generation)
                                && surface.controller.find_input_intent() == probe.intent
                        };
                        let bar = probe.bar.upgrade().ok_or("原查找栏已释放")?;
                        let fields = step(&after, || bar.fields())?;
                        if step(&after, || fields[1].currentEditor())?.is_some() {
                            return Err("正常辞任没有结束原替换客户".into());
                        } // 原路由应在实际成功交接时变化，不能用旧路由检查拒绝合法辞任。
                        Ok(())
                    })?;
                    probe.route = None; // 真实辞任成功后才结束字段路由票；正文 Undo 仍保原栏、Doc 和准确意图。
                    probe.action_started = Instant::now();
                    self.click_toolbar_button(1, 22)
                }
                5 => {
                    probe.action_started = Instant::now();
                    self.click_toolbar_button(2, 23)
                }
                6 => {
                    probe.action_started = Instant::now();
                    with_active_native_markdown_surface(|surface| {
                        let generation = facts.generation.checked_add(3).ok_or("Redo代次溢出")?;
                        if !self.find_replacement_current(
                            surface,
                            &facts,
                            &probe,
                            generation,
                            probe.intent,
                        ) {
                            return Err("查找组合保存前原票已撤销".into());
                        }
                        surface.save_current_document() // 只用正常保存，不直接写测试期望到磁盘。
                    })
                }
                7 => {
                    println!(
                        "native_find_replacement_composition actual_next_previous_buttons=true marked_preserved=true actual_replace_rejected=true actual_commit_replace=true canonical_undo_redo_exact=true production_save_exact=true physical_ime=false formal=false"
                    );
                    Ok(())
                }
                _ => Err("未知查找组合动作阶段".into()),
            }
        })();
        self.find_replacement.replace(Some(probe)); // 错误也归还原弱状态，仍走唯一真实 Return/strict-close。
        result
    }

    /// 已知原NSAlert装配完成后才登记一次modal-mode动作；不查找或遍历系统窗口子树。
    pub(in crate::native_surface) fn observe_conflict_modal_ready(
        alert: &NSAlert,
        window: &objc2_app_kit::NSWindow,
        field: Option<&NSTextField>,
    ) {
        let operation = CONFLICT_OPERATION.with_borrow(std::rc::Weak::upgrade);
        let Some(operation) = operation.filter(|operation| {
            operation.active.get()
                && !operation.delivered.get()
                && operation.action != ConflictProbeAction::ReturnOriginal
        }) else {
            return; // 普通产品对话框和未武装验收不产生回调或输入。
        };
        if operation.modal_seen.replace(true) {
            *operation.modal_result.borrow_mut() =
                Some(Err("同一合成冲突操作重复打开对话框".into()));
            return;
        }
        let expected_copy = operation.action == ConflictProbeAction::CopyDraft;
        let titles = if expected_copy {
            ("保留本地草稿", "另存为新笔记")
        } else {
            ("重新加载磁盘版本？", "放弃草稿并重新加载")
        };
        let buttons = alert.buttons();
        if buttons.count() != 2 {
            *operation.modal_result.borrow_mut() =
                Some(Err("实际冲突对话框缺少原确认/取消按钮".into()));
            return; // 不点击身份未知的按钮，更不能为验收伪造NSAlert返回码。
        }
        let confirm = buttons.objectAtIndex(0);
        let cancel = buttons.objectAtIndex(1);
        let prompt_matches = operation.original.current()
            && alert
                .messageText()
                .isEqualToString(&NSString::from_str(titles.0))
            && confirm
                .title()
                .isEqualToString(&NSString::from_str(titles.1))
            && cancel.title().isEqualToString(ns_string!("取消"))
            && field.is_some() == expected_copy;
        let mtm = window.mtm();
        let controls = MainThreadBound::new(
            (
                Weak::from(window),
                Weak::from_retained(&confirm),
                Weak::from_retained(&cancel),
                field.map(Weak::from),
                Rc::clone(&operation),
            ),
            mtm,
        );
        let callback = block2::RcBlock::new(move || {
            let mtm = MainThreadMarker::new().expect("原modal run loop位于主线程");
            let (window, confirm, cancel, field, operation) = controls.get(mtm);
            if !operation.active.get() {
                return; // 失败清理已撤权的旧block不能点击另一张后来出现的对话框。
            }
            let result = (|| {
                if !prompt_matches || !operation.original.current() {
                    return Err("真实冲突对话框文案、输入类型或原票不符".into());
                }
                let window = window.load().ok_or("原冲突对话框已经释放")?;
                let actual = NSApplication::sharedApplication(mtm)
                    .modalWindow()
                    .ok_or("原NSAlert尚未进入真实modal会话")?;
                if !std::ptr::eq(&*actual, &*window) {
                    return Err("modal输入目标不是本次实际NSAlert".into());
                }
                drop(actual);
                if let Some(field) = field {
                    // 名称仍由原独占字段/provider处理；真实输入先替换默认标题，再由原确认按钮辞任。
                    let field = field.load().ok_or("原名称字段已经释放")?;
                    if !window.makeFirstResponder(Some(&field)) {
                        return Err("原名称字段拒绝真实焦点".into());
                    }
                    let editor = field
                        .currentEditor()
                        .ok_or("原名称字段没有真实field editor")?
                        .downcast::<NSTextView>()
                        .map_err(|_| "原名称字段editor类型不同")?;
                    let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
                    if input.hasMarkedText() {
                        return Err("合成名称字段意外包含未完成组合".into());
                    }
                    let length = editor.string().length();
                    // SAFETY: 标准NSTextInputClient只替换这个已明确武装、有限名称字段的完整默认值。
                    unsafe {
                        input.insertText_replacementRange(
                            &NSString::from_str(CONFLICT_COPY_TITLE),
                            NSRange::new(0, length),
                        );
                    }
                    if !editor
                        .string()
                        .isEqualToString(&NSString::from_str(CONFLICT_COPY_TITLE))
                    {
                        return Err("真实名称输入没有产生预声明的有限标题".into());
                    }
                }
                Ok(())
            })();
            let use_cancel =
                result.is_err() || operation.action == ConflictProbeAction::CancelReload;
            *operation.modal_result.borrow_mut() = Some(result);
            let button = if use_cancel { cancel } else { confirm };
            let events = (|| {
                let window = window.load().ok_or("原冲突对话框已释放")?;
                let button = button.load().ok_or("原对话框按钮已释放")?;
                let current = || {
                    operation.active.get()
                        && NSApplication::sharedApplication(mtm)
                            .modalWindow()
                            .is_some_and(|actual| std::ptr::eq(&*actual, &*window))
                        && (use_cancel
                            || (operation.original.current()
                                && with_active_native_markdown_surface(|surface| {
                                    let editor =
                                        surface.editor.as_ref().ok_or("原modal正文已释放")?;
                                    Ok(editor
                                        .text_view
                                        .input_state()
                                        .map_err(|cause| cause.to_string())?
                                        == operation.input)
                                })
                                .unwrap_or(false)))
                };
                // 失败只能取消这一个仍在真实modal中的旧窗口；确认还必须保持原Doc完整Input。
                // 先准备完整鼠标对，所有控件/窗口临时claim退栈后才入原NSApp队列。
                button_mouse::prepare(&button, &window, &current)
            })();
            match events {
                Ok(events) => button_mouse::post(events, mtm),
                Err(cause) => *operation.modal_result.borrow_mut() = Some(Err(cause)),
            }
            let completion = MainThreadBound::new(Rc::clone(operation), mtm);
            enqueue_native_markdown_main_task(move || {
                let mtm = MainThreadMarker::new().expect("原modal栈退回后沿主队列交付");
                let operation = completion.get(mtm);
                if NSApplication::sharedApplication(mtm)
                    .modalWindow()
                    .is_some()
                {
                    return; // 鼠标尚未被原modal消费时不能把排队当作实际取消；原超时仍报告失败。
                }
                if operation.action == ConflictProbeAction::CancelReload
                    || operation
                        .modal_result
                        .borrow()
                        .as_ref()
                        .is_some_and(Result::is_err)
                {
                    operation.deliver();
                } // 复制/重载成功只能等下面真实安装通知；队列退回不足以证明旧Doc释放。
            });
        });
        let run_loop = objc2_core_foundation::CFRunLoop::main().expect("验收已有AppKit主run loop");
        // SAFETY: 框架导出的NSString模式常量可无拷贝桥接为CFString；主run loop复制一次heap block。
        // block仅借主线程封装的Weak和原合成票，不在serial dispatch block中等待后续dispatch任务。
        unsafe {
            let mode: &CFString = objc2_app_kit::NSModalPanelRunLoopMode.as_ref();
            run_loop.perform_block(Some(mode.as_ref()), Some(&callback));
        }
        run_loop.wake_up();
    }

    /// 原生产finish_note_switch成功后调用；同时要求旧Doc已真实消失和新路径已安装。
    pub(in crate::native_surface) fn observe_conflict_note_installed() {
        let operation = CONFLICT_OPERATION.with_borrow(std::rc::Weak::upgrade);
        let Some(operation) = operation.filter(|operation| {
            operation.active.get()
                && !operation.delivered.get()
                && operation.action != ConflictProbeAction::CancelReload
        }) else {
            return;
        };
        let controller = operation.controller.load();
        let installed = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            active.as_ref().is_some_and(|surface| {
                controller
                    .as_ref()
                    .is_some_and(|controller| std::ptr::eq(&**controller, &*surface.controller))
                    && surface.pending_note_switch.is_none()
                    && surface.current_note_relative_path.as_deref()
                        == Some(operation.destination.as_str())
                    && surface.editor.is_some()
                    && operation.original.document.strong_count() == 0
            })
        });
        drop(controller); // 强Native临时值先归还，后继只投原driver事件。
        if installed {
            operation.deliver();
        }
    }

    /// 每个真实动作只武装一次原票；上一个终点被消费后才允许下一个动作。
    fn arm_conflict_operation(
        &self,
        action: ConflictProbeAction,
        destination: &str,
        expected_terminal: NativeMarkdownHostedTableStep,
    ) -> Result<(), String> {
        if self.conflict_operation.borrow().is_some() {
            return Err("上一冲突操作的实际终点尚未消费".into());
        }
        let (original, controller, input) = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("冲突动作没有实际原文稿")?;
            let path = surface
                .current_note_relative_path
                .as_deref()
                .ok_or("冲突动作没有实际路径")?;
            let original = NativeMarkdownCurrentNoteIntent::for_document(
                &surface.controller,
                &surface.panel,
                editor.document(),
                path,
            );
            let input = editor
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            Ok((original, Weak::from_retained(&surface.controller), input))
        })?;
        let operation = Rc::new(ConflictProbeOperation {
            controller,
            original,
            input,
            action,
            destination: destination.to_owned(),
            expected_terminal,
            signal: RefCell::new(None),
            active: Cell::new(true),
            modal_seen: Cell::new(false),
            delivered: Cell::new(false),
            modal_result: RefCell::new(None),
        });
        CONFLICT_OPERATION.with_borrow_mut(|slot| *slot = Rc::downgrade(&operation));
        self.conflict_operation.replace(Some(operation));
        Ok(())
    }

    /// 消费真实完成收据，不能把新路径的某个后来状态补签为旧操作成功。
    fn consume_conflict_operation(
        &self,
        expected_terminal: NativeMarkdownHostedTableStep,
    ) -> Result<(), String> {
        let operation = self
            .conflict_operation
            .borrow_mut()
            .take()
            .ok_or("缺少原冲突操作收据")?;
        operation.active.set(false);
        CONFLICT_OPERATION.with_borrow_mut(|slot| *slot = std::rc::Weak::new());
        if operation.expected_terminal != expected_terminal || !operation.delivered.get() {
            return Err("冲突实际终点不是预声明阶段".into());
        }
        if operation.action != ConflictProbeAction::ReturnOriginal {
            operation
                .modal_result
                .borrow()
                .clone()
                .ok_or("真实NSAlert输入没有执行")??;
        }
        Ok(())
    }

    /// 517只使用原字段历史已经正式保存的短夹具；独立写磁盘后必须走真实保存冲突。
    pub(crate) fn begin_conflict_bar_case(&self) -> Result<(), String> {
        let facts = self.facts()?;
        let conflict = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("冲突前没有原文稿")?;
            let document = editor.document().borrow();
            let snapshot = ConflictDocumentSnapshot::capture(&document)?;
            if self.case != NativeMarkdownHostedTableCase::ToolbarFieldHistory
                || !WeakMarkdownDocument::ptr_eq(&facts.document, &Rc::downgrade(editor.document()))
                || Some(snapshot.revision) != facts.after
                || Some(snapshot.selection) != facts.after_selection
                || snapshot.dirty
                || !snapshot.undo
                || snapshot.redo
            {
                return Err("原字段历史与生产保存没有全部通过，不能开始冲突用例".into());
            }
            Self::verify_conflict_text(&document, CONFLICT_DRAFT)?;
            drop(document);
            let original = NativeMarkdownCurrentNoteIntent::for_document(
                &surface.controller,
                &surface.panel,
                editor.document(),
                &self.relative_path,
            );
            let bar = surface.controller.ivars().conflict_bar.get().unwrap();
            Ok(ConflictProbeFacts {
                original,
                publication: surface.controller.conflict_publication(),
                snapshot,
                current_document: Rc::downgrade(editor.document()),
                current_path: self.relative_path.clone(),
                current_text: CONFLICT_DRAFT,
                original_disk_text: CONFLICT_DRAFT,
                bar: Weak::from(bar.probe_controls().0),
                completed: false,
            })
        })?;
        self.conflict.replace(Some(conflict)); // 先记有限夹具事实，失败也只能对这一自有原票做清理。
        std::fs::write(&self.path, CONFLICT_EXTERNAL).map_err(|cause| cause.to_string())?;
        self.conflict
            .borrow_mut()
            .as_mut()
            .unwrap()
            .original_disk_text = CONFLICT_EXTERNAL;
        with_active_native_markdown_surface(|surface| {
            if surface.save_current_document().is_ok() {
                return Err("真实外部覆写没有触发生产保存冲突".into());
            }
            self.conflict.borrow_mut().as_mut().unwrap().publication =
                surface.controller.conflict_publication();
            Ok(())
        })?;
        self.verify_conflict_source_unchanged()?;
        self.verify_conflict_geometry()?;
        Self::verify_conflict_file(&self.path, CONFLICT_EXTERNAL)?;
        self.arm_conflict_operation(
            ConflictProbeAction::CancelReload,
            &self.relative_path,
            NativeMarkdownHostedTableStep::VerifyConflictCancelAndCopy,
        )?;
        self.click_conflict_button(4)
    }

    /// 518只接受真实取消；原字节/选择/Undo不动后再点击原另存按钮。
    pub(crate) fn verify_conflict_cancel_and_copy(&self) -> Result<(), String> {
        self.consume_conflict_operation(
            NativeMarkdownHostedTableStep::VerifyConflictCancelAndCopy,
        )?;
        self.verify_conflict_source_unchanged()?;
        Self::verify_conflict_file(&self.path, CONFLICT_EXTERNAL)?;
        let copy = self.path.parent().unwrap().join(CONFLICT_COPY_PATH);
        match std::fs::metadata(&copy) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => return Err("取消重载意外产生了另存文件".into()),
            Err(error) => return Err(error.to_string()),
        }
        println!(
            "native_conflict_cancel actual_alert_cancel=true disk_unchanged=true canonical_selection_history_unchanged=true formal=false"
        );
        self.arm_conflict_operation(
            ConflictProbeAction::CopyDraft,
            CONFLICT_COPY_PATH,
            NativeMarkdownHostedTableStep::VerifyConflictCopyAndReturn,
        )?;
        self.click_conflict_button(5)
    }

    /// 519由真实新笔记安装交付；副本必须保存原草稿，外部版本不能被覆盖。
    pub(crate) fn verify_conflict_copy_and_return(&self) -> Result<(), String> {
        self.consume_conflict_operation(
            NativeMarkdownHostedTableStep::VerifyConflictCopyAndReturn,
        )?;
        let original = self.conflict.borrow().as_ref().unwrap().original.clone();
        if original.document.strong_count() != 0 {
            return Err("另存安装新笔记后原Doc仍活".into());
        }
        self.accept_conflict_document(CONFLICT_COPY_PATH, CONFLICT_DRAFT)?;
        Self::verify_conflict_file(&self.path, CONFLICT_EXTERNAL)?;
        Self::verify_conflict_file(
            &self.path.parent().unwrap().join(CONFLICT_COPY_PATH),
            CONFLICT_DRAFT,
        )?;
        println!(
            "native_conflict_copy actual_name_field_input=true actual_alert_confirm=true exact_copy=true original_external_bytes_preserved=true original_document_released=true formal=false"
        );
        self.arm_conflict_operation(
            ConflictProbeAction::ReturnOriginal,
            &self.relative_path,
            NativeMarkdownHostedTableStep::VerifyConflictOriginalAndReload,
        )?;
        switch_native_markdown_note(&self.relative_path, None)
    }

    /// 520已真实回到同名A2；旧A票必须因Doc身份失效，随后才允许这次A2真实重载。
    pub(crate) fn verify_conflict_aba_and_reload(&self) -> Result<(), String> {
        self.consume_conflict_operation(
            NativeMarkdownHostedTableStep::VerifyConflictOriginalAndReload,
        )?;
        let (original, publication, copy_document) = {
            let conflict = self.conflict.borrow();
            let conflict = conflict.as_ref().unwrap();
            (
                conflict.original.clone(),
                conflict.publication,
                conflict.current_document.clone(),
            )
        };
        if copy_document.strong_count() != 0 {
            return Err("从副本切回原路径后副本Doc仍活".into());
        }
        self.accept_conflict_document(&self.relative_path, CONFLICT_EXTERNAL)?;
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().unwrap();
            if original.relative_path != surface.current_note_relative_path.as_deref().unwrap()
                || WeakMarkdownDocument::ptr_eq(
                    &original.document,
                    &Rc::downgrade(editor.document()),
                )
                || original.document.strong_count() != 0
            {
                return Err("ABA没有采用同一路径与新的实际Doc身份".into());
            }
            Ok(())
        })?;
        if original.current() {
            return Err("同名A2意外恢复了旧A操作票".into());
        }
        original.perform_resolution(4, publication)?; // 直接消费原旧票，不能从现在A2重新捕获权限。
        self.verify_conflict_source_unchanged()?;
        println!(
            "native_conflict_aba same_relative_path=true different_document_identity=true old_document_released=true old_ticket_rejected=true formal=false"
        );
        std::fs::write(&self.path, CONFLICT_RELOADED).map_err(|cause| cause.to_string())?;
        self.conflict
            .borrow_mut()
            .as_mut()
            .unwrap()
            .original_disk_text = CONFLICT_RELOADED;
        with_active_native_markdown_surface(|surface| {
            if surface.save_current_document().is_ok() {
                return Err("第二次真实外部修改没有触发生产保存冲突".into());
            }
            Ok(())
        })?;
        self.arm_conflict_operation(
            ConflictProbeAction::ConfirmReload,
            &self.relative_path,
            NativeMarkdownHostedTableStep::VerifyConflictReload,
        )?;
        self.click_conflict_button(4)
    }

    /// 521只在真实重载安装后检查；最后的原生产Return仍须再通过保存和完整strict释放。
    pub(crate) fn verify_conflict_reload(&self) -> Result<(), String> {
        self.consume_conflict_operation(NativeMarkdownHostedTableStep::VerifyConflictReload)?;
        let previous = self
            .conflict
            .borrow()
            .as_ref()
            .unwrap()
            .current_document
            .clone();
        if previous.strong_count() != 0 {
            return Err("真实重载后被放弃的A2 Doc仍活".into());
        }
        self.accept_conflict_document(&self.relative_path, CONFLICT_RELOADED)?;
        Self::verify_conflict_file(&self.path, CONFLICT_RELOADED)?;
        Self::verify_conflict_file(
            &self.path.parent().unwrap().join(CONFLICT_COPY_PATH),
            CONFLICT_DRAFT,
        )?;
        with_active_native_markdown_surface(|surface| {
            if !surface
                .controller
                .ivars()
                .conflict_bar
                .get()
                .unwrap()
                .probe_controls()
                .0
                .isHidden()
            {
                return Err("成功重载后旧冲突条仍显示".into());
            }
            surface.save_current_document()
        })?;
        self.verify_conflict_source_unchanged()?;
        self.conflict.borrow_mut().as_mut().unwrap().completed = true;
        println!(
            "native_conflict_reload actual_alert_confirm=true actual_handoff=true exact_external_source=true old_document_released=true formal=false"
        );
        Ok(())
    }

    /// 只按固定合成文本逐UTF16比较真实canonical；不构造实际正文副本。
    fn verify_conflict_text(document: &MarkdownDocumentBuffer, text: &str) -> Result<(), String> {
        if document.byte_len() != text.len() || document.utf16_len() != text.encode_utf16().count()
        {
            return Err("冲突处理后的实际正文长度不同".into());
        }
        for (offset, unit) in text.encode_utf16().enumerate() {
            if document
                .utf16_code_unit_at(offset)
                .map_err(|cause| cause.to_string())?
                != unit
            {
                return Err("冲突处理改变了预声明范围以外的实际源".into());
            }
        }
        Ok(())
    }

    /// 只读自有短文件的明确字节与EOF；FD在返回前关闭，不成为产品资源观察者。
    fn verify_conflict_file(path: &Path, text: &str) -> Result<(), String> {
        let mut file = std::fs::File::open(path).map_err(|cause| cause.to_string())?;
        let mut bytes = [0_u8; 256];
        std::io::Read::read_exact(&mut file, &mut bytes[..text.len()])
            .map_err(|cause| cause.to_string())?;
        let mut tail = [0_u8; 1];
        if &bytes[..text.len()] != text.as_bytes()
            || std::io::Read::read(&mut file, &mut tail).map_err(|cause| cause.to_string())? != 0
        {
            return Err("冲突流程的磁盘字节或EOF不符".into());
        }
        Ok(())
    }

    /// 认证真实新Doc再更新观察事实；没有重签任何生产操作票。
    fn accept_conflict_document(&self, path: &str, text: &'static str) -> Result<(), String> {
        let (document, snapshot) = with_active_native_markdown_surface(|surface| {
            if surface.current_note_relative_path.as_deref() != Some(path)
                || surface.pending_note_switch.is_some()
            {
                return Err("真实笔记安装的目标或完成状态不符".into());
            }
            let editor = surface.editor.as_ref().ok_or("真实安装没有原生编辑器")?;
            let document = editor.document().borrow();
            Self::verify_conflict_text(&document, text)?;
            let snapshot = ConflictDocumentSnapshot::capture(&document)?;
            if snapshot.dirty || snapshot.undo || snapshot.redo {
                return Err("新安装Doc继承了旧草稿或历史".into());
            }
            Ok((Rc::downgrade(editor.document()), snapshot))
        })?;
        let mut conflict = self.conflict.borrow_mut();
        let conflict = conflict.as_mut().unwrap();
        conflict.current_document = document;
        conflict.snapshot = snapshot;
        conflict.current_path = path.to_owned();
        conflict.current_text = text;
        Ok(())
    }

    /// 取消、Find和缩放都必须保持原根、选区、dirty与Undo/Redo；比较真实正文Native选区。
    fn verify_conflict_source_unchanged(&self) -> Result<(), String> {
        let (document, snapshot, path, text) = {
            let conflict = self.conflict.borrow();
            let conflict = conflict.as_ref().unwrap();
            (
                conflict.current_document.clone(),
                conflict.snapshot,
                conflict.current_path.clone(),
                conflict.current_text,
            )
        };
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("冲突操作丢失原编辑器")?;
            if surface.current_note_relative_path.as_deref() != Some(path.as_str())
                || !WeakMarkdownDocument::ptr_eq(&document, &Rc::downgrade(editor.document()))
                || ConflictDocumentSnapshot::capture(&editor.document().borrow())? != snapshot
            {
                return Err("冲突取消或只读交互改变了原Doc、选择或历史".into());
            }
            Self::verify_conflict_text(&editor.document().borrow(), text)?;
            let start = snapshot
                .selection
                .anchor_utf16
                .min(snapshot.selection.head_utf16);
            let end = snapshot
                .selection
                .anchor_utf16
                .max(snapshot.selection.head_utf16);
            if ProtocolObject::<dyn NSTextInputClient>::from_ref(&**editor.text_view)
                .selectedRange()
                != NSRange::new(start, end - start)
            {
                return Err("冲突交互改变了真实正文选区".into());
            }
            Ok(())
        })
    }

    /// 始终点击这条实际安装的按钮；不调用perform_resolution代替用户入口。
    fn click_conflict_button(&self, tag: isize) -> Result<(), String> {
        let operation = self
            .conflict_operation
            .borrow()
            .clone()
            .ok_or("原冲突操作未武装")?;
        let owner = operation
            .controller
            .load()
            .ok_or("原冲突controller已释放")?;
        let owner_identity = std::ptr::from_ref(&*owner).addr();
        drop(owner); // 只留下原controller标量；真实Native临时claim不跨ACTIVE租约。
        let (events, mtm) = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("原冲突正文已释放")?;
            let before = editor
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            let body = Rc::downgrade(&editor.body);
            let publication = surface.controller.conflict_publication();
            let current = || {
                operation.active.get()
                    && !operation.delivered.get()
                    && std::ptr::from_ref(&*surface.controller).addr() == owner_identity
                    && native_markdown_controller_has_foreground_authority(&surface.controller)
                    && WeakMarkdownDocument::ptr_eq(
                        &operation.original.document,
                        &Rc::downgrade(editor.document()),
                    )
                    && std::rc::Weak::ptr_eq(&body, &Rc::downgrade(&editor.body))
                    && surface.controller.conflict_publication() == publication
                    && editor
                        .text_view
                        .input_state()
                        .is_ok_and(|actual| actual == before)
            };
            let (_, message, buttons) = surface
                .controller
                .ivars()
                .conflict_bar
                .get()
                .unwrap()
                .probe_controls();
            if !step(&current, || message.stringValue())?.isEqualToString(ns_string!(
                "检测到磁盘版本变化，本地草稿已保留。请选择处理方式。"
            )) || !current()
            {
                return Err("真实保存结果没有发布准确中文冲突提示".into());
            }
            let button = &buttons[if tag == 4 { 0 } else { 1 }];
            if step(&current, || button.tag())? != tag
                || !step(&current, || button.refusesFirstResponder())?
            {
                return Err("实际冲突处理按钮不可用或夺取输入焦点".into());
            }
            let events = button_mouse::prepare(button, &surface.panel, &current)?;
            Ok((events, surface.panel.mtm()))
        })?;
        button_mouse::post(events, mtm); // 动作结果仍由原modal/新Doc安装终点交付，入队不等于保存成功。
        Ok(())
    }

    /// 最窄窗口叠加真实Find/Replace后，只比较实际控件frame；不调用生产几何函数签发结果。
    fn verify_conflict_geometry(&self) -> Result<(), String> {
        let panel = with_active_native_markdown_surface(|surface| Ok(surface.panel.clone()))?;
        let original = panel
            .contentView()
            .ok_or("冲突窗口缺少内容根")?
            .frame()
            .size;
        show_native_markdown_find_bar(true)?;
        let checked = (|| {
            panel.setContentSize(NSSize::new(900.0, 600.0)); // 原windowDidResize负责布局，不由验收补调用布局实现。
            panel.displayIfNeeded();
            self.verify_conflict_source_unchanged()?;
            with_active_native_markdown_surface(|surface| {
                let editor = surface.editor.as_ref().ok_or("最窄窗口丢失原文稿")?;
                let content = surface
                    .panel
                    .contentView()
                    .ok_or("最窄窗口缺少实际内容根")?;
                let size = content.frame().size;
                if size != NSSize::new(900.0, 600.0) {
                    return Err("窗口没有采用原最窄内容尺寸".into());
                }
                let (root, message, buttons) = surface
                    .controller
                    .ivars()
                    .conflict_bar
                    .get()
                    .unwrap()
                    .probe_controls();
                let frame = root.frame();
                let old_root = self.conflict.borrow().as_ref().unwrap().bar.clone();
                let actual_root = old_root.load().ok_or("原冲突条已释放")?;
                if !std::ptr::eq(&*actual_root, root)
                    || root.isHidden()
                    || !root
                        .accessibilityLabel()
                        .is_some_and(|label| label.isEqualToString(ns_string!("笔记冲突处理")))
                    || !message.stringValue().isEqualToString(ns_string!(
                        "检测到磁盘版本变化，本地草稿已保留。请选择处理方式。"
                    ))
                {
                    return Err("真实冲突条身份、中文提示或AX不符".into());
                }
                drop(actual_root);
                let message_frame = message.frame();
                let mut parts = vec![message_frame];
                for (button, title) in buttons
                    .iter()
                    .zip(["重新加载磁盘版本", "将本地草稿另存为新笔记"])
                {
                    if !button.title().isEqualToString(&NSString::from_str(title))
                        || !button
                            .accessibilityLabel()
                            .is_some_and(|label| label.isEqualToString(&NSString::from_str(title)))
                        || !button.isEnabled()
                        || !button.refusesFirstResponder()
                        || button.isHidden()
                    {
                        return Err("原两个直达按钮的中文标题、可用性或焦点规则不同".into());
                    }
                    parts.push(button.frame());
                }
                for (index, part) in parts.iter().enumerate() {
                    if part.origin.x < 0.0
                        || part.origin.y < 0.0
                        || part.size.width <= 0.0
                        || part.size.height <= 0.0
                        || part.origin.x + part.size.width > frame.size.width
                        || part.origin.y + part.size.height > frame.size.height
                        || parts[index + 1..]
                            .iter()
                            .any(|other| NSIntersectsRect(*part, *other))
                    {
                        return Err("最窄窗口冲突文字与两个按钮裁切或相互重叠".into());
                    }
                }
                let find = editor
                    .find_bar
                    .borrow()
                    .clone()
                    .ok_or("冲突验收没有真实Find栏")?;
                let find = find
                    .visible_frame_for_probe()
                    .ok_or("最窄窗口隐藏了实际Find栏")?;
                let body = editor.scroll_view().frame();
                let footers = surface
                    .controller
                    .ivars()
                    .footer_labels
                    .get()
                    .unwrap()
                    .each_ref()
                    .map(|label| label.frame());
                let status = surface.controller.ivars().status_label.get().unwrap();
                let feedback = (!status.isHidden()).then(|| status.frame());
                let controls = surface.controller.ivars().workspace_controls.get().unwrap();
                let format = controls[NATIVE_MARKDOWN_TOOLBAR_START + 5]
                    .enclosingScrollView()
                    .ok_or("实际格式栏缺少滚动视口")?
                    .frame();
                let mut regions = vec![frame, find, body, format];
                regions.extend(footers);
                regions.extend(feedback);
                for (index, region) in regions.iter().enumerate() {
                    if region.origin.x < 0.0
                        || region.origin.y < 0.0
                        || region.size.width <= 0.0
                        || region.size.height <= 0.0
                        || region.origin.x + region.size.width > size.width
                        || region.origin.y + region.size.height > size.height
                        || regions[index + 1..]
                            .iter()
                            .any(|other| NSIntersectsRect(*region, *other))
                    {
                        return Err("最窄窗口的冲突条、Find、正文、反馈或底栏互相遮挡".into());
                    }
                }
                Ok(())
            })
        })();
        let hidden = perform_native_markdown_find_action(MarkdownFindAction::Hide);
        panel.setContentSize(original);
        panel.displayIfNeeded();
        drop(panel); // 不跨对话框或最终关闭保留原窗口强引用。
        checked?;
        hidden?;
        self.verify_conflict_source_unchanged()?;
        println!(
            "native_conflict_geometry minimum=900x600 actual_find_replace=true actual_native_frames_nonoverlapping=true chinese_buttons=true source_selection_history_unchanged=true formal=false"
        );
        Ok(())
    }

    /// 只观察原信息卡的具体已知控件，不遍历AppKit子树、构造影子控件或以对象数量判通过。
    fn verify_information_card(&self, facts: &AppendFacts, visible: bool) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, facts, facts.generation);
            if !current() {
                return Err("信息卡观察失去原文稿权限".into());
            }
            let panel = surface
                .controller
                .ivars()
                .information_panel
                .borrow()
                .clone()
                .ok_or("真实信息动作没有构造信息卡")?;
            let (root, scroll, values, close, refresh) = panel.probe_controls();
            let first = self.information_root.borrow().clone();
            if let Some(first) = first {
                let first =
                    step(&current, || first.load())?.ok_or("重复打开前原信息卡根已被替换")?;
                if !std::ptr::eq(&*first, root) {
                    return Err("反复开关信息卡重新分配原生控件".into());
                }
            } else {
                self.information_root
                    .replace(Some(step(&current, || Weak::from(root))?));
            }
            // SAFETY: 原实际卡片的父关系只在AppKit主线程读取；无状态借用跨Native。
            let parent = step(&current, || unsafe { root.superview() })?;
            if !visible {
                if parent.is_some() {
                    return Err("收起信息卡后仍挂接正文父视图".into());
                }
                for value in values {
                    let text = step(&current, || value.stringValue())?;
                    if step(&current, || text.length())? != 0
                        || step(&current, || value.toolTip())?.is_some()
                    {
                        return Err("隐藏卡片仍持有旧标题、路径、统计或提示".into());
                    }
                }
                return Ok(());
            }
            let content =
                step(&current, || surface.panel.contentView())?.ok_or("信息卡原窗口没有内容根")?;
            if parent
                .as_ref()
                .is_none_or(|parent| !std::ptr::eq(&**parent, &*content))
            {
                return Err("信息卡没有在原窗口正文内显示".into());
            }
            let editor = surface.editor.as_ref().ok_or("信息卡观察缺少原编辑器")?;
            let expected = {
                let document = editor.document().borrow();
                if document
                    .current_revision()
                    .map_err(|error| error.to_string())?
                    != facts.baseline
                    || document.is_dirty()
                    || document.can_undo()
                    || document.can_redo()
                {
                    return Err("只读信息卡提前提交了单格草稿或canonical历史".into());
                }
                [
                    self.path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .ok_or("原表格标题不是 UTF-8")?
                        .to_owned(),
                    self.relative_path.clone(),
                    document.character_count().to_string(),
                    document.word_count().to_string(),
                    document.line_count().to_string(),
                    document.byte_len().to_string(),
                ]
            };
            for (value, expected) in values.iter().zip(expected) {
                let text = step(&current, || value.stringValue())?;
                if !step(&current, || {
                    text.isEqualToString(&NSString::from_str(&expected))
                })? {
                    return Err("信息卡标题、相对路径或原canonical聚合不同".into());
                }
            }
            let modified = step(&current, || values[6].stringValue())?;
            if step(&current, || modified.length())? == 0
                || step(&current, || modified.isEqualToString(ns_string!("未获取")))?
            {
                return Err("合成文件的信息卡没有真实修改时间".into());
            }
            for (button, explanation) in [
                (close, ns_string!("关闭文档信息")),
                (refresh, ns_string!("刷新当前文档词数")),
            ] {
                if !step(&current, || button.accessibilityLabel())?
                    .is_some_and(|label| label.isEqualToString(explanation))
                {
                    return Err("信息卡关闭或刷新按钮缺少原中文无障碍说明".into());
                }
                if !step(&current, || button.refusesFirstResponder())?
                    || !step(&current, || button.isEnabled())?
                {
                    return Err("信息卡动作会抢文本焦点或不可用".into());
                }
            }
            let frame = step(&current, || root.frame())?;
            let body = step(&current, || editor.scroll_view().frame())?;
            if frame.size.width != 270.0
                || frame.size.height <= 0.0
                || (frame.origin.x + frame.size.width - (body.origin.x + body.size.width - 14.0))
                    .abs()
                    > 0.1
                || (frame.origin.y + frame.size.height - (body.origin.y + body.size.height - 12.0))
                    .abs()
                    > 0.1
                || frame.origin.y < body.origin.y
            {
                return Err("信息卡不是原正文右上角浮层，或越过正文边界".into());
            }
            let clip = step(&current, || scroll.contentView())?;
            let document =
                step(&current, || scroll.documentView())?.ok_or("信息卡缺少实际可滚动内容")?;
            let content_height = step(&current, || document.frame())?.size.height;
            let viewport = step(&current, || clip.bounds())?;
            if viewport.size.height <= 0.0
                || content_height + 0.1 < viewport.size.height
                || (viewport.origin.y + viewport.size.height - content_height).abs() > 0.1
            {
                return Err("首次或重复打开信息卡未从内容顶端显示".into());
            }
            println!(
                "native_information_card visible=true original_root_reused=true original_doc_revision_unchanged=true title_path_counts=true original_overlay_frame=true formal=false physical_ime=false"
            );
            Ok(())
        })
    }

    /// 卡内关闭/刷新沿实际按钮target/action执行；不直接调用hide、刷新helper或改控件内容。
    fn click_information_control(
        &self,
        facts: &AppendFacts,
        refresh_words: bool,
    ) -> Result<(), String> {
        let (events, mtm) = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("信息卡没有原正文")?;
            let before = editor
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            let child = editor.body.active_cell_body();
            let child_view = child
                .as_ref()
                .map(|child| child.view())
                .transpose()
                .map_err(|cause| cause.to_string())?;
            let child_before = child_view
                .as_ref()
                .map(|view| view.input_state())
                .transpose()
                .map_err(|cause| cause.to_string())?;
            // 信息动作不能结束格内组合或切换客户；原父子强claim只活本次鼠标准备同步栈。
            let current = || {
                self.current(surface, facts, facts.generation)
                    && editor
                        .text_view
                        .input_state()
                        .is_ok_and(|actual| actual == before)
                    && match (&child, editor.body.active_cell_body()) {
                        (Some(original), Some(actual)) => Rc::ptr_eq(original, &actual),
                        (None, None) => true,
                        _ => false,
                    }
                    && child_view
                        .as_ref()
                        .map(|view| view.input_state())
                        .transpose()
                        .is_ok_and(|actual| actual == child_before)
            };
            let panel = surface
                .controller
                .ivars()
                .information_panel
                .borrow()
                .clone()
                .ok_or("信息卡尚未真实打开")?;
            let (_, _, _, close, refresh) = panel.probe_controls();
            let button = if refresh_words { refresh } else { close };
            if !step(&current, || button.refusesFirstResponder())? {
                return Err("信息卡按钮会夺取原输入焦点".into());
            }
            let events = button_mouse::prepare(button, &surface.panel, &current)?;
            Ok((events, surface.panel.mtm()))
        })?;
        button_mouse::post(events, mtm); // 下一个具名Host步骤核真实刷新/关闭，不在本栈补发action。
        Ok(())
    }

    /// 工具栏点击走原窗口命中路由；一次准备完整 Down/Up，不能在 Down 的 tracking 栈里才发送 Up。
    /// 原controller仍自行捕获意图并排队，事件入队不代表格式、历史或信息卡已经完成。
    fn click_toolbar_button(&self, delta: u64, tag: isize) -> Result<(), String> {
        use crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError as ViewError;

        let started = Instant::now();
        let facts = self.facts()?;
        let generation = facts
            .generation
            .checked_add(delta)
            .ok_or("工具栏代次溢出")?;
        // canonical历史先完成，实际按钮的enable通知仍可能排在主队列；只固定原客户和按钮弱票，
        // 不把较新的Input重采为本次前态，也不让等待保活已经移走的格、按钮或窗口。
        let (before, child_identity, child_before, button_identity) =
            with_active_native_markdown_surface(|surface| {
                let current = || self.current(surface, &facts, generation);
                if !current() {
                    return Err("工具栏点击前原文档已改变".into());
                }
                let editor = surface.editor.as_ref().unwrap();
                let before = editor
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?;
                let child = editor.body.active_cell_body();
                let child_before = child
                    .as_ref()
                    .map(|body| body.view())
                    .transpose()
                    .map_err(|cause| cause.to_string())?
                    .as_ref()
                    .map(|view| view.input_state())
                    .transpose()
                    .map_err(|cause| cause.to_string())?;
                let controls = surface.controller.ivars().workspace_controls.get().unwrap();
                for control in controls.iter().skip(NATIVE_MARKDOWN_TOOLBAR_START) {
                    if let Some(button) = control.downcast_ref::<NSButton>()
                        && step(&current, || button.tag())? == tag
                    {
                        return Ok((
                            before,
                            child.as_ref().map(Rc::downgrade),
                            child_before,
                            Weak::from(button),
                        ));
                    }
                }
                Err("实际工具栏没有所需按钮".into())
            })?;
        loop {
            let events = with_active_native_markdown_surface(|surface| {
                let current = || self.current(surface, &facts, generation);
                if !current() {
                    return Err("工具栏准入期间原文档已改变".into());
                }
                let editor = surface.editor.as_ref().unwrap();
                if let Some(cause) = editor.body.take_failure() {
                    return Err(format!("工具栏等待时原正文失败：{cause}"));
                }
                if let Some(cause) = editor.text_view.take_failure()
                    && !matches!(cause, ViewError::Pending)
                {
                    return Err(format!("工具栏等待时原输入失败：{cause}"));
                }
                // Pending前仍核原Source健康；零端点只触健康/身份，不借正文页或复制内容。
                editor
                    .document()
                    .borrow()
                    .byte_offset_for_utf16(0)
                    .map_err(|cause| cause.to_string())?;
                // 格草稿与父canonical是两个真实客户；工具栏滚动不能确认组合或偷偷切换活动格。
                // 这些临时claim只服务本次Native坐标准备，不进入事件或下一轮观察槽。
                let child = editor.body.active_cell_body();
                let same_child = match (&child_identity, &child) {
                    (Some(before), Some(after)) => {
                        std::rc::Weak::ptr_eq(before, &Rc::downgrade(after))
                    }
                    (None, None) => true,
                    _ => false,
                };
                if !same_child {
                    return Err("工具栏等待时原活动格已换走".into());
                }
                let child_view = child
                    .as_ref()
                    .map(|child| child.view())
                    .transpose()
                    .map_err(|cause| cause.to_string())?;
                if let Some(child) = &child
                    && let Some(cause) = child.take_failure()
                {
                    return Err(format!("工具栏等待时原格正文失败：{cause}"));
                }
                if let Some(view) = &child_view
                    && let Some(cause) = view.take_failure()
                    && !matches!(cause, ViewError::Pending)
                {
                    return Err(format!("工具栏等待时原格输入失败：{cause}"));
                }
                if editor
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    != before
                    || child_view
                        .as_ref()
                        .map(|view| view.input_state())
                        .transpose()
                        .map_err(|cause| cause.to_string())?
                        != child_before
                {
                    return Err("工具栏等待改变了原父子完整Input".into());
                }
                let current = || {
                    current()
                        && editor
                            .text_view
                            .input_state()
                            .is_ok_and(|state| state == before)
                        && match (&child_identity, editor.body.active_cell_body()) {
                            (Some(original), Some(actual)) => {
                                std::rc::Weak::ptr_eq(original, &Rc::downgrade(&actual))
                            }
                            (None, None) => true,
                            _ => false,
                        }
                        && child_view
                            .as_ref()
                            .map(|view| view.input_state())
                            .transpose()
                            .is_ok_and(|state| state == child_before)
                };
                let original_button = button_identity.load().ok_or("原工具栏按钮已释放")?;
                let controls = surface.controller.ivars().workspace_controls.get().unwrap();
                for control in controls.iter().skip(NATIVE_MARKDOWN_TOOLBAR_START) {
                    let Some(button) = control.downcast_ref::<NSButton>() else {
                        continue; // 非按钮不代发动作；实际格式与历史仍按原自有按钮身份和tag发送。
                    };
                    if step(&current, || button.tag())? == tag {
                        if !std::ptr::eq(button, &*original_button) {
                            return Err("工具栏等待期间原按钮被替换".into());
                        }
                        if !step(&current, || button.refusesFirstResponder())? {
                            return Err("工具栏按钮不可用或会夺取文本焦点".into());
                        }
                        let enabled = step(&current, || button.isEnabled())?;
                        let target_view = child_view.as_ref().unwrap_or(&editor.text_view);
                        let target_body = child.as_ref().unwrap_or(&editor.body);
                        let mut history_frame = None;
                        if matches!(tag, 22 | 23) {
                            // 只有真实历史已经可用，才把尚未到达的UI发布作为等待。marked、空历史、
                            // 其它disabled按钮都仍是立即失败，不能刷新控件或代发命令让验收通过。
                            let target_input = child_before.unwrap_or(before);
                            let available = if target_input.marked.is_some() {
                                false
                            } else if child.is_some() {
                                let history = target_view
                                    .cell_history_facts()
                                    .map_err(|cause| cause.to_string())?
                                    .ok_or("工具栏原格没有真实局部历史")?;
                                if tag == 22 {
                                    history.can_undo
                                } else {
                                    history.can_redo
                                }
                            } else {
                                let document = editor.document().borrow();
                                if tag == 22 {
                                    document.can_undo()
                                } else {
                                    document.can_redo()
                                }
                            };
                            if !available {
                                return Err("工具栏原客户没有所需真实历史".into());
                            }
                            if target_body.is_preparing() {
                                return Ok(None);
                            }
                            let key = match target_view.capture_ready_scene_key() {
                                Ok(key) => key,
                                Err(ViewError::Pending) => return Ok(None),
                                Err(cause) => return Err(cause.to_string()),
                            };
                            let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(
                                &**target_body,
                                &key.request,
                            )
                            .map_err(|cause| cause.to_string())?;
                            scene
                                .validate_ready(&current)
                                .map_err(|cause| cause.to_string())?;
                            if key.request.input != target_input {
                                return Err("工具栏历史帧不属于原完整Input".into());
                            }
                            let Some((cpu, paints)) = target_view.probe_cpu_published_frame()
                            else {
                                return Ok(None);
                            };
                            if !enabled
                                || !cpu.same(&key)
                                || paints == 0
                                || target_view.cpu_paints() != Some(paints)
                                || target_view.cpu_paint_pending() != Some(false)
                            {
                                return Ok(None);
                            }
                            history_frame = Some((scene, key, paints));
                        } else if !enabled {
                            return Err("工具栏按钮不可用或会夺取文本焦点".into());
                        }
                        let bounds = step(&current, || button.bounds())?;
                        step(&current, || button.scrollRectToVisible(bounds))?;
                        // 只显露原按钮并命中它的真实可见中心；不能用离屏坐标或直接action替代失败点击。
                        // 这不是正文Reveal，原有Source目标和canonical选择均不获得新权限。
                        let [down, up] = button_mouse::prepare(button, &surface.panel, &current)?;
                        // 两事件都构造成功才可发布；失败不能只留一个Down把原按钮tracking悬挂。
                        // 显露横向工具栏只允许几何变化；父/子客户、组合态和完整选择不能借此重采。
                        let same_child = match (&child, editor.body.active_cell_body()) {
                            (Some(before), Some(after)) => Rc::ptr_eq(before, &after),
                            (None, None) => true,
                            _ => false,
                        };
                        if !current()
                            || !same_child
                            || editor
                                .text_view
                                .input_state()
                                .map_err(|cause| cause.to_string())?
                                != before
                            || child_view
                                .as_ref()
                                .map(|view| view.input_state())
                                .transpose()
                                .map_err(|cause| cause.to_string())?
                                != child_before
                        {
                            return Err("工具栏命中准备改变了原父子客户或完整Input".into());
                        }
                        if let Some((scene, key, paints)) = history_frame {
                            // 按钮显露和坐标转换也会进入AppKit；返回后先保Source/Font首错，再核
                            // 原CPU仍同key。只有这一次完整前态能发点击，几何换代仍沿同期限等待。
                            scene
                                .validate_ready(&current)
                                .map_err(|cause| cause.to_string())?;
                            if !NativeMarkdownPagedTextViewDelegate::scene_current(
                                &**target_body,
                                &key,
                            ) || !target_view
                                .probe_cpu_published_frame()
                                .is_some_and(|(cpu, actual)| cpu.same(&key) && actual == paints)
                                || target_view.cpu_paints() != Some(paints)
                                || target_view.cpu_paint_pending() != Some(false)
                            {
                                return Ok(None);
                            }
                        }
                        return Ok(Some((down, up, surface.panel.mtm())));
                    }
                }
                Err("实际工具栏没有所需按钮".into())
            })?; // 本轮ACTIVE、按钮、父子客户和Scene强claim全部先退租，才允许下次主循环。
            if let Some((down, up, mtm)) = events {
                if started.elapsed() > Duration::from_secs(3) {
                    return Err("工具栏原按钮准入超过三秒期限".into());
                }
                // ACTIVE、原按钮和父子客户强claim已归还；先把完整事件对放进同一AppKit队列，
                // NSButton.mouseDown的真实tracking才可取到对应Up，不需全局事件权限或嵌套等待。
                button_mouse::post([down, up], mtm);
                return Ok(());
            }
            let remaining = Duration::from_secs(3)
                .checked_sub(started.elapsed())
                .ok_or("工具栏原历史和UI没有在三秒内同时就绪")?;
            // SAFETY: 复用原主runloop等待；同一次准入只有这一份剩余期限，没有补发点击或刷新UI。
            unsafe {
                objc2_core_foundation::CFRunLoop::run_in_mode(
                    objc2_core_foundation::kCFRunLoopDefaultMode,
                    remaining.as_secs_f64(),
                    true,
                )
            };
        }
    }

    /// 字段激活前由真实窗口resize通知完成布局，往返后正文、选择和历史必须仍是同一基线。
    fn verify_toolbar_resize(&self, facts: &AppendFacts) -> Result<(), String> {
        let started = Instant::now();
        // 显式目录只选择本次合成UI图的输出位置；普通验收不分配整窗bitmap、不写图。
        let capture = ui_render::directory()?;
        let (panel, input) = with_active_native_markdown_surface(|surface| {
            if !self.current(surface, facts, facts.generation) {
                return Err("缩放前原窗口已改变".into());
            }
            let input = surface
                .editor
                .as_ref()
                .unwrap()
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            Ok((Weak::from_retained(&surface.panel), input))
        })?;
        let original = panel
            .load()
            .ok_or("缩放原窗口已释放")?
            .contentView()
            .ok_or("缩放窗口缺少内容根")?
            .frame()
            .size;
        let result: Result<(), String> = (|| {
            for size in [
                NSSize::new(900.0, 600.0),
                NSSize::new(NATIVE_MARKDOWN_WIDTH, NATIVE_MARKDOWN_HEIGHT),
                NSSize::new(1400.0, 900.0),
            ] {
                // ACTIVE已归还；setContentSize触发原windowDidResize，不在probe调用布局实现补签。
                let window = panel.load().ok_or("缩放原窗口已释放")?;
                window.setContentSize(size);
                window.displayIfNeeded();
                drop(window); // 只保原Weak跨CPU等待，不让诊断保活Native窗口。
                self.verify_actual_toolbar_frames(facts, size)?;
                if let Some(directory) = &capture {
                    // 三个尺寸和最后恢复共用原started；PNG编码/写盘也计入这次三秒，绝不重置期限。
                    self.wait_parent(facts, facts.generation, started, true)?;
                    self.capture_toolbar_ui(facts, input, size, directory)?;
                    if started.elapsed() > Duration::from_secs(3) {
                        return Err("三尺寸实际CPU与内部渲染图超过原三秒期限".into());
                    }
                }
            }
            Ok(())
        })();
        let restored = (|| {
            let window = panel.load().ok_or("恢复尺寸前原窗口已释放")?;
            window.setContentSize(original); // 失败也恢复原大小，保留最初验收错误。
            window.displayIfNeeded();
            drop(window);
            self.verify_actual_toolbar_frames(facts, original)
        })();
        result?;
        restored?;
        // 控件frame同步正确不代表正文已完成异步重排；restore同样会撤旧几何和CPU票。
        // 原panel强claim及ACTIVE租约均已归还，才沿本次三尺寸动作的同一三秒期限
        // 等待当前Ready与真实CPU同key；true要求字段尚未激活，不把后来子格当父终点。
        self.wait_parent(facts, facts.generation, started, true)?;
        self.verify_actual_toolbar_frames(facts, original)?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, facts, facts.generation)
                || surface
                    .editor
                    .as_ref()
                    .unwrap()
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    != input
            {
                return Err("窗口缩放等待改变了原完整正文输入".into());
            }
            Ok(())
        })?;
        println!(
            "native_toolbar_resize actual_delegate=true minimum=900x600 larger=1400x900 original_size_restored=true source_selection_history_unchanged=true formal=false"
        );
        Ok(())
    }

    /// 三尺寸只借原父正文已核CPU；共享模块独占bitmap与文件I/O，不让输出改变原帧权限。
    fn capture_toolbar_ui(
        &self,
        facts: &AppendFacts,
        input: NativeMarkdownPagedInputState,
        size: NSSize,
        directory: &Path,
    ) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("UI图缺少原正文")?;
            let key = editor
                .text_view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let (published, paints) = editor
                .text_view
                .probe_cpu_published_frame()
                .ok_or("UI图没有实际已发布CPU帧")?;
            let current = || {
                self.current(surface, facts, facts.generation)
                    && key.request.input == input
                    && editor
                        .text_view
                        .input_state()
                        .is_ok_and(|actual| actual == input)
                    && NativeMarkdownPagedTextViewDelegate::scene_current(&*editor.body, &key)
                    && published.same(&key)
                    && paints != 0
                    && editor.text_view.cpu_paints() == Some(paints)
                    && editor.text_view.cpu_paint_pending() == Some(false)
            };
            let scene =
                NativeMarkdownPagedTextViewDelegate::ready_scene(&*editor.body, &key.request)
                    .map_err(|cause| cause.to_string())?;
            scene
                .validate_ready(&current)
                .map_err(|cause| cause.to_string())?;
            let content =
                step(&current, || surface.panel.contentView())?.ok_or("UI图缺少原窗口内容根")?;
            if step(&current, || content.bounds())?.size != size {
                return Err("UI图内容根尺寸不是本次预声明窗口".into());
            }
            // 最小正式窗口沿同一次真实CPU终点导图，静态标签便于直接找到窄窗布局；
            // 不再创建第二张图片，输出仍计入原三尺寸动作共用的三秒期限。
            let label = if size == NSSize::new(900.0, 600.0) {
                "table-toolbar-compact"
            } else {
                "table-toolbar"
            };
            let result = ui_render::write(directory, &content, label, paints, &current);
            // Native缓存/编码即使先报撤权，仍保原Source/Font首因和实际updateLayer失败；不以PNG洗错。
            scene
                .validate_ready(&current)
                .map_err(|cause| cause.to_string())?;
            if let Some(cause) = editor.body.take_failure() {
                return Err(format!("UI内部渲染时原正文失败：{cause}"));
            }
            if let Some(cause) = editor.text_view.take_failure() {
                return Err(format!("UI内部渲染时原输入失败：{cause}"));
            }
            result
        })
    }

    /// 仅此合成工具栏用例读取实际控件矩形、标题和中文AX；不调用生产几何函数推导“实际结果”。
    fn verify_actual_toolbar_frames(
        &self,
        facts: &AppendFacts,
        size: NSSize,
    ) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("缩放后没有原编辑器")?;
            if std::ptr::from_ref(&*surface.controller).addr() != facts.owner
                || !native_markdown_controller_has_foreground_authority(&surface.controller)
                || !WeakMarkdownDocument::ptr_eq(&Rc::downgrade(editor.document()), &facts.document)
            {
                return Err("缩放期间已替换原窗口或正文".into());
            }
            {
                let document = editor.document().borrow();
                if document.generation() != facts.generation
                    || document
                        .current_revision()
                        .map_err(|cause| cause.to_string())?
                        != facts.baseline
                    || document.selection() != facts.selection
                    || document.is_dirty()
                    || document.can_undo()
                    || document.can_redo()
                {
                    return Err("窗口缩放改变了源、选择或历史".into());
                }
            } // 所有canonical借用先结束，再读取实际AppKit控件。
            let content = surface.panel.contentView().ok_or("缩放后缺少实际内容根")?;
            if content.frame().size != size {
                return Err("窗口没有采用请求的真实内容尺寸".into());
            }
            let controls = surface.controller.ivars().workspace_controls.get().unwrap();
            let mode = surface
                .controller
                .ivars()
                .presentation_mode_button
                .get()
                .unwrap();
            let status = surface.controller.ivars().status_label.get().unwrap();
            if !surface
                .note_title
                .stringValue()
                .isEqualToString(&NSString::from_str(
                    self.path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .ok_or("原标题不是 UTF-8")?,
                ))
                || !surface
                    .note_title
                    .accessibilityLabel()
                    .is_some_and(|label| label.isEqualToString(ns_string!("当前笔记")))
            {
                return Err("实际标题带入扩展名或丢失中文AX说明".into());
            }
            if !mode.title().isEqualToString(ns_string!("源码"))
                || !mode.refusesFirstResponder()
                || !mode
                    .accessibilityLabel()
                    .is_some_and(|label| label.isEqualToString(ns_string!("切换源码显示")))
                || mode.state()
                    != if surface.presentation_mode == MarkdownPresentationMode::Source {
                        objc2_app_kit::NSControlStateValueOn
                    } else {
                        objc2_app_kit::NSControlStateValueOff
                    }
            {
                return Err("顶部原源码按钮的标签、焦点或真实模式状态不同".into());
            }
            let mut headers = vec![
                surface.note_title.frame(),
                surface.note_path.frame(),
                mode.frame(),
                controls[1].frame(),
            ];
            for (control, label) in controls
                .iter()
                .skip(NATIVE_MARKDOWN_TOOLBAR_START)
                .take(5)
                .zip([
                    "撤销当前笔记的上一步编辑",
                    "重做当前笔记的编辑",
                    "在当前笔记中查找",
                    "查看当前笔记信息",
                    "将当前笔记的原始Markdown全文复制到系统剪贴板",
                ])
            {
                if control.isHidden()
                    || !control.refusesFirstResponder()
                    || !control
                        .accessibilityLabel()
                        .is_some_and(|value| value.isEqualToString(&NSString::from_str(label)))
                {
                    return Err("实际常用按钮被隐藏、会夺焦或中文AX不同".into());
                }
                headers.push(control.frame());
            }
            for control in
                &controls[NATIVE_MARKDOWN_TOOLBAR_START..NATIVE_MARKDOWN_TOOLBAR_START + 2]
            {
                if control.isEnabled() {
                    return Err("空历史的撤销或重做按钮仍显示可用".into());
                } // 原Doc已确认无Undo/Redo，不因文档存在就启用历史。
            }
            // 直接核对当前日常编辑入口与中文语义，不将生产布局结果当作oracle。
            let expected = [
                (3, "导入附件并在原光标处插入链接"),
                (100, "一级标题"),
                (101, "二级标题"),
                (102, "三级标题"),
                (103, "加粗"),
                (104, "斜体"),
                (105, "下划线"),
                (106, "删除线"),
                (107, "插入行内代码"),
                (108, "插入链接"),
                (110, "无序列表"),
                (111, "有序列表"),
                (112, "任务列表"),
                (109, "引用"),
                (118, "插入表格"),
                (116, "插入代码块"),
                (115, "插入分隔线"),
            ];
            let formats = &controls[NATIVE_MARKDOWN_TOOLBAR_START + 5..];
            if formats.len() != expected.len() {
                return Err("附件或日常格式入口有缺失或重复".into());
            }
            let strip = formats[0]
                .enclosingScrollView()
                .ok_or("格式按钮缺少真实水平滚动视口")?;
            let document_view = strip.documentView().ok_or("格式视口缺少真实按钮内容根")?;
            let mut previous_end = 0.0;
            for (control, (tag, label)) in formats.iter().zip(expected) {
                let button = control
                    .downcast_ref::<NSButton>()
                    .ok_or("原一击格式入口变成菜单或其它控件")?;
                let frame = button.frame();
                let same_scroll = button
                    .enclosingScrollView()
                    .is_some_and(|candidate| std::ptr::eq(&*candidate, &*strip));
                if !same_scroll
                    || button.tag() != tag
                    || button.isHidden()
                    || !button.isEnabled()
                    || !button.refusesFirstResponder()
                    || !button
                        .accessibilityLabel()
                        .is_some_and(|value| value.isEqualToString(&NSString::from_str(label)))
                    || frame.origin.x < previous_end
                    || frame.origin.y < 0.0
                    || frame.size.height != 29.0
                    || frame.origin.y + frame.size.height > document_view.frame().size.height
                    || frame.origin.x + frame.size.width > document_view.frame().size.width
                {
                    return Err("日常格式顺序、局部按钮几何、焦点或中文AX不同".into());
                }
                previous_end = frame.origin.x + frame.size.width;
            }
            if strip.hasVerticalScroller()
                || !strip.hasHorizontalScroller()
                || strip.frame().size.height != 42.0
            {
                return Err("原格式栏没有保持42pt单行水平滚动".into());
            }
            let clip = strip.contentView();
            let original_origin = clip.bounds().origin;
            let first = formats[0].frame();
            let last = formats.last().unwrap().frame();
            let reachable: Result<(), String> = (|| {
                for frame in [last, first] {
                    // 在同一真实NSClipView滚至末/首入口，确认窄窗没有以隐藏动作代替横滚。
                    clip.scrollToPoint(NSPoint::new(
                        (frame.origin.x + frame.size.width - clip.bounds().size.width).max(0.0),
                        0.0,
                    ));
                    strip.reflectScrolledClipView(&clip);
                    let visible = clip.bounds();
                    if frame.origin.x < visible.origin.x
                        || frame.origin.x + frame.size.width > visible.origin.x + visible.size.width
                        || frame.origin.y < visible.origin.y
                        || frame.origin.y + frame.size.height
                            > visible.origin.y + visible.size.height
                    {
                        return Err("格式栏首项或末项不能沿真实水平视口完整到达".into());
                    }
                }
                Ok(())
            })();
            clip.scrollToPoint(original_origin);
            strip.reflectScrolledClipView(&clip); // 任何断言失败也先恢复原横滚位置，不影响后续真实格式点击。
            reachable?;
            headers.push(strip.frame()); // 按钮frame属于局部内容；全局不重叠核验只使用真实scroll根。
            let body = editor.scroll_view().frame();
            let footers = surface
                .controller
                .ivars()
                .footer_labels
                .get()
                .unwrap()
                .each_ref()
                .map(|label| label.frame());
            let feedback = (!status.isHidden()).then(|| status.frame());
            let find_bar = editor.find_bar.borrow().clone();
            let find_frame = find_bar
                .as_ref()
                .and_then(|bar| bar.visible_frame_for_probe());
            if let (Some(feedback), Some(find)) = (feedback, find_frame)
                && NSIntersectsRect(feedback, find)
            {
                return Err("真实反馈行与查找栏互相覆盖".into());
            }
            for label in surface.controller.ivars().footer_labels.get().unwrap() {
                if label.isHidden() || label.font().is_none_or(|font| font.pointSize() != 11.0) {
                    return Err("原footer三列未按正常字号显示".into());
                }
            }
            for pair in footers.windows(2) {
                if NSIntersectsRect(pair[0], pair[1]) {
                    return Err("原footer三列相互重叠".into());
                }
            }
            for (index, frame) in headers.iter().enumerate() {
                if frame.origin.x < NATIVE_MARKDOWN_SIDEBAR_WIDTH
                    || frame.origin.y < size.height - sidebar_geometry::HEADER_HEIGHT
                    || frame.origin.x + frame.size.width > size.width
                    || frame.origin.y + frame.size.height > size.height
                    || frame.size.width <= 0.0
                    || frame.size.height <= 0.0
                    || NSIntersectsRect(*frame, body)
                    || footers
                        .iter()
                        .any(|footer| NSIntersectsRect(*frame, *footer))
                    || feedback.is_some_and(|feedback| NSIntersectsRect(*frame, feedback))
                {
                    return Err("真实工具栏超出窗口或遮住正文和底栏".into());
                }
                for other in &headers[index + 1..] {
                    if NSIntersectsRect(*frame, *other) {
                        return Err("真实工具栏按钮或标题发生重叠".into());
                    }
                }
            }
            let search = controls[0].frame();
            let list = surface
                .sidebar_list
                .table
                .enclosingScrollView()
                .ok_or("实际笔记列表缺少滚动视图")?
                .frame();
            for frame in [
                search,
                list,
                controls[2].frame(),
                controls[3].frame(),
                footers[0],
                footers[1],
                footers[2],
                body,
            ]
            .into_iter()
            .chain(feedback)
            {
                if frame.origin.x < 0.0
                    || frame.origin.y < 0.0
                    || frame.origin.x + frame.size.width > size.width
                    || frame.origin.y + frame.size.height > size.height
                {
                    return Err("缩放后搜索、列表、正文或底栏超出可见窗口".into());
                }
            }
            if NSIntersectsRect(search, list)
                || NSIntersectsRect(list, controls[2].frame())
                || NSIntersectsRect(list, controls[3].frame())
                || footers.iter().any(|footer| NSIntersectsRect(body, *footer))
                || feedback.is_some_and(|feedback| NSIntersectsRect(body, feedback))
            {
                return Err("真实搜索、列表或底栏相互遮挡".into());
            }
            Ok(())
        })
    }

    /// 关闭只撤本次观察和交还真实 Host 的原会话；失败不能关后来切入的其它笔记。
    fn require_return_owner(&self) -> Result<(), String> {
        let facts = self.facts()?;
        let conflict = self
            .conflict
            .borrow()
            .as_ref()
            .map(|c| (c.current_document.clone(), c.current_path.clone()));
        with_active_native_markdown_surface(|surface| {
            let editor = surface
                .editor
                .as_ref()
                .ok_or("Hosted 表格原会话已离开前台")?;
            if !native_markdown_controller_has_foreground_authority(&surface.controller)
                || std::ptr::from_ref(&*surface.controller).addr() != facts.owner
            {
                return Err("Hosted 表格不能关闭后继窗口".into());
            }
            let (document, path) = conflict.as_ref().map_or(
                (&facts.document, self.relative_path.as_str()),
                |(document, path)| (document, path.as_str()),
            );
            if surface.current_note_relative_path.as_deref() != Some(path)
                || !WeakMarkdownDocument::ptr_eq(document, &Rc::downgrade(editor.document()))
                || conflict.is_none()
                    && !std::rc::Weak::ptr_eq(&facts.body, &Rc::downgrade(&editor.body))
            {
                return Err("Hosted 表格关闭的原 Doc、路径或 Body 已撤销".into());
            }
            Ok(())
        })
    }

    pub fn request_return(&self, failed: bool) -> Result<(), String> {
        MainThreadMarker::new().ok_or("Hosted 表格返回必须在 AppKit 主线程")?;
        self.require_return_owner()?;
        let operation = self.conflict_operation.borrow_mut().take();
        if let Some(operation) = operation {
            operation.active.set(false);
        }
        CONFLICT_OPERATION.with_borrow_mut(|slot| *slot = std::rc::Weak::new());
        if failed && self.conflict.borrow().is_some() {
            let (document, path, text) = {
                let facts = self.conflict.borrow();
                let facts = facts.as_ref().unwrap();
                (
                    facts.current_document.clone(),
                    facts.current_path.clone(),
                    facts.current_text,
                )
            };
            with_active_native_markdown_surface(|surface| {
                let editor = surface
                    .editor
                    .as_ref()
                    .ok_or("失败夹具清理不能取得原 Doc")?;
                if surface.current_note_relative_path.as_deref() != Some(path.as_str())
                    || !WeakMarkdownDocument::ptr_eq(&document, &Rc::downgrade(editor.document()))
                {
                    return Err("失败清理的原 Doc/path 已撤销".into());
                }
                Self::verify_conflict_text(&editor.document().borrow(), text)
            })?;
            // 仅恢复本入口拥有的固定冲突夹具；driver 的首失败仍然 sticky，不能把清理当业务成功。
            std::fs::write(self.path.parent().unwrap().join(&path), text)
                .map_err(|e| e.to_string())?;
            if path == self.relative_path {
                self.conflict
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .original_disk_text = text;
            }
        }
        self.require_return_owner()?; // 清理的 Native weak 归还和文件写入后仍必须属于原实际会话。
        request_native_markdown_window_exit()
    }

    pub fn verify_native_closed(&self) -> Result<(), String> {
        MainThreadMarker::new().ok_or("Hosted 表格关闭核验必须在 AppKit 主线程")?;
        if ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow(Option::is_some)
            || BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some()
            || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
        {
            return Err("Hosted 表格真实 Native owner 尚未关闭".into());
        }
        let facts = self.facts()?;
        if facts.document.strong_count() != 0
            || facts.body.strong_count() != 0
            || self
                .previous_cells
                .borrow()
                .iter()
                .any(|cell| cell.strong_count() != 0)
        {
            return Err("真实关闭后原父正文、文稿或单格仍存活".into());
        }
        let information = self.information_root.borrow().clone();
        if information
            .as_ref()
            .is_some_and(|root| root.load().is_some())
        {
            return Err("真实关闭后信息卡仍存活".into());
        }
        drop(information); // objc weak load/drop 不跨本观察槽的 RefCell 借用。
        let conflict = self.conflict.borrow().as_ref().map(|c| {
            (
                c.current_document.clone(),
                c.bar.clone(),
                c.original_disk_text,
                c.completed,
            )
        });
        if let Some((document, bar, original, completed)) = conflict {
            if document.strong_count() != 0 || bar.load().is_some() {
                return Err("冲突往返后目标 Doc 或原冲突条仍存活".into());
            }
            Self::verify_conflict_file(&self.path, original)?;
            let copy = self.path.parent().unwrap().join(CONFLICT_COPY_PATH);
            if completed || copy.exists() {
                Self::verify_conflict_file(&copy, CONFLICT_DRAFT)?;
            }
        } else {
            // 最终磁盘值来自本用例预声明的字符结果；不把实际字节重新当作期望。
            let expected = match self.case {
                NativeMarkdownHostedTableCase::FormatSelection
                | NativeMarkdownHostedTableCase::ToolbarFormatSelection => SELECTED_AFTER,
                NativeMarkdownHostedTableCase::FindReplacementComposition => FIND_REPLACEMENT_AFTER,
                _ => self.initial_text(),
            };
            Self::verify_conflict_file(&self.path, expected)?;
        }
        let find = self.find_replacement.borrow_mut().take();
        if find.as_ref().is_some_and(|p| p.bar.strong_count() != 0) {
            return Err("真实关闭后原查找栏仍存活".into());
        }
        println!(
            "native_table_append_strict_close passed=true real_paged_clients=true source_disk_checked=true historical_cell_weaks_released=true physical_tab=false physical_ime=false"
        );
        Ok(())
    }
}
