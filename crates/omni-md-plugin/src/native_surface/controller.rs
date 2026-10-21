//! Owns native action targets, foreground controls and the single autosave deadline.
//! Delayed work carries weak originating identity; controls and timer detach before session release.

use super::*;

mod footer;
pub(super) use footer::NativeMarkdownFooterDocument;
use footer::{NativeMarkdownDocumentAreaBinding, NativeMarkdownFooterSaveResult};

/// An empty-window action changes only the next note's default mode; its original window and input epochs survive getters and dispatch.
pub(super) struct NativeMarkdownEmptyModeAction {
    owner: usize,
    find_input: u64,
    workspace_input: u64,
    header: u64,
    before: MarkdownPresentationMode,
}

impl NativeMarkdownEmptyModeAction {
    pub(super) fn empty(
        controller: &NativeMarkdownController,
        before: MarkdownPresentationMode,
    ) -> Self {
        Self {
            owner: std::ptr::from_ref(controller).addr(),
            find_input: controller.find_input_intent(),
            workspace_input: controller.workspace_query_intent(),
            header: controller.ivars().header_layout_revision.get(),
            before,
        }
    }

    fn current_surface(&self, surface: &NativeMarkdownWindowSurface) -> bool {
        let controller = &surface.controller;
        if self.owner != std::ptr::from_ref(&**controller).addr()
            || !native_markdown_controller_has_foreground_authority(controller)
            || self.find_input != controller.find_input_intent()
            || self.workspace_input != controller.workspace_query_intent()
            || self.header != controller.ivars().header_layout_revision.get()
            || self.before != surface.presentation_mode
        {
            // A changed input, window, or mode identity revokes the queued Source-button action rather than renewing its authority.
            return false;
        }
        surface.editor.is_none() // An old empty-state action cannot change a document opened later.
    }

    /// Reads pure metadata at Active delivery; never recaptures an intent after native getters.
    pub(super) fn is_current(&self) -> bool {
        ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| {
            surface
                .as_ref()
                .is_some_and(|surface| self.current_surface(surface))
        })
    }

    pub(super) fn read_destination(
        &self,
        surface: &NativeMarkdownWindowSurface,
        sender: &AnyObject,
    ) -> Result<MarkdownPresentationMode, String> {
        if !self.current_surface(surface) {
            // Type queries may enter the native runtime, so the original action must be authorized before the first query.
            return Err("展示模式动作已失效".into());
        }
        let button = sender.downcast_ref::<NSButton>();
        if !self.current_surface(surface) {
            // Reentrant type queries cannot attach a new input epoch to the old sender.
            return Err("展示模式动作已被新的输入撤销".into());
        }
        let button = button.ok_or("展示模式动作缺少原源码按钮")?;
        let owned = surface
            .controller
            .ivars()
            .presentation_mode_button
            .get()
            .expect("原窗口已安装源码按钮");
        if !std::ptr::eq(button, &**owned) {
            return Err("展示模式动作不属于原源码按钮".into()); // A foreign button has no mode-publication authority for this window.
        }
        let state = button.state();
        if !self.current_surface(surface) {
            // Rechecks the same intent after the state getter; does not recapture document, selection, or window authority.
            return Err("展示模式选择期间笔记或输入已变更".into());
        }
        if state == objc2_app_kit::NSControlStateValueOn {
            // The standard toggle click enables Source; failure compensation still displays the previously published mode.
            Ok(MarkdownPresentationMode::Source)
        } else if state == objc2_app_kit::NSControlStateValueOff {
            Ok(MarkdownPresentationMode::InlinePreview) // The same button disables Source through the existing live-preview transaction.
        } else {
            Err("展示模式选择无效".into()) // Mixed or unknown state cannot be guessed as a default mode.
        }
    }
}

/// Observes actual history completion only; canRedo emits Checkpoint itself, so that notification must not trigger refresh.
fn native_markdown_history_notifications() -> [&'static objc2_foundation::NSNotificationName; 3] {
    // SAFETY: Foundation notification names are process-static NSStrings, subscribed only on the AppKit main thread.
    unsafe {
        [
            objc2_foundation::NSUndoManagerDidCloseUndoGroupNotification,
            objc2_foundation::NSUndoManagerDidUndoChangeNotification,
            objc2_foundation::NSUndoManagerDidRedoChangeNotification,
        ]
    }
}

/// Owns foreground controls and one cancellable autosave deadline; queued work retains only weak identity.
pub(super) struct NativeMarkdownControllerIvars {
    pub(super) owned: Rc<NativeMarkdownOwnedLedger>,
    pub(super) undo: Rc<NativeMarkdownSessionUndo>,
    pub(super) field_editor_provider: NativeMarkdownFieldEditorProvider,
    pub(super) editing_window: Weak<NativeMarkdownPanel>,
    pub(super) borrowed: RefCell<NativeMarkdownBorrowedEditorSnapshot>,
    pub(super) exit_button: OnceCell<Retained<NSButton>>,
    pub(super) presentation_mode_button: OnceCell<Retained<NSButton>>,
    /// Reflects only the window's accepted mode; Busy may restore the picker from it but cannot authorize a mode action.
    pub(super) published_presentation_mode: Cell<MarkdownPresentationMode>,
    pub(super) table_menu_item: OnceCell<Retained<NSMenuItem>>,
    pub(super) copy_document_menu_item: OnceCell<Retained<NSMenuItem>>,
    pub(super) status_label: OnceCell<Retained<NSTextField>>,
    /// Persistent footer columns and transient feedback own separate display values, without document or save authority.
    pub(super) footer_labels: OnceCell<[Retained<NSTextField>; 3]>,
    /// Creates the information card on first explicit opening; hiding retains only this session's empty controls, not a document.
    pub(super) information_panel:
        RefCell<Option<Rc<super::information_panel::NativeMarkdownInformationPanel>>>,
    /// The conflict bar owns fixed controls and the original note's weak intent, without duplicating save state or document ownership.
    pub(super) conflict_bar: OnceCell<super::conflict_bar::NativeMarkdownConflictBar>,
    footer_document: Cell<Option<NativeMarkdownFooterDocument>>,
    footer_save_result: Cell<NativeMarkdownFooterSaveResult>,
    feedback_visible: Cell<bool>,
    document_area: RefCell<Option<NativeMarkdownDocumentAreaBinding>>,
    document_area_revision: Cell<u64>,
    /// Header layout observes existing controls; its epoch revokes stale setters during reentrant resize.
    pub(super) header_controls: OnceCell<NativeMarkdownHeaderControls>,
    pub(super) header_layout_revision: Cell<u64>,
    /// Document actions and history buttons share actual document availability; menus read it without another state mirror.
    pub(super) document_actions_available: Cell<bool>,
    history_update_queued: Cell<bool>,
    history_refreshing: Cell<bool>,
    history_observing: Cell<bool>,
    history_manager: RefCell<Option<Weak<objc2_foundation::NSUndoManager>>>,
    /// Stores only a Rust Weak to the original bar; input notifications may revoke it during a surface lease without strong control/document edges.
    find_input_bar: RefCell<std::rc::Weak<NativeMarkdownFindBar>>,
    find_input_intent: Cell<u64>,
    pub(super) close_finalizer_scheduled: Cell<bool>,
    pub(super) workspace_controls: OnceCell<Vec<Retained<NSControl>>>,
    pub(super) next_sidebar_page: OnceCell<Retained<NSButton>>,
    // One foreground-only debounce timer; cancellation precedes note handoff and native close.
    pub(super) autosave_timer: RefCell<Option<Retained<objc2_foundation::NSTimer>>>,
    pub(super) autosave_generation: Cell<u64>,
    /// Search owns one foreground one-shot deadline; its callback weakly references this controller without capturing query text.
    workspace_search_timer: RefCell<Option<Retained<objc2_foundation::NSTimer>>>,
    workspace_search_generation: Cell<u64>,
}

