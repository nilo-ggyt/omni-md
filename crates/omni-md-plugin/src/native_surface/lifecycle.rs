//! Owns foreground native slots, save/responder preflight and strict editor/window release handoffs.
//! Closing native owners, immediate canonical destruction and actual native-deallocation authority are distinct.

use super::*;

thread_local! {
    pub(super) static ACTIVE_NATIVE_MARKDOWN_SURFACE: RefCell<Option<NativeMarkdownWindowSurface>> = const { RefCell::new(None) };
    pub(super) static CLOSING_NATIVE_MARKDOWN_SURFACE: RefCell<Option<NativeMarkdownWindowSurface>> = const { RefCell::new(None) };
    pub(super) static FINALIZING_NATIVE_MARKDOWN_CLOSE: RefCell<Option<NativeMarkdownCloseCompletion>> = const { RefCell::new(None) };
    pub(super) static NATIVE_MARKDOWN_CLOSE_GENERATION: Cell<u64> = const { Cell::new(0) };
    pub(super) static VERIFYING_NATIVE_MARKDOWN_CLOSE: Cell<Option<(u64, NativeMarkdownCloseIntent)>> = const { Cell::new(None) };
    pub(super) static BORROWED_NATIVE_MARKDOWN_OWNER: Cell<Option<usize>> = const { Cell::new(None) };
    // At most one accepted note/file transition exists. A ready completion carries only weak identity
    // and scalar operation metadata while a reentrant callback temporarily observes its owner leased.
    static READY_NATIVE_MARKDOWN_CONTINUATION: RefCell<Option<Box<dyn FnOnce() + Send>>> = const { RefCell::new(None) };
}

/// Run one foreground transaction with the unique surface local, ending all TLS borrows before AppKit.
pub(super) fn with_active_native_markdown_surface<R>(
    operation: impl FnOnce(&mut NativeMarkdownWindowSurface) -> Result<R, String>,
) -> Result<R, String> {
    ACTIVE_NATIVE_MARKDOWN_SURFACE
        .with(|slot| {
            BORROWED_NATIVE_MARKDOWN_OWNER.with(|busy| {
                crate::foreground_owner::with_foreground_owner(
                    slot,
                    busy,
                    |surface| std::ptr::from_ref(&*surface.controller).addr(),
                    operation,
                    enqueue_restored_native_markdown_continuation,
                )
            })
        })
        .map_err(|error| match error {
            // Reentrant native work is rejected with no effects, and the local owner is restored by its lease.
            crate::foreground_owner::ForegroundOwnerError::Busy => {
                "Another Markdown native transaction is in progress".to_owned()
            }
            // A truly absent surface has no document or native ownership to lend.
            crate::foreground_owner::ForegroundOwnerError::Missing => {
                "Markdown is closed".to_owned()
            }
        })?
}
/// Preserve accepted handoff work until this exact synchronous foreground lease has been restored.
pub(super) fn defer_native_markdown_continuation_until_restored(
    work: impl FnOnce() + Send + 'static,
) {
    READY_NATIVE_MARKDOWN_CONTINUATION.with_borrow_mut(|pending| {
        // Note/file admission is mutually exclusive. A second accepted completion would violate
        // that invariant rather than authorize overwriting or dropping the first transition.
        assert!(
            pending.is_none(),
            "two accepted Markdown continuations overlapped"
        );
        *pending = Some(Box::new(work));
    });
}

/// Resume from the real owner-restoration event, without polling native deallocation or retrying work.
fn enqueue_restored_native_markdown_continuation() {
    let ready = READY_NATIVE_MARKDOWN_CONTINUATION.with_borrow_mut(Option::take);
    if let Some(ready) = ready {
        // The lease restored its owner/gate and ended the slot borrow before this asynchronous queue call.
        enqueue_native_markdown_main_task(ready);
    }
}

