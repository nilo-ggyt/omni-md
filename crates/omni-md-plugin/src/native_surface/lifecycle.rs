//! Owns foreground native slots, save/responder preflight and strict editor/window release handoffs.
//! Closing native owners, immediate canonical destruction and actual native-deallocation authority are distinct.

use super::*;

thread_local! {
    pub(super) static ACTIVE_NATIVE_MARKDOWN_SURFACE: RefCell<Option<NativeMarkdownWindowSurface>> = const { RefCell::new(None) };
    pub(super) static CLOSING_NATIVE_MARKDOWN_SURFACE: RefCell<Option<NativeMarkdownWindowSurface>> = const { RefCell::new(None) };
    pub(super) static FINALIZING_NATIVE_MARKDOWN_CLOSE: RefCell<Option<NativeMarkdownCloseCompletion>> = const { RefCell::new(None) };
    pub(super) static NATIVE_MARKDOWN_CLOSE_GENERATION: Cell<u64> = const { Cell::new(0) };
    pub(super) static VERIFYING_NATIVE_MARKDOWN_CLOSE: Cell<Option<(u64, PluginCloseRequest)>> = const { Cell::new(None) };
    static NATIVE_MARKDOWN_CLOSE_CALL_DEPTH: Cell<usize> = const { Cell::new(0) };
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

/// Preserve Host query/resume semantics through an explicit ordinary foreground request.
pub(crate) fn request_native_markdown_window_dismiss() -> Result<(), String> {
    request_native_markdown_close(PluginCloseRequest {
        intent: NativeMarkdownCloseIntent::DismissAndResume,
        scope: PluginCloseScope::ForegroundTransition,
    })
}

/// Return to the launcher only after ordinary complete-graph release.
pub(crate) fn request_native_markdown_window_exit() -> Result<(), String> {
    request_native_markdown_close(PluginCloseRequest {
        intent: NativeMarkdownCloseIntent::ExitToLauncher,
        scope: PluginCloseScope::ForegroundTransition,
    })
}

/// Restore scalar call-stack evidence on both ordinary return and Rust unwinding.
struct NativeMarkdownCloseCallGuard<'a>(&'a Cell<usize>);

impl Drop for NativeMarkdownCloseCallGuard<'_> {
    fn drop(&mut self) {
        // Every successful entry owns one claim; no native value or callback is dropped under this scalar update.
        self.0.set(self.0.get() - 1);
    }
}

/// Reject a framework-only shell claim while any plugin close request may still own a local panel clone.
pub(crate) fn request_native_markdown_close(request: PluginCloseRequest) -> Result<(), String> {
    NATIVE_MARKDOWN_CLOSE_CALL_DEPTH.with(|depth| {
        let next = depth
            .get()
            .checked_add(1)
            .ok_or("Markdown close reentry depth exhausted")?;
        depth.set(next);
        let _guard = NativeMarkdownCloseCallGuard(depth);
        // Cell access holds no state borrow across native callbacks; nested calls own independent balanced claims.
        request_native_markdown_close_inner(request)
    })
}