define_class!(
    // SAFETY: NSObject has no extra subclass invariants. The controller, delegates and
    // controls are installed, called and detached only on the AppKit main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NativeMarkdownControllerIvars]
    pub(super) struct NativeMarkdownController;

    // SAFETY: the controller inherits initialized NSObject identity and remains on the AppKit main thread.
    unsafe impl NSObjectProtocol for NativeMarkdownController {}
    // SAFETY: the window surface owns this controller; the panel delegate is weak.
    // windowWillClose: schedules the destruction barrier on the next main-queue transaction.
    unsafe impl NSWindowDelegate for NativeMarkdownController {
        #[unsafe(method_id(windowWillReturnFieldEditor:toObject:))]
        /// Standard AppKit clients use the lazy session editor; the default editor is not assumed to follow the window history delegate.
        fn field_editor(&self, window: &objc2_app_kit::NSWindow, client: Option<&AnyObject>) -> Option<Retained<AnyObject>> {
            if native_markdown_controller_owns_editing_window(self, window) {
                // Closing/borrowed owners use the same provider during resignation preflight; accepted freeze independently forbids construction.
                self.ivars().field_editor_provider.provide(client, self.mtm())
            } else {
                // An old delegate or foreign window cannot borrow this session's editor or Undo authority.
                None
            }
        }

        #[unsafe(method_id(windowWillReturnUndoManager:))]
        /// Provides this window's isolated manager only for actual native history requests; no manager is created after closure.
        fn undo_manager(&self, window: &objc2_app_kit::NSWindow) -> Option<Retained<objc2_foundation::NSUndoManager>> {
            if native_markdown_controller_owns_editing_window(self, window) {
                // A matching actual request may create history lazily; the helper remains sealed after completion.
                self.ivars().undo.manager(&self.ivars().owned)
            } else {
                // Does not expose this session's manager to stale or foreign windows.
                None
            }
        }

        #[unsafe(method(windowDidResize:))]
        /// Uses the sole window delegate; actual resize synchronously updates existing frames without queueing or reconstruction.
        fn window_did_resize(&self, notification: &NSNotification) {
            let Some(window) = notification.object().and_then(|object| object.downcast::<objc2_app_kit::NSWindow>().ok()) else {
                // Foreign notifications or a lost window identity grant no layout authority.
                return;
            };
            if !native_markdown_controller_owns_editing_window(self, &window) {
                // Preconstruction and stale-owner notifications do not enter runtime layout; construction uses the same helper separately.
                return;
            }
            if let Some(controls) = self.ivars().header_controls.get() {
                // OnceCell exposes immutable weak metadata only; the helper holds no TLS/RefMut borrow across native calls.
                controls.layout(self, &window, NativeMarkdownHeaderAuthority::Foreground);
            }
            // Relayouts visible Find UI synchronously after the header, using the same delegate without another observer or timer.
            self.layout_document_area(NativeMarkdownHeaderAuthority::Foreground);
        }

        #[unsafe(method(windowShouldClose:))]
        /// Native close requests emit the same typed intent; actual close waits for saving and the strict release barrier.
        fn window_should_close(&self, window: &objc2_app_kit::NSWindow) -> bool {
            if native_markdown_controller_owns_editing_window(self, window)
                && !self.ivars().close_finalizer_scheduled.get()
                && let Some(panel) = self.ivars().editing_window.load()
            {
                // Does not close here; lifecycle coalescing and weak identity checks handle stale or repeated requests.
                panel.enqueue_markdown_window_close();
            }
            // Rejects AppKit bypassing save; the approved production path uses close(), which does not call shouldClose again.
            false
        }

        #[unsafe(method(windowWillClose:))]
        /// Records one real native close notification and queues its owning lifecycle finalizer.
        fn window_will_close(&self, _notification: &NSNotification) {
            if self.ivars().close_finalizer_scheduled.replace(true) {
                // Repeated native notifications cannot release the same graph or notify the host twice.
                return;
            }
            self.enqueue_close_finalizer();
        }
    }

    // SAFETY: Search fields borrow this main-thread controller; closure clears delegates before releasing the owner.
    unsafe impl NSControlTextEditingDelegate for NativeMarkdownController {
        #[unsafe(method(controlTextDidBeginEditing:))]
        /// Records the shared editor's actual history ownership on a real begin callback.
        fn control_text_did_begin_editing(&self, notification: &NSNotification) {
            self.observe_control_editing(notification);
            if let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok()) {
                self.observe_find_control_input(&field, false, false); // Actual begin synchronizes state only; focus movement is not a text edit.
            }
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        /// Ends field history only after native client completion; failed validation does not emit this notification.
        fn control_text_did_end_editing(&self, notification: &NSNotification) {
            if let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok()) {
                self.observe_find_control_input(&field, false, true); // Consumes the notification's original field and records actual completion before leaving the protocol stack, without early refresh.
                // The notification carries its field identity; the shared boundary checks late/repeated completion without guessing from the current window.
                let borrowed = self.ivars().borrowed.borrow().clone();
                if let Err(error) = borrowed.finished_control_editing(&field, &self.ivars().undo, &self.ivars().owned) {
                    // Preserves committed field input and unknown history; reports the original failure through normal status feedback.
                    self.present_error(&error);
                }
                self.queue_history_availability_refresh();
            }
        }

        #[unsafe(method(controlTextDidChange:))]
        /// Merges actual Find notifications before observing history; refresh waits for protocol return while composition stays in its original field.
        fn control_text_did_change(&self, notification: &NSNotification) {
            if let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok())
                && !self.observe_find_control_input(&field, true, false) && self.owns_workspace_query(&field) {
                // Sidebar input revokes the original Find request without routing document Find through workspace scanning.
                self.ivars().find_input_intent.set(self.find_input_intent().checked_add(1).expect("Find input intent overflow"));
            }
            self.observe_control_editing(notification);
            if let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok()) {
                // Only this window's search field may submit; other field notifications cannot scan the workspace.
                self.enqueue_workspace_search(&field);
            }
        }
    }
    // SAFETY: NSTextFieldDelegate inherits the declared text callbacks under the same lifetime.
    unsafe impl NSTextFieldDelegate for NativeMarkdownController {}

    impl NativeMarkdownController {
        #[unsafe(method(markdownFindOptionsDidChange:))]
        /// Only this bar's three actual options revoke the old intent; ordinary buttons retain explicit navigation/replacement entry points.
        fn markdown_find_options_did_change(&self, sender: &AnyObject) {
            let Some(button) = sender.downcast_ref::<NSButton>() else {
                return; // A foreign option control cannot refresh current-document Find.
            };
            let bar = self.ivars().find_input_bar.borrow().upgrade();
            if let Some(bar) = bar.filter(|bar| bar.owns_search_option(button)) {
                self.enqueue_find_input_feedback(&bar, true);
            }
        }

        #[unsafe(method(markdownHistoryDidChange:))]
        /// Consumes completion notifications from the original session manager only; never queries canRedo inside Undo/group notifications.
        fn markdown_history_did_change(&self, notification: &NSNotification) {
            let Some(manager) = notification.object().and_then(|object| object.downcast::<objc2_foundation::NSUndoManager>().ok()) else {
                return; // A foreign manager notification cannot lend another window's history to this toolbar.
            };
            if self.ivars().undo.owns_observed_manager(&manager) {
                self.queue_history_availability_refresh();
            }
        }

        #[unsafe(method(performMarkdownFindAction:))]
        /// Field Return and buttons share bounded action tags; queued delivery carries only this controller's weak identity and tag.
        fn perform_markdown_find_action(&self, sender: &AnyObject) {
            if let Some(control) = sender.downcast_ref::<NSControl>()
                && let Some(action) = MarkdownFindAction::from_tag(control.tag())
            {
                native_markdown_trace! {
                    // Records the acquired bounded tag and pure input epoch to distinguish native field action from explicit driver Next.
                    eprintln!("[probe] find control action queued tag={} intent={}", action as isize, self.find_input_intent());
                }
                // Captures neither field text nor bar ownership in dispatch; execution verifies the actual live note.
                self.enqueue_owned(move || {
                    native_markdown_trace! {
                        eprintln!("[probe] find control action executing tag={}", action as isize);
                    }
                    if let Err(error) = perform_native_markdown_find_action(action) {
                        present_native_markdown_error(&error);
                    }
                });
            }
        }

        #[unsafe(method(refreshMarkdownWorkspace:))]
        /// Explicit directory return clears the actual query first; newer input revokes the queued button intent.
        fn refresh_markdown_workspace(&self, _sender: &AnyObject) {
            // Fixes input authority before native weak construction or dispatch; execution cannot acquire a successor's epoch.
            let intent = self.workspace_query_intent();
            let identity = std::ptr::from_ref(self).addr();
            self.enqueue_owned(move || {
                if let Err(error) = return_native_markdown_workspace_to_directory(identity, intent) {
                    // Busy or absent owners have no feedback target; the helper reports actual input failure under the original intent.
                    present_native_markdown_error(&error);
                }
            });
        }

        #[unsafe(method(nextMarkdownWorkspacePage:))]
        /// Advances the foreground directory cursor without replacing the open note.
        fn next_markdown_workspace_page(&self, _sender: &AnyObject) {
            // The next page retains the same document owner and advances only bounded directory traversal.
            self.enqueue_owned(|| {
                if let Err(error) = perform_native_markdown_workspace_action(10) {
                    // Page traversal failure leaves the current editor and bounded previous metadata intact.
                    present_native_markdown_error(&error);
                }
            });
        }

        #[unsafe(method(searchMarkdownWorkspace:))]
        /// Reads a bounded query into the session mailbox; queued notification retains no field text.
        fn search_markdown_workspace(&self, sender: &AnyObject) {
            if let Some(field) = sender.downcast_ref::<NSTextField>() {
                // Return revokes pending automatic submission so a late timer cannot submit the same input again.
                self.cancel_workspace_search();
                if let Err(error) = enqueue_native_markdown_workspace_search(self, field) {
                    self.present_error(&error);
                }
            }
        }

        #[unsafe(method(performMarkdownWorkspaceAction:))]
        /// Owned menus use bounded tags; queued document edits still consume the original selection intent captured before getters.
        fn perform_markdown_workspace_action(&self, sender: &AnyObject) {
            // Captures the save-result publication before sender/cell getters; reentry cannot renew an old click's authority.
            let resolution_revision = self.conflict_publication();
            // Fixes the original document/path for file actions first, since capturing a cell selection may itself call native getters.
            let original_note = NativeMarkdownCurrentNoteIntent::capture(self);
            // Sender type and menu getters may reenter; captures the original document intent before querying a possibly new selection.
            let original_paged = NativeMarkdownWorkspacePagedEditIntent::capture(self);
            if let Some(action) = native_markdown_workspace_menu_action(self, sender) {
                if matches!(action, 22 | 23 | 100..=112 | 115..=116 | 118) {
                    if let Some(intent) = original_paged.filter(NativeMarkdownWorkspacePagedEditIntent::current)
                        && let Err(cause) = intent.perform(action)
                    { self.present_error(&cause); }
                    return; // Source consumes only its own original Input intent; it neither queries preview fields nor substitutes them for document input.
                }
                let original_note = if matches!(action, 4..=6 | 30..=34) {
                    let Some(intent) = original_note.filter(NativeMarkdownCurrentNoteIntent::current) else {
                        return; // A menu getter changed the document; reopening the same path cannot renew file-action authority.
                    };
                    Some(MainThreadBound::new(intent, self.mtm()))
                } else {
                    None // Other menu actions retain their own authority without an added current-note file intent.
                };
                if matches!(action, 4 | 5) && self.conflict_publication() != resolution_revision {
                    return; // The result changed during menu getters; a new conflict on the same document cannot authorize the old click.
                }
                // Accepts actual leaves of the owned menu only; groups or foreign tags cannot execute document commands.
                self.enqueue_owned(move || {
                    let mtm = MainThreadMarker::new().expect("正文菜单沿原主队列执行");
                    if original_note.as_ref().is_some_and(|intent| !intent.get(mtm).current()) {
                        return; // Even A-to-B-to-A in the same window cannot consume the old document's file action.
                    }
                    // Enters the existing transaction only after accepting the original intent; modal work still runs after target callback and surface borrow end.
                    let result = match original_note.as_ref() {
                        // Current-note actions consume the original path/document intent instead of choosing a sidebar target at execution.
                        Some(intent) if matches!(action, 4 | 5) => intent.get(mtm).perform_resolution(action, resolution_revision),
                        Some(intent) if action == 6 => super::information_panel::toggle(intent.get(mtm)),
                        Some(intent) => intent.get(mtm).perform(action),
                        None => perform_native_markdown_workspace_action(action),
                    };
                    if let Err(error) = result {
                        if matches!(action, 4 | 5) && original_note.as_ref().is_none_or(|intent| !intent.get(mtm).current()) {
                            // The confirmed document has departed; its conflict failure cannot overwrite a successor's feedback.
                            eprintln!("原笔记的冲突处理未完成，所属窗口或文稿已经变化");
                            return;
                        }
                        present_native_markdown_error(&error);
                    }
                });
            }
        }

        #[unsafe(method(resolveMarkdownConflict:))]
        /// Conflict buttons capture the displayed weak document intent and publication epoch before reading sender or dispatching.
        fn resolve_markdown_conflict(&self, sender: &AnyObject) {
            super::conflict_bar::enqueue_action(self, sender);
        }

        #[unsafe(method(closeMarkdownInformation:))]
        /// Closing the information card removes only that card, without committing input or closing the plugin/window.
        fn close_markdown_information(&self, _sender: &AnyObject) {
            self.hide_information_panel();
        }

        #[unsafe(method(refreshMarkdownInformationWords:))]
        /// Word-count refresh uses the displayed card's original weak document identity without resigning document or cell input.
        fn refresh_markdown_information_words(&self, _sender: &AnyObject) {
            self.refresh_information_panel(true);
        }

        #[unsafe(method(exitMarkdownPlugin:))]
        /// Queues save-before-exit using this controller identity only.
        fn exit_markdown_plugin(&self, _sender: &AnyObject) {
            self.enqueue_owned(move || {
                if let Err(error) = request_native_markdown_window_exit() {
                    // A rejected save/lifecycle transition leaves the draft active and reports the failure.
                    eprintln!("Markdown exit failed: {error}");
                }
            });
        }

        #[unsafe(method(copyEntireMarkdownDocument:))]
        /// Streams current unsaved source to the system pasteboard and reports its explicit result.
        fn copy_entire_markdown_document(&self, _sender: &AnyObject) {
            match copy_active_markdown_document_to_general_pasteboard() {
                // Successful pasteboard ownership confirms the explicit copy action.
                Ok(()) => self.present_copy_confirmation(),
                // A read/write failure preserves both current document and visible session.
                Err(error) => self.present_error(&error),
            }
        }

        #[unsafe(method(insertMarkdownTable:))]
        /// Queues one canonical source-table insertion outside native target dispatch.
        fn insert_markdown_table(&self, _sender: &AnyObject) {
            self.enqueue_owned(move || {
                if let Err(error) = insert_table_into_active_native_markdown_editor() {
                    // Rejected insertion retains the active piece tree and surfaces its bounded error.
                    present_native_markdown_error(&error);
                }
            });
        }

        #[unsafe(method(changeMarkdownPresentationMode:))]
        /// Fixes mode authority before the first native getter; dispatch consumes this intent without acquiring a new document epoch.
        fn change_markdown_presentation_mode(&self, sender: &AnyObject) {
            if let Some(original) = NativeMarkdownWorkspacePagedEditIntent::capture(self) {
                // Captures Input before sender getters; Body keeps one mode intent and waits for actual Ready before normal resignation/commit.
                // TLS lends only a Rust Rc; native getters run after the borrow, and Pending retains the click.
                let body = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    active.as_ref()?.editor.as_ref().map(|paged| Rc::clone(&paged.body))
                });
                if let Some(body) = body {
                    // Synchronous rejection is reported by Body under the original weak Input/document and click epoch, never on successor UI.
                    body.accept_presentation_mode_click(self, original, sender);
                }
                return;
            }
            // sender.downcast may call native code; leases the original surface and captures source/input authority before any getter.
            let feedback = (self.find_input_intent(), self.workspace_query_intent(), self.ivars().header_layout_revision.get());
            let original_note = NativeMarkdownCurrentNoteIntent::capture(self);
            let admitted = with_active_native_markdown_surface(|surface| {
                if !std::ptr::eq(&*surface.controller, self) || !native_markdown_controller_has_foreground_authority(self) {
                    // A foreign or closing window cannot obtain mode authority from the sender's current value.
                    return Err("展示模式动作不属于当前窗口".to_owned());
                }
                // Without a document, changes only the next opening's default mode; cannot recapture a document that appears later.
                if surface.editor.is_some() { return Ok(None); }
                    let ticket = NativeMarkdownEmptyModeAction::empty(self, surface.presentation_mode);
                    let mode = ticket.read_destination(surface, sender)?;
                    Ok((mode != surface.presentation_mode).then_some((mode, ticket)))
            });
            match admitted {
                Ok(Some((mode, ticket))) => {
                    let ticket = MainThreadBound::new(ticket, self.mtm());
                    let original_note = MainThreadBound::new(original_note, self.mtm());
                    // Uses the existing main-queue weak-controller pattern, permitting only this callback's necessary picker compensation.
                    let weak = MainThreadBound::new(Weak::from(self), self.mtm());
                    enqueue_native_markdown_main_task(move || {
                        let mtm = MainThreadMarker::new().expect("mode action is delivered on main thread");
                        let ticket = ticket.get(mtm);
                        let Some(controller) = weak.get(mtm).load() else {
                            // An expired controller cancels the weak intent; no successor window may execute the old command.
                            return;
                        };
                        if !ticket.is_current() {
                            // Busy or newer input revokes this weak intent without replay or acquiring new document authority.
                            if native_markdown_controller_has_foreground_authority(&controller) {
                                // The picker restores only this controller's accepted mode; the scalar cannot authorize switching or document navigation.
                                controller.select_presentation_mode(controller.ivars().published_presentation_mode.get());
                            }
                            return;
                        }
                        if let Err(error) = set_active_native_markdown_presentation_mode(mode) {
                            // An authorized action may still fail IME, draft commit, or mounting; the original entry retains the actual failure.
                            eprintln!("展示模式切换失败：{error}");
                            if original_note.get(mtm).as_ref().is_some_and(NativeMarkdownCurrentNoteIntent::current)
                                && feedback == (controller.find_input_intent(), controller.workspace_query_intent(), controller.ivars().header_layout_revision.get())
                            {
                                // Failure may revoke the mode intent; reports its cause only while the original note and feedback remain current.
                                controller.present_error(&format!("展示模式未切换：{error}"));
                            }
                        }
                    });
                }
                // Reselecting the accepted mode already clears the intent synchronously; no dispatch or UI compensation is needed.
                Ok(None) => {},
                Err(error) => {
                    eprintln!("展示模式切换未接受：{error}");
                    // Busy neither reads a newer surface through dispatch nor replays the command; restores the picker only from this controller's published mode.
                    if native_markdown_controller_has_foreground_authority(self) {
                        // Restores the same owner's picker first, then rechecks the original feedback intent after native reentry.
                        self.select_presentation_mode(self.ivars().published_presentation_mode.get());
                        if native_markdown_controller_has_foreground_authority(self)
                            && feedback == (self.find_input_intent(), self.workspace_query_intent(), self.ivars().header_layout_revision.get())
                            && original_note.as_ref().is_some_and(NativeMarkdownCurrentNoteIntent::current) {
                            // Shows the error row only for the unchanged note and feedback, rather than updating a hidden label or successor note.
                            self.present_error(&format!("展示模式未切换：{error}"));
                        }
                    }
                }
            }
        }
    }
);

