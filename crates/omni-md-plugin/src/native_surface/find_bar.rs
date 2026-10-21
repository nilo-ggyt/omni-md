//! Owned Find/Replace bar uses the window's session field editor/undo without taking over private framework controls.

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
    feature = "native-paged-body-probe",
    feature = "native-markdown-runtime-probe"
))]
pub use matching::backend::run_native_paged_find_body_control;
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

/// Key commands belong to this note only; showing and matching are separate, so navigation first opens an absent query bar.
pub(super) enum MarkdownFindKeyCommand {
    Show(bool),
    Action(MarkdownFindAction),
}

/// Accept explicit local Command combinations only, preserving system input-source switching and Control bindings.
pub(super) fn markdown_find_key_command(event: &NSEvent) -> Option<MarkdownFindKeyCommand> {
    let relevant = event.modifierFlags()
        & (NSEventModifierFlags::Command
            | NSEventModifierFlags::Option
            | NSEventModifierFlags::Control
            | NSEventModifierFlags::Shift);
    match (event.keyCode(), relevant) {
        (3, flags) if flags == NSEventModifierFlags::Command => {
            Some(MarkdownFindKeyCommand::Show(false))
        } // Cmd-F opens Find only; the separate existing command explicitly expands replacement.
        (3, flags) if flags == (NSEventModifierFlags::Command | NSEventModifierFlags::Option) => {
            Some(MarkdownFindKeyCommand::Show(true))
        } // Existing Option-Cmd-F replacement command.
        (5, flags) if flags == NSEventModifierFlags::Command => {
            Some(MarkdownFindKeyCommand::Action(MarkdownFindAction::Next))
        } // Existing forward navigation command.
        (5, flags) if flags == (NSEventModifierFlags::Command | NSEventModifierFlags::Shift) => {
            Some(MarkdownFindKeyCommand::Action(MarkdownFindAction::Previous))
        } // Existing reverse navigation command.
        _ => None, // Other keys continue through the native responder chain.
    }
}

/// Share the menu entry point; an absent bar opens for explicit input without reading the shared Find pasteboard.
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
                // A bar already bound to the current editor can execute its local navigation transaction.
                perform_native_markdown_find_action(action).map(|_| ())
            } else {
                // Showing an absent bar cannot invent a query or start matching from another application's search state.
                show_native_markdown_find_bar(false)
            }
        }
    }
}

/// Return applies only to this bar's actual field editor; marked text stays with the system instead of premature matching.
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
        return None; // Other keys and system modifier combinations are not bar Return commands.
    }
    let bar = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        active
            .as_ref()
            .and_then(|surface| surface.editor.as_ref())
            .and_then(|editor| editor.find_bar().borrow().clone())
    })?;
    if !bar.is_visible() {
        // A hidden bar cannot intercept Return from body, sidebar, or table input.
        return None;
    }
    let responder = panel.firstResponder()?.downcast::<NSTextView>().ok()?;
    // SAFETY: The live main-thread NSTextView implements public NSTextInputClient; this queries actual composition only.
    if unsafe { msg_send![&*responder, hasMarkedText] } {
        // Return during composition belongs to the IME; uncommitted query text cannot authorize matching.
        return None;
    }
    let current_is = |field: &NSTextField| {
        field.currentEditor().is_some_and(|editor| {
            std::ptr::from_ref(&*editor).cast::<()>()
                == std::ptr::from_ref(&*responder).cast::<()>()
        })
    };
    if current_is(&bar.query) {
        // Query Return navigates from its current committed input; Shift retains the existing reverse direction.
        Some(
            if event.modifierFlags().contains(NSEventModifierFlags::Shift) {
                MarkdownFindAction::Previous
            } else {
                MarkdownFindAction::Next
            },
        )
    } else if bar.replace_visible.get() && current_is(&bar.replacement) {
        Some(MarkdownFindAction::ReplaceNext) // Replacement Return retains the existing replace-next behavior.
    } else {
        None // Other search/sidebar/table/body clients retain their native input routes.
    }
}

/// Fixed actions apply within the current note only, never as Host/plugin identity or cross-session authority.
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
    /// Sender tags must belong to this bar's finite action set; unknown values grant no execution authority.
    pub(super) fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            1 => Some(Self::Next),        // Forward navigation.
            2 => Some(Self::Previous),    // Reverse navigation.
            3 => Some(Self::SelectAll),   // Bounded multiple selection.
            4 => Some(Self::ReplaceNext), // Replace the current match and advance.
            5 => Some(Self::ReplaceAll),  // Replace all within one transaction.
            6 => Some(Self::Hide),        // Hide this bar only.
            _ => None,                    // Arbitrary tags/selectors cannot execute.
        }
    }
}

