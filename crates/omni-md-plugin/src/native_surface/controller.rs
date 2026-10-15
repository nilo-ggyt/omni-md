//! Owns native action targets, foreground controls and the single autosave deadline.
//! Delayed work carries weak originating identity; controls and timer detach before session release.

use super::*;

/// Owns foreground controls and one cancellable autosave deadline; queued work retains only weak identity.
pub(super) struct NativeMarkdownControllerIvars {
    pub(super) exit_button: OnceCell<Retained<NSButton>>,
    pub(super) presentation_mode_button: OnceCell<Retained<NSPopUpButton>>,
    pub(super) table_button: OnceCell<Retained<NSButton>>,
    pub(super) copy_document_button: OnceCell<Retained<NSButton>>,
    pub(super) status_label: OnceCell<Retained<NSTextField>>,
    pub(super) close_finalizer_scheduled: Cell<bool>,
    pub(super) workspace_controls: OnceCell<Vec<Retained<NSControl>>>,
    pub(super) next_sidebar_page: OnceCell<Retained<NSButton>>,
    // One foreground-only debounce timer; cancellation precedes note handoff and native close.
    pub(super) autosave_timer: RefCell<Option<Retained<objc2_foundation::NSTimer>>>,
    pub(super) autosave_generation: Cell<u64>,
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

