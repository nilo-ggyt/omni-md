//! 项目自有 Find/Replace 输入栏；普通字段使用窗口的会话 editor/Undo，不接管框架私有控件。

use super::*;

mod geometry;
use geometry::{find_bar_height, find_frames};

mod input;
pub(super) use input::NativeMarkdownFindProtocolState;

mod reveal;
pub(super) use reveal::*;

mod matching;
mod viewport;
#[cfg(all(
    feature = "native-paged-find-probe",
    feature = "native-markdown-runtime-probe"
))]
pub use matching::backend::run_native_paged_find_backend_control;
#[cfg(all(
    feature = "native-paged-body-probe",
    feature = "native-markdown-runtime-probe"
))]
pub use matching::backend::run_native_paged_find_body_transaction_control;
pub(super) use matching::backend::{NativeMarkdownFindBackend, execute_find_backend};
#[cfg(feature = "native-paged-production")]
pub(super) use matching::backend::{
    NativeMarkdownFindExecution, NativeMarkdownFindExecutionError, NativeMarkdownPagedFindBackend,
    NativeMarkdownPagedFindRevealPort, NativeMarkdownPagedFindRevealProgress,
    NativeMarkdownPagedFindRevealRequest,
};
use matching::*;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) use viewport::NativeMarkdownFindHighlightBatch;
pub(super) use viewport::NativeMarkdownFindViewportFeedback;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) use viewport::{MAX_DECORATION_VIEWPORT_UNITS, MAX_SPECIAL_CHARACTER_MARKERS};
#[cfg(feature = "native-paged-production")]
pub(super) use viewport::{NativeMarkdownFindVisibleSource, NativeMarkdownPagedFindHighlightError};

#[cfg(feature = "native-markdown-runtime-probe")]
mod probe;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) use probe::{
    begin_list_marker_find_probe, execute_multi_selection_find_probe,
    finish_list_marker_find_probe, multi_selection_find_probe_bar_stamp,
    prepare_multi_selection_find_probe,
};

/// 快捷键仅描述本笔记动作；显示与匹配分离，尚无查询时 Cmd-G 先打开输入。
pub(super) enum MarkdownFindKeyCommand {
    Show(bool),
    Action(MarkdownFindAction),
}

/// 仅接受明确的 Command 组合，不吞掉系统输入源切换或 Control 绑定。
pub(super) fn markdown_find_key_command(event: &NSEvent) -> Option<MarkdownFindKeyCommand> {
    let relevant = event.modifierFlags()
        & (NSEventModifierFlags::Command
            | NSEventModifierFlags::Option
            | NSEventModifierFlags::Control
            | NSEventModifierFlags::Shift);
    match (event.keyCode(), relevant) {
        (3, flags) if flags == NSEventModifierFlags::Command => {
            Some(MarkdownFindKeyCommand::Show(false))
        } // Cmd-F 只打开查找；替换由原独立命令显式展开。
        (3, flags) if flags == (NSEventModifierFlags::Command | NSEventModifierFlags::Option) => {
            Some(MarkdownFindKeyCommand::Show(true))
        } // Option-Cmd-F。
        (5, flags) if flags == NSEventModifierFlags::Command => {
            Some(MarkdownFindKeyCommand::Action(MarkdownFindAction::Next))
        } // Cmd-G。
        (5, flags) if flags == (NSEventModifierFlags::Command | NSEventModifierFlags::Shift) => {
            Some(MarkdownFindKeyCommand::Action(MarkdownFindAction::Previous))
        } // Shift-Cmd-G。
        _ => None, // 其他按键仍走原生 responder 链。
    }
}

/// 与菜单同一调用入口；没有栏时先显示并等待显式查询，不读共享 Find pasteboard。
pub(super) fn dispatch_markdown_find_key_command(
    command: MarkdownFindKeyCommand,
) -> Result<(), String> {
    match command {
        MarkdownFindKeyCommand::Show(replace) => show_native_markdown_find_bar(replace),
        MarkdownFindKeyCommand::Action(action) => {
            let exists = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                active
                    .as_ref()
                    .and_then(|surface| surface.editor.as_ref())
                    .is_some_and(|editor| editor.find_bar().borrow().is_some())
            });
            if exists {
                perform_native_markdown_find_action(action).map(|_| ())
            } else {
                show_native_markdown_find_bar(false)
            }
        }
    }
}