/// A bar weakly binds its actual presentation backend; mode handoff cannot retain drawing authority for two body graphs.
pub(super) struct NativeMarkdownFindBar {
    /// Fix original document weak identity at construction; queued input cannot recapture authority from a successor note.
    document: WeakMarkdownDocument,
    /// Production assembly weakly observes its original graph; paged feedback never constructs a legacy TextKit body.
    presentation_body: RefCell<Option<std::rc::Weak<super::paged_body::NativeMarkdownPagedBody>>>,
    /// Track only the original input awaiting feedback; scroll-driven highlight refresh cannot overwrite explicit navigation status.
    viewport_feedback_intent: Cell<Option<u64>>,
    /// Track composition state of the two real fields only, without candidate text or another input history.
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
    /// Synchronous resize publishes one frame generation; newer resize or accepted close revokes old setters.
    layout_revision: Cell<u64>,
}

impl NativeMarkdownFindBar {
    /// Weakly bind the current body only; mode switch neither rebuilds the bar nor keeps another native/body graph alive.
    pub(super) fn bind_body(
        self: &Rc<Self>,
        body: std::rc::Weak<super::paged_body::NativeMarkdownPagedBody>,
        controller: &NativeMarkdownController,
    ) {
        let previous = self.presentation_body.replace(Some(body.clone()));
        if let Some(previous) = previous.as_ref().and_then(std::rc::Weak::upgrade) {
            // Revoke the prior graph's highlight route before attaching this bar to its successor backend.
            previous.unbind_find_highlights(&Rc::downgrade(self));
        }
        drop(previous);
        self.viewport_feedback_intent.set(None);
        if let Some(body) = body.upgrade() {
            // Only an actual surviving original body can receive the bar's weak highlight binding.
            body.bind_find_highlights(Rc::downgrade(self), controller);
        }
    }

    /// Query/options request count feedback; replacement input only clears stale status, using the same input generation.
    pub(super) fn stage_viewport_feedback(&self, intent: u64, refresh: bool) {
        self.viewport_feedback_intent.set(refresh.then_some(intent));
    }

    /// Exact field identity controls effects: query refreshes visible matches, while replacement revokes stale replacement feedback.
    pub(super) fn input_refresh_for_field(&self, field: &NSTextField) -> Option<bool> {
        if std::ptr::eq(&*self.query, field) {
            Some(true)
        } else if std::ptr::eq(&*self.replacement, field) {
            Some(false)
        } else {
            None // Other fields cannot borrow this bar's document or input generation.
        }
    }

    /// Recognize these three actual option instances, never granting authority from an arbitrary tag or equal title.
    pub(super) fn owns_search_option(&self, button: &NSButton) -> bool {
        [&self.case, &self.regex, &self.word]
            .iter()
            .any(|actual| std::ptr::eq(&***actual, button))
    }

    /// Scalar visibility retains no field and calls no native getter, allowing Escape to check current ownership.
    pub(super) fn is_visible(&self) -> bool {
        self.visible.get() && !self.closed.get()
    }

    /// Actual visibility determines the sole height; callers keep no second Find mode or occupied-body-height cache.
    pub(super) fn occupied_height(&self) -> f64 {
        if self.is_visible() {
            find_bar_height(self.replace_visible.get())
        } else {
            0.0
        }
    }

    /// Feedback/resize moves the original root/fields only, preserving the same bar's captured layout-generation authority.
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
    /// Observe this visible bar's real native frame without constructing a probe view.
    pub(super) fn visible_frame_for_probe(&self) -> Option<NSRect> {
        self.is_visible().then(|| self.root.frame())
    }