/// Weak-only evidence forbids successor acquisition while the old canonical/native graph survives.
pub(super) struct NativeMarkdownEditorReleaseBarrier {
    cohort: NativeMarkdownDeallocationCohort,
    document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    document_released_synchronously: bool,
    text_storage: Weak<PieceBackedMarkdownTextStorage>,
    scroll_view: Weak<NSScrollView>,
    text_view: Weak<PieceBackedMarkdownTextView>,
    gutter: Weak<NativeMarkdownGutter>,
    content: Weak<objc2_app_kit::NSTextContentStorage>,
    manager: Weak<NSTextLayoutManager>,
    container: Weak<NSTextContainer>,
    cell: Option<Weak<NSTextField>>,
}

impl NativeMarkdownEditorReleaseBarrier {
    /// Queue one strict handoff only after this editor's complete tracked graph actually deallocates.
    pub(super) fn when_released(&self, work: impl FnOnce() + 'static) {
        self.cohort.when_released(work);
    }

    /// Join surviving roles to a window-close cohort that may interrupt an earlier note/file handoff.
    pub(super) fn track_surviving_nodes(
        &self,
        cohort: &mut NativeMarkdownDeallocationCohort,
        mtm: MainThreadMarker,
    ) {
        cohort.track_weak(&self.text_storage, mtm);
        cohort.track_weak(&self.scroll_view, mtm);
        cohort.track_weak(&self.text_view, mtm);
        cohort.track_weak(&self.gutter, mtm);
        cohort.track_weak(&self.content, mtm);
        cohort.track_weak(&self.manager, mtm);
        cohort.track_weak(&self.container, mtm);
        if let Some(cell) = &self.cell {
            // The lazy cell role exists only after explicit table editing and must join final close when present.
            cohort.track_weak(cell, mtm);
        }
    }

    /// Report the teardown-stack observation independently of later native marker timing.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn canonical_released_synchronously(&self) -> bool {
        self.document_released_synchronously
    }

    /// Expose actual native completion latency separately from immediate canonical destruction.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn native_release_elapsed(&self) -> std::time::Duration {
        self.cohort.elapsed()
    }

    /// Require immediate canonical teardown, actual native completion and the complete strict nil graph.
    pub(super) fn verify_released(&self) -> Result<(), String> {
        if !self.document_released_synchronously {
            // Rust document/history/mapping ownership must end inside teardown, regardless of later AppKit work.
            return Err(
                "The canonical Markdown document survived synchronous surface teardown".into(),
            );
        }
        if !self.cohort.has_completed() {
            // A queue turn alone cannot substitute for the platform's actual release of tracked native owners.
            return Err("The native Markdown deallocation cohort has not completed".into());
        }
        // Convert weak upgrades to booleans immediately; the observation itself retains no native graph.
        let document = self.document.upgrade().is_some();
        let storage = self.text_storage.load().is_some();
        let scroll = self.scroll_view.load().is_some();
        let view = self.text_view.load().is_some();
        let gutter = self.gutter.load().is_some();
        let content = self.content.load().is_some();
        let manager = self.manager.load().is_some();
        let container = self.container.load().is_some();
        let cell = self.cell.as_ref().is_some_and(|cell| cell.load().is_some());
        if document
            || storage
            || scroll
            || view
            || gutter
            || content
            || manager
            || container
            || cell
        {
            // No surviving native shell, field, ruler, layout graph or canonical resource may overlap a successor.
            return Err(format!(
                "The previous Markdown graph survived its release transaction: document={document} storage={storage} scroll={scroll} view={view} gutter={gutter} content={content} manager={manager} container={container} cell={cell}"
            ));
        }
        Ok(())
    }
}

