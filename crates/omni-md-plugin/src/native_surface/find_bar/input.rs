//! Observe composition through the original field's protocol stack and notifications.
//! Keep only weak identities and scalar facts; AppKit retains input and history ownership.

use super::*;

/// Bound each of the five diagnostic entry categories to 128 records. Log only facts
/// already obtained by the caller; diagnostics must not read candidates or call Native.
#[cfg(feature = "native-markdown-runtime-probe")]
fn trace_find_input(selector: &'static str, stage: &'static str, details: std::fmt::Arguments<'_>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTS: [AtomicUsize; 5] = [const { AtomicUsize::new(0) }; 5];
    let index = match selector {
        "marked" => 0,
        "unmark" => 1,
        "insert" => 2,
        "observe" => 3,
        _ => 4,
    };
    if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some()
        && COUNTS[index].fetch_add(1, Ordering::Relaxed) < 128
    {
        eprintln!("native_find_input selector={selector} stage={stage} {details} formal=false");
    }
}

/// Exists only within one actual public input-protocol stack. This ticket owns no
/// bar, field, controller, or document and cannot keep a closed session alive.
struct NativeMarkdownFindInputTicket {
    feedback: NativeMarkdownFindViewportFeedback,
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    controller: Weak<NativeMarkdownController>,
    field: Weak<NSTextField>,
    field_address: usize,
    editor_address: usize,
    client_revision: u64,
    notified_change: bool,
    ended: bool,
    _selector: &'static str,
}