/// Return 只在本栏实际字段 editor 中生效；marked text 仍交给系统完成组合，不提前执行查找。
pub(super) fn markdown_find_return_action(
    panel: &NativeMarkdownPanel,
    event: &NSEvent,
) -> Option<MarkdownFindAction> {
    if !matches!(event.keyCode(), 36 | 76)
        || event.modifierFlags().intersects(
            NSEventModifierFlags::Command
                | NSEventModifierFlags::Option
                | NSEventModifierFlags::Control,
        )
    {
        return None; // 其他键与系统组合键不属于栏的 Return 命令。
    }
    let bar = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        active
            .as_ref()
            .and_then(|surface| surface.editor.as_ref())
            .and_then(|editor| editor.find_bar().borrow().clone())
    })?;
    if !bar.is_visible() {
        // 隐藏栏不夺取正文、sidebar 或表格的 Return。
        return None;
    }
    let responder = panel.firstResponder()?.downcast::<NSTextView>().ok()?;
    // SAFETY: 当前主线程 NSTextView 实现公开 NSTextInputClient；这里只读实际 composition 状态。
    if unsafe { msg_send![&*responder, hasMarkedText] } {
        // 正在组合的 Return 属于系统 IME，不能提前使用尚未提交的查询值。
        return None;
    }
    let current_is = |field: &NSTextField| {
        field.currentEditor().is_some_and(|editor| {
            std::ptr::from_ref(&*editor).cast::<()>()
                == std::ptr::from_ref(&*responder).cast::<()>()
        })
    };
    if current_is(&bar.query) {
        Some(
            if event.modifierFlags().contains(NSEventModifierFlags::Shift) {
                MarkdownFindAction::Previous
            } else {
                MarkdownFindAction::Next
            },
        )
    } else if bar.replace_visible.get() && current_is(&bar.replacement) {
        Some(MarkdownFindAction::ReplaceNext) // 替换字段 Enter 保留 baseline replace-next 行为。
    } else {
        None // search/sidebar/表格/正文仍走各自原生输入。
    }
}

/// 固定动作表只在当前笔记内有效，不能作为 Host/plugin 身份或跨会话权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(isize)]
pub(super) enum MarkdownFindAction {
    Next = 1,
    Previous = 2,
    SelectAll = 3,
    ReplaceNext = 4,
    ReplaceAll = 5,
    Hide = 6,
}

impl MarkdownFindAction {
    /// sender tag 必须来自本栏的有限动作集合，未知值没有执行权限。
    pub(super) fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            1 => Some(Self::Next),        // 正向导航。
            2 => Some(Self::Previous),    // 反向导航。
            3 => Some(Self::SelectAll),   // 有限多选。
            4 => Some(Self::ReplaceNext), // 当前匹配替换并前进。
            5 => Some(Self::ReplaceAll),  // 单事务全部替换。
            6 => Some(Self::Hide),        // 只隐藏本栏。
            _ => None,                    // 不执行任意 tag 或 selector。
        }
    }
}

/// 同一栏只弱绑定实际显示后端，模式交接不能同时保留两张正文图的绘图权限。
pub(super) struct NativeMarkdownFindBar {
    /// 创建栏时固定原文稿的弱身份；输入排队前取得代次，不能在后继笔记上重新领票。
    document: WeakMarkdownDocument,
    /// 正式装配只弱观察原图；Paged 栏不会为反馈创建旧 TextKit 正文。
    presentation_body: RefCell<Option<std::rc::Weak<super::paged_body::NativeMarkdownPagedBody>>>,
    /// 只标记一次真实输入尚待反馈的原intent；滚动重建高亮不能覆盖显式导航文案。
    viewport_feedback_intent: Cell<Option<u64>>,
    /// 只记录两个真实字段的组合状态，不保存候选文字或另建输入历史。
    query_marked: Cell<bool>,
    replacement_marked: Cell<bool>,
    root: Retained<NSView>,
    query: Retained<NSTextField>,
    replacement: Retained<NSTextField>,
    case: Retained<NSButton>,
    regex: Retained<NSButton>,
    word: Retained<NSButton>,
    buttons: Vec<Retained<NSButton>>,
    status: Retained<NSTextField>,
    visible: Cell<bool>,
    replace_visible: Cell<bool>,
    closed: Cell<bool>,
    /// 同步resize只发布同一代frame，较新resize或accepted close会撤旧setter序列。
    layout_revision: Cell<u64>,
}