    /// Host fixtures know only initial candidates, not document-wide totals; this recognizes success-status shape as supplementary evidence.
    /// Actual success still requires original request completion and a full frame from the same Scene, not this text predicate.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn probe_found_feedback(&self) -> bool {
        let value = self.status.stringValue();
        if value.length() > 64 {
            return false; // Success text contains two usize numbers and short labels only; do not copy arbitrary long status.
        }
        let value = value.to_string();
        let Some(numbers) = value
            .strip_prefix("第 ")
            .and_then(|value| value.strip_suffix(" 处匹配"))
        else {
            return false;
        };
        let Some((ordinal, total)) = numbers.split_once(" / ") else {
            return false;
        };
        let (Ok(ordinal), Ok(total)) = (ordinal.parse::<usize>(), total.parse::<usize>()) else {
            return false;
        };
        // Canonical reconstruction excludes signs, leading zeroes, and added text, without proving the source's actual match count.
        ordinal != 0 && ordinal <= total && value == format!("第 {ordinal} / {total} 处匹配")
    }

    /// Known synthetic sources require exact ordinal/total checks; shape recognition cannot weaken real navigation/wrap assertions.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn probe_numbered_feedback(&self, ordinal: usize, total: usize) -> bool {
        self.status
            .stringValue()
            .isEqualToString(&NSString::from_str(&format!(
                "第 {ordinal} / {total} 处匹配"
            )))
    }

    /// Observe original control status for the real race where old input feedback must not overwrite explicit actions.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn probe_feedback_status(&self) -> Retained<NSString> {
        self.status.stringValue()
    }

    /// Borrow the original action's actual button, without a fabricated sender, control, or parallel state.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn probe_button(&self, action: MarkdownFindAction) -> &NSButton {
        let index = match action {
            MarkdownFindAction::Previous => 0,
            MarkdownFindAction::Next => 1,
            MarkdownFindAction::SelectAll => 2,
            MarkdownFindAction::ReplaceNext => 3,
            MarkdownFindAction::ReplaceAll => 4,
            MarkdownFindAction::Hide => 5,
        }; // Six fixed buttons share construction order; the action enum cannot query arbitrary views.
        &self.buttons[index]
    }

    /// Construct/register fixed roles before mounting on the actual editor parent; failure publishes no incomplete bar.
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
        // Document Find differs from workspace note search; placeholder/accessibility names identify scope without rewriting query text.
        let query = Self::field(
            "在当前文档中查找",
            MarkdownFindAction::Next,
            controller,
            ledger,
            mtm,
        )?;
        // Root follows window width; child fields consume unified geometry without separate autoresizing that could shift them twice.
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
            // Register control/cell for six fixed buttons; action tags retain no query or document text.
            // SAFETY: Controller remains live during construction; NSButton has a nonowning target and the implemented action ABI.
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
            // Like body toolbar controls, action buttons do not take first responder from query/replacement fields.
            // Actual mouse input retains NSButton tracking/target behavior while selection/composition stays with the original client.
            button.setRefusesFirstResponder(true);
            button.setBordered(true);
            button.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            ledger.include_control("find.button", &button)?;
            root.addSubview(&button);
            buttons.push(button);
        }
        // Keep the compact close glyph while accessibility/tooltip explain its actual localized action.
        buttons[5].setAccessibilityLabel(Some(ns_string!("关闭查找栏")));
        buttons[5].setToolTip(Some(ns_string!("关闭查找栏")));
        let status = NSTextField::labelWithString(ns_string!(""), mtm);
        status.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        // Status occupies the options row's remaining area from unified geometry, without competing with action buttons.
        status.setAutoresizingMask(NSAutoresizingMaskOptions::ViewNotSizable);
        ledger.include_control("find.status", &status)?;
        root.addSubview(&query);
        root.addSubview(&replacement);
        root.addSubview(&case);
        root.addSubview(&regex);
        root.addSubview(&word);
        root.addSubview(&status);
        // After assembly register only the bar's Rust Weak; controller cannot become a second bar or document owner.
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

    /// Standard NSTextFieldCell does not override fieldEditorForView; the window's owned provider still supplies real input.
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
        // Query/replacement actions require actual Return or buttons; focus loss, validation, or shared-editor handoff
        // must not implicitly send Next/ReplaceNext. Set both policies explicitly instead of relying on system defaults.
        field.setContinuous(false);
        let cell = field.cell().ok_or("查找字段缺少原生输入单元")?;
        cell.setSendsActionOnEndEditing(false);
        drop(cell); // The same control still owns its cell; retain no temporary native claim across bars.
        // SAFETY: The window retains controller; accepted release detaches field delegate/target before their owners drop.
        unsafe {
            field.setDelegate(Some(ProtocolObject::from_ref(controller)));
            field.setTarget(Some(controller));
            field.setAction(Some(sel!(performMarkdownFindAction:)));
        }
        ledger.include_control("find.input", &field)?;
        Ok(field)
    }

    /// Options contain native scalar state only, with no observer, timer, or permanent query cache.
    fn checkbox(
        title: &str,
        controller: &NativeMarkdownController,
        ledger: &NativeMarkdownOwnedLedger,
        mtm: MainThreadMarker,
    ) -> Result<Retained<NSButton>, String> {
        // SAFETY: The original window owns controller; the button's nonowning target is detached by accepted cleanup.
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

    /// Adjust owned siblings under one layout generation, checking Active or the exact leased owner throughout.
    fn show(
        self: &Rc<Self>,
        scroll: &NSScrollView,
        replace: bool,
        controller: &NativeMarkdownController,
    ) -> Result<(), String> {
        if self.closed.get() || !native_markdown_controller_has_foreground_authority(controller) {
            // A retained bar cannot acquire focus or layout authority after its original foreground closes.
            return Err("查找栏所属笔记窗口正在关闭".into());
        }
        // Every actual show registers the same Weak; direct embedding has no window binding and creates no substitute graph.
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
        // SAFETY: Borrow the live scroll's parent for this synchronous transaction only, rechecking original ownership after the getter.
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
            // Initial mount uses the original parent only; close or newer resize cannot authorize reconstruction or remount.
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
            // Close/new resize revoked this layout; the old stack cannot recapture a new generation and continue stealing focus.
            return Err("查找栏布局已更新，请重试".into());
        }
        ensure_current()?;
        if self.query_has_native_focus(controller, &current)? {
            // Preserve actual caret, selection, and Chinese candidates when query already has focus; repeated show cannot reselect all.
            return ensure_current();
        }
        ensure_current()?;
        // SAFETY: Normal selectText preserves system input/IME/undo, with original owner/generation checked after dispatch too.
        unsafe { self.query.selectText(None) };
        ensure_current()
    }

    /// Confirm focus using actual fields and first responder; a shared editor address alone cannot distinguish query from replacement.
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
            return Ok(false); // Without a session editor, query cannot already have focus; this read-only check must not create one.
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
            return Ok(false); // An idle query still uses original selectText for standard focus and select-all behavior.
        };
        if !editor
            .downcast_ref::<NativeMarkdownSessionFieldEditor>()
            .is_some_and(|session| {
                session.belongs_to(&controller.ivars().undo, &controller.ivars().owned)
            })
        {
            return Err("查询输入不属于当前会话".into()); // A foreign editor cannot establish this session's focus/candidate facts.
        }
        let (address, _) = identity;
        if std::ptr::from_ref(&*editor).addr() != address {
            return Err("查询输入已重新交接".into()); // Native output must identify the session owner fixed before the first getter.
        }
        let replacement =
            native_markdown_table_intent_step(&current, || self.replacement.currentEditor())
                .map_err(|_| "查询编辑路由已变化")?;
        let duplicate = replacement
            .as_ref()
            .is_some_and(|replacement| std::ptr::eq(&**replacement, &*editor));
        drop(replacement);
        if !current() || duplicate {
            return Err("查询与替换的实际输入归属无法确认".into()); // Shared-object identity alone cannot establish query focus.
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
        // First-responder getters and temporary-claim drops may hand off input; finally confirm original field still uses original editor.
        let confirmed = native_markdown_table_intent_step(&current, || self.query.currentEditor())
            .map_err(|_| "查询输入已重新交接")?;
        let same = confirmed
            .as_ref()
            .is_some_and(|confirmed| std::ptr::eq(&**confirmed, &*editor));
        drop((confirmed, editor));
        if !same || !current() {
            return Err("查询输入已重新交接".into()); // Successor fields/candidates cannot authorize an old show's focus decision.
        }
        Ok(focused)
    }

    /// A nonwrapping scalar generation is shared by show/resize, allowing reentrant new layouts to revoke old setter sequences.
    fn next_layout_revision(&self) -> Option<u64> {
        let revision = self.layout_revision.get().checked_add(1)?;
        self.layout_revision.set(revision);
        Some(revision)
    }

    /// Change existing frames only, checking generation, visibility, close, and original ownership around native calls.
    fn apply_layout(
        &self,
        width: f64,
        controller: &NativeMarkdownController,
        revision: u64,
    ) -> bool {
        let Some(frames) = find_frames(width, self.replace_visible.get()) else {
            // Invalid geometry publishes no partial frame set and reads/writes no input values.
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
            return false; // A newer resize or close already owns layout authority.
        }
        self.query.setFrame(native(frames.query));
        if !current() {
            return false; // Query-setter reentry cannot continue writing replacement geometry.
        }
        self.replacement.setFrame(native(frames.replacement));
        for (button, frame) in self.buttons.iter().zip(frames.buttons) {
            // Before-button checks also cover the preceding setter return; fixed order preserves actions/tags.
            if !current() {
                return false;
            }
            button.setFrame(native(frame));
        }
        for (option, frame) in [&self.case, &self.regex, &self.word]
            .into_iter()
            .zip(frames.options)
        {
            // Option scalars stay unchanged; a surviving Rc cannot authorize frame writes after close.
            if !current() {
                return false;
            }
            option.setFrame(native(frame));
        }
        if !current() {
            return false; // Even final status geometry requires the original generation and live window.
        }
        self.status.setFrame(native(frames.status));
        current()
    }

    /// Hide ends the current client and returns to body, preserving this note's query/options for existing local navigation.
    pub(super) fn hide(
        &self,
        scroll: &NSScrollView,
        view: &NSView,
        controller: &NativeMarkdownController,
    ) -> Result<bool, String> {
        if !self.visible.get() {
            return Ok(false); // No visible Find interface can consume Escape.
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
            // Normal resignation may refuse; preserve editable original fields instead of clearing or claiming a hidden bar.
            if !window.makeFirstResponder(Some(view)) {
                return Err("查找输入尚未结束，请完成输入后重试".into());
            }
            ensure_current()?;
        }
        self.root.removeFromSuperview();
        ensure_current()?;
        // SAFETY: The same note still owns scroll; read parent bounds without adding obsolete Find occupancy twice.
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

    /// Resource tracking includes both owned inputs, including a hidden replacement field still carrying this note's query state.
    pub(super) fn fields(&self) -> [Retained<NSTextField>; 2] {
        [self.query.clone(), self.replacement.clone()]
    }

    /// Only accepted close clears business values and all project callbacks; refused preflight never enters this irreversible path.
    pub(super) fn finish_accepted(&self) {
        self.closed.set(true);
        let binding = self.presentation_body.borrow_mut().take();
        if let Some(body) = binding.as_ref().and_then(std::rc::Weak::upgrade) {
            body.clear_find_highlights_binding();
        }
        drop(binding); // Returning weak binding metadata crosses no bar/body RefCell borrow.
        self.viewport_feedback_intent.set(None);
        self.query_marked.set(false);
        self.replacement_marked.set(false);
        self.root.removeFromSuperview();
        for field in [&self.query, &self.replacement] {
            // Provider already ended the actual client/owned history; fields retain no old strings, placeholders, or callback target.
            field.setStringValue(ns_string!(""));
            field.setPlaceholderString(None);
            field.setEditable(false);
            field.setSelectable(false);
            // SAFETY: These project-created fields own this callback configuration; no unknown framework delegate is changed.
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
            // Original actions lose controller routing; late messages must still pass current panel/controller identity checks.
            // SAFETY: Clear only project-installed target/action while the local owner keeps each button live.
            unsafe {
                button.setTarget(None);
                button.setAction(None);
            }
        }
        self.status.setStringValue(ns_string!(""));
        self.visible.set(false);
    }
}

