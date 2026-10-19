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
            if native_markdown_panel_is_current(self)
                && let Ok(view) = active_document_view()
            {
                let (handled, system_mouse_up) = view.route_table_pointer_event(event);
                if handled {
                    if system_mouse_up {
                        // 选择已由原 Pointer 处理；NSWindow 仍须收到自己真实 Down 的同一 Up 结束内部跟踪。
                        // SAFETY: 原事件仍在本 sendEvent 栈，仅该 preview Up 转发一次；不重播 Down/drag 或 raw-field Up。
                        unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
                    }
                    return; // 其它已消费事件不二次派发；Up 也不落回下方通用 super 路径。
                }
            }
            if native_markdown_panel_is_current(self)
                && matches!(event.r#type(),NSEventType::KeyDown|NSEventType::ScrollWheel|NSEventType::LeftMouseDown|NSEventType::RightMouseDown|NSEventType::OtherMouseDown|NSEventType::LeftMouseDragged|NSEventType::RightMouseDragged|NSEventType::OtherMouseDragged)
                && let Ok(view)=active_document_view()
            {
                // 父panel在NSScroller开始内部tracking前即撤销旧权限；布局bounds变化不走此入口。
                // anchor本来就须在动作返回前消费，这里只防御迟到状态，并取消旧单元格激活意图。
                view.cancel_table_continuity_for_new_input();
            }
            if event.r#type() == NSEventType::LeftMouseDown
                && native_markdown_panel_is_current(self)
                && let Ok(view) = active_document_view()
                && view.begin_table_raw_field_shift(event)
            {
                return; // 原真实 Shift-Down 已进入该格唯一 Pointer，后继 drag/up 沿同一个事件槽。
            }
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
                // The field editor can own a table draft or finder input. Its composition gets
                // first refusal before an actual physical Escape may affect the document window.
                let marked = self
                    .firstResponder()
                    .and_then(|responder| responder.downcast::<NSTextView>().ok())
                    .is_some_and(|view| {
                        // SAFETY: AppKit NSTextView implements NSTextInputClient on the main thread.
                        unsafe { msg_send![&*view, hasMarkedText] }
                    });
                if !marked {
                    // Clone the editor outside TLS before native selection/focus operations can reenter.
                    if let Ok(view) = active_document_view() {
                        let editing_cell = view.ivars().active_table_cell.borrow().is_some();
                        if editing_cell {
                            // First Escape cancels only the reusable cell draft.
                            view.cancel_active_table_cell();
                            return;
                        }
                    }
                    if dismiss_active_native_markdown_find_bar() {
                        // The finder consumes Escape without closing the unsaved document session.
                        return;
                    }
                    self.enqueue_markdown_window_close();
                    return;
                }
            }
            let table_mouse = if event.r#type() == NSEventType::LeftMouseDown
                && native_markdown_panel_is_current(self)
                && let Ok(view) = active_document_view()
            {
                view.capture_table_field_mouse_selection(event)
            } else {
                None // 只跨此原 Down 的同步 tracking 记录；不挂观察器或保留后续事件队列。
            };
            // SAFETY: the unconsumed live native event is forwarded once on the AppKit main thread.
            unsafe {
                let _: () = msg_send![super(self), sendEvent: event];
            }
            if native_markdown_panel_is_current(self) && let Ok(view)=active_document_view() {
                if let Some(mouse) = table_mouse {
                    view.finish_table_field_mouse_selection(mouse);
                }
                if event.r#type() == NSEventType::KeyDown {
                    // 真实按键已返回；只用实际 caret 或仍保留原 anchor 的连续 Shift 续接格内方向。
                    view.observe_active_table_field_selection(Some(event));
                }
                // 同步entry已自行消费；若重入路径遗留权限，也只能在本事件结束前尝试一次再撤销。
                view.finish_table_mutation_continuity();
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