impl NativeMarkdownFindBar {
    /// Find 仅弱绑定当前正文；模式切换不重建栏，也不保活 Body 或另一张 Native 图。
    pub(super) fn bind_body(
        self: &Rc<Self>,
        body: std::rc::Weak<super::paged_body::NativeMarkdownPagedBody>,
        controller: &NativeMarkdownController,
    ) {
        let previous = self.presentation_body.replace(Some(body.clone()));
        if let Some(previous) = previous.as_ref().and_then(std::rc::Weak::upgrade) {
            previous.unbind_find_highlights(&Rc::downgrade(self));
        }
        drop(previous);
        self.viewport_feedback_intent.set(None);
        if let Some(body) = body.upgrade() {
            body.bind_find_highlights(Rc::downgrade(self), controller);
        }
    }

    /// query/选项请求一次计数反馈，替换输入只清旧文案；不增加第二套输入代次。
    pub(super) fn stage_viewport_feedback(&self, intent: u64, refresh: bool) {
        self.viewport_feedback_intent.set(refresh.then_some(intent));
    }

    /// 原字段身份决定输入后果：query刷新可见匹配，replacement仅撤旧替换反馈。
    pub(super) fn input_refresh_for_field(&self, field: &NSTextField) -> Option<bool> {
        if std::ptr::eq(&*self.query, field) {
            Some(true)
        } else if std::ptr::eq(&*self.replacement, field) {
            Some(false)
        } else {
            None // 其他字段不能借本栏的Doc或输入代次。
        }
    }

    /// 只认本栏原三个实例，不根据任意tag或同名标题授予选项变化权限。
    pub(super) fn owns_search_option(&self, button: &NSButton) -> bool {
        [&self.case, &self.regex, &self.word]
            .iter()
            .any(|actual| std::ptr::eq(&***actual, button))
    }

    /// 纯标量查询不保留字段或调用原生 getter，可用于 Escape 的当前 owner 判定。
    pub(super) fn is_visible(&self) -> bool {
        self.visible.get() && !self.closed.get()
    }

    /// 真实显示状态决定唯一高度；调用方不缓存第二份Find模式或正文占用。
    pub(super) fn occupied_height(&self) -> f64 {
        if self.is_visible() {
            find_bar_height(self.replace_visible.get())
        } else {
            0.0
        }
    }

    /// 反馈显隐和resize只移动原root并重排原字段，权限仍由同bar的原布局代次核验。
    pub(super) fn position_in_document_area(
        &self,
        frame: NSRect,
        controller: &NativeMarkdownController,
    ) -> bool {
        let Some(revision) = self.next_layout_revision() else {
            return false;
        };
        let area_revision = controller.document_area_layout_revision();
        let current = || {
            self.is_visible()
                && self.layout_revision.get() == revision
                && controller.document_area_layout_revision() == area_revision
                && native_markdown_controller_has_foreground_authority(controller)
        };
        if !current() {
            return false;
        }
        self.root.setFrame(frame);
        if !current() {
            return false;
        }
        self.apply_layout(frame.size.width, controller, revision)
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn visible_frame_for_probe(&self) -> Option<NSRect> {
        self.is_visible().then(|| self.root.frame())
    }

    /// 仅作原请求成功收据的UI附证，不以旧文案或slot消失推断定位成功。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn probe_found_feedback(&self) -> bool {
        self.status
            .stringValue()
            .isEqualToString(ns_string!("找到匹配"))
    }