impl NativeMarkdownController {
    /// Queues only original weak controller/document identities for save; a late key cannot save a successor note.
    pub(super) fn enqueue_document_save(&self, panel: &NativeMarkdownPanel) -> bool {
        // Constructs native weak identity before borrowing TLS, then checks the actual owner after possible getter reentry.
        let controller = MainThreadBound::new(Weak::from(self), self.mtm());
        let document = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active.as_ref()?;
            if !std::ptr::eq(&*surface.controller, self) || !std::ptr::eq(&*surface.panel, panel) {
                // A foreign delegate or replaced window cannot obtain save authority for the current document.
                return None;
            }
            // Binds an empty window to None too; a document opened while waiting cannot inherit the old key's save.
            Some(
                surface
                    .editor
                    .as_ref()
                    .map(|editor| Rc::downgrade(editor.document())),
            )
        });
        let Some(document) = document else {
            // A stale event queues nothing, reads no document, and leaves current feedback unchanged.
            return false;
        };
        let document = MainThreadBound::new(document, self.mtm());
        // All TLS/document borrows have ended; dispatch carries only weak identities without extending window, document, or mapping lifetime.
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("保存快捷键仅在主队列执行");
            let Some(controller) = controller.get(mtm).load() else {
                // An expired original controller leaves no valid save target for this late request.
                return;
            };
            let is_current = || {
                ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    let Some(surface) = active.as_ref() else {
                        // A closing or leased owner cannot redirect this save to another note.
                        return false;
                    };
                    if !std::ptr::eq(&*surface.controller, &*controller) {
                        // Reopening the same file under another controller does not renew the old event's authority.
                        return false;
                    }
                    match (document.get(mtm), surface.editor.as_ref()) {
                        // The live editor owns the current document; weak comparison checks allocation identity without reading or copying text.
                        (Some(expected), Some(editor)) => {
                            std::rc::Weak::ptr_eq(expected, &Rc::downgrade(editor.document()))
                        }
                        (None, None) => true, // The original window is still empty, preserving the existing save no-op.
                        _ => false, // Note handoff or creation changed the target; saving requires a new user action.
                    }
                })
            };
            if !is_current() {
                // A handoff during dispatch revokes the key; neither save nor stale feedback may reach the new target.
                return;
            }
            if let Err(error) = perform_native_markdown_workspace_action(1) {
                // The same entry commits table/IME input and checks the authoritative disk revision; failure preserves the original draft.
                if is_current() {
                    // Close/handoff reentry during save also prevents its old error from overwriting the new document's status.
                    controller.present_error(&error);
                }
            }
        });
        true
    }

    /// Input notifications may arrive through native reentry; queues a weak request before cancelling scans or creating deadlines outside the borrow.
    fn enqueue_workspace_search(&self, field: &NSTextField) {
        if !self.owns_workspace_query(field) {
            // A foreign notification cannot cancel this search field's deadline or read its contents.
            return;
        }
        self.cancel_workspace_search();
        let generation = self.ivars().workspace_search_generation.get();
        let controller = MainThreadBound::new(Weak::from(self), self.mtm());
        let field = MainThreadBound::new(Weak::from(field), self.mtm());
        self.enqueue_owned(move || {
            let mtm = MainThreadMarker::new().expect("workspace search uses the main queue");
            let (Some(controller), Some(field)) =
                (controller.get(mtm).load(), field.get(mtm).load())
            else {
                // The weak request keeps neither window nor field alive and allocates no search resources after closure.
                return;
            };
            if controller.ivars().workspace_search_generation.get() != generation {
                // Newer input or Return within the event supersedes this request; do not restart its old deadline.
                return;
            }
            if let Err(error) = controller.schedule_workspace_search(&field) {
                // Failure preserves the field and document text; status feedback never consumes input.
                controller.present_error(&error);
            }
        });
    }

    /// Records only the fully assembled bar's weak identity; note replacement revokes it without extra field/document ownership.
    pub(super) fn register_find_input_bar(&self, bar: &Rc<NativeMarkdownFindBar>) {
        *self.ivars().find_input_bar.borrow_mut() = Rc::downgrade(bar);
    }

    /// Pure Rust identity checks permit reentrant feedback setters; the borrow ends before native calls.
    pub(super) fn owns_find_input_bar(&self, bar: &std::rc::Weak<NativeMarkdownFindBar>) -> bool {
        std::rc::Weak::ptr_eq(&self.ivars().find_input_bar.borrow(), bar)
    }

    /// Settled refresh upgrades only the registered original bar, without creating controls or inferring query ownership from a filename.
    pub(super) fn current_find_input_bar(&self) -> Option<Rc<NativeMarkdownFindBar>> {
        self.ivars().find_input_bar.borrow().upgrade()
    }

    /// Explicit actions and actual input share the original intent; advancing it first revokes queued old feedback.
    pub(super) fn advance_find_input_intent(&self) -> u64 {
        let intent = self
            .find_input_intent()
            .checked_add(1)
            .expect("Find input intent overflow");
        self.ivars().find_input_intent.set(intent);
        intent
    }

    /// Input first revokes the old intent and feedback; dispatch carries original weak bar/document identities without recapturing a successor.
    pub(in crate::native_surface) fn enqueue_find_input_feedback(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        refresh: bool,
    ) {
        let intent = self.advance_find_input_intent();
        bar.stage_viewport_feedback(intent, refresh);
        let Some(feedback) =
            NativeMarkdownFindViewportFeedback::capture(bar, self, intent, refresh)
        else {
            return; // An old bar or expired document cancels the request without native input reads or restored feedback.
        };
        feedback.clear(self);
        if !feedback.current(self) {
            return; // New input/close reentry while clearing status revokes the intent, so the old query cannot be scheduled.
        }
        feedback.schedule_highlights(self);
        if !feedback.current(self) {
            return; // Native view claims and redraw requests may reenter too; revoked feedback is never rescheduled.
        }
        let controller = MainThreadBound::new(Weak::from(self), self.mtm());
        let feedback = MainThreadBound::new(feedback, self.mtm());
        self.enqueue_owned(move || {
            let mtm = MainThreadMarker::new().expect("Find反馈只在主队列执行");
            if let Some(controller) = controller.get(mtm).load() {
                feedback.get(mtm).run(&controller);
            } // An expired controller drops the weak intent without replacement UI or a retry task.
        });
    }

    /// Pure reads let input revoke the old executor while Borrowed, without needing an available Active slot.
    pub(super) fn find_input_intent(&self) -> u64 {
        self.ivars().find_input_intent.get()
    }

    /// Reads only the input epoch; after getter reentry delivers didChange, old Next/query work cannot revoke new input.
    pub(super) fn workspace_query_intent(&self) -> u64 {
        self.ivars().workspace_search_generation.get()
    }

    /// Only the owned search field may submit; sender or mutable display labels cannot substitute for identity.
    pub(super) fn owns_workspace_query(&self, field: &NSTextField) -> bool {
        self.ivars()
            .workspace_controls
            .get()
            .and_then(|controls| controls.first())
            .is_some_and(|control| {
                // Construction places search first; pointer equality is used only while both controls are live.
                std::ptr::eq(&**control, field.as_super())
            })
    }

    /// Preserves the original 140 ms search delay; defers bounded field reads without retaining past query copies.
    fn schedule_workspace_search(&self, field: &NSTextField) -> Result<(), String> {
        if !self.owns_workspace_query(field) || !native_markdown_controller_is_current(self) {
            // Closure, foreign fields, and old sessions have no authority to cancel the current query or create a timer.
            return Ok(());
        }
        self.cancel_workspace_search();
        with_active_native_markdown_surface(|surface| {
            // New input immediately revokes old scan/result authority, preventing stale results during debounce.
            surface.stop_search();
            Ok(())
        })?;
        if field.currentEditor().is_some_and(|editor| {
            // Inspects only an existing actual editor; search never creates another field editor.
            editor
                .downcast_ref::<NSTextView>()
                .is_some_and(|view| view.hasMarkedText())
        }) {
            // Uncommitted IME composition waits for the next actual didChange before scheduling search.
            return Ok(());
        }
        let generation = self.ivars().workspace_search_generation.get();
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        let field = MainThreadBound::new(Weak::from(field), self.mtm());
        let block = block2::RcBlock::new(move |_timer: NonNull<objc2_foundation::NSTimer>| {
            let mtm = MainThreadMarker::new().expect("workspace search timer uses the main thread");
            let Some(controller) = weak.get(mtm).load() else {
                // A timer after closure keeps no controller, document, or query string alive.
                return;
            };
            if controller.ivars().workspace_search_generation.get() != generation
                || !native_markdown_controller_is_current(&controller)
            {
                // A stale deadline returns without cancelling a newer timer or scan.
                return;
            }
            controller.cancel_workspace_search();
            if let Some(field) = field.get(mtm).load() {
                // The current owner rereads the actual bounded field; failure retains user input and reports its cause.
                if let Err(error) = enqueue_native_markdown_workspace_search(&controller, &field) {
                    controller.present_error(&error);
                }
            }
        });
        // SAFETY: This main-thread one-shot timer captures only MainThreadBound<Weak> and scalars, not the business object graph.
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                0.14, false, &block,
            )
        };
        if let Err(error) = self.ivars().owned.include("workspace.search.timer", &timer) {
            // An unrecorded candidate cannot leave a live timer; input and the existing document remain intact.
            timer.invalidate();
            return Err(error);
        }
        if self.ivars().workspace_search_generation.get() != generation
            || !native_markdown_controller_is_current(self)
        {
            // Recording includes native weak operations; an invalidated candidate cannot replace a newer input deadline.
            timer.invalidate();
            return Ok(());
        }
        let previous = self.ivars().workspace_search_timer.replace(Some(timer));
        drop(previous);
        Ok(())
    }

    /// Revokes deadline authority before invalidating outside the RefCell borrow, releasing native callbacks on closure.
    pub(super) fn cancel_workspace_search(&self) {
        self.ivars().workspace_search_generation.set(
            self.ivars()
                .workspace_search_generation
                .get()
                .wrapping_add(1),
        );
        let timer = self.ivars().workspace_search_timer.take();
        if let Some(timer) = timer {
            // Explicitly ends run-loop ownership; block release may reenter after the slot is empty and its epoch is revoked.
            timer.invalidate();
        }
    }

    /// Actual character commit resets the existing 850 ms one-shot deadline; one weak note intent spans native cancellation, creation, and expiry.
    pub(super) fn schedule_autosave(
        &self,
        original: &NativeMarkdownCurrentNoteIntent,
        current: &impl Fn() -> bool,
    ) {
        self.cancel_autosave();
        if !current() {
            return; // Old timer release may reenter note switching or save; revoked work cannot occupy the successor note's sole slot.
        }
        let generation = self.ivars().autosave_generation.get();
        let weak = MainThreadBound::new((Weak::from(self), original.clone()), self.mtm());
        if !current() || self.ivars().autosave_generation.get() != generation {
            return; // Native weak construction also occurs outside the slot borrow; a deadline created by reentry retains ownership.
        }
        let block = block2::RcBlock::new(move |_timer: NonNull<objc2_foundation::NSTimer>| {
            let mtm = MainThreadMarker::new().expect("原自动保存期限只在主队列执行");
            let (weak, original) = weak.get(mtm);
            let Some(controller) = weak.load() else {
                return; // The expiry callback retains no window, document, or mapping; a finished session cannot obtain save authority.
            };
            if controller.ivars().autosave_generation.get() != generation
                || !native_markdown_controller_is_current(&controller)
                || !original.current()
            {
                return; // A successor note or newer deadline under the same controller cannot inherit this original note intent.
            }
            controller.cancel_autosave();
            if !original.current()
                || controller.ivars().autosave_generation.get() != generation.wrapping_add(1)
            {
                return; // invalidate may reenter; a new deadline or note switch revokes this save and prevents clearing the newer slot.
            }
            let result = with_active_native_markdown_surface(|surface| {
                if native_markdown_operation_is_busy(
                    surface.pending_note_switch.is_some(),
                    surface.pending_file_mutation.is_some(),
                ) {
                    return Ok(()); // An accepted handoff exclusively owns save/release; this deadline neither retries nor races that handoff.
                }
                // After leasing the original surface, checks the same document/path without recapturing another intent from Active.
                if !std::ptr::eq(&*surface.controller, &*controller)
                    || surface.current_note_relative_path.as_deref()
                        != Some(original.relative_path.as_str())
                    || !surface.editor.as_ref().is_some_and(|editor| {
                        std::ptr::eq(original.document.as_ptr(), Rc::as_ptr(editor.document()))
                    })
                {
                    return Ok(()); // The original document left this use case; no disk effect may target another document.
                }
                if let Some(source) = surface.editor.as_ref()
                    && (source.body.active_cell_body().is_some()
                        || source
                            .text_view
                            .input_state()
                            .map_err(|cause| cause.to_string())?
                            .marked
                            .is_some())
                {
                    // The sole deadline slot is released; cell drafts and IME candidates remain with their input clients and cannot be forcibly committed by a timer.
                    // Draft completion or actual document commit/unmark restores the deadline through existing observers, without polling or normal-input errors.
                    return Ok(());
                }
                surface.save_current_document()
            });
            if let Err(error) = result
                && original.current()
            {
                // Preserves committed-draft and save-error semantics; a late failure cannot reach a successor note's status.
                controller.present_error(&error);
            }
        });
        // SAFETY: Captures only original weak controller/document intents in MainThreadBound plus a scalar epoch; callbacks stay on the main run loop.
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                0.85, false, &block,
            )
        };
        if !current() || self.ivars().autosave_generation.get() != generation {
            timer.invalidate();
            return; // Invalidates only this uninstalled local timer, never a successor deadline created by reentry.
        }
        let previous = self.ivars().autosave_timer.replace(Some(timer));
        drop(previous); // Native destruction occurs outside the slot borrow; the checked epoch prevents replacing a timer just published by another callback.
    }

    /// Invalidates the sole foreground timer before discarding its native owner or selecting another note.
    pub(super) fn cancel_autosave(&self) {
        self.ivars()
            .autosave_generation
            .set(self.ivars().autosave_generation.get().wrapping_add(1));
        let timer = self.ivars().autosave_timer.borrow_mut().take();
        if let Some(timer) = timer {
            // End the Rust timer-state borrow before native invalidation may release the copied block.
            timer.invalidate();
        }
    }

    /// Runs delayed work only while this controller still owns the active foreground surface.
    pub(super) fn enqueue_owned<F: Send + FnOnce() + 'static>(&self, work: F) {
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("Markdown target work runs on the main queue");
            let current = weak
                .get(mtm)
                .load()
                .is_some_and(|controller| native_markdown_controller_is_current(&controller));
            if !current {
                // Old query/menu/button events carry no authority into the next session.
                return;
            }
            work();
        });
    }

    /// Associates windowWillClose finalization with its exact closing surface identity.
    pub(super) fn enqueue_close_finalizer(&self) {
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        enqueue_native_markdown_main_task(move || {
            let mtm =
                MainThreadMarker::new().expect("Markdown finalization runs on the main queue");
            let current = weak.get(mtm).load().is_some_and(|controller| {
                // End all controller/TLS borrows before releasing the native graph or notifying the host.
                CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|closing| {
                    closing
                        .as_ref()
                        .is_some_and(|surface| std::ptr::eq(&*surface.controller, &*controller))
                })
            });
            if current {
                // Only the callback belonging to this pending close may cross its lifecycle barrier.
                finalize_native_markdown_close();
            }
        });
    }

    /// Continue a handoff only after actual old-graph deallocation, preserving this weak controller identity.
    pub(super) fn enqueue_owned_after_release<F: Send + FnOnce() + 'static>(
        &self,
        barrier: Option<&NativeMarkdownEditorReleaseBarrier>,
        work: F,
    ) {
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        let continuation = move || run_accepted_native_markdown_continuation(weak, Box::new(work));
        if let Some(barrier) = barrier {
            // Native completion, not an estimated queue boundary, triggers the one strict old-graph check.
            barrier.when_released(continuation);
        } else {
            // Operations from an empty/unaffected editor still unwind their menu callback before filesystem work.
            enqueue_native_markdown_main_task(continuation);
        }
    }

    /// Seals window-level query input during accepted handoff so a shared editor cannot acquire new business input over the old release receipt.
    pub(super) fn set_workspace_input_enabled(&self, enabled: bool) {
        if let Some(controls) = self.ivars().workspace_controls.get() {
            for control in controls {
                if let Some(field) = control.downcast_ref::<NSTextField>() {
                    // Configures only project-created search fields, without taking over AppKit's private Find subtree.
                    field.setEnabled(enabled);
                    field.setEditable(enabled);
                    field.setSelectable(enabled);
                }
            }
        }
    }

    /// Necessary empty-state compensation continues only for the same empty owner; checks after every setter prevent modifying successor controls.
    pub(super) fn set_workspace_input_enabled_while(
        &self,
        enabled: bool,
        still_owned: impl Fn() -> bool,
    ) {
        if let Some(controls) = self.ivars().workspace_controls.get() {
            for control in controls {
                // Touches only owned fields; newer query input does not revoke necessary compensation, so all three properties stay consistent.
                if let Some(field) = control.downcast_ref::<NSTextField>() {
                    if !still_owned() {
                        return;
                    }
                    field.setEnabled(enabled);
                    if !still_owned() {
                        return;
                    }
                    field.setEditable(enabled);
                    if !still_owned() {
                        return;
                    }
                    field.setSelectable(enabled);
                }
            }
        }
    }

    /// Clones the tracker before native getters; no RefCell borrow spans actual editing callbacks.
    fn observe_control_editing(&self, notification: &NSNotification) {
        if let Some(field) = notification
            .object()
            .and_then(|object| object.downcast::<NSTextField>().ok())
        {
            // This controller delegates owned search fields only; document/table bridges observe their own inputs.
            let borrowed = self.ivars().borrowed.borrow().clone();
            if let Err(error) =
                borrowed.observe_editing(&field, &self.ivars().undo, &self.ivars().owned)
            {
                // Ownership failure reaches later preflight without clearing unknown history or discarding user input.
                self.present_error(&error);
            }
            self.queue_history_availability_refresh();
        }
    }

    /// Preserves field delegates; actual event completion supplements composition-state boundaries and observes original shared-editor history.
    pub(super) fn observe_native_field_editor(&self, editor: &NSTextView) {
        self.observe_find_native_editor(editor); // Event completion only observes missing state; input already handled by protocol hooks does not advance intent again.
        let borrowed = self.ivars().borrowed.borrow().clone();
        if let Err(error) =
            borrowed.observe_native_editor(editor, &self.ivars().undo, &self.ivars().owned)
        {
            // Retains bounded ownership failure; subsequent closure preserves the original editing surface.
            self.present_error(&error);
        }
        self.queue_history_availability_refresh();
    }

    /// Install empty foreground control slots before NSObject initialization can dispatch any delegate method.
    pub(super) fn new(
        owned: &Rc<NativeMarkdownOwnedLedger>,
        panel: &NativeMarkdownPanel,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let undo = Rc::new(NativeMarkdownSessionUndo::default());
        let borrowed = NativeMarkdownBorrowedEditorSnapshot::default();
        let field_editor_provider = NativeMarkdownFieldEditorProvider::new(&undo, owned, &borrowed);
        let controller = Self::alloc(mtm).set_ivars(NativeMarkdownControllerIvars {
            owned: Rc::clone(owned),
            undo,
            field_editor_provider,
            editing_window: Weak::from(panel),
            borrowed: RefCell::new(borrowed),
            exit_button: OnceCell::new(),
            presentation_mode_button: OnceCell::new(),
            published_presentation_mode: Cell::new(MarkdownPresentationMode::InlinePreview),
            table_menu_item: OnceCell::new(),
            copy_document_menu_item: OnceCell::new(),
            status_label: OnceCell::new(),
            footer_labels: OnceCell::new(),
            information_panel: RefCell::new(None),
            conflict_bar: OnceCell::new(),
            footer_document: Cell::new(None),
            footer_save_result: Cell::new(NativeMarkdownFooterSaveResult::Current),
            feedback_visible: Cell::new(false),
            document_area: RefCell::new(None),
            document_area_revision: Cell::new(0),
            header_controls: OnceCell::new(),
            header_layout_revision: Cell::new(0),
            document_actions_available: Cell::new(false),
            history_update_queued: Cell::new(false),
            history_refreshing: Cell::new(false),
            history_observing: Cell::new(false),
            history_manager: RefCell::new(None),
            find_input_bar: RefCell::new(std::rc::Weak::new()),
            find_input_intent: Cell::new(0),
            close_finalizer_scheduled: Cell::new(false),
            workspace_controls: OnceCell::new(),
            next_sidebar_page: OnceCell::new(),
            autosave_timer: RefCell::new(None),
            autosave_generation: Cell::new(0),
            workspace_search_timer: RefCell::new(None),
            workspace_search_generation: Cell::new(0),
        });
        // SAFETY: NSObject's initializer has no additional preconditions and all Rust ivars are initialized.
        unsafe { msg_send![super(controller), init] }
    }

    /// Transfers header controls into the unique controller owner before native actions become available.
    pub(super) fn install_controls(
        &self,
        exit_button: Retained<NSButton>,
        presentation_mode_button: Retained<NSButton>,
        status_label: Retained<NSTextField>,
        footer_labels: [Retained<NSTextField>; 3],
    ) {
        assert!(
            self.ivars().exit_button.set(exit_button).is_ok(),
            "native Markdown exit control must be installed exactly once"
        );
        assert!(
            self.ivars()
                .presentation_mode_button
                .set(presentation_mode_button)
                .is_ok(),
            "native Markdown presentation mode control must be installed exactly once"
        );
        assert!(
            self.ivars().status_label.set(status_label).is_ok(),
            "native Markdown status control must be installed exactly once"
        );
        assert!(
            self.ivars().footer_labels.set(footer_labels).is_ok(),
            "原footer三列只能安装一次"
        );
    }

    /// Displays a bounded status message without changing the draft or storing its source text.
    pub(super) fn present_error(&self, message: &str) {
        let bounded = message
            .chars()
            .take(MAX_NATIVE_MARKDOWN_STATUS_CHARACTERS)
            .collect::<String>();
        if let Some(status_label) = self.ivars().status_label.get() {
            // A mounted status control receives only the bounded message, never a complete document body.
            let native_message = NSString::from_str(&bounded);
            status_label.setStringValue(&native_message);
            // The tooltip exposes the same bounded Chinese error when the compact status row cannot fit it; no document text is retained.
            status_label.setToolTip(Some(&native_message));
            status_label.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                0.96, 0.43, 0.43, 1.0,
            )));
        }
        self.set_feedback_visible(true);
    }

    /// Reflects actual cursor exhaustion without probing the filesystem from idle UI.
    pub(super) fn set_next_sidebar_page_available(&self, available: bool) {
        // The button is installed once and remains borrowed by its native target until close detaches it.
        if let Some(button) = self.ivars().next_sidebar_page.get() {
            // Only a live pager with remaining entries enables advancing this bounded metadata page.
            button.setEnabled(available);
        }
    }

    /// Coalesces actual editing/focus events and history completion; dispatch weakly references the original controller without document or manager captures.
    pub(super) fn queue_history_availability_refresh(&self) {
        if !self.ivars().document_actions_available.get()
            || !native_markdown_controller_has_foreground_authority(self)
            || self.ivars().history_update_queued.replace(true)
        {
            return; // Empty, closing, or already queued state creates neither a duplicate task nor history objects.
        }
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("历史按钮更新沿原主队列执行");
            let Some(controller) = weak.get(mtm).load() else {
                return; // The old task keeps no window alive and cannot search for a successor controller.
            };
            controller.ivars().history_update_queued.set(false);
            controller.refresh_history_availability();
        });
    }

    /// Displays history that the button will actually execute: the real table field uses local history, otherwise the sole Core history applies.
    fn refresh_history_availability(&self) {
        if !self.ivars().document_actions_available.get()
            || !native_markdown_controller_has_foreground_authority(self)
            || self.ivars().history_refreshing.replace(true)
        {
            return; // canRedo checkpoint may reenter; this refresh stack does not start another getter round.
        }
        // The lease moves the surface out of the TLS borrow before all native getters and control updates.
        let _ = with_active_native_markdown_surface(|surface| {
            if !std::ptr::eq(&*surface.controller, self)
                || native_markdown_operation_is_busy(
                    surface.pending_note_switch.is_some(),
                    surface.pending_file_mutation.is_some(),
                )
            {
                return Ok(()); // A document in handoff cannot publish editable toolbar state.
            }
            let Some(editor) = surface.editor.as_ref() else {
                return Ok(()); // Empty-state buttons were disabled synchronously by set_copy_document_available(false).
            };
            let source = editor;
            {
                let parent_before = source
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?;
                let cell = source.body.active_cell_body();
                let target = match &cell {
                    Some(cell) => cell.view().map_err(|cause| cause.to_string())?,
                    None => Rc::clone(&source.text_view),
                };
                let before = target.input_state().map_err(|cause| cause.to_string())?;
                let current = || {
                    native_markdown_controller_has_foreground_authority(self)
                        && self.ivars().document_actions_available.get()
                        && source
                            .text_view
                            .input_state()
                            .is_ok_and(|state| state == parent_before)
                        && match (&cell, source.body.active_cell_body()) {
                            (None, None) => true,
                            (Some(original), Some(actual)) => Rc::ptr_eq(original, &actual),
                            _ => false, // Finishing or reopening the cell during queries prevents publishing old button state to the new client.
                        }
                        && target.input_state().is_ok_and(|state| state == before)
                };
                let available = if before.marked.is_some() {
                    [false; 2] // Candidates belong to the current IME; history buttons cannot force confirmation or switch history layers.
                } else if cell.is_some() {
                    let facts = target
                        .cell_history_facts()
                        .map_err(|cause| cause.to_string())?
                        .ok_or("当前单元格没有原局部历史客户")?;
                    [facts.can_undo, facts.can_redo]
                } else {
                    let document = editor.document().borrow();
                    [document.can_undo(), document.can_redo()]
                };
                if let Some(header) = self.ivars().header_controls.get() {
                    header.set_history_available(available, &current);
                }
                Ok(()) // Cells query the actual local stack and document input queries canonical history; no field Undo manager is created.
            }
        });
        self.ivars().history_refreshing.set(false); // Only subsequent actual notifications may refresh after this synchronous stack returns.
    }

    /// Accepted graph teardown first removes Weak state, then unregisters this controller's three original history observations without touching history contents.
    fn remove_history_notifications(&self) {
        let previous = self.ivars().history_manager.take();
        if !self.ivars().history_observing.replace(false) {
            return; // A controller that never subscribed owns no notification-center cleanup.
        }
        let center = objc2_foundation::NSNotificationCenter::defaultCenter();
        for name in native_markdown_history_notifications() {
            // SAFETY: Removes only this observer's exact three names; nil object covers its previously registered manager without affecting other observers.
            unsafe { center.removeObserver_name_object(self, Some(name), None) };
        }
        drop(previous); // Drops native Weak outside state borrows, retaining neither manager nor document strongly.
    }

    /// Enables full-document copy only for an actual canonical document; empty state cannot imply exportable content.
    pub(super) fn set_copy_document_available(&self, available: bool) {
        self.ivars().document_actions_available.set(available);
        if let Some(copy_document_item) = self.ivars().copy_document_menu_item.get() {
            // An empty workspace has no copyable document, so the menu must remain unavailable.
            copy_document_item.setEnabled(available);
        }
        if let Some(controls) = self.ivars().workspace_controls.get() {
            for control in controls.iter().skip(NATIVE_MARKDOWN_TOOLBAR_START + 2) {
                if self.ivars().close_finalizer_scheduled.get() {
                    return;
                }
                // Other document actions retain the document-availability gate; Undo/Redo separately reflect the history they execute.
                control.setEnabled(available);
            }
        }
        if let Some(button) = self.ivars().presentation_mode_button.get() {
            // Source switching retains its document gate; an empty surface cannot produce a document mode action.
            button.setEnabled(available);
        }
        set_current_note_actions_available(self, available); // The same document availability updates the five existing More actions.
        if !available {
            // Accepted document handoff disables old buttons immediately; queued refresh cannot reach a successor or empty document.
            self.remove_history_notifications();
            if let Some(header) = self.ivars().header_controls.get() {
                header.set_history_available([false; 2], &|| {
                    !self.ivars().document_actions_available.get()
                });
            }
        } else {
            self.queue_history_availability_refresh(); // Queries the original Core after initial publication, without creating a manager during construction.
        }
    }

    /// Enables the table menu from existing document availability without adding a persistent toolbar button.
    pub(super) fn set_table_insertion_available(&self, available: bool) {
        if let Some(table_item) = self.ivars().table_menu_item.get() {
            // The table menu is available only when a canonical document exists.
            table_item.setEnabled(available);
        }
    }

    /// Clears transient command feedback after an operation without allocating a status history.
    pub(super) fn clear_status(&self) {
        if let Some(status_label) = self.ivars().status_label.get() {
            // Clear the existing label without creating a background status lifecycle.
            status_label.setStringValue(ns_string!(""));
            // Clearing visible status also clears old tooltips so a new document cannot inherit the previous decision's feedback.
            status_label.setToolTip(None);
        }
        self.set_feedback_visible(false);
    }

    /// Ends all borrowed header-control target/action relations before releasing the native controller.
    pub(super) fn detach_controls(&self) {
        self.ivars().document_actions_available.set(false);
        self.ivars().history_update_queued.set(false);
        self.remove_history_notifications();
        // Foreground close cancels persistence work before detaching any target or host lifecycle edge.
        self.cancel_autosave();
        self.cancel_workspace_search();
        // Accepted closure clears paths and release-related errors from owned status controls.
        self.clear_status();
        self.clear_footer();
        self.detach_information_panel();
        self.detach_conflict_bar();
        // Extra workspace controls borrow this target; detach them before the surface releases it.
        if let Some(controls) = self.ivars().workspace_controls.get() {
            // Every foreground workspace control must stop borrowing this target before controller release.
            for control in controls {
                // Clear both selector and target; a surviving native control cannot call into a closed session.
                // SAFETY: all controls and the target belong to this main-thread surface.
                unsafe {
                    control.setTarget(None);
                    control.setAction(None);
                }
                if let Some(field) = control.downcast_ref::<NSTextField>() {
                    // Disconnects only project-configured search delegates, preserving framework Find delegates.
                    // SAFETY: This controller configured the main-thread field's delegate; nil validly disconnects that relationship.
                    unsafe {
                        field.setDelegate(None);
                    }
                }
            }
        }
        detach_native_markdown_workspace_menu(self);
        if let Some(exit_button) = self.ivars().exit_button.get() {
            // SAFETY: the live main-thread control borrows this controller; clearing its target/action precedes owner release.
            unsafe {
                exit_button.setTarget(None);
                exit_button.setAction(None);
            }
        }
        if let Some(copy_document_item) = self.ivars().copy_document_menu_item.get() {
            // SAFETY: The main-thread copy menu disconnects its borrowed callback before controller release.
            unsafe {
                copy_document_item.setTarget(None);
                copy_document_item.setAction(None);
            }
        }
        if let Some(table_item) = self.ivars().table_menu_item.get() {
            // SAFETY: The main-thread table menu disconnects its borrowed callback before controller release.
            unsafe {
                table_item.setTarget(None);
                table_item.setAction(None);
            }
        }
        if let Some(presentation_mode_button) = self.ivars().presentation_mode_button.get() {
            // SAFETY: the live main-thread mode control is detached before this target is released.
            unsafe {
                presentation_mode_button.setTarget(None);
                presentation_mode_button.setAction(None);
            }
        }
    }

    /// Synchronizes the accepted actual mode with its native selector; the scalar is used only for feedback compensation.
    pub(super) fn select_presentation_mode(&self, mode: MarkdownPresentationMode) {
        // The caller supplies an accepted mode or initial window truth; updates the pure feedback scalar before native setters.
        self.ivars().published_presentation_mode.set(mode);
        if let Some(button) = self.ivars().presentation_mode_button.get() {
            // Success and refusal both display the accepted mode; an unfinished button click cannot become document truth.
            button.setState(if mode == MarkdownPresentationMode::Source {
                objc2_app_kit::NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
        }
        // Menus follow the accepted actual mode; document availability alone cannot re-enable folding in preview.
        set_current_note_actions_available(self, self.ivars().document_actions_available.get());
        self.render_footer();
    }
}

/// Queues weak notification after actual character callbacks return; the same note intent spans status projection and the sole autosave deadline.
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) fn queue_native_markdown_autosave_for_storage(storage: &PieceBackedMarkdownTextStorage) {
    #[cfg(feature = "native-markdown-runtime-probe")]
    {
        // The explicit probe records actual entry before dispatch; rejected input must never reach it, rather than merely leaving a timer unexecuted.
        let count = storage.ivars().autosave_enqueue_count.get();
        storage
            .ivars()
            .autosave_enqueue_count
            .set(count.checked_add(1).expect("探针自动保存入队次数溢出"));
    }
    let weak = MainThreadBound::new(Weak::from(storage), storage.mtm());
    enqueue_native_markdown_main_task(move || {
        let mtm = MainThreadMarker::new().expect("原正文修改通知只在主队列执行");
        let Some(storage) = weak.get(mtm).load() else {
            // Expired storage cannot schedule a disk save for a successor document through late notification.
            return;
        };
        let original = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active.as_ref()?;
            let editor = surface.editor.as_ref()?;
            let document = storage.document()?;
            if !Rc::ptr_eq(editor.document(), &document) {
                return None; // Late old storage may only be released; it cannot observe or schedule a successor note's save.
            }
            let original = NativeMarkdownCurrentNoteIntent::for_document(
                &surface.controller,
                &surface.panel,
                editor.document(),
                surface.current_note_relative_path.as_deref()?,
            );
            Some((surface.controller.clone(), original))
        }); // Captures only the existing original intent and controller claim; all document borrows/native refreshes occur after the TLS borrow.
        if let Some((controller, original)) = original {
            publish_native_markdown_document_edit(controller, original);
        }
    });
}