impl NativeMarkdownFindInputTicket {
    /// Capture the actual window delegate and specific client before input. Recheck
    /// the original client revision after every potentially reentrant getter.
    fn capture(editor: &NSTextView, _selector: &'static str) -> Option<Self> {
        let session = editor.downcast_ref::<NativeMarkdownSessionFieldEditor>()?;
        let client_revision = session.input_client_revision();
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            _selector,
            "capture_begin",
            format_args!(
                "revision={client_revision} editor={}",
                std::ptr::from_ref(editor).addr()
            ),
        );
        let Some(window) = editor.window() else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_window_none",
                format_args!("revision={client_revision}"),
            );
            return None;
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            _selector,
            "capture_window",
            format_args!(
                "window={} revision={}",
                std::ptr::from_ref(&*window).addr(),
                session.input_client_revision()
            ),
        );
        if session.input_client_revision() != client_revision {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_window_revoked",
                format_args!(
                    "expected={client_revision} actual={}",
                    session.input_client_revision()
                ),
            );
            // The window getter reassigned the editor; its successor cannot authorize this input.
            return None;
        }
        let Some(delegate) = window.delegate() else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_delegate_none",
                format_args!("revision={client_revision}"),
            );
            return None;
        };
        if session.input_client_revision() != client_revision {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_delegate_revoked",
                format_args!(
                    "expected={client_revision} actual={}",
                    session.input_client_revision()
                ),
            );
            // Delegate lookup can also change clients; no specific field is authorized yet.
            return None;
        }
        let object: &AnyObject = AsRef::<AnyObject>::as_ref(&*delegate);
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            _selector,
            "capture_delegate",
            format_args!(
                "delegate={} revision={}",
                std::ptr::from_ref(object).addr(),
                session.input_client_revision()
            ),
        );
        let Some(controller) = object.downcast_ref::<NativeMarkdownController>() else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_foreign_delegate",
                format_args!("revision={client_revision}"),
            );
            return None;
        };
        if !session.belongs_to(&controller.ivars().undo, &controller.ivars().owned) {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_foreign_session",
                format_args!("revision={client_revision}"),
            );
            // A previous session's editor cannot borrow the new controller's input authority.
            return None;
        }
        let Some(bar) = controller.current_find_input_bar() else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_bar_none",
                format_args!("revision={client_revision}"),
            );
            return None;
        };
        let Some(feedback) = NativeMarkdownFindViewportFeedback::capture(
            &bar,
            controller,
            controller.find_input_intent(),
            false,
        ) else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_feedback_none",
                format_args!(
                    "revision={client_revision} intent={}",
                    controller.find_input_intent()
                ),
            );
            return None;
        };
        // Feedback, client revision, and the original undo/resource owners must all agree;
        // a surviving window or editor address alone cannot establish session identity.
        let current = || {
            feedback.current(controller)
                && session.input_client_revision() == client_revision
                && session.belongs_to(&controller.ivars().undo, &controller.ivars().owned)
        };
        let Ok(weak_controller) =
            native_markdown_table_intent_step(&current, || Weak::from(controller))
        else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_weak_controller_revoked",
                format_args!(
                    "expected={client_revision} actual={}",
                    session.input_client_revision()
                ),
            );
            return None;
        };
        let Ok(fields) = native_markdown_table_intent_step(&current, || bar.fields()) else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_fields_revoked",
                format_args!(
                    "expected={client_revision} actual={}",
                    session.input_client_revision()
                ),
            );
            return None;
        };
        let editor_address = std::ptr::from_ref(editor).addr();
        let mut original = None;
        for field in fields {
            // Identify actual fields in diagnostics without adding a release-build traversal counter.
            #[cfg(feature = "native-markdown-runtime-probe")]
            let field_address = std::ptr::from_ref(&*field).addr();
            let Ok(actual) = native_markdown_table_intent_step(&current, || field.currentEditor())
            else {
                #[cfg(feature = "native-markdown-runtime-probe")]
                trace_find_input(
                    _selector,
                    "capture_field_revoked",
                    format_args!(
                        "field_address={field_address} expected={client_revision} actual={} feedback={}",
                        session.input_client_revision(),
                        feedback.current(controller)
                    ),
                );
                return None;
            };
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_field",
                format_args!(
                    "field_address={field_address} editor={:?} original_editor={editor_address} revision={}",
                    actual
                        .as_ref()
                        .map(|value| std::ptr::from_ref(&**value).addr()),
                    session.input_client_revision()
                ),
            );
            if actual
                .as_ref()
                .is_some_and(|actual| std::ptr::from_ref(&**actual).addr() == editor_address)
            {
                if original.is_some() {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    trace_find_input(
                        _selector,
                        "capture_duplicate",
                        format_args!(
                            "field_address={field_address} editor={editor_address} revision={client_revision}"
                        ),
                    );
                    // Two fields claiming one editor have no unique source; traversal order
                    // must not decide which query or replacement field receives this input.
                    return None;
                }
                let Ok(weak) =
                    native_markdown_table_intent_step(&current, || Weak::from_retained(&field))
                else {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    trace_find_input(
                        _selector,
                        "capture_weak_field_revoked",
                        format_args!(
                            "field_address={field_address} expected={client_revision} actual={}",
                            session.input_client_revision()
                        ),
                    );
                    return None;
                };
                original = Some((weak, std::ptr::from_ref(&*field).addr()));
            }
        }
        let Some((field, field_address)) = original else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_no_client",
                format_args!("editor={editor_address} revision={client_revision}"),
            );
            return None;
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            _selector,
            "capture_end",
            format_args!(
                "field={field_address} expected={client_revision} actual={} current={}",
                session.input_client_revision(),
                current()
            ),
        );
        current().then_some(Self {
            feedback,
            bar: Rc::downgrade(&bar),
            controller: weak_controller,
            field,
            field_address,
            editor_address,
            client_revision,
            notified_change: false,
            ended: false,
            _selector,
        })
    }

    /// Merge notifications only into the original field's live protocol stack.
    /// This scalar-only check is safe while the ticket has a short RefCell borrow.
    fn accepts_notification(
        &self,
        controller: &NativeMarkdownController,
        field: &NSTextField,
        client_revision: u64,
    ) -> bool {
        self.field_address == std::ptr::from_ref(field).addr()
            && self.client_revision == client_revision
            && self.feedback.current(controller)
    }

    /// Read final composition only after the outermost super call returns. The
    /// current client after that call cannot replace the ticket's original identity.
    fn finish(self, original_editor: &NSTextView) {
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            self._selector,
            "finish_begin",
            format_args!(
                "field={} editor={} expected_revision={} notified={} ended={}",
                self.field_address,
                self.editor_address,
                self.client_revision,
                self.notified_change,
                self.ended
            ),
        );
        let Some(session) = original_editor.downcast_ref::<NativeMarkdownSessionFieldEditor>()
        else {
            return;
        };
        if std::ptr::from_ref(original_editor).addr() != self.editor_address {
            // Only the editor on the original protocol/event stack may consume this ticket.
            return;
        }
        let Some(controller) = self.controller.load() else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                self._selector,
                "finish_controller_none",
                format_args!("expected_revision={}", self.client_revision),
            );
            // The original window ended; there is no feedback destination to recover.
            return;
        };
        let current = || {
            self.feedback.current(&controller)
                && session.input_client_revision() == self.client_revision
                && session.belongs_to(&controller.ivars().undo, &controller.ivars().owned)
        };
        let Some(field) = native_markdown_table_intent_step(&current, || self.field.load())
            .ok()
            .flatten()
        else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                self._selector,
                "finish_field_none_or_revoked",
                format_args!(
                    "expected_revision={} actual_revision={} feedback={}",
                    self.client_revision,
                    session.input_client_revision(),
                    self.feedback.current(&controller)
                ),
            );
            // Reentry, closure, or field destruction invalidates this ticket; do not inspect a successor.
            return;
        };
        let Ok(actual) = native_markdown_table_intent_step(&current, || field.currentEditor())
        else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                self._selector,
                "finish_client_revoked",
                format_args!(
                    "expected_revision={} actual_revision={} feedback={}",
                    self.client_revision,
                    session.input_client_revision(),
                    self.feedback.current(&controller)
                ),
            );
            return;
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            self._selector,
            "finish_client",
            format_args!(
                "actual_editor={:?} original_editor={} expected_revision={} actual_revision={} ended={}",
                actual
                    .as_ref()
                    .map(|value| std::ptr::from_ref(&**value).addr()),
                self.editor_address,
                self.client_revision,
                session.input_client_revision(),
                self.ended
            ),
        );
        let marked = match actual {
            Some(actual)
                if !self.ended && std::ptr::from_ref(&*actual).addr() == self.editor_address =>
            {
                let Some(editor) = actual.downcast_ref::<NSTextView>() else {
                    // Only the original actual NSTextView can supply the public marked-text state.
                    return;
                };
                let Ok(marked) =
                    native_markdown_table_intent_step(&current, || editor.hasMarkedText())
                else {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    trace_find_input(
                        self._selector,
                        "finish_marked_revoked",
                        format_args!(
                            "expected_revision={} actual_revision={} feedback={}",
                            self.client_revision,
                            session.input_client_revision(),
                            self.feedback.current(&controller)
                        ),
                    );
                    return;
                };
                marked
            }
            // The original field's real didEnd notification and absence of a replacement
            // client together establish that its composition ended.
            None if self.ended => false,
            _ => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                trace_find_input(
                    self._selector,
                    "finish_client_mismatch",
                    format_args!(
                        "ended={} expected_revision={} actual_revision={}",
                        self.ended,
                        self.client_revision,
                        session.input_client_revision()
                    ),
                );
                // Editor reuse by another field or a restarted original field cannot continue this input.
                return;
            }
        };
        let Ok(confirmed) = native_markdown_table_intent_step(&current, || field.currentEditor())
        else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                self._selector,
                "finish_confirmation_revoked",
                format_args!(
                    "expected_revision={} actual_revision={} feedback={}",
                    self.client_revision,
                    session.input_client_revision(),
                    self.feedback.current(&controller)
                ),
            );
            return;
        };
        let same_client = if self.ended {
            // A completed field must still have no client; the earlier nil result is insufficient.
            confirmed.is_none()
        } else {
            confirmed
                .as_ref()
                .is_some_and(|editor| std::ptr::from_ref(&**editor).addr() == self.editor_address)
        };
        // Returning the temporary native claim can reenter too; validate once more before publication.
        drop(confirmed);
        if !same_client || !current() {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                self._selector,
                "finish_final_rejected",
                format_args!(
                    "same_client={same_client} expected_revision={} actual_revision={} feedback={} marked={marked}",
                    self.client_revision,
                    session.input_client_revision(),
                    self.feedback.current(&controller)
                ),
            );
            return;
        }
        if current()
            && let Some(bar) = self.bar.upgrade()
        {
            bar.publish_input_state(
                &controller,
                &field,
                marked,
                self.notified_change,
                self._selector,
            );
        }
    }
}