/// Preflight visible input and persistence, or promote only an already accepted scope.
fn request_native_markdown_close_inner(request: PluginCloseRequest) -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "原生 Markdown 窗口必须在 AppKit 主线程关闭".to_owned())?;
    let accepted = FINALIZING_NATIVE_MARKDOWN_CLOSE
        .with_borrow(|completion| completion.as_ref().map(|completion| completion.request))
        .or_else(|| {
            VERIFYING_NATIVE_MARKDOWN_CLOSE
                .with(Cell::get)
                .map(|(_, request)| request)
        })
        .or_else(|| {
            CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| {
                // Copy identity before effects; no native owner is lent by close admission.
                surface.as_ref().and_then(|surface| surface.closing_request)
            })
        });
    match crate::close_policy::admit_close(accepted, request).map_err(str::to_owned)? {
        crate::close_policy::CloseAdmission::Coalesced => {
            // An already committed identical request shares its one completion, even during verification.
            return Ok(());
        }
        crate::close_policy::CloseAdmission::Promote => {
            // Promotion never repeats composition, persistence, resignation or the native close selector.
            return promote_native_markdown_close(request);
        }
        crate::close_policy::CloseAdmission::First => {
            // First acceptance still requires every native/data preflight and Host authorization below.
        }
    }
    if BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some() {
        // Unaccepted reentry cannot acquire the temporarily local owner or pretend another close succeeded.
        return Err("Another Markdown native transaction is in progress".into());
    }
    if ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_none()) {
        // No accepted transaction or live owner exists to supply save/release authority.
        return Err("原生 Markdown 窗口已经关闭".to_owned());
    }
    let save_result = with_active_native_markdown_surface(|surface| {
        surface.commit_marked_text()?;
        surface.save_current_document()?;
        // Actual responder refusal vetoes the close before the Host accepts its scope or destination.
        surface.resign_editor_responder()
    });
    if let Err(error) = save_result {
        // A failed save/input/resignation preserves the original usable draft and its ordinary owner.
        present_native_markdown_error(&error);
        return Err(error);
    }
    let lifecycle = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| {
        surface
            .as_ref()
            .expect("the preflight restored its exact owner")
            .lifecycle
            .clone()
    });
    if let Err(error) = lifecycle.begin_close(request) {
        // Host rejects terminal scope without pending Quit; restore input to the saved foreground owner.
        let message = error.to_string();
        present_native_markdown_error(&message);
        let _ = with_active_native_markdown_surface(|surface| {
            if let Some(editor) = &surface.editor {
                // No close was accepted, so restoring focus does not revive a discarded document.
                surface.panel.makeFirstResponder(Some(editor.text_view()));
            }
            Ok(())
        });
        return Err(message);
    }
    let Some(mut surface) = ACTIVE_NATIVE_MARKDOWN_SURFACE.take() else {
        // A reentrant completion already consumed ownership; never create another native close.
        return Ok(());
    };
    surface.closing_request = Some(request);
    surface.prepare_to_close();
    let panel = surface.panel.clone();
    CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow_mut(|closing| *closing = Some(surface));
    panel.close();
    Ok(())
}

/// Authorize one monotonic scope change while preserving original intent, generation and native effects.
fn promote_native_markdown_close(request: PluginCloseRequest) -> Result<(), String> {
    if VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some() {
        // The completion is local during native verification. Uncommitted reentry rejects Busy with no
        // effects; Host may cancel Quit while the original ordinary transaction continues unchanged.
        return Err("Markdown close verification is in progress".into());
    }
    if let Some(mut completion) = FINALIZING_NATIVE_MARKDOWN_CLOSE.take() {
        let generation = completion.generation;
        VERIFYING_NATIVE_MARKDOWN_CLOSE
            .with(|gate| gate.set(Some((generation, completion.request))));
        // The Host validates its actual pending Quit. No TLS borrow or native graph owner crosses this call.
        let result = completion
            .lifecycle
            .begin_close(request)
            .map_err(|error| error.to_string());
        if result.is_ok() {
            // Only successful Host authorization commits terminal scope; failure preserves the old receipt identity.
            completion.request = request;
        }
        let schedule = result.is_ok() && completion.reserve_terminal_check();
        FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
        VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(None));
        if schedule {
            // A recorded actual resource-release event already occurred. This queues its single
            // terminal verification, not a timer, retry or guessed deallocation boundary.
            enqueue_native_markdown_main_task(move || {
                finish_native_markdown_close_release(generation, CloseBoundary::Resources)
            });
        }
        return result;
    }
    if let Some(mut surface) = CLOSING_NATIVE_MARKDOWN_SURFACE.take() {
        let accepted = surface
            .closing_request
            .expect("a closing surface has an accepted request");
        VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(Some((0, accepted))));
        let result = surface
            .lifecycle
            .begin_close(request)
            .map_err(|error| error.to_string());
        if result.is_ok() {
            // Native windowWillClose/finalization remains the original operation; only its authorized scope changes.
            surface.closing_request = Some(request);
        }
        CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow_mut(|closing| *closing = Some(surface));
        VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(None));
        return result;
    }
    // Admission and effects are synchronous on the main thread; disappearance cannot authorize a guessed promotion.
    Err("The accepted Markdown close is no longer available for promotion".into())
}