/// Detaches and drops the sole editor owner, returning only weak release observations.
pub(super) fn destroy_native_markdown_editor(
    editor: NativeMarkdownEditorSurface,
) -> NativeMarkdownEditorReleaseBarrier {
    let mtm = MainThreadMarker::new().expect("native Markdown destruction uses the main thread");
    // Seal input before hierarchy removal itself can trigger native responder/container callbacks.
    editor.stop_accepting_input();
    // Finish hierarchy removal while the editor still owns TextKit; local temporary native retains drain first.
    // SAFETY: the editor owns this main-thread scroll view and no borrowed parent is retained by the query.
    if unsafe { editor.scroll_view().superview() }.is_some() {
        // Only a mounted hierarchy needs symmetric removal; this call may finish native view callbacks.
        autoreleasepool(|_| editor.scroll_view().removeFromSuperview());
    }
    // Every observation is weak: the barrier cannot become an extra document/native owner itself.
    let document = Rc::downgrade(editor.document());
    let text_storage = Weak::from_retained(&editor.model.text_storage);
    let scroll_view = Weak::from_retained(&editor.scroll_view);
    let text_view = Weak::from_retained(&editor.text_view);
    let gutter = Weak::from_retained(&editor.gutter);
    let content = Weak::from_retained(&editor.text_content_storage);
    let manager = Weak::from_retained(&editor.text_layout_manager);
    let container = Weak::from_retained(&editor.text_container);
    let cell = editor
        .text_view
        .ivars()
        .cell_editor
        .get()
        .map(Weak::from_retained);
    // Attach only weak/counter observations before releasing any native ownership. Duplicate roles
    // are deduplicated by exact object identity inside the operation's unique association cohort.
    let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
    cohort.track(&*editor.model.text_storage, mtm);
    cohort.track(&*editor.scroll_view, mtm);
    cohort.track(&*editor.text_view, mtm);
    cohort.track(&*editor.gutter, mtm);
    cohort.track(&*editor.text_content_storage, mtm);
    cohort.track(&*editor.text_layout_manager, mtm);
    cohort.track(&*editor.text_container, mtm);
    if let Some(cell) = editor.text_view.ivars().cell_editor.get() {
        // An uncreated reusable field contributes no phantom object or unreleasable counter claim.
        cohort.track(&**cell, mtm);
    }
    // Drop the unique owner now. Rust resources end synchronously; AppKit's temporary event ownership
    // completes only through actual deallocation signals, followed by one strict check without polling/retries.
    autoreleasepool(|_| drop(editor));
    let document_released_synchronously = document.upgrade().is_none();
    cohort.seal();
    NativeMarkdownEditorReleaseBarrier {
        cohort,
        document,
        document_released_synchronously,
        text_storage,
        scroll_view,
        text_view,
        gutter,
        content,
        manager,
        container,
        cell,
    }
}

/// The plugin preserves the SDK close-intent identity throughout its native lifecycle.
pub(super) type NativeMarkdownCloseIntent = omni_plugin_sdk::PluginCloseIntent;

/// Construct one foreground owner synchronously, then publish its mounted view and input visibility.
pub(crate) fn show_native_markdown_window(
    session: MarkdownSessionAccess,
    lifecycle: Rc<dyn PluginHostLifecycle>,
    host: &mut dyn OmniBarHostContext,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "原生 Markdown 窗口必须在 AppKit 主线程创建".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some())
        || FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow(|completion| completion.is_some())
        || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
        || BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some()
    {
        // A successor cannot allocate while either native close or strict weak release is unfinished.
        return Err("原生 Markdown 窗口仍在关闭".to_owned());
    }

    if ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // Static activation never overlays a second document/window graph on an existing foreground owner.
        return Err("原生 Markdown 窗口已经存在".to_owned());
    }

    let surface = NativeMarkdownWindowSurface::new(session, lifecycle, mtm)?;
    let native_view = surface
        .native_view()
        .ok_or_else(|| "原生 Markdown 窗口没有可挂载的内容视图".to_owned())?;
    host.mount_native_view(native_view)
        .map_err(|error| error.to_string())?;
    if let Err(error) = surface.lifecycle.commit_creation() {
        // Failed host publication drops this still-local surface rather than exposing an orphan active slot.
        return Err(error.to_string());
    }
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow_mut(|active_surface| {
        *active_surface = Some(surface);
    });
    // Native visibility/focus may synchronously call delegates; lend the owner without a state borrow.
    with_active_native_markdown_surface(|surface| {
        surface.show();
        Ok(())
    })
}