/// Nested protocol calls share one weak ticket. No event, text buffer, or second
/// editing owner remains resident after the actual protocol stack unwinds.
#[derive(Default)]
pub(in crate::native_surface) struct NativeMarkdownFindProtocolState {
    depth: Cell<usize>,
    ticket: RefCell<Option<NativeMarkdownFindInputTicket>>,
}

impl NativeMarkdownFindProtocolState {
    /// Establish nesting before reentrant getters. A nested protocol call must not
    /// replace the outermost call's original client ticket.
    pub(in crate::native_surface) fn enter<'a>(
        &'a self,
        editor: &'a NSTextView,
        _selector: &'static str,
    ) -> NativeMarkdownFindProtocolScope<'a> {
        let depth = self.depth.get();
        self.depth
            .set(depth.checked_add(1).expect("Find 输入栈深度溢出"));
        let scope = NativeMarkdownFindProtocolScope {
            state: self,
            editor,
            active: true,
            _selector,
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(_selector, "hook_enter", format_args!("depth={depth}"));
        if depth == 0 {
            let ticket = NativeMarkdownFindInputTicket::capture(editor, _selector);
            let previous = self.ticket.replace(ticket);
            // Weak-claim destruction occurs after the RefCell borrow has ended.
            drop(previous);
        }
        scope
    }

    /// Accumulate notification facts for this original field; only the outermost
    /// call's actual marked-text state decides whether committed feedback is ready.
    pub(in crate::native_surface) fn defer_notification(
        &self,
        controller: &NativeMarkdownController,
        field: &NSTextField,
        client_revision: u64,
        changed: bool,
        ended: bool,
    ) -> bool {
        let mut ticket = self.ticket.borrow_mut();
        let Some(ticket) = ticket
            .as_mut()
            .filter(|ticket| ticket.accepts_notification(controller, field, client_revision))
        else {
            // An unrelated notification follows its normal immediate path rather than being swallowed.
            return false;
        };
        ticket.notified_change |= changed;
        ticket.ended |= ended;
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            ticket._selector,
            "notification_deferred",
            format_args!(
                "changed={changed} ended={ended} accumulated_change={} accumulated_end={} revision={client_revision}",
                ticket.notified_change, ticket.ended
            ),
        );
        true
    }

    /// Accepted cleanup revokes the weak ticket. Actual outstanding protocol scopes
    /// still own, and must return, their nesting depth.
    pub(in crate::native_surface) fn seal(&self) {
        let previous = self.ticket.take();
        drop(previous);
    }

    /// Move the ticket only when the actual outermost scope leaves. Depth accounting
    /// performs no Native getters, input observation, or borrowed-state publication.
    fn leave(&self) -> Option<NativeMarkdownFindInputTicket> {
        let depth = self
            .depth
            .get()
            .checked_sub(1)
            .expect("Find 输入栈未成对归还");
        self.depth.set(depth);
        (depth == 0).then(|| self.ticket.take()).flatten()
    }
}