/// Actual document and cell input share the existing history-refresh queue; weak document identity restricts ownership without keeping text alive.
pub(super) fn queue_native_markdown_history_for_document(document: &WeakMarkdownDocument) {
    let controller = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        let surface = active.as_ref()?;
        let editor = surface.editor.as_ref()?;
        std::rc::Weak::ptr_eq(document, &Rc::downgrade(editor.document()))
            .then(|| surface.controller.clone())
    });
    if let Some(controller) = controller {
        // Creates the original native weak intent after the TLS borrow; dispatch queries the current client without carrying stale canUndo state.
        controller.queue_history_availability_refresh();
    }
}

/// Draft completion restores the original canonical save deadline; local draft input updates history without resetting document debounce.
pub(super) fn queue_native_markdown_local_input_for_document(document: &WeakMarkdownDocument) {
    let mtm = MainThreadMarker::new().expect("单格输入反馈沿原主线程排队");
    let document = MainThreadBound::new(document.clone(), mtm);
    enqueue_native_markdown_main_task(move || {
        let mtm = MainThreadMarker::new().expect("单格输入反馈沿原主队列执行");
        let document = document.get(mtm);
        // Copy or mode changes lease the surface before draft completion; waits for lease restoration instead of treating Borrowed as closed.
        // Dispatch carries only the original document Weak; reopening/switching revokes it without retained dirty or history-availability snapshots.
        let resume = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            active.as_ref().is_some_and(|surface| {
                surface.editor.as_ref().is_some_and(|editor| {
                    std::rc::Weak::ptr_eq(document, &Rc::downgrade(editor.document()))
                        && editor.body.active_cell_body().is_none()
                        && editor.document().borrow().is_dirty()
                })
            })
        });
        queue_native_markdown_history_for_document(document);
        if resume {
            // Escape cancellation or mode change in an unchanged cell may follow timer expiry; the same document still needs its deadline restored in preview.
            // An active cell fails this condition; local keystrokes neither reset document save nor cause polling or forced candidate confirmation.
            queue_native_markdown_autosave_for_document(document.clone());
        }
    });
}