/// Actual-release authority remains distinct for ordinary graph completion and terminal payload completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseBoundary {
    FullGraph,
    Resources,
}

/// A successful proof reports its real boundary, never relabelling an OS-held panel as deallocated.
enum NativeMarkdownCloseReceipt {
    ForegroundGraphReleased,
    ApplicationResourcesReleased(crate::terminal_close::FrameworkWindowLeaseReceipt),
}

/// Holds weak observations and copied scope identity after every project/native owning root was dropped.
struct NativeMarkdownCloseCompletion {
    generation: u64,
    request: PluginCloseRequest,
    lifecycle: Rc<dyn PluginHostLifecycle>,
    editors: Vec<NativeMarkdownEditorReleaseBarrier>,
    cohort: NativeMarkdownDeallocationCohort,
    resources: NativeMarkdownDeallocationCohort,
    terminal_authority: crate::close_policy::TerminalReleaseAuthority,
    close_notified: bool,
    project_owner_released: bool,
    panel: Weak<NativeMarkdownPanel>,
    controller: Weak<NativeMarkdownController>,
    content: Option<Weak<NSView>>,
}

impl NativeMarkdownCloseCompletion {
    /// Coalesce an already recorded actual event into exactly one terminal verification authority.
    fn reserve_terminal_check(&mut self) -> bool {
        // A completed counter is set only by final marker destruction; it also covers promotion
        // before the resource callback itself runs. No elapsed-time or queue-boundary inference exists.
        self.terminal_authority
            .reserve(self.request.scope, self.resources.has_completed())
    }

    /// Require every predecessor/editor and window payload identity to be actually nil.
    fn verify_resources_released(&self) -> Result<(), String> {
        if !self.resources.has_completed() || !self.project_owner_released {
            // A resource signal and synchronous unique-owner destruction are independent requirements.
            return Err("The Markdown project-resource release is incomplete".into());
        }
        for editor in &self.editors {
            // Interrupted handoffs retain their original strict canonical/TextKit barrier without exceptions.
            editor.verify_released()?;
        }
        if self.controller.load().is_some()
            || self
                .content
                .as_ref()
                .is_some_and(|content| content.load().is_some())
        {
            // Terminal scope permits only the panel shell; controller/content survival is still a strict failure.
            return Err(
                "The Markdown controller or content survived project-resource release".into(),
            );
        }
        Ok(())
    }

    /// Produce one scope-specific typed receipt after actual release, with no retained window escaping its pool.
    fn verify_released(&mut self) -> Result<NativeMarkdownCloseReceipt, String> {
        self.verify_resources_released()?;
        match self.request.scope {
            PluginCloseScope::ForegroundTransition => {
                // Ordinary dismissal/back/resume still require the unchanged all-node cohort and exact nil panel.
                if !self.cohort.has_completed() || self.panel.load().is_some() {
                    return Err(
                        "The complete Markdown foreground window graph has not deallocated".into(),
                    );
                }
                Ok(NativeMarkdownCloseReceipt::ForegroundGraphReleased)
            }
            PluginCloseScope::ApplicationTermination => {
                // All state getters below copy only ownership-presence flags; native queries occur after their borrows end.
                let project_owner_present = ACTIVE_NATIVE_MARKDOWN_SURFACE
                    .with_borrow(Option::is_some)
                    || CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
                    || BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some()
                    || NATIVE_MARKDOWN_CLOSE_CALL_DEPTH.with(Cell::get) != 0;
                let receipt = autoreleasepool(|_| {
                    let panel = self.panel.load();
                    let mut facts = crate::terminal_close::TerminalWindowFacts {
                        close_notified: self.close_notified,
                        project_owner_present,
                        panel_present: panel.is_some(),
                        visible: false,
                        key: false,
                        content_attached: false,
                        delegate_attached: false,
                        initial_responder_attached: false,
                        payload_responder_attached: false,
                        sheet_attached: false,
                    };
                    if let Some(panel) = &panel {
                        // This is the sole transient panel upgrade. Public getters prove a closed/detached
                        // shell; no retainCount, private ownership mutation or caller-pool operation is used.
                        facts.visible = panel.isVisible();
                        facts.key = panel.isKeyWindow();
                        facts.content_attached = panel.contentView().is_some();
                        facts.delegate_attached = panel.delegate().is_some();
                        facts.initial_responder_attached = panel.initialFirstResponder().is_some();
                        facts.payload_responder_attached =
                            panel.firstResponder().is_some_and(|responder| {
                                // NSWindow may itself be its inert responder; any different responder remains a forbidden payload edge.
                                !std::ptr::eq(
                                    std::ptr::from_ref(&*responder).cast::<()>(),
                                    std::ptr::from_ref(&**panel).cast::<()>(),
                                )
                            });
                        facts.sheet_attached = panel.attachedSheet().is_some();
                    }
                    let receipt = facts.verify().map_err(str::to_owned)?;
                    // Revoke full-graph callback authority before removing only its own association.
                    // Marker removal is cancelled observation, not deallocation; no project callback
                    // or marker metadata remains attached to the AppKit-owned terminal panel shell.
                    self.cohort.cancel_observation(panel.as_deref());
                    Ok::<_, String>(receipt)
                })?;
                // The weak upgrade, all getter temporaries and their local pool ended before receipt use.
                Ok(NativeMarkdownCloseReceipt::ApplicationResourcesReleased(
                    receipt,
                ))
            }
        }
    }
}

