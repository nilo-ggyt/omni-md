//! Owns the plugin panel and physical keyboard/responder routing.
//! A delayed key action carries weak panel identity and cannot close a later foreground session.
//! NativeMarkdownPanel adds no Rust ivars or payload owner: its overrides are static routing methods.
//! Terminal scope may accept this custom subclass only after all project associations/callbacks
//! are removed and its closed, detached framework shell is independently verified.

use super::*;

define_class!(
    // SAFETY: NSPanel permits these keyboard-window overrides. All responder access is
    // main-thread-only and close intents return asynchronously to the same lifecycle state machine.
    // This custom subclass has no Rust ivars or retained project payload; terminal shell proof still
    // requires removal of its operation association and every delegate/content/responder edge.
    #[unsafe(super = NSPanel)]
    #[thread_kind = MainThreadOnly]
    pub(super) struct NativeMarkdownPanel;

    impl NativeMarkdownPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        /// Allows the nonactivating plugin panel to receive real text input.
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        /// Keeps application main-window status with the host while this foreground panel edits.
        fn can_become_main_window(&self) -> bool {
            false
        }

        #[unsafe(method(sendEvent:))]
        /// Routes a physical Escape only after the actual field/editor composition has first refusal.
        fn send_event(&self, event: &NSEvent) {
            if event.r#type() == NSEventType::KeyDown
                && native_markdown_panel_is_current(self)
                && let Some(action) = markdown_find_return_action(self, event)
            {
                // 本栏实际未 marked 的 Return 由同一查找动作消费，不再让 target/action 重复提交。
                if let Err(error) = perform_native_markdown_find_action(action) {
                    present_native_markdown_error(&error);
                }
                return;
            }
            if event.r#type() == NSEventType::KeyDown
                && event.keyCode() == 53
                && native_markdown_panel_is_current(self)
            {
                // 在首个responder getter前固定真实分页子格及其输入；这里仅克隆Rust owner，
                // 不调用Native、不持TLS借用跨过后续焦点或IME回调。
                let paged_cell = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    let surface = active.as_ref()?;
                    let source = surface.editor.as_ref()?;
                    let cell = source.body.active_cell_body()?;
                    let captured = cell
                        .view()
                        .map_err(|cause| cause.to_string())
                        .and_then(|view| {
                            let state = view.input_state().map_err(|cause| cause.to_string())?;
                            Ok((Rc::clone(&source.body), cell, view, state))
                        });
                    Some(captured)
                });
                // The field editor can own a table draft or finder input. Its composition gets
                // first refusal before an actual physical Escape may affect the document window.
                let marked = self
                    .firstResponder()
                    .is_some_and(|responder| {
                        if let Some(view) = responder.downcast_ref::<NSTextView>() {
                            // SAFETY: 原生字段编辑器实现 NSTextInputClient，主线程借用贯穿本次查询。
                            unsafe { msg_send![view, hasMarkedText] }
                        } else if let Some(view) = responder.downcast_ref::<
                            super::paged_text_view::NativeMarkdownPagedTextView,
                        >() {
                            // 分页正文直接实现同一输入协议；不能只识别旧 NSTextView 后误把组字 Esc 当返回。
                            // SAFETY: 已核实际具体类型，公开 hasMarkedText 只查询当前输入状态，不复制正文。
                            unsafe { msg_send![view, hasMarkedText] }
                        } else {
                            false // 非文本 responder 没有组字所有权，继续原面板的返回优先级。
                        }
                    });
                if !marked {
                    if let Some(captured) = paged_cell {
                        // 第一枚Esc只取消原草稿，不能因只识别旧NSTextView而关闭整个插件。
                        // 新输入或重新开格撤销旧Esc；不把当前焦点重新捕获成另一格的取消权限。
                        let result = captured.and_then(|(parent, cell, view, state)| {
                            let current = native_markdown_panel_is_current(self)
                                && view.session_is_current()
                                && parent
                                    .active_cell_body()
                                    .is_some_and(|actual| Rc::ptr_eq(&actual, &cell))
                                && view.input_state().is_ok_and(|actual| actual == state);
                            if !current {
                                return Err("原单元格输入已改变，本次取消未执行".into());
                            }
                            view.command(sel!(cancelOperation:))
                                .map_err(|cause| cause.to_string())
                        });
                        if let Err(error) = result {
                            present_native_markdown_error(&error);
                        }
                        return; // 本次物理Esc已交同一真实client一次，不再落入Find或窗口返回分支。
                    }
                    if dismiss_active_native_markdown_find_bar() {
                        // The finder consumes Escape without closing the unsaved document session.
                        return;
                    }
                    self.enqueue_markdown_window_close();
                    return;
                }
            }
            // SAFETY: the unconsumed live native event is forwarded once on the AppKit main thread.
            unsafe {
                let _: () = msg_send![super(self), sendEvent: event];
            }
            if event.r#type() == NSEventType::KeyDown {
                // 真实输入派发已经完成；Find 私有字段委托不被替换，只记录实际共享 editor 的历史归属。
                if let Some(editor) = self.firstResponder().and_then(|value| value.downcast::<NSTextView>().ok())
                    && editor.isFieldEditor()
                    && let Some(delegate) = self.delegate()
                {
                    let object: &AnyObject = AsRef::<AnyObject>::as_ref(&*delegate);
                    if let Some(controller) = object.downcast_ref::<NativeMarkdownController>() {
                        // controller 随窗口 delegate 归属，不跨 TLS 借用调用原生 getter。
                        controller.observe_native_field_editor(&editor);
                    }
                }
            }
            if matches!(event.r#type(), NSEventType::KeyDown | NSEventType::LeftMouseDown | NSEventType::RightMouseDown | NSEventType::OtherMouseDown)
                && native_markdown_panel_is_current(self)
                && let Some(delegate) = self.delegate()
            {
                let object: &AnyObject = AsRef::<AnyObject>::as_ref(&*delegate);
                if let Some(controller) = object.downcast_ref::<NativeMarkdownController>() {
                    // 实际按键/点击可能只改变焦点而不改文字；派发完成后合并一次原按钮状态更新。
                    controller.queue_history_availability_refresh();
                }
            }
        }

        #[unsafe(method(cancelOperation:))]
        /// Preserves selector-only input-source cancellation in the native responder chain.
        fn cancel_operation(&self, sender: Option<&AnyObject>) {
            // Selector-only input-source cancellation cannot borrow the physical Escape close policy.
            // SAFETY: this is the matching main-thread superclass action with its original live sender.
            unsafe {
                let _: () = msg_send![super(self), cancelOperation: sender];
            }
        }

        #[unsafe(method(performKeyEquivalent:))]
        /// 保存、查找和关闭沿用各自已有命令；保存离开实际按键栈后再提交正文。
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            if let Some(command) = markdown_find_key_command(event)
                && native_markdown_panel_is_current(self)
            {
                // 项目 Find 快捷键不依赖未安装的应用菜单；失败保留当前输入并报告，不落入旧标准 Finder。
                if let Err(error) = dispatch_markdown_find_key_command(command) {
                    present_native_markdown_error(&error);
                }
                true
            } else if is_exact_markdown_save_event(event)
                && native_markdown_panel_is_current(self)
            {
                // 只向本窗口实际安装的 typed delegate 请求弱排队；不在事件栈内同步保存。
                if let Some(delegate) = self.delegate() {
                    let object: &AnyObject = AsRef::<AnyObject>::as_ref(&*delegate);
                    if let Some(controller) = object.downcast_ref::<NativeMarkdownController>() {
                        // enqueue 内再纯核对 panel/controller/正文身份，getter 重入不能借权。
                        return controller.enqueue_document_save(self).into();
                    }
                }
                // 已失去项目 delegate 的窗口没有保存权限，保留原生未处理结果。
                false
            } else if is_exact_markdown_command_w_event(event) {
                // 既有本地关闭命令复用保存、释放和返回启动器流程，不创建可见系统按钮。
                self.enqueue_markdown_window_close();
                true
            } else {
                // SAFETY: key equivalents not owned by Omni forward exactly once to NSPanel.
                unsafe { msg_send![super(self), performKeyEquivalent: event] }
            }
        }

        #[unsafe(method(performClose:))]
        /// 标准窗口关闭只排队原窗口的返回请求，不退出整个应用。
        fn perform_close(&self, _sender: Option<&AnyObject>) {
            self.enqueue_markdown_window_close();
        }
    }
);