/// Paged input carries only the original canonical Weak; a late commit cannot reschedule autosave for a switched note.
pub(super) fn queue_native_markdown_autosave_for_document(document: WeakMarkdownDocument) {
    let mtm = MainThreadMarker::new().expect("正文提交反馈仅在主线程排队");
    let document = MainThreadBound::new(document, mtm);
    enqueue_native_markdown_main_task(move || {
        let mtm = MainThreadMarker::new().expect("正文提交反馈仅在主线程执行");
        let identity = document.get(mtm);
        let original = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active.as_ref()?;
            let editor = surface.editor.as_ref()?;
            if !std::rc::Weak::ptr_eq(identity, &Rc::downgrade(editor.document())) {
                return None;
            }
            Some((
                surface.controller.clone(),
                NativeMarkdownCurrentNoteIntent::for_document(
                    &surface.controller,
                    &surface.panel,
                    editor.document(),
                    surface.current_note_relative_path.as_deref()?,
                ),
            ))
        });
        if let Some((controller, original)) = original {
            publish_native_markdown_document_edit(controller, original);
        }
    });
}

/// Both actual editing backends share the existing footer, conflict result, and sole autosave deadline without another save policy.
fn publish_native_markdown_document_edit(
    controller: Retained<NativeMarkdownController>,
    original: NativeMarkdownCurrentNoteIntent,
) {
    if !original.current() {
        return; // Storage-claim release or accepted handoff cannot restore an old notification's authority.
    }
    let Some(document) = original.document.upgrade() else {
        return;
    };
    let (generation, footer) = {
        let document = document.borrow();
        (
            document.generation(),
            NativeMarkdownFooterDocument::capture(&document),
        )
    };
    drop(document); // Retains only O(1) display scalars; later native reentry spans neither a canonical borrow nor a strong document claim.
    let save_result = controller.ivars().footer_save_result.get();
    let current = || {
        if !original.current() || controller.ivars().footer_save_result.get() != save_result {
            return false;
        }
        let Some(document) = original.document.upgrade() else {
            return false;
        };
        let document = document.borrow();
        document.generation() == generation
            && NativeMarkdownFooterDocument::capture(&document) == footer
    }; // Reentrant edits/save/conflict changes on the same document revoke old aggregates, preventing cancellation of a newer deadline or rescheduling paused save.
    if !current() {
        return;
    }
    controller.refresh_footer_document(Some(footer));
    if !current() {
        return; // Footer layout setters changed the note or same document; stop subsequent information-card and timer updates.
    }
    controller.refresh_information_panel(false);
    if !current() {
        return; // The information card updates bounded aggregates only; its native callbacks cannot transfer save decisions to a successor.
    }
    if footer.is_dirty() {
        controller.schedule_autosave(&original, &current);
    } else {
        controller.cancel_autosave(); // Actual save or Undo to clean cancels only the deadline still owned by this intent.
    }
    if !current() {
        return; // Timer construction/cancellation may also reenter; the original history refresh cannot use an old notification to access a new note.
    }
    controller.queue_history_availability_refresh();
}