/// Drop foreground owners after the real windowWillClose and install independent actual-release authorities.
pub(super) fn finalize_native_markdown_close() {
    let completion = autoreleasepool(|_| {
        let mut surface = CLOSING_NATIVE_MARKDOWN_SURFACE.take()?;
        let request = surface
            .closing_request
            .expect("closing Markdown preserves its accepted request");
        let close_notified = surface.controller.ivars().close_finalizer_scheduled.get();
        let generation = NATIVE_MARKDOWN_CLOSE_GENERATION.with(|generation| {
            // An operation generation prevents stale callbacks from observing or completing a successor.
            let next = generation.get().wrapping_add(1);
            generation.set(next);
            next
        });
        let mtm = MainThreadMarker::new().expect("native close finalization uses the main thread");
        let mut editors = Vec::new();
        if let Some(pending) = surface.pending_note_switch.take()
            && let Some(barrier) = pending.release_barrier
        {
            // Closing includes an earlier graph whose handoff has not yet finished.
            editors.push(barrier);
        }
        if let Some(pending) = surface.pending_file_mutation.take()
            && let Some(barrier) = pending.release_barrier
        {
            // Filesystem handoff cannot exempt a preceding canonical/native editor from terminal or ordinary release.
            editors.push(barrier);
        }
        if let Some(editor) = surface.editor.take() {
            // Preflight already saved and resigned; teardown synchronously drops canonical/history/mapping ownership.
            editors.push(destroy_native_markdown_editor(editor));
        }
        let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
        let mut resources = NativeMarkdownDeallocationCohort::new(mtm);
        for editor in &editors {
            // Independent keys preserve the ordinary full graph while allowing terminal resource release to complete.
            editor.track_surviving_nodes(&mut cohort, mtm);
            editor.track_surviving_nodes(&mut resources, mtm);
        }
        cohort.track(&*surface.panel, mtm);
        cohort.track(&*surface.controller, mtm);
        resources.track(&*surface.controller, mtm);
        let content = surface.panel.contentView().map(|content| {
            // Root content is a payload identity under both scopes, never part of the permitted framework shell.
            cohort.track(&*content, mtm);
            resources.track(&*content, mtm);
            Weak::from_retained(&content)
        });
        let mut completion = NativeMarkdownCloseCompletion {
            generation,
            request,
            lifecycle: surface.lifecycle.clone(),
            editors,
            cohort,
            resources,
            terminal_authority: crate::close_policy::TerminalReleaseAuthority::default(),
            close_notified,
            project_owner_released: false,
            panel: Weak::from_retained(&surface.panel),
            controller: Weak::from_retained(&surface.controller),
            content,
        };
        surface.release_native_ownership();
        drop(surface);
        // The unique surface/model/search owner has ended synchronously; callbacks retain only scalar/weak evidence.
        completion.project_owner_released = true;
        completion.cohort.seal();
        completion.resources.seal();
        Some(completion)
    });
    let Some(completion) = completion else {
        // Duplicate notifications cannot consume another native owner or register another close authority.
        return;
    };
    let generation = completion.generation;
    completion.cohort.when_released(move || {
        finish_native_markdown_close_release(generation, CloseBoundary::FullGraph)
    });
    completion
        .resources
        .when_released(move || observe_native_markdown_resource_release(generation));
    FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
}