impl NativeMarkdownPanel {
    /// 沿用无边框插件面板；不创建系统标题栏或三色窗口按钮。
    pub(super) fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let panel = Self::alloc(mtm).set_ivars(());
        // SAFETY: Rust ivar 已初始化，公开 designated initializer 仅在 AppKit 主线程调用。
        // 保留原可缩放、非激活面板组合；返回与保存仍由已有插件生命周期管理。
        unsafe {
            msg_send![
                super(panel),
                initWithContentRect: NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(NATIVE_MARKDOWN_WIDTH, NATIVE_MARKDOWN_HEIGHT),
                ),
                styleMask: NSWindowStyleMask::Borderless
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        }
    }

    /// 关闭只保留原 panel 的弱身份；迟到事件不能关闭后继窗口或延长旧窗口生命。
    pub(super) fn enqueue_markdown_window_close(&self) {
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("Markdown 窗口关闭沿主队列执行");
            let is_current = weak
                .get(mtm)
                .load()
                .is_some_and(|panel| native_markdown_panel_is_current(&panel));
            if !is_current {
                // 原窗口已释放或已换成后继会话，旧关闭动作没有后继窗口的权限。
                return;
            }
            if let Err(error) = request_native_markdown_window_exit() {
                // 保存冲突或生命周期拒绝保留原草稿；只有真实释放屏障完成后 Host 才恢复启动器。
                eprintln!("关闭 Markdown 窗口失败：{error}");
            }
        });
    }
}