/// Checks immutable panel identity without invoking reentrant native methods under the TLS borrow.
pub(super) fn native_markdown_panel_is_current(panel: &NativeMarkdownPanel) -> bool {
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        active
            .as_ref()
            .is_some_and(|surface| std::ptr::eq(&*surface.panel, panel))
    })
}

/// Checks immutable controller identity before a delayed action can address active document state.
pub(super) fn native_markdown_controller_is_current(controller: &NativeMarkdownController) -> bool {
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        active
            .as_ref()
            .is_some_and(|surface| std::ptr::eq(&*surface.controller, controller))
    })
}

/// Normal UI mutation requires Active or the exact borrowed owner; accepted preparation and Closing grant no frame/input publication authority.
pub(super) fn native_markdown_controller_has_foreground_authority(
    controller: &NativeMarkdownController,
) -> bool {
    let preparing_close = VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some();
    let close_scheduled = controller.ivars().close_finalizer_scheduled.get();
    let identity = std::ptr::from_ref(controller).addr();
    let borrowed = BORROWED_NATIVE_MARKDOWN_OWNER.with(|owner| owner.get() == Some(identity));
    let active = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|owner| {
        owner.as_ref().is_some_and(|surface| {
            std::ptr::eq(&*surface.controller, controller) && surface.closing_request.is_none()
        })
    });
    // Reads only TLS identity/scalars, without weak upgrades or native calls across borrows; does not query the Closing slot.
    foreground_flags_allow_mutation(active, borrowed, preparing_close, close_scheduled)
}

