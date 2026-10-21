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
                // The current unmarked field's Return invokes the original Find
                // action once, without a second target/action submission.
                if let Err(error) = perform_native_markdown_find_action(action) {
                    present_native_markdown_error(&error);
                }
                return;
            }
            if event.r#type() == NSEventType::KeyDown
                && event.keyCode() == 53
                && native_markdown_panel_is_current(self)
            {
                // Capture the actual paged cell and its Input before the first
                // responder getter. Only Rust owners are cloned here; no TLS
                // borrow crosses later Native focus or IME callbacks.
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
                            // SAFETY: The actual main-thread field editor implements
                            // NSTextInputClient and remains borrowed for this query.
                            unsafe { msg_send![view, hasMarkedText] }
                        } else if let Some(view) = responder.downcast_ref::<
                            super::paged_text_view::NativeMarkdownPagedTextView,
                        >() {
                            // The paged body implements the same input protocol;
                            // recognizing only NSTextView would turn composition
                            // Escape into an unintended plugin return.
                            // SAFETY: The concrete type is verified. Public
                            // hasMarkedText queries state without copying text.
                            unsafe { msg_send![view, hasMarkedText] }
                        } else {
                            false // Nontext responders own no composition; retain the existing return priority.
                        }
                    });
                if !marked {
                    if let Some(captured) = paged_cell {
                        // The first Escape cancels only this original cell draft.
                        // New input or another cell revokes this captured action;
                        // recapturing current focus would grant the wrong cancel authority.
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
                        return; // This physical Escape reached its original client once; do not also dismiss Find or the panel.
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
                // Actual input dispatch has ended. Keep Find's field delegate
                // intact and observe history ownership of the real shared editor.
                if let Some(editor) = self.firstResponder().and_then(|value| value.downcast::<NSTextView>().ok())
                    && editor.isFieldEditor()
                    && let Some(delegate) = self.delegate()
                {
                    let object: &AnyObject = AsRef::<AnyObject>::as_ref(&*delegate);
                    if let Some(controller) = object.downcast_ref::<NativeMarkdownController>() {
                        // The window's delegate owns this controller; Native
                        // getters execute without a TLS owner borrow.
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
                    // Real input may change focus without editing text. Coalesce
                    // one original history-button refresh after dispatch.
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
        /// Preserve existing local Find/save/return commands. Save commits only
        /// after the actual key-event stack has returned.
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            if let Some(command) = markdown_find_key_command(event)
                && native_markdown_panel_is_current(self)
            {
                // Local Find routing does not depend on an application menu.
                // Failure retains current input and reports through the original owner.
                if let Err(error) = dispatch_markdown_find_key_command(command) {
                    present_native_markdown_error(&error);
                }
                true
            } else if is_exact_markdown_save_event(event)
                && native_markdown_panel_is_current(self)
            {
                // Queue through this window's actual typed delegate, carrying
                // weak identity rather than saving synchronously on the event stack.
                if let Some(delegate) = self.delegate() {
                    let object: &AnyObject = AsRef::<AnyObject>::as_ref(&*delegate);
                    if let Some(controller) = object.downcast_ref::<NativeMarkdownController>() {
                        // Enqueue rechecks original panel/controller/document
                        // identity; getter reentry cannot transfer save authority.
                        return controller.enqueue_document_save(self).into();
                    }
                }
                // A window without its project delegate has no save authority;
                // preserve the ordinary unhandled Native result.
                false
            } else if is_exact_markdown_command_w_event(event) {
                // The existing local return command shares the save/release/
                // launcher flow without introducing visible system window buttons.
                self.enqueue_markdown_window_close();
                true
            } else {
                // SAFETY: key equivalents not owned by Omni forward exactly once to NSPanel.
                unsafe { msg_send![super(self), performKeyEquivalent: event] }
            }
        }

        #[unsafe(method(performClose:))]
        /// Standard panel close queues the original return request, leaving the
        /// Host application's independent lifetime unchanged.
        fn perform_close(&self, _sender: Option<&AnyObject>) {
            self.enqueue_markdown_window_close();
        }
    }
);

impl NativeMarkdownPanel {
    /// Preserve the borderless plugin panel and its original resize/input behavior.
    pub(super) fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let panel = Self::alloc(mtm).set_ivars(());
        // SAFETY: Rust ivars are initialized before the public designated
        // initializer runs on the AppKit main thread. The existing resizable,
        // nonactivating combination retains the original plugin save/return lifecycle.
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

    /// Carry only the original panel's weak identity. A delayed action cannot
    /// prolong the old panel or close its successor session.
    pub(super) fn enqueue_markdown_window_close(&self) {
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("Markdown 窗口关闭沿主队列执行");
            let is_current = weak
                .get(mtm)
                .load()
                .is_some_and(|panel| native_markdown_panel_is_current(&panel));
            if !is_current {
                // The original panel ended or was replaced; its action owns no
                // authority over the successor foreground session.
                return;
            }
            if let Err(error) = request_native_markdown_window_exit() {
                // Save conflict or lifecycle rejection preserves the draft.
                // Host returns only after the original actual release barrier.
                eprintln!(
                    "native_markdown_window operation=request_exit outcome=failed error={error}"
                );
            }
        });
    }
}

/// Use logical characters rather than an ANSI physical position, preserving
/// the same save command under the current Chinese input source.
fn is_exact_markdown_save_event(event: &NSEvent) -> bool {
    let Some(characters) = event.charactersIgnoringModifiers() else {
        // Missing logical characters grant no save authority; ordinary Native dispatch continues.
        return false;
    };
    autoreleasepool(|pool| {
        // SAFETY: The Foundation borrow stays within this pool's finite comparison,
        // never entering the queued save closure.
        is_exact_markdown_save_key(unsafe { characters.to_str(pool) }, event.modifierFlags())
    })
}

/// Accept the existing Command-S. Caps Lock preserves save, while additional
/// command modifiers retain their own Native meanings.
fn is_exact_markdown_save_key(characters: &str, modifiers: NSEventModifierFlags) -> bool {
    let command_modifiers = NSEventModifierFlags::Command
        | NSEventModifierFlags::Control
        | NSEventModifierFlags::Option
        | NSEventModifierFlags::Shift;
    characters.eq_ignore_ascii_case("s")
        && modifiers.intersection(command_modifiers) == NSEventModifierFlags::Command
}

/// Identify the existing Command-W by logical character, preserving its original
/// modifier contract rather than introducing a physical-key command.
fn is_exact_markdown_command_w_event(event: &NSEvent) -> bool {
    let Some(characters) = event.charactersIgnoringModifiers() else {
        // Missing logical characters grant no return authority; retain system routing.
        return false;
    };
    autoreleasepool(|pool| {
        // SAFETY: The character borrow ends within this pool, before any window-return transaction.
        is_exact_markdown_command_w(unsafe { characters.to_str(pool) }, event.modifierFlags())
    })
}

/// Only the original Command-W alone returns this panel; extra modifiers retain
/// their own Native meanings.
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
        // Case and Caps Lock preserve save; logical characters may come from
        // the current input source without selecting another source here.
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
        // Extra modifiers, other letters, and composition text must not lose
        // their Native actions to the local save route.
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