/// Project menus and existing local keys share the same current-owner construction entry.
pub(super) fn show_native_markdown_find_bar(replace: bool) -> Result<(), String> {
    with_active_native_markdown_surface(|surface| {
        let editor = surface.editor.as_ref().ok_or("当前没有打开的笔记")?;
        // End the original active cell before focusing a field; prefill uses its actual reconciled canonical selection.
        editor
            .body
            .finish_cell_before_find(&surface.controller, surface.controller.find_input_intent())
            .map_err(|cause| cause.to_string())?;
        let existing = editor.find_bar().borrow().clone();
        let bar = match existing {
            Some(bar) => bar, // Repeated show in the same note preserves original query/history.
            None => {
                // Publish only after every construction role registers; the outer unique-owner lease rejects close during construction.
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
        // Repeated show borrows the original bar/layout feedback; only initial assembly or explicit mode handoff changes body binding.
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
                // Existing interaction prefills at most 100 UTF-16 selection units after leaving body, without global Find pasteboard access.
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

/// Synchronous actions borrow the live note only; native getters/matching execute after state borrows end.
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
        // Hide preserves the actual viewport through its original body anchor; clip changes cannot turn a measured prefix into document EOF.
        body.finish_cell_before_find(&controller, intent)
            .map_err(|cause| cause.to_string())?;
        body.hide_find_bar(&bar, &scroll, &controller)?;
        return Ok(NativeMarkdownFindExecution::Complete);
    }
    match body.execute_paged_find_action(&bar, &controller, intent, action) {
        Ok(NativeMarkdownFindExecution::Complete) => Ok(NativeMarkdownFindExecution::Complete),
        Ok(NativeMarkdownFindExecution::Pending(request)) => {
            // Return the original in-flight request; acceptance observes its real publication endpoint while ordinary UI may ignore admission output.
            if let Some(cause) = request.take_failure() {
                Err(cause.to_string())
            } else {
                Ok(NativeMarkdownFindExecution::Pending(request))
            }
        }
        Err(cause) => Err(cause.to_string()),
    }
}
