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
                    self.enqueue_markdown_dismissal();
                    return;
                }
            }
            // SAFETY: the unconsumed live native event is forwarded once on the AppKit main thread.
            unsafe {
                let _: () = msg_send![super(self), sendEvent: event];
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
        /// Accepts exact Command-W through the scoped save-before-close path.
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            if is_exact_markdown_command_w_event(event) {
                // The same scoped close protocol handles the exact physical shortcut and normal window close action.
                self.enqueue_markdown_dismissal();
                true
            } else {
                // SAFETY: key equivalents not owned by Omni forward exactly once to NSPanel.
                unsafe { msg_send![super(self), performKeyEquivalent: event] }
            }
        }

        #[unsafe(method(performClose:))]
        /// Treats standard native close actions as a scoped foreground close request.
        fn perform_close(&self, _sender: Option<&AnyObject>) {
            self.enqueue_markdown_dismissal();
        }
    }
);

impl NativeMarkdownPanel {
    /// Allocate the retained foreground panel without opening any document or publishing active session state.
    pub(super) fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let panel = Self::alloc(mtm).set_ivars(());
        // SAFETY: all Rust ivars are initialized before NSPanel's public designated initializer.
        // The window is constructed and destroyed only on the AppKit main thread.
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

    /// Defers close for this panel only, so a late native event cannot dismiss its successor.
    pub(super) fn enqueue_markdown_dismissal(&self) {
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        enqueue_native_markdown_main_task(move || {
            let mtm =
                MainThreadMarker::new().expect("Markdown panel dismissal runs on the main queue");
            let is_current = weak
                .get(mtm)
                .load()
                .is_some_and(|panel| native_markdown_panel_is_current(&panel));
            if !is_current {
                // The original panel disappeared or another foreground session replaced it.
                return;
            }
            if let Err(error) = request_native_markdown_window_dismiss() {
                // Persistence failure vetoes close and preserves the original draft.
                eprintln!("Markdown dismissal failed: {error}");
            }
        });
    }
}