/// Pair actual public protocol calls. Rust unwinding revokes the ticket without
/// running Native feedback or pretending the interrupted input completed.
pub(in crate::native_surface) struct NativeMarkdownFindProtocolScope<'a> {
    state: &'a NativeMarkdownFindProtocolState,
    editor: &'a NSTextView,
    active: bool,
    _selector: &'static str,
}

impl NativeMarkdownFindProtocolScope<'_> {
    /// End stack-state borrows after super returns, before feedback setters may
    /// reenter and begin a new input protocol.
    pub(in crate::native_surface) fn finish(mut self) {
        self.active = false;
        let ticket = self.state.leave();
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            self._selector,
            "hook_return",
            format_args!(
                "remaining_depth={} ticket={}",
                self.state.depth.get(),
                ticket.is_some()
            ),
        );
        if let Some(ticket) = ticket {
            ticket.finish(self.editor);
        }
    }
}

impl Drop for NativeMarkdownFindProtocolScope<'_> {
    fn drop(&mut self) {
        if self.active {
            // Unwinding revokes only the original weak capability; it publishes no composition state.
            drop(self.state.leave());
        }
    }
}

impl NativeMarkdownFindBar {
    /// Store this field's final composition state before revoking old feedback, so
    /// setter reentry cannot let this outer call overwrite a successor's state.
    fn publish_input_state(
        &self,
        controller: &NativeMarkdownController,
        field: &NSTextField,
        marked: bool,
        changed: bool,
        _selector: &'static str,
    ) {
        let Some(refresh) = self.input_refresh_for_field(field) else {
            return;
        };
        let state = if refresh {
            &self.query_marked
        } else {
            &self.replacement_marked
        };
        let before = state.replace(marked);
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            _selector,
            "publish_state",
            format_args!(
                "query={refresh} before_marked={before} after_marked={marked} notified={changed} should_refresh={} intent={}",
                before != marked || (changed && !marked),
                controller.find_input_intent()
            ),
        );
        if (before != marked || (changed && !marked))
            && let Some(bar) = controller
                .current_find_input_bar()
                .filter(|bar| std::ptr::eq(&**bar, self))
        {
            // Revoke once at composition start/end. Candidate changes while still
            // marked neither read field text nor repeatedly refresh the query.
            controller.enqueue_find_input_feedback(&bar, refresh);
        }
    }
}