    /// 只观察原控件有限状态值，供旧输入反馈不得覆盖显式动作的真实竞态回归。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn probe_feedback_status(&self) -> Retained<NSString> {
        self.status.stringValue()
    }

    /// 回归只借用原动作对应的真实按钮；不创建 sender、控件或额外状态。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn probe_button(&self, action: MarkdownFindAction) -> &NSButton {
        let index = match action {
            MarkdownFindAction::Previous => 0,
            MarkdownFindAction::Next => 1,
            MarkdownFindAction::SelectAll => 2,
            MarkdownFindAction::ReplaceNext => 3,
            MarkdownFindAction::ReplaceAll => 4,
            MarkdownFindAction::Hide => 5,
        }; // 同一构造顺序的固定六按钮；动作枚举不能扩展为任意视图查询。
        &self.buttons[index]
    }

    /// 先构造并登记固定角色，再挂载到实际 editor 的父视图；失败不发布不完整栏。
    fn new(
        controller: &NativeMarkdownController,
        ledger: &NativeMarkdownOwnedLedger,
        document: WeakMarkdownDocument,
        mtm: MainThreadMarker,
    ) -> Result<Rc<Self>, String> {
        let root = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
        root.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
        );
        ledger.include("find.root", &root)?;
        // 文档查找与侧栏笔记搜索范围不同；占位和无障碍名称都明确当前文档，不改查询值。
        let query = Self::field(
            "在当前文档中查找",
            MarkdownFindAction::Next,
            controller,
            ledger,
            mtm,
        )?;
        // 栏根随父窗口变宽；子字段只接受统一几何，不能再被独立弹簧二次平移。
        query.setAutoresizingMask(NSAutoresizingMaskOptions::ViewNotSizable);
        let replacement = Self::field(
            "替换为",
            MarkdownFindAction::ReplaceNext,
            controller,
            ledger,
            mtm,
        )?;
        replacement.setAutoresizingMask(NSAutoresizingMaskOptions::ViewNotSizable);
        let case = Self::checkbox("区分大小写", controller, ledger, mtm)?;
        let regex = Self::checkbox("正则表达式", controller, ledger, mtm)?;
        regex.setToolTip(Some(ns_string!(
            "使用原生 ICU 正则表达式；遇到不支持的语法时提示错误并保留文档内容。"
        )));
        let word = Self::checkbox("全字匹配", controller, ledger, mtm)?;
        let mut buttons = Vec::new();
        for (title, action) in [
            ("上一个", MarkdownFindAction::Previous),
            ("下一个", MarkdownFindAction::Next),
            ("全部选中", MarkdownFindAction::SelectAll),
            ("替换", MarkdownFindAction::ReplaceNext),
            ("全部替换", MarkdownFindAction::ReplaceAll),
            ("×", MarkdownFindAction::Hide),
        ] {
            // 固定六个按钮各自登记 control/cell；动作只携带有限 tag，不持查询或正文。
            // SAFETY: 同步构造期间 controller 存活，NSButton target 不强持有它；唯一 action ABI 已实现。
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(title),
                    Some(controller),
                    Some(sel!(performMarkdownFindAction:)),
                    mtm,
                )
            };
            button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewNotSizable);
            button.setTag(action as isize);
            button.setBordered(true);
            button.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            ledger.include_control("find.button", &button)?;
            root.addSubview(&button);
            buttons.push(button);
        }
        // 关闭符号保留紧凑外观，但朗读与悬停必须说明其实际中文操作。
        buttons[5].setAccessibilityLabel(Some(ns_string!("关闭查找栏")));
        buttons[5].setToolTip(Some(ns_string!("关闭查找栏")));
        let status = NSTextField::labelWithString(ns_string!(""), mtm);
        status.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        // 状态留在选项行的独立剩余区，宽度由统一几何计算，不与动作争用同一行。
        status.setAutoresizingMask(NSAutoresizingMaskOptions::ViewNotSizable);
        ledger.include_control("find.status", &status)?;
        root.addSubview(&query);
        root.addSubview(&replacement);
        root.addSubview(&case);
        root.addSubview(&regex);
        root.addSubview(&word);
        root.addSubview(&status);
        // 控件装配完成后仅登记原bar的Rust Weak；Controller不能成为第二个栏或文稿owner。
        let bar = Rc::new(Self {
            document,
            presentation_body: RefCell::new(None),
            viewport_feedback_intent: Cell::new(None),
            query_marked: Cell::new(false),
            replacement_marked: Cell::new(false),
            root,
            query,
            replacement,
            case,
            regex,
            word,
            buttons,
            status,
            visible: Cell::new(false),
            replace_visible: Cell::new(false),
            closed: Cell::new(false),
            layout_revision: Cell::new(0),
        });
        controller.register_find_input_bar(&bar);
        Ok(bar)
    }

    /// 普通 NSTextFieldCell 不覆写 fieldEditorForView，实际输入仍由窗口自有 provider 提供。
    fn field(
        title: &str,
        action: MarkdownFindAction,
        controller: &NativeMarkdownController,
        ledger: &NativeMarkdownOwnedLedger,
        mtm: MainThreadMarker,
    ) -> Result<Retained<NSTextField>, String> {
        let field = NSTextField::textFieldWithString(ns_string!(""), mtm);
        field.setPlaceholderString(Some(&NSString::from_str(title)));
        field.setAccessibilityLabel(Some(&NSString::from_str(title)));
        field.setTag(action as isize);
        // 查询/替换只有真实Return或按钮才执行动作；失焦、validate或切换同一field editor
        // 不能偷偷发送Next/ReplaceNext。两项明确设置不依赖NSTextField当前系统默认值。
        field.setContinuous(false);
        let cell = field.cell().ok_or("查找字段缺少原生输入单元")?;
        cell.setSendsActionOnEndEditing(false);
        drop(cell); // 仍为原control自有的同一个cell，不新增角色或跨栏保存临时Native claim。
        // SAFETY: controller 由同一窗口强拥有；字段 delegate/target 在 accepted release 清理后才 drop。
        unsafe {
            field.setDelegate(Some(ProtocolObject::from_ref(controller)));
            field.setTarget(Some(controller));
            field.setAction(Some(sel!(performMarkdownFindAction:)));
        }
        ledger.include_control("find.input", &field)?;
        Ok(field)
    }

    /// 选项只有 native scalar state；没有观察器、timer 或永久查询缓存。
    fn checkbox(
        title: &str,
        controller: &NativeMarkdownController,
        ledger: &NativeMarkdownOwnedLedger,
        mtm: MainThreadMarker,
    ) -> Result<Retained<NSButton>, String> {
        // SAFETY: 原窗口拥有controller，标准按钮不强持target；accepted清理会断开同一入口。
        let button = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str(title),
                Some(controller),
                Some(sel!(markdownFindOptionsDidChange:)),
                mtm,
            )
        };
        button.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewNotSizable);
        ledger.include_control("find.option", &button)?;
        Ok(button)
    }

    /// 调整自有sibling；所有阶段持同一布局代次，并核对Active或确切借出owner。
    fn show(
        self: &Rc<Self>,
        scroll: &NSScrollView,
        replace: bool,
        controller: &NativeMarkdownController,
    ) -> Result<(), String> {
        if self.closed.get() || !native_markdown_controller_has_foreground_authority(controller) {
            return Err("查找栏所属笔记窗口正在关闭".into());
        }
        // 每个真实show入口都登记同一Weak；直接embedding没有Window绑定，不伪建另一图。
        controller.bind_find_bar(scroll, self)?;
        let revision = self.next_layout_revision().ok_or("查找栏布局代次已耗尽")?;
        let area_revision = controller.document_area_layout_revision();
        let current = || {
            !self.closed.get()
                && self.layout_revision.get() == revision
                && controller.document_area_layout_revision() == area_revision
                && native_markdown_controller_has_foreground_authority(controller)
        };
        let ensure_current = || {
            current()
                .then_some(())
                .ok_or_else(|| "查找栏所属笔记窗口正在关闭或布局已更新".to_owned())
        };
        ensure_current()?;
        // SAFETY: 活scroll的父视图只在此同步事务中使用；getter后再次核原owner。
        let parent = unsafe { scroll.superview() }.ok_or("笔记编辑器尚未就绪")?;
        ensure_current()?;
        let bounds = parent.bounds();
        ensure_current()?;
        let base = controller.document_area_base(scroll, bounds)?;
        ensure_current()?;
        let new_height = find_bar_height(replace);
        let frames =
            header_layout::editor_area_frames(base, controller.feedback_visible(), new_height)
                .ok_or("查找栏和正文没有合法可用布局")?;
        let frame = NSRect::new(
            NSPoint::new(frames.scroll.x, frames.scroll.y),
            NSSize::new(frames.scroll.width, frames.scroll.height),
        );
        scroll.setFrame(frame);
        ensure_current()?;
        if !self.visible.replace(true) {
            // 首次挂载只用原parent，关闭/后继resize撤权后不补建或重挂栏。
            parent.addSubview(&self.root);
            ensure_current()?;
        }
        self.root.setFrame(NSRect::new(
            NSPoint::new(frame.origin.x, frame.origin.y + frame.size.height),
            NSSize::new(frame.size.width, new_height),
        ));
        ensure_current()?;
        self.replace_visible.set(replace);
        self.replacement.setHidden(!replace);
        ensure_current()?;
        self.buttons[3].setHidden(!replace);
        ensure_current()?;
        self.buttons[4].setHidden(!replace);
        ensure_current()?;
        if !self.apply_layout(frame.size.width, controller, revision) {
            // 此布局被关闭或新resize撤权；不在旧栈重领新代次继续夺焦点。
            return Err("查找栏布局已更新，请重试".into());
        }
        ensure_current()?;
        if self.query_has_native_focus(controller, &current)? {
            // 已在原查询内时保留实际 caret、选区和中文候选；重复打开不再触发标准全选。
            return ensure_current();
        }
        ensure_current()?;
        // SAFETY: 正常活字段selectText保留系统输入/IME/Undo；此后亦核同一owner和代次。
        unsafe { self.query.selectText(None) };
        ensure_current()
    }

    /// 用真实字段和第一响应者确认现有焦点；共享 editor 地址本身不能区分查询与替换。
    fn query_has_native_focus(
        self: &Rc<Self>,
        controller: &NativeMarkdownController,
        layout_current: &impl Fn() -> bool,
    ) -> Result<bool, String> {
        let feedback = NativeMarkdownFindViewportFeedback::capture(
            self,
            controller,
            controller.find_input_intent(),
            false,
        )
        .ok_or("查找输入所属文稿已变化")?;
        let Some(identity) = controller
            .ivars()
            .field_editor_provider
            .current_input_client_identity()
        else {
            return Ok(false); // 尚无会话 editor 就不可能已聚焦查询，不为只读判断提前创建它。
        };
        let current = || {
            layout_current()
                && feedback.current(controller)
                && controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    == Some(identity)
        };
        let editor = native_markdown_table_intent_step(&current, || self.query.currentEditor())
            .map_err(|_| "查询焦点已变化")?;
        let Some(editor) = editor else {
            return Ok(false); // 查询尚未编辑；仍由原 selectText 执行标准聚焦和全选。
        };
        if !editor
            .downcast_ref::<NativeMarkdownSessionFieldEditor>()
            .is_some_and(|session| {
                session.belongs_to(&controller.ivars().undo, &controller.ivars().owned)
            })
        {
            return Err("查询输入不属于当前会话".into()); // 外来 editor 不能提供本会话的焦点或候选事实。
        }
        let (address, _) = identity;
        if std::ptr::from_ref(&*editor).addr() != address {
            return Err("查询输入已重新交接".into()); // 原生返回值必须属于首 getter 前固定的会话 owner。
        }
        let replacement =
            native_markdown_table_intent_step(&current, || self.replacement.currentEditor())
                .map_err(|_| "查询编辑路由已变化")?;
        let duplicate = replacement
            .as_ref()
            .is_some_and(|replacement| std::ptr::eq(&**replacement, &*editor));
        drop(replacement);
        if !current() || duplicate {
            return Err("查询与替换的实际输入归属无法确认".into()); // 不把同一个共享对象猜成查询焦点。
        }
        let window = native_markdown_table_intent_step(&current, || editor.window())
            .map_err(|_| "查询窗口已变化")?
            .ok_or("查询输入已经离开窗口")?;
        let responder = native_markdown_table_intent_step(&current, || window.firstResponder())
            .map_err(|_| "查询第一响应者已变化")?;
        let focused = responder
            .as_ref()
            .is_some_and(|responder| std::ptr::from_ref(&**responder).addr() == address);
        drop((responder, window));
        // 第一响应者 getter 及临时 claim 析构均可能交接；最终再次确认原字段仍使用原 editor。
        let confirmed = native_markdown_table_intent_step(&current, || self.query.currentEditor())
            .map_err(|_| "查询输入已重新交接")?;
        let same = confirmed
            .as_ref()
            .is_some_and(|confirmed| std::ptr::eq(&**confirmed, &*editor));
        drop((confirmed, editor));
        if !same || !current() {
            return Err("查询输入已重新交接".into()); // 不能用后继路由的字段或候选决定旧 show 是否夺焦点。
        }
        Ok(focused)
    }

    /// 纯标量代次不回绕；show与resize共享它，重入的新布局能撤旧序列。
    fn next_layout_revision(&self) -> Option<u64> {
        let revision = self.layout_revision.get().checked_add(1)?;
        self.layout_revision.set(revision);
        Some(revision)
    }

    /// 只改已有frame；每次Native调用前后同时核代次、可见性、关闭与原owner。
    fn apply_layout(
        &self,
        width: f64,
        controller: &NativeMarkdownController,
        revision: u64,
    ) -> bool {
        let Some(frames) = find_frames(width, self.replace_visible.get()) else {
            // 非法尺寸不发布部分矩形，也不读写输入值。
            return false;
        };
        let current = || {
            geometry::find_layout_is_current(
                revision,
                self.layout_revision.get(),
                self.visible.get(),
                self.closed.get(),
                native_markdown_controller_has_foreground_authority(controller),
            )
        };
        let native = |frame: geometry::FindControlFrame| {
            NSRect::new(
                NSPoint::new(frame.x, frame.y),
                NSSize::new(frame.width, frame.height),
            )
        };
        if !current() {
            return false; // 较新resize或关闭已经取得权限。
        }
        self.query.setFrame(native(frames.query));
        if !current() {
            return false; // query setter重入后不再写replacement。
        }
        self.replacement.setFrame(native(frames.replacement));
        for (button, frame) in self.buttons.iter().zip(frames.buttons) {
            // 每个按钮前检查也覆盖上一setter的返回，固定顺序不改变动作/tag。
            if !current() {
                return false;
            }
            button.setFrame(native(frame));
        }
        for (option, frame) in [&self.case, &self.regex, &self.word]
            .into_iter()
            .zip(frames.options)
        {
            // 选项scalar不变；旧Rc的存活本身不授予关闭后frame写入权限。
            if !current() {
                return false;
            }
            option.setFrame(native(frame));
        }
        if !current() {
            return false; // 最后状态也必须属于原代次和活窗口。
        }
        self.status.setFrame(native(frames.status));
        current()
    }

    /// 隐藏只结束当前客户端并回到正文；活动笔记内仍保留查询/选项供 Cmd-G 使用。
    pub(super) fn hide(
        &self,
        scroll: &NSScrollView,
        view: &NSView,
        controller: &NativeMarkdownController,
    ) -> Result<bool, String> {
        if !self.visible.get() {
            return Ok(false); // 没有可消费 Escape 的 Find 界面。
        }
        let revision = self.next_layout_revision().ok_or("查找栏布局代次已耗尽")?;
        let area_revision = controller.document_area_layout_revision();
        let current = || {
            self.is_visible()
                && self.layout_revision.get() == revision
                && controller.document_area_layout_revision() == area_revision
                && native_markdown_controller_has_foreground_authority(controller)
        };
        let ensure_current = || {
            current()
                .then_some(())
                .ok_or_else(|| "查找栏所属笔记窗口正在关闭或布局已更新".to_owned())
        };
        ensure_current()?;
        let window = self.root.window();
        ensure_current()?;
        if let Some(window) = window {
            // 正常辞任可拒绝；拒绝时原栏和值保持可编辑，不清空或伪装已隐藏。
            if !window.makeFirstResponder(Some(view)) {
                return Err("查找输入尚未结束，请完成输入后重试".into());
            }
            ensure_current()?;
        }
        self.root.removeFromSuperview();
        ensure_current()?;
        // SAFETY: 原scroll仍由同笔记拥有；只读取父内容尺寸，绝不叠加旧Find占用。
        let parent = unsafe { scroll.superview() }.ok_or("笔记正文已经关闭")?;
        ensure_current()?;
        let bounds = parent.bounds();
        ensure_current()?;
        let base = controller.document_area_base(scroll, bounds)?;
        ensure_current()?;
        let frames = header_layout::editor_area_frames(base, controller.feedback_visible(), 0.0)
            .ok_or("笔记正文没有合法可用布局")?;
        scroll.setFrame(NSRect::new(
            NSPoint::new(frames.scroll.x, frames.scroll.y),
            NSSize::new(frames.scroll.width, frames.scroll.height),
        ));
        ensure_current()?;
        self.visible.set(false);
        self.query_marked.set(false);
        self.replacement_marked.set(false);
        Ok(true)
    }

    /// 资源账本需要所有自有输入，包括已隐藏但仍保留本笔记查询的 replacement 字段。
    pub(super) fn fields(&self) -> [Retained<NSTextField>; 2] {
        [self.query.clone(), self.replacement.clone()]
    }

    /// accepted 才清空业务值与全部项目回调；正常失败预检不调用此不可逆路径。
    pub(super) fn finish_accepted(&self) {
        self.closed.set(true);
        let binding = self.presentation_body.borrow_mut().take();
        if let Some(body) = binding.as_ref().and_then(std::rc::Weak::upgrade) {
            body.clear_find_highlights_binding();
        }
        drop(binding); // Native weak 的归还不跨栏或 Body 的 RefCell 借用。
        self.viewport_feedback_intent.set(None);
        self.query_marked.set(false);
        self.replacement_marked.set(false);
        self.root.removeFromSuperview();
        for field in [&self.query, &self.replacement] {
            // provider 已结束实际客户端与自有历史；字段不持旧字符串、placeholder 或回调 target。
            field.setStringValue(ns_string!(""));
            field.setPlaceholderString(None);
            field.setEditable(false);
            field.setSelectable(false);
            // SAFETY: 本项目创建的字段明确拥有这些 callback 配置，不改框架未知 delegate。
            unsafe {
                field.setDelegate(None);
                field.setTarget(None);
                field.setAction(None);
            }
        }
        for button in self
            .buttons
            .iter()
            .chain([&self.case, &self.regex, &self.word])
        {
            // 原动作失去 controller 入口；迟到消息仍须通过 panel/controller 当前身份门禁。
            // SAFETY: 清理本项目设置的 target/action，按钮仍由局部 owner 保持有效。
            unsafe {
                button.setTarget(None);
                button.setAction(None);
            }
        }
        self.status.setStringValue(ns_string!(""));
        self.visible.set(false);
    }
}