/// 读取逻辑字符而非 ANSI 物理键位；中文输入法下的 Command-S 仍识别为同一保存动作。
fn is_exact_markdown_save_event(event: &NSEvent) -> bool {
    let Some(characters) = event.charactersIgnoringModifiers() else {
        // 没有逻辑字符的事件不能猜测为保存，原生派发继续处理它。
        return false;
    };
    autoreleasepool(|pool| {
        // SAFETY: Foundation 借用仅用于当前 pool 内的有限比较，不进入排队闭包。
        is_exact_markdown_save_key(unsafe { characters.to_str(pool) }, event.modifierFlags())
    })
}

/// 仅接标准 Command-S；Caps Lock 不改变保存，其他命令修饰键保留各自原生语义。
fn is_exact_markdown_save_key(characters: &str, modifiers: NSEventModifierFlags) -> bool {
    let command_modifiers = NSEventModifierFlags::Command
        | NSEventModifierFlags::Control
        | NSEventModifierFlags::Option
        | NSEventModifierFlags::Shift;
    characters.eq_ignore_ascii_case("s")
        && modifiers.intersection(command_modifiers) == NSEventModifierFlags::Command
}

/// 窗口按原逻辑字符识别既有 Command-W；不以物理键码新增命令或改变修饰键合同。
fn is_exact_markdown_command_w_event(event: &NSEvent) -> bool {
    let Some(characters) = event.charactersIgnoringModifiers() else {
        // 缺少逻辑字符的事件没有本窗口关闭权限，继续交原系统路由。
        return false;
    };
    autoreleasepool(|pool| {
        // SAFETY: 字符借用只活当前池内，比较结束后不进入窗口关闭事务。
        is_exact_markdown_command_w(unsafe { characters.to_str(pool) }, event.modifierFlags())
    })
}

/// 只有原单独 Command-W 才是本窗口关闭；额外修饰键保持各自原生含义。
pub(super) fn is_exact_markdown_command_w(
    characters_ignoring_modifiers: &str,
    modifiers: NSEventModifierFlags,
) -> bool {
    characters_ignoring_modifiers.eq_ignore_ascii_case("w")
        && modifiers.intersection(NSEventModifierFlags::DeviceIndependentFlagsMask)
            == NSEventModifierFlags::Command
}

#[cfg(test)]
mod save_shortcut_tests {
    use super::*;

    #[test]
    fn save_accepts_logical_letter_and_caps_lock_without_a_physical_key_assumption() {
        // 大小写和 Caps Lock 不能使标准保存失效；逻辑字符可来自任意输入源。
        assert!(is_exact_markdown_save_key(
            "s",
            NSEventModifierFlags::Command
        ));
        assert!(is_exact_markdown_save_key(
            "S",
            NSEventModifierFlags::Command | NSEventModifierFlags::CapsLock
        ));
    }

    #[test]
    fn other_commands_and_uncommitted_text_are_not_claimed_as_save() {
        // 额外命令修饰键、其他字母与组合文本不能借保存入口吞掉其原生动作。
        for modifiers in [
            NSEventModifierFlags::empty(),
            NSEventModifierFlags::Control,
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
            NSEventModifierFlags::Command | NSEventModifierFlags::Option,
            NSEventModifierFlags::Command | NSEventModifierFlags::Control,
        ] {
            assert!(!is_exact_markdown_save_key("s", modifiers));
        }
        for text in ["", "w", "保存", "ss"] {
            assert!(!is_exact_markdown_save_key(
                text,
                NSEventModifierFlags::Command
            ));
        }
    }
}