impl NativeMarkdownController {
    /// Field notifications and protocol calls share actual input-state observation.
    /// Membership comes from the original bar's field identity, never a numeric tag.
    pub(in crate::native_surface) fn observe_find_control_input(
        &self,
        field: &NSTextField,
        changed: bool,
        ended: bool,
    ) -> bool {
        let Some(bar) = self
            .current_find_input_bar()
            .filter(|bar| bar.input_refresh_for_field(field).is_some())
        else {
            return false;
        };
        if self
            .ivars()
            .field_editor_provider
            .defer_find_input(self, field, changed, ended)
        {
            // History/end-editing keep their original notification path; only Find
            // feedback waits until the actual outermost protocol call returns.
            return true;
        }
        let Some(feedback) = NativeMarkdownFindViewportFeedback::capture(
            &bar,
            self,
            self.find_input_intent(),
            false,
        ) else {
            // The original bar closed or body changed; the notification cannot migrate to a successor query.
            return true;
        };
        let current = || feedback.current(self);
        let Ok(actual) = native_markdown_table_intent_step(&current, || field.currentEditor())
        else {
            return true;
        };
        let (marked, editor_identity) = match actual {
            Some(actual) => {
                let Some(editor) = actual.downcast_ref::<NSTextView>() else {
                    return true;
                };
                let Some(session) = editor.downcast_ref::<NativeMarkdownSessionFieldEditor>()
                else {
                    // A foreign shared editor cannot publish scalars into this session;
                    // the history path retains responsibility for its ownership error.
                    return true;
                };
                if !session.belongs_to(&self.ivars().undo, &self.ivars().owned) {
                    // Matching types do not authorize reading another window's undo/ledger client.
                    return true;
                }
                let address = std::ptr::from_ref(editor).addr();
                let revision = session.input_client_revision();
                let input_current = || {
                    current()
                        && self
                            .ivars()
                            .field_editor_provider
                            .current_input_client_revision(address)
                            == Some(revision)
                };
                let Ok(marked) =
                    native_markdown_table_intent_step(&input_current, || editor.hasMarkedText())
                else {
                    return true;
                };
                (marked, Some((address, revision)))
            }
            // A real didEnd with no replacement client can end the original composition.
            None if ended => (false, None),
            // Begin/change without the actual editor cannot guess an unmarked state
            // or inspect the control's still-uncommitted candidate value.
            None => return true,
        };
        let current = || {
            feedback.current(self)
                && editor_identity.is_none_or(|(address, revision)| {
                    self.ivars()
                        .field_editor_provider
                        .current_input_client_revision(address)
                        == Some(revision)
                })
        };
        let Ok(confirmed) = native_markdown_table_intent_step(&current, || field.currentEditor())
        else {
            return true;
        };
        let same_client = match editor_identity {
            Some((address, _)) => confirmed
                .as_ref()
                .is_some_and(|editor| std::ptr::from_ref(&**editor).addr() == address),
            // A new client after didEnd must keep its own composition state untouched.
            None => confirmed.is_none(),
        };
        drop(confirmed);
        if !same_client || !current() {
            // Even the last native-claim release has completed; revoked authority
            // cannot update marked state or enqueue stale feedback.
            return true;
        }
        if current() {
            bar.publish_input_state(self, field, marked, changed, "notification");
        }
        true
    }

    /// After the original Panel event, fill only missing state transitions. Input
    /// already observed by protocol hooks/notifications must not advance intent twice.
    pub(in crate::native_surface) fn observe_find_native_editor(&self, editor: &NSTextView) {
        if let Some(ticket) = NativeMarkdownFindInputTicket::capture(editor, "observe")
            && ticket.feedback.current(self)
        {
            ticket.finish(editor);
        }
    }
}