/// Borrowed show and Active resize share this pure gate; liveness or an old revision cannot authorize mutation after closure.
fn foreground_flags_allow_mutation(
    active: bool,
    borrowed: bool,
    preparing_close: bool,
    close_scheduled: bool,
) -> bool {
    (active || borrowed) && !preparing_close && !close_scheduled
}

/// Matches both window and controller to actual Active, Closing, or borrowed ownership; TLS borrows read identities only.
pub(super) fn native_markdown_controller_owns_editing_window(
    controller: &NativeMarkdownController,
    window: &objc2_app_kit::NSWindow,
) -> bool {
    let Some(panel) = controller.ivars().editing_window.load() else {
        // The controller has no strong window edge; an expired window cannot regain input capability.
        return false;
    };
    let owned_window: &objc2_app_kit::NSWindow = &panel;
    let exact_window = std::ptr::eq(owned_window, window);
    drop(panel);
    if !exact_window {
        // Ends the temporary weak upgrade before global-owner reads; a foreign window grants no authority.
        return false;
    }
    let identity = std::ptr::from_ref(controller).addr();
    let matches =
        |surface: &NativeMarkdownWindowSurface| std::ptr::eq(&*surface.controller, controller);
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|owner| owner.as_ref().is_some_and(matches))
        || CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|owner| owner.as_ref().is_some_and(matches))
        || BORROWED_NATIVE_MARKDOWN_OWNER.with(|owner| owner.get() == Some(identity))
}