    impl NativeMarkdownController {
        #[unsafe(method(refreshMarkdownWorkspace:))]
        /// Starts a new bounded metadata page after the current target callback returns.
        fn refresh_markdown_workspace(&self, _sender: &AnyObject) {
            // Refresh reconstructs a foreground metadata cursor without touching unsaved editor text.
            self.enqueue_owned(|| {
                if let Err(error) = perform_native_markdown_workspace_action(9) {
                    // A failed cursor refresh preserves the active body and reports only its foreground status.
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
        /// Copies one submitted query before deferring cancellable body/title search.
        fn search_markdown_workspace(&self, sender: &AnyObject) {
            // Return submits one bounded query; no search service exists while the plugin is idle.
            if let Some(field) = sender.downcast_ref::<NSTextField>() {
                // A real search control supplies the query; arbitrary senders have no search authority.
                let query = field.stringValue().to_string();
                self.enqueue_owned(move || {
                    // Search failure keeps the current editor and its unsaved piece tree intact.
                    if let Err(error) = search_native_markdown_workspace(query) {
                        present_native_markdown_error(&error);
                    }
                });
            }
        }

        #[unsafe(method(performMarkdownWorkspaceAction:))]
        /// Captures a finite local menu tag and defers modal/document work to its owning session.
        fn perform_markdown_workspace_action(&self, sender: &AnyObject) {
            // Pop-up tags are local UI commands, independent of the host's plugin protocol.
            if let Some(button) = sender.downcast_ref::<NSPopUpButton>()
                && let Some(item) = button.selectedItem()
            {
                // Only the selected item of the owning popup contributes an executable local command tag.
                let action = item.tag();
                self.enqueue_owned(move || {
                    // Modal actions run after the target callback, with no surface borrow held.
                    if let Err(error) = perform_native_markdown_workspace_action(action) {
                        present_native_markdown_error(&error);
                    }
                });
            }
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
        /// Validates a local mode choice before deferring its presentation-only transaction.
        fn change_markdown_presentation_mode(&self, sender: &AnyObject) {
            let Some(button) = sender.downcast_ref::<NSPopUpButton>() else {
                // A different sender cannot supply a valid presentation-mode selection.
                return;
            };
            let Ok(index) = usize::try_from(button.indexOfSelectedItem()) else {
                // Native absence is represented by a negative index and has no mode mapping.
                return;
            };
            let Some(mode) = MarkdownPresentationMode::from_popup_index(index) else {
                // Out-of-range menu indices cannot invent a new source/preview state.
                return;
            };
            self.enqueue_owned(move || {
                if let Err(error) = set_active_native_markdown_presentation_mode(mode) {
                    // A failed cell commit vetoes the display transition and keeps its editable draft.
                    eprintln!("Markdown presentation mode change failed: {error}");
                }
            });
        }
    }
);

impl NativeMarkdownController {
    /// Resets the baseline's 850 ms foreground debounce using one weak, nonrepeating native timer.
    pub(super) fn schedule_autosave(&self) {
        self.cancel_autosave();
        let generation = self.ivars().autosave_generation.get();
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        let block = block2::RcBlock::new(move |_timer: NonNull<objc2_foundation::NSTimer>| {
            let mtm = MainThreadMarker::new()
                .expect("the foreground autosave timer uses the main run loop");
            let Some(controller) = weak.get(mtm).load() else {
                // The timer holds no strong controller/document owner after foreground teardown.
                return;
            };
            if controller.ivars().autosave_generation.get() != generation
                || !native_markdown_controller_is_current(&controller)
            {
                // An invalidated or replaced timer has no authority over a later note/session.
                return;
            }
            // Release timer ownership before persistence or status setters can reenter native callbacks.
            controller.cancel_autosave();
            let result = with_active_native_markdown_surface(|surface| {
                if native_markdown_operation_is_busy(
                    surface.pending_note_switch.is_some(),
                    surface.pending_file_mutation.is_some(),
                ) {
                    // A handoff already owns save/release; the debounce cannot overlap that transaction.
                    return Ok(());
                }
                surface.save_current_document()
            });
            if let Err(error) = result {
                // Conflicts preserve the live canonical draft and external file; no retry timer is created.
                controller.present_error(&error);
            }
        });
        // SAFETY: the block captures only Send MainThreadBound<Weak> and a scalar generation.
        // It is scheduled on the owning main run loop and neither captures nor extends a document.
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                0.85, false, &block,
            )
        };
        *self.ivars().autosave_timer.borrow_mut() = Some(timer);
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

    /// Install empty foreground control slots before NSObject initialization can dispatch any delegate method.
    pub(super) fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let controller = Self::alloc(mtm).set_ivars(NativeMarkdownControllerIvars {
            exit_button: OnceCell::new(),
            presentation_mode_button: OnceCell::new(),
            table_button: OnceCell::new(),
            copy_document_button: OnceCell::new(),
            status_label: OnceCell::new(),
            close_finalizer_scheduled: Cell::new(false),
            workspace_controls: OnceCell::new(),
            next_sidebar_page: OnceCell::new(),
            autosave_timer: RefCell::new(None),
            autosave_generation: Cell::new(0),
        });
        // SAFETY: NSObject's initializer has no additional preconditions and all Rust ivars are initialized.
        unsafe { msg_send![super(controller), init] }
    }

    /// Transfers header controls into the unique controller owner before native actions become available.
    pub(super) fn install_controls(
        &self,
        exit_button: Retained<NSButton>,
        presentation_mode_button: Retained<NSPopUpButton>,
        table_button: Retained<NSButton>,
        copy_document_button: Retained<NSButton>,
        status_label: Retained<NSTextField>,
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
            self.ivars().table_button.set(table_button).is_ok(),
            "native Markdown table control must be installed exactly once"
        );
        assert!(
            self.ivars()
                .copy_document_button
                .set(copy_document_button)
                .is_ok(),
            "native Markdown copy control must be installed exactly once"
        );
        assert!(
            self.ivars().status_label.set(status_label).is_ok(),
            "native Markdown status control must be installed exactly once"
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
            status_label.setStringValue(&NSString::from_str(&bounded));
            status_label.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                0.96, 0.43, 0.43, 1.0,
            )));
        }
    }

    /// Acknowledges an explicit system pasteboard write without retaining a copied document.
    pub(super) fn present_copy_confirmation(&self) {
        if let Some(status_label) = self.ivars().status_label.get() {
            // Confirmation uses the existing foreground label and schedules no reset timer.
            status_label.setStringValue(ns_string!("已复制全文"));
            status_label.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                0.45, 0.72, 0.56, 1.0,
            )));
        }
    }

    /// Reflects actual cursor exhaustion without probing the filesystem from idle UI.
    pub(super) fn set_next_sidebar_page_available(&self, available: bool) {
        // The button is installed once and remains borrowed by its native target until close detaches it.
        if let Some(button) = self.ivars().next_sidebar_page.get() {
            // Only a live pager with remaining entries enables advancing this bounded metadata page.
            button.setEnabled(available);
        }
    }

    /// Enables explicit whole-document export only while a canonical document is open.
    pub(super) fn set_copy_document_available(&self, available: bool) {
        if let Some(copy_document_button) = self.ivars().copy_document_button.get() {
            // An empty/failed editor has no canonical source to export.
            copy_document_button.setEnabled(available);
        }
    }

    /// Enables native table insertion only while the current editor can accept source edits.
    pub(super) fn set_table_insertion_available(&self, available: bool) {
        if let Some(table_button) = self.ivars().table_button.get() {
            // Source insertion requires a mounted canonical document rather than an empty-state label.
            table_button.setEnabled(available);
        }
    }

    /// Clears transient command feedback after an operation without allocating a status history.
    pub(super) fn clear_status(&self) {
        if let Some(status_label) = self.ivars().status_label.get() {
            // Clear the existing label without creating a background status lifecycle.
            status_label.setStringValue(ns_string!(""));
        }
    }

    /// Ends all borrowed header-control target/action relations before releasing the native controller.
    pub(super) fn detach_controls(&self) {
        // Foreground close cancels persistence work before detaching any target or host lifecycle edge.
        self.cancel_autosave();
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
            }
        }
        if let Some(exit_button) = self.ivars().exit_button.get() {
            // SAFETY: the live main-thread control borrows this controller; clearing its target/action precedes owner release.
            unsafe {
                exit_button.setTarget(None);
                exit_button.setAction(None);
            }
        }
        if let Some(copy_document_button) = self.ivars().copy_document_button.get() {
            // SAFETY: the live main-thread copy control is detached before this target is released.
            unsafe {
                copy_document_button.setTarget(None);
                copy_document_button.setAction(None);
            }
        }
        if let Some(table_button) = self.ivars().table_button.get() {
            // SAFETY: the live main-thread table control is detached before this target is released.
            unsafe {
                table_button.setTarget(None);
                table_button.setAction(None);
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

    /// Synchronizes the native mode control with the canonical presentation setting.
    pub(super) fn select_presentation_mode(&self, mode: MarkdownPresentationMode) {
        if let Some(button) = self.ivars().presentation_mode_button.get() {
            // A vetoed/accepted presentation transaction always restores the actual canonical mode choice.
            button.selectItemAtIndex(mode.popup_index() as isize);
        }
    }
}

/// Queues a weak dirty-document observation after the enclosing native edit callback returns.
pub(super) fn queue_native_markdown_autosave_for_storage(storage: &PieceBackedMarkdownTextStorage) {
    let weak = MainThreadBound::new(Weak::from(storage), storage.mtm());
    enqueue_native_markdown_main_task(move || {
        let mtm = MainThreadMarker::new().expect("Markdown dirty notifications use the main queue");
        let Some(storage) = weak.get(mtm).load() else {
            // A closed native storage cannot schedule persistence for a successor document.
            return;
        };
        let dirty = storage
            .document()
            .is_some_and(|document| document.borrow().is_dirty());
        let controller = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            // Pointer identity rejects queued edits from an earlier note even under the same controller.
            active
                .as_ref()
                .filter(|surface| {
                    surface
                        .editor
                        .as_ref()
                        .is_some_and(|editor| std::ptr::eq(&*editor.model.text_storage, &*storage))
                })
                .map(|surface| surface.controller.clone())
        });
        if let Some(controller) = controller {
            // The exact current document, rather than an earlier retained shell, owns this deadline decision.
            if dirty {
                // Real canonical mutations reset one debounce; no timer exists for unchanged text.
                controller.schedule_autosave();
            } else {
                // Explicit save or undo-to-clean may have completed before this queued notification.
                controller.cancel_autosave();
            }
        }
    });
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