/// Return a copied borrowed-view descriptor; the active surface remains its only native ownership root.
pub(crate) fn active_native_markdown_view() -> Option<PluginNativeView> {
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| {
        surface
            .as_ref()
            .and_then(NativeMarkdownWindowSurface::native_view)
    })
}

/// Updates the current native editor mode while preserving source bytes and session ownership.
pub(super) fn set_active_native_markdown_presentation_mode(
    mode: MarkdownPresentationMode,
) -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "Markdown 展示模式必须在 AppKit 主线程切换".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // Display changes cannot reopen callbacks after close has sealed the current editor's input.
        return Err("原生 Markdown 窗口正在关闭".to_owned());
    }
    with_active_native_markdown_surface(|surface| surface.set_presentation_mode(mode))
}

/// Saves and releases the old note before scheduling installation of the requested successor.
pub(super) fn switch_native_markdown_note(relative_path: &str) -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "原生 Markdown 笔记必须在 AppKit 主线程切换".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // A close owns the last save/release transition and cannot be replaced by a sidebar destination.
        return Err("原生 Markdown 窗口正在关闭".to_owned());
    }

    let (switch_started, controller) = autoreleasepool(|_| {
        with_active_native_markdown_surface(|surface| {
            let switch_result = surface.begin_note_switch(relative_path);
            if let Err(error) = &switch_result {
                // Rejected switch/open recovery is visible while the retained original draft stays authoritative.
                surface.controller.present_error(error);
            }
            switch_result.map(|started| (started, surface.controller.clone()))
        })
    })?;
    if switch_started {
        // Register only after the pending transition owns its weak evidence and the local editor owner has dropped.
        schedule_native_markdown_note_switch_completion(&controller)?;
    }
    Ok(())
}

/// Arm one origin-scoped continuation against the pending editor's actual release cohort.
pub(super) fn schedule_native_markdown_note_switch_completion(
    controller: &NativeMarkdownController,
) -> Result<(), String> {
    with_active_native_markdown_surface(|surface| {
        let pending = surface
            .pending_note_switch
            .as_ref()
            .ok_or("No Markdown note switch is pending")?;
        controller.enqueue_owned_after_release(pending.release_barrier.as_ref(), || {
            if let Err(error) = finish_native_markdown_note_switch() {
                // Preserve the failed graph/open contract and its visible status, without another automatic attempt.
                eprintln!("Markdown note switch failed: {error}");
            }
        });
        Ok(())
    })
}

/// Installs a pending successor only after strict old-document release verification.
pub(super) fn finish_native_markdown_note_switch() -> Result<(), String> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "原生 Markdown 笔记必须在 AppKit 主线程完成切换".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // The close path has consumed foreground authority, so this old queued switch does nothing.
        return Ok(());
    }

    autoreleasepool(|_| {
        with_active_native_markdown_surface(|surface| {
            let switch_result = surface.finish_note_switch(mtm);
            if let Err(error) = &switch_result {
                // Rejected switch/open recovery is visible while the retained original draft stays authoritative.
                surface.controller.present_error(error);
            }
            switch_result
        })
    })
}

/// Preserve Host query/resume semantics for a physical Escape dismissal.
pub(crate) fn request_native_markdown_window_dismiss() -> Result<(), String> {
    request_native_markdown_close(NativeMarkdownCloseIntent::DismissAndResume)
}

/// Return to the launcher only through the same save and strict release protocol as dismissal.
pub(crate) fn request_native_markdown_window_exit() -> Result<(), String> {
    request_native_markdown_close(NativeMarkdownCloseIntent::ExitToLauncher)
}