/// 项目菜单和实际快捷键都通过当前 owner 的同一建立入口。
pub(super) fn show_native_markdown_find_bar(replace: bool) -> Result<(), String> {
    with_active_native_markdown_surface(|surface| {
        let editor = surface.editor.as_ref().ok_or("当前没有打开的笔记")?;
        // 字段取得焦点前结束原活动格，预填查询使用真实清洗后的 canonical 选择。
        editor
            .body
            .finish_cell_before_find(&surface.controller, surface.controller.find_input_intent())
            .map_err(|cause| cause.to_string())?;
        let existing = editor.find_bar().borrow().clone();
        let bar = match existing {
            Some(bar) => bar, // 同一笔记重复显示保留原查询和历史。
            None => {
                // 所有构造角色登记成功后才发布；外层唯一 owner lease 拒绝构造期重入关闭。
                let bar = NativeMarkdownFindBar::new(
                    &surface.controller,
                    &editor.find_owned_ledger(),
                    Rc::downgrade(editor.document()),
                    surface.panel.mtm(),
                )?;
                bar.bind_body(Rc::downgrade(&editor.body), &surface.controller);
                *editor.find_bar().borrow_mut() = Some(Rc::clone(&bar));
                bar
            }
        };
        // 重复显示借原栏与原布局反馈；只有上方首次装配或明确模式交接才改正文弱绑定。
        if !native_markdown_controller_has_foreground_authority(&surface.controller)
            || bar.closed.get()
        {
            return Err("查找栏所属笔记窗口正在关闭".into());
        }
        let query_is_idle = bar.query.currentEditor().is_none();
        if !native_markdown_controller_has_foreground_authority(&surface.controller)
            || bar.closed.get()
        {
            return Err("查找栏所属笔记窗口正在关闭".into());
        }
        if query_is_idle {
            let selection = editor
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .primary_range;
            if selection.length > 0 && selection.length <= 100 {
                // 与原编辑器相同：离开正文后打开 Find，以至多 100 units 的选择预填，不读全局 Find pasteboard。
                let selected = editor
                    .document()
                    .borrow()
                    .materialize_viewport(
                        selection.location..selection.location.saturating_add(selection.length),
                        0,
                    )
                    .map_err(|error| error.to_string())?
                    .text;
                if !native_markdown_controller_has_foreground_authority(&surface.controller)
                    || bar.closed.get()
                {
                    return Err("查找栏所属笔记窗口正在关闭".into());
                }
                bar.query
                    .setStringValue(&NSString::from_str(&selected.replace('\n', "\\n")));
                if !native_markdown_controller_has_foreground_authority(&surface.controller)
                    || bar.closed.get()
                {
                    return Err("查找栏所属笔记窗口正在关闭".into());
                }
            }
        }
        bar.show(editor.scroll_view(), replace, &surface.controller)?;
        editor.capture_find_resources()
    })
}