/// Preserve the real payload-deallocation event while an ordinary close may still await its panel.
fn observe_native_markdown_resource_release(generation: u64) {
    let Some(mut completion) = FINALIZING_NATIVE_MARKDOWN_CLOSE.take() else {
        // A later completed transaction has no resource event left to consume.
        return;
    };
    if completion.generation != generation {
        // An earlier event cannot annotate a later operation's readiness.
        FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
        return;
    }
    let finish = completion.reserve_terminal_check();
    FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
    if finish {
        // This is already the actual-release relay after the final native destruction stack, not an added wait phase.
        finish_native_markdown_close_release(generation, CloseBoundary::Resources);
    }
}

/// Consume exactly one matching actual boundary; a failed proof retains honest Closing state.
fn finish_native_markdown_close_release(generation: u64, boundary: CloseBoundary) {
    let Some(mut completion) = FINALIZING_NATIVE_MARKDOWN_CLOSE.take() else {
        // Late callbacks retain no native owners and cannot emit a second Host receipt.
        return;
    };
    if completion.generation != generation
        || (completion.request.scope == PluginCloseScope::ForegroundTransition
            && boundary != CloseBoundary::FullGraph)
        || (completion.request.scope == PluginCloseScope::ApplicationTermination
            && boundary != CloseBoundary::Resources)
    {
        // Scope promotion invalidates ordinary completion authority without changing original identity or destination.
        FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
        return;
    }
    VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(Some((generation, completion.request))));
    let verified = autoreleasepool(|_| completion.verify_released());
    match verified {
        Ok(receipt) => {
            // Only the typed proof authorizes its scope. Terminal diagnostics explicitly report a
            // framework lease instead of claiming the surviving panel's native deallocation.
            match receipt {
                NativeMarkdownCloseReceipt::ForegroundGraphReleased => {
                    native_markdown_trace! { eprintln!("[probe] Markdown foreground graph deallocated after_us={}", completion.cohort.elapsed().as_micros()); }
                }
                NativeMarkdownCloseReceipt::ApplicationResourcesReleased(lease) => {
                    native_markdown_trace! { eprintln!("[probe] Markdown terminal resources released after_us={} framework_panel_pending={}", completion.resources.elapsed().as_micros(), lease.framework_panel_pending); }
                }
            }
            VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(None));
            completion.lifecycle.finish_close(completion.request);
        }
        Err(error) => {
            // A failed resource/shell proof cannot retry, downgrade or announce a successful terminal receipt.
            FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
            VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(None));
            eprintln!("Markdown close release failed: {error}");
        }
    }
}

#[cfg(test)]
mod close_stack_tests {
    use super::NativeMarkdownCloseCallGuard;
    use std::cell::Cell;

    /// Nested close work and rejected/unwinding effects cannot leave a false framework-only ownership claim.
    #[test]
    fn close_call_stack_claims_restore_after_nested_unwind() {
        let depth = Cell::new(1);
        let outer = NativeMarkdownCloseCallGuard(&depth);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            depth.set(depth.get() + 1);
            let _inner = NativeMarkdownCloseCallGuard(&depth);
            assert_eq!(depth.get(), 2);
            panic!("synthetic close effect rejected by unwinding");
        }));
        assert!(unwind.is_err());
        assert_eq!(depth.get(), 1);
        drop(outer);
        assert_eq!(depth.get(), 0);
    }
}