/// Preflight visible input and persistence before accepting one immutable close intent.
pub(crate) fn request_native_markdown_close(
    intent: NativeMarkdownCloseIntent,
) -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "原生 Markdown 窗口必须在 AppKit 主线程关闭".to_owned())?;
    let accepted = FINALIZING_NATIVE_MARKDOWN_CLOSE
        .with_borrow(|completion| {
            // A weak-only completion retains the same accepted intent after actual surface ownership ends.
            completion.as_ref().map(|completion| completion.intent)
        })
        .or_else(|| {
            VERIFYING_NATIVE_MARKDOWN_CLOSE
                .with(Cell::get)
                .map(|(_, intent)| intent)
        })
        .or_else(|| {
            CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| {
                // Before native owner teardown, the closing surface is the accepted intent's unique authority.
                surface.as_ref().and_then(|surface| surface.closing_intent)
            })
        });
    if crate::close_policy::coalesce_accepted_close(accepted, intent).map_err(str::to_owned)? {
        // Repeated requests share one native close and one host completion callback.
        return Ok(());
    }
    if BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some() {
        // Reentry during an unaccepted save/layout/row transaction cannot be mistaken for an accepted close.
        return Err("Another Markdown native transaction is in progress".into());
    }
    if ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_none()) {
        // No accepted in-flight intent and no foreground owner means there is no session left to close.
        return Err("原生 Markdown 窗口已经关闭".to_owned());
    }

    let save_result = with_active_native_markdown_surface(|surface| {
        surface.commit_marked_text()?;
        surface.save_current_document()?;
        // A real AppKit resignation refusal vetoes close before the host lifecycle changes state.
        surface.resign_editor_responder()
    });
    if let Err(error) = save_result {
        // Persistence or visible-draft rejection vetoes closing before any native owner is removed.
        present_native_markdown_error(&error);
        return Err(error);
    }

    let lifecycle = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| {
        surface
            .as_ref()
            .expect("native Markdown existence was checked immediately above")
            .lifecycle
            .clone()
    });
    if let Err(error) = lifecycle.begin_close(intent) {
        // Host lifecycle rejection preserves the saved foreground surface and its responder graph.
        let message = error.to_string();
        present_native_markdown_error(&message);
        let _ = with_active_native_markdown_surface(|surface| {
            if let Some(editor) = &surface.editor {
                // The host vetoed closing; restore normal input focus to the still-owned saved document.
                surface.panel.makeFirstResponder(Some(editor.text_view()));
            }
            Ok(())
        });
        return Err(message);
    }

    let Some(mut surface) = ACTIVE_NATIVE_MARKDOWN_SURFACE.take() else {
        // Reentrant completion already consumed the unique owner; no second native close is needed.
        return Ok(());
    };
    surface.closing_intent = Some(intent);
    surface.prepare_to_close();
    let panel = surface.panel.clone();
    CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow_mut(|closing_surface| {
        *closing_surface = Some(surface);
    });
    panel.close();
    Ok(())
}

/// Holds only weak graph observations and original host lifecycle identity after native owner teardown.
struct NativeMarkdownCloseCompletion {
    generation: u64,
    intent: NativeMarkdownCloseIntent,
    lifecycle: Rc<dyn PluginHostLifecycle>,
    editors: Vec<NativeMarkdownEditorReleaseBarrier>,
    cohort: NativeMarkdownDeallocationCohort,
    panel: Weak<NativeMarkdownPanel>,
    controller: Weak<NativeMarkdownController>,
    content: Option<Weak<NSView>>,
}

impl NativeMarkdownCloseCompletion {
    /// Verify actual deallocation once, without allowing a stale weak-only transaction to notify the host.
    fn verify_released(&self) -> Result<(), String> {
        if !self.cohort.has_completed() {
            // Window completion waits for all interrupted/editor/window roles, not only the current native callback.
            return Err(
                "The complete Markdown window deallocation cohort has not completed".into(),
            );
        }
        for editor in &self.editors {
            // Include a predecessor released by an interrupted note/file transition as well as the current editor.
            editor.verify_released()?;
        }
        let panel = self.panel.load().is_some();
        let controller = self.controller.load().is_some();
        let content = self
            .content
            .as_ref()
            .is_some_and(|content| content.load().is_some());
        if panel || controller || content {
            // A closed-but-retained window/controller/content owner cannot be treated as completed teardown.
            return Err(format!(
                "The Markdown window graph survived close: panel={panel} controller={controller} content={content}"
            ));
        }
        Ok(())
    }
}