/// Reports a bounded failure only on the active main-thread native controller.
pub(super) fn present_native_markdown_error(error: &str) {
    if MainThreadMarker::new().is_none() {
        // Worker failures must return through main-queue completion before touching native status.
        return;
    }
    // End the global state borrow before native label/font setters can deliver synchronous callbacks.
    let controller = ACTIVE_NATIVE_MARKDOWN_SURFACE
        .with_borrow(|active| active.as_ref().map(|surface| surface.controller.clone()));
    if let Some(controller) = controller {
        // A closed or temporarily lent surface has no current status target and is never recreated for an error.
        controller.present_error(error);
    }
}

/// Execute an accepted handoff only for its originating controller, after any active owner lease ends.
fn run_accepted_native_markdown_continuation(
    weak: MainThreadBound<Weak<NativeMarkdownController>>,
    work: Box<dyn FnOnce() + Send>,
) {
    let mtm = MainThreadMarker::new().expect("native handoff continuations use the main queue");
    let Some(controller) = weak.get(mtm).load() else {
        // Deallocated controller identity cannot grant authority over a successor session.
        return;
    };
    let identity = std::ptr::from_ref(&*controller).addr();
    let leased = BORROWED_NATIVE_MARKDOWN_OWNER.with(|busy| busy.get() == Some(identity));
    if leased {
        // Actual native completion can arrive in a synchronous nested AppKit loop. Preserve this
        // already accepted operation until the exact lease's restoration event, retaining no controller.
        drop(controller);
        defer_native_markdown_continuation_until_restored(move || {
            run_accepted_native_markdown_continuation(weak, work);
        });
        return;
    }
    let current = native_markdown_controller_is_current(&controller);
    drop(controller);
    if !current {
        // Window close or a replacement supersedes the old transition; no destination mutation may run.
        return;
    }
    // End native weak upgrades and all TLS borrows before the canonical/filesystem transition executes.
    work();
}