/// 同步动作只借用活笔记；所有 native getter/匹配调用均在状态借用结束后执行。
pub(super) fn perform_native_markdown_find_action(
    action: MarkdownFindAction,
) -> Result<NativeMarkdownFindExecution, String> {
    let (bar, body, scroll, controller, intent) = with_active_native_markdown_surface(|surface| {
        let editor = surface.editor.as_ref().ok_or("当前没有打开的笔记")?;
        let bar = editor.find_bar.borrow().clone().ok_or("查找栏尚未打开")?;
        let intent = surface.controller.advance_find_input_intent();
        bar.viewport_feedback_intent.set(None);
        Ok((
            bar,
            Rc::clone(&editor.body),
            editor.scroll_view.clone(),
            surface.controller.clone(),
            intent,
        ))
    })?;
    if action == MarkdownFindAction::Hide {
        // 隐藏沿原 Body 锚保留真实视口；不能因关闭栏改变 clip 而将已测前缀误当全文末端。
        body.finish_cell_before_find(&controller, intent)
            .map_err(|cause| cause.to_string())?;
        body.hide_find_bar(&bar, &scroll, &controller)?;
        return Ok(NativeMarkdownFindExecution::Complete);
    }
    match body.execute_paged_find_action(&bar, &controller, intent, action) {
        Ok(NativeMarkdownFindExecution::Complete) => Ok(NativeMarkdownFindExecution::Complete),
        Ok(NativeMarkdownFindExecution::Pending(request)) => {
            // 返回原在途请求；验收观察真实发布终点，普通界面可忽略已接纳结果。
            if let Some(cause) = request.take_failure() {
                Err(cause.to_string())
            } else {
                Ok(NativeMarkdownFindExecution::Pending(request))
            }
        }
        Err(cause) => Err(cause.to_string()),
    }
}