/// Drop foreground owners after windowWillClose, then observe actual complete native deallocation.
pub(super) fn finalize_native_markdown_close() {
    let completion = autoreleasepool(|_| {
        let mut surface = CLOSING_NATIVE_MARKDOWN_SURFACE.take()?;
        let intent = surface
            .closing_intent
            .expect("closing Markdown preserves its original intent");
        let generation = NATIVE_MARKDOWN_CLOSE_GENERATION.with(|generation| {
            // Every ownership teardown has a distinct scalar identity for its one queued weak observation.
            let next = generation.get().wrapping_add(1);
            generation.set(next);
            next
        });
        let mtm = MainThreadMarker::new().expect("native close finalization uses the main thread");
        let mut editors = Vec::new();
        if let Some(pending) = surface.pending_note_switch.take()
            && let Some(barrier) = pending.release_barrier
        {
            // Closing may interrupt a still-releasing predecessor even though the current editor slot is empty.
            editors.push(barrier);
        }
        if let Some(pending) = surface.pending_file_mutation.take()
            && let Some(barrier) = pending.release_barrier
        {
            // A pending filesystem handoff cannot let its earlier native document escape the final close gate.
            editors.push(barrier);
        }
        if let Some(editor) = surface.editor.take() {
            // Preflight already saved and resigned input; destruction accepts no later source mutation.
            editors.push(destroy_native_markdown_editor(editor));
        }
        let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
        for editor in &editors {
            // Already deallocated roles need no marker; each surviving identity joins one final operation cohort.
            editor.track_surviving_nodes(&mut cohort, mtm);
        }
        cohort.track(&*surface.panel, mtm);
        cohort.track(&*surface.controller, mtm);
        let content = surface.panel.contentView().map(|content| {
            // End this temporary content retain before final native ownership is removed below.
            cohort.track(&*content, mtm);
            Weak::from_retained(&content)
        });
        let mut completion = NativeMarkdownCloseCompletion {
            generation,
            intent,
            lifecycle: surface.lifecycle.clone(),
            editors,
            cohort,
            panel: Weak::from_retained(&surface.panel),
            controller: Weak::from_retained(&surface.controller),
            content,
        };
        surface.release_native_ownership();
        // End canonical/editor/window/session ownership here; local AppKit autoreleases drain before enqueue.
        drop(surface);
        completion.cohort.seal();
        Some(completion)
    });
    let Some(completion) = completion else {
        // Duplicate native close notifications have no original foreground owner to consume.
        return;
    };
    let generation = completion.generation;
    // The completion owns weak-only evidence; its callback captures only the original scalar generation.
    completion
        .cohort
        .when_released(move || finish_native_markdown_close_release(generation));
    FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
}

/// Consume the actual-deallocation signal once; a failed complete graph check retains honest Closing state.
pub(super) fn finish_native_markdown_close_release(generation: u64) {
    let Some(completion) = FINALIZING_NATIVE_MARKDOWN_CLOSE.take() else {
        // Duplicate queued events have no remaining close authority or strong native ownership.
        return;
    };
    if completion.generation != generation {
        // Preserve a later accepted token when an earlier queued callback arrives out of order.
        FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
        return;
    }
    // Keep reentrant activation/close policy gated using only Copy scalars while all native weak loads
    // run outside RefCell/TLS state borrows. No controller/view/delegate is captured across verification.
    VERIFYING_NATIVE_MARKDOWN_CLOSE
        .with(|verifying| verifying.set(Some((generation, completion.intent))));
    match completion.verify_released() {
        // Complete deallocation authorizes exactly one original host lifecycle callback.
        Ok(()) => {
            native_markdown_trace! {
                eprintln!("[probe] Markdown native close completed after_us={}", completion.cohort.elapsed().as_micros());
            }
            VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|verifying| verifying.set(None));
            completion.lifecycle.finish_close(completion.intent);
        }
        // Reinsert weak-only evidence before releasing the scalar gate; failures preserve Host Closing.
        Err(error) => {
            FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
            VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|verifying| verifying.set(None));
            eprintln!("Markdown close release failed: {error}");
        }
    }
}
