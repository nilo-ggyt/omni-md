//! Sort the directory on one foreground-owned worker; main-queue delivery carries only weak mailbox identity and scalar authority.

use super::workspace_browse_policy::{
    NativeMarkdownBrowseCompletionPolicy, NativeMarkdownBrowseTicket,
};
use super::*;
use crate::workspace_ready::{WorkspaceDelivery, WorkspaceReadySignal};
use omni_markdown_core::{
    MarkdownWorkspacePage, MarkdownWorkspacePageProgress, MarkdownWorkspacePager,
};

/// Only mailbox/worker own the bounded page and pager; close can drain them synchronously before notification delivery.
pub(super) type NativeMarkdownBrowseResult = Result<
    (MarkdownWorkspacePager, MarkdownWorkspacePage),
    omni_markdown_core::MarkdownWorkspaceError,
>;

/// Only explicit directory-return/refresh clears the query; save, recovery, and collapse preserve their original input contracts.
pub(super) fn return_native_markdown_workspace_to_directory(
    owner_identity: usize,
    intent: u64,
) -> Result<(), String> {
    with_active_native_markdown_surface(|surface| {
        if std::ptr::from_ref(&*surface.controller).addr() != owner_identity
            || !surface.workspace_refresh_intent_is_current(intent)
        {
            // A new input/window revokes this queued action, without cancelling the replacement query's deadline.
            return Ok(());
        }
        let mut feedback = Some(intent);
        let result = surface.clear_workspace_query_and_refresh(intent, &mut feedback);
        if let Err(error) = result
            && feedback.is_some_and(|ticket| surface.workspace_refresh_intent_is_current(ticket))
        {
            // Report only to the original intent or this action's exact successor generation, never input created by native reentry.
            surface.controller.present_error(&error);
        }
        Ok(())
    })
}

impl NativeMarkdownWindowSurface {
    /// End the known query client before clearing; marked text, validation refusal, or unknown history preserves original input.
    fn clear_workspace_query_and_refresh(
        &mut self,
        intent: u64,
        feedback: &mut Option<u64>,
    ) -> Result<(), String> {
        let field = self
            .controller
            .ivars()
            .workspace_controls
            .get()
            .and_then(|controls| controls.first())
            .and_then(|control| control.downcast_ref::<NSTextField>())
            .ok_or("搜索输入框不可用")?;
        self.controller.ivars().undo.preflight()?;
        let borrowed = self.controller.ivars().borrowed.borrow().clone();
        if let Some(editor) = field.currentEditor() {
            // Query an existing field editor only, without creating an editor or undo manager for the clear action.
            if !self.workspace_refresh_intent_is_current(intent) {
                return Ok(()); // Getter reentry revoked the old button's authority to resign or clear the replacement client.
            }
            let editor = editor
                .downcast::<NSTextView>()
                .map_err(|_| "搜索输入控件不支持原生文本编辑".to_owned())?;
            if !editor
                .downcast_ref::<NativeMarkdownSessionFieldEditor>()
                .is_some_and(|editor| {
                    editor.belongs_to(
                        &self.controller.ivars().undo,
                        &self.controller.ivars().owned,
                    )
                })
            {
                // Preserve unknown input/history; neither global undo clearing nor field identity can infer session ownership.
                return Err("搜索输入控件不属于当前笔记会话".into());
            }
            if editor.hasMarkedText() {
                // Never clear provisional Chinese composition; this action can be retried after composition ends normally.
                return Err("请先完成搜索框的输入，再返回目录".into());
            }
            if !self.workspace_refresh_intent_is_current(intent) {
                return Ok(()); // A composition getter cannot authorize recapturing the new input generation.
            }
            let responder = self.panel.firstResponder();
            if !self.workspace_refresh_intent_is_current(intent) {
                // The responder getter may reenter input; retain no authority over a replacement client.
                return Ok(());
            }
            if !responder.as_ref().is_some_and(|value| {
                std::ptr::from_ref(&**value).cast::<()>()
                    == std::ptr::from_ref(&*editor).cast::<()>()
            }) {
                // A nominal currentEditor that differs from the actual responder cannot justify stealing another client's focus.
                return Err("搜索输入焦点已经变化，请重试".into());
            }
            if !self.panel.makeFirstResponder(None) {
                // Native validation refusal preserves the value, history, and existing query mode.
                return Err("搜索输入尚未结束，请完成输入后重试".into());
            }
            if !self.workspace_refresh_intent_is_current(intent) {
                return Ok(()); // Successful didEnd can synchronously publish new input, which this old action cannot clear.
            }
            if field.currentEditor().is_some() {
                // makeFirstResponder's return value cannot substitute for the actual client's end state.
                return Err("搜索输入仍在编辑，请重试".into());
            }
        }
        if !self.workspace_refresh_intent_is_current(intent) {
            return Ok(()); // Even the no-editor getter branch can synchronously reenter a new input intent.
        }
        self.controller.ivars().undo.preflight()?;
        if let Some(error) = borrowed.failure() {
            // didEnd ends only the owned client's history; clearing the field cannot erase a recorded unknown-history failure.
            return Err(error);
        }
        field.setStringValue(ns_string!(""));
        if !self.workspace_refresh_intent_is_current(intent) {
            return Ok(()); // A replacement intent after the setter retains its own scheduling; stop cancelling or refreshing old work.
        }
        self.refresh_workspace_sidebar_with_policy(
            self.panel.mtm(),
            intent,
            NativeMarkdownBrowseCompletionPolicy::OpenFirstIfEmpty,
            feedback,
        )
    }

    /// Explicit refresh enters directory mode; the user action, not opening a Core cursor, authorizes cancelling old search.
    pub(super) fn refresh_workspace_sidebar(
        &mut self,
        mtm: MainThreadMarker,
    ) -> Result<(), String> {
        let intent = self.controller.workspace_query_intent();
        self.refresh_workspace_sidebar_with_policy(
            mtm,
            intent,
            NativeMarkdownBrowseCompletionPolicy::OpenFirstIfEmpty,
            &mut None,
        )
    }

    /// After failure the directory permits reselection only; pager policy prevents later Next from automatically opening a note.
    pub(super) fn refresh_workspace_sidebar_for_reselection(
        &mut self,
        mtm: MainThreadMarker,
        intent: u64,
        feedback_intent: &mut Option<u64>,
    ) -> Result<(), String> {
        self.refresh_workspace_sidebar_with_policy(
            mtm,
            intent,
            NativeMarkdownBrowseCompletionPolicy::RequireReselection,
            feedback_intent,
        )
    }

    /// Fix the intent before native cancellation; timer-destruction reentry cannot relabel a new intent onto the old refresh.
    pub(super) fn refresh_workspace_sidebar_with_policy(
        &mut self,
        mtm: MainThreadMarker,
        intent: u64,
        policy: NativeMarkdownBrowseCompletionPolicy,
        feedback_intent: &mut Option<u64>,
    ) -> Result<(), String> {
        *feedback_intent = None;
        if !self.workspace_refresh_intent_is_current(intent) {
            // Revoked input grants neither cancellation nor feedback authority over the current query.
            return Ok(());
        }
        self.stop_search();
        let after_cancel = intent.wrapping_add(1);
        if !self.workspace_refresh_intent_is_current(after_cancel) {
            // Only this action's exact cancellation successor may acquire a new directory pager.
            return Ok(());
        }
        // Observe this action's cancellation before opening the pager; only that exact successor may report open failure.
        *feedback_intent = Some(after_cancel);
        let pager = self
            .session
            .open_workspace_pager()
            .map_err(|error| error.to_string())?;
        self.sidebar_pager = Some(pager);
        self.sidebar_intent = Some(after_cancel);
        self.sidebar_completion_policy = policy;
        // Revoke old metadata first; deleted paths cannot count as remaining notes, while error status remains intact.
        self.notes = Vec::new();
        self.sidebar_rows = Vec::new();
        self.sidebar_has_more = false;
        // Next reserves after_cancel+1 before native cancellation; intervening input invalidates the feedback ticket.
        *feedback_intent = Some(after_cancel.wrapping_add(1));
        self.install_next_sidebar_page(mtm)
    }

    /// Separate input authority from empty-state repair: new input revokes old delivery/error authority without leaving controls half-restored.
    pub(super) fn workspace_refresh_intent_is_current(&self, intent: u64) -> bool {
        self.workspace_surface_owner_is_current()
            && self.controller.workspace_query_intent() == intent
    }

    /// Require the current borrowed stack's original owner using TLS scalars/identity only, without native weak loads or messages.
    fn workspace_surface_owner_is_current(&self) -> bool {
        let identity = std::ptr::from_ref(&*self.controller).addr();
        let owner = BORROWED_NATIVE_MARKDOWN_OWNER
            .with(|borrowed| borrowed.get() == Some(identity))
            || super::controller::native_markdown_controller_is_current(&self.controller);
        owner
            && self.closing_request.is_none()
            && !self.controller.ivars().close_finalizer_scheduled.get()
    }

    /// Strict release must restore a usable empty-body state; changed input alone cannot prevent repair for the same empty owner.
    pub(super) fn empty_workspace_compensation_is_current(&self) -> bool {
        self.workspace_surface_owner_is_current()
            && self.editor.is_none()
            && self.current_note_relative_path.is_none()
            && self.pending_note_switch.is_none()
            && self.pending_file_mutation.is_none()
    }

    /// Continue the sole unfinished scan after mounting, without capturing the surface or recursively borrowing Active.
    pub(super) fn continue_initial_sidebar_scan(
        &mut self,
        mtm: MainThreadMarker,
    ) -> Result<(), String> {
        if !self.sidebar_scan_pending || self.sidebar_pager.is_none() {
            // A small workspace is already Ready; close or a replacement query that took the pager grants no restart authority.
            return Ok(());
        }
        self.install_next_sidebar_page(mtm)
    }

    /// Next transfers one pager; bounded 512-entry steps execute on its worker, never by rescanning on AppKit's main thread.
    pub(super) fn install_next_sidebar_page(
        &mut self,
        _mtm: MainThreadMarker,
    ) -> Result<(), String> {
        let intent = self.controller.workspace_query_intent();
        if self.search_query.is_some() || self.sidebar_intent != Some(intent) {
            // A new query/IME intent revoked directory mode; its epoch cannot be attached to an old pager.
            return Ok(());
        }
        let mut pager = self
            .sidebar_pager
            .take()
            .ok_or("目录仍在读取或已经结束，请等待或刷新")?;
        self.controller.cancel_workspace_search();
        let accepted_intent = intent.wrapping_add(1);
        if self.controller.workspace_query_intent() != accepted_intent {
            // Timer release can reenter input; drop the unpublished local pager without cancelling the replacement deadline.
            return Ok(());
        }
        self.stop_search_attempt();
        self.sidebar_intent = Some(accepted_intent);
        self.sidebar_scan_pending = true;
        self.sidebar_has_more = false;
        self.controller.set_next_sidebar_page_available(false);
        if self.controller.workspace_query_intent() != accepted_intent
            || self.search_query.is_some()
        {
            // Recheck pure authority after native setters, preserving existing recovery/save feedback.
            return Ok(());
        }
        self.search_cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled = std::sync::Arc::clone(&self.search_cancelled);
        let generation = self.search_generation;
        let collapsed = self.collapsed_directories.clone();
        // Workers hold no native Weak/MainThreadBound: main-thread cancellation joins them, so a worker destructor
        // synchronously dispatching back would deadlock. The nonowning address must match the exact mailbox below.
        let owner_identity = std::ptr::from_ref(&*self.controller).addr();
        let mailbox = std::sync::Arc::new(crate::cancelable_mailbox::CancelableMailbox::new());
        self.sidebar_results = Some(std::sync::Arc::clone(&mailbox));
        let worker = std::thread::Builder::new()
            .name("omni-md-directory".into())
            .spawn(move || {
                let results = loop {
                    // Each step scans at most 512 entries with Core cancellation/session checks; Pending never enters the UI mailbox.
                    match pager.next_page_cancellable(
                        MAX_VISIBLE_NATIVE_MARKDOWN_WORKSPACE_ENTRIES - 1,
                        MAX_SCANNED_NATIVE_MARKDOWN_WORKSPACE_ENTRIES,
                        &collapsed,
                        &cancelled,
                    ) {
                        Ok(MarkdownWorkspacePageProgress::Pending) => {
                            // Continue on the same worker with another bounded step, without main-queue frame-by-frame retries.
                        }
                        Ok(MarkdownWorkspacePageProgress::Ready(page)) => break Ok((pager, page)), // Transfer one completed page and its cursor.
                        Err(error) => break Err(error), // Preserve the original failure instead of publishing a partial page.
                    }
                };
                if !mailbox.publish(results) {
                    // Close or a query change sealed the mailbox; release result and pager on the worker stack.
                    return;
                }
                let signal = WorkspaceReadySignal::Directory(WorkspaceDelivery {
                    owner: owner_identity,
                    generation,
                    intent: accepted_intent,
                    mailbox: std::sync::Arc::downgrade(&mailbox),
                });
                dispatch2::DispatchQueue::main().exec_async(move || {
                    // Deliver a weak-only signal; a borrowed owner waits for its actual restoration event without losing notice or rescanning.
                    deliver_native_markdown_workspace_signal(signal);
                });
            })
            .map_err(|error| {
                // Failed spawn acquired no thread; release this mailbox and pending flag instead of leaving permanent loading state.
                if let Some(mailbox) = self.sidebar_results.take() {
                    mailbox.cancel();
                }
                self.sidebar_scan_pending = false;
                native_markdown_worker_start_error(NativeMarkdownWorker::Directory, &error)
            })?;
        self.search_worker = Some(worker);
        Ok(())
    }

    /// Pure admission performs no native call; all native phases retain the original input and worker generations.
    fn browse_delivery_is_current(&self, generation: u64, intent: u64) -> bool {
        NativeMarkdownBrowseTicket { generation, intent }.accepts(
            self.search_generation,
            self.controller.workspace_query_intent(),
            self.sidebar_intent,
            self.search_query.is_some(),
            self.closing_request.is_some(),
        )
    }

    /// Install at most 13 rows from a complete page, rechecking original authority after layout/control calls.
    fn install_workspace_page(
        &mut self,
        page: MarkdownWorkspacePage,
        generation: u64,
        intent: u64,
        _mtm: MainThreadMarker,
    ) -> Result<bool, String> {
        if !self.browse_delivery_is_current(generation, intent)
            || !page
                .authority
                .is_current(&self.session, &self.collapsed_directories)
        {
            // Wrong mode, stale query, or local publication must not dismantle the current valid projection.
            return Ok(false);
        }
        let root = self
            .session
            .resolve_directory_for_system_action("")
            .map_err(|error| error.to_string())?;
        let root_title = native_markdown_path_name(&root.to_string_lossy());
        let (notes, mut rows) = native_markdown_sidebar_rows(page.snapshot, &root_title);
        for row in &mut rows {
            // Touch only this bounded page; collapse is user state and does not authorize a whole-workspace parent scan.
            row.is_collapsed = self.collapsed_directories.contains(&row.relative_path);
        }
        self.notes = Vec::new();
        self.sidebar_rows = Vec::new();
        self.sidebar_list.clear()?;
        if !self.browse_delivery_is_current(generation, intent) {
            // Input reentry during removal revokes the old page's authority to construct controls.
            return Ok(false);
        }
        self.sidebar_list
            .replace(&rows, self.current_note_relative_path.as_deref())?;
        if !self.browse_delivery_is_current(generation, intent)
            || !page
                .authority
                .is_current(&self.session, &self.collapsed_directories)
        {
            // Revoke the just-built obsolete projection after native replacement reentry, without publishing its pager.
            self.sidebar_list.clear()?;
            return Ok(false);
        }
        self.notes = notes;
        self.sidebar_rows = rows;
        self.sidebar_has_more = page.has_more;
        self.controller
            .set_next_sidebar_page_available(page.has_more);
        if !self.browse_delivery_is_current(generation, intent) {
            // Even final setEnabled can reenter; callers must not then publish the old pager or auto-open an old note.
            return Ok(false);
        }
        if let Some(label) = &self.empty_label {
            // Once initial Pending ends, a note-free page must replace the obsolete loading label with the policy's status.
            label.setStringValue(&NSString::from_str(
                self.sidebar_completion_policy
                    .empty_message(self.sidebar_rows.len() == 1 && !page.has_more),
            ));
        }
        if !self.browse_delivery_is_current(generation, intent) {
            // Label setters are reentry boundaries too; leave replacement input state untouched.
            return Ok(false);
        }
        // Preserve active-note recovery/save feedback, rechecking local publication authority after the final native setter.
        self.update_sidebar_mode_label(false, intent);
        // The mode title is another native setter and must pass the same original final authority check.
        Ok(self.browse_delivery_is_current(generation, intent)
            && page
                .authority
                .is_current(&self.session, &self.collapsed_directories))
    }
}

/// Main-queue signal admission still requires original owner/generations and exact mailbox identity at consumption.
pub(super) fn deliver_native_markdown_directory_result(
    delivery: WorkspaceDelivery<NativeMarkdownBrowseResult>,
) {
    let WorkspaceDelivery {
        owner: owner_identity,
        generation,
        intent: accepted_intent,
        mailbox,
    } = delivery;

    let mtm = MainThreadMarker::new().expect("目录结果在主队列交付");
    let Some(mailbox) = mailbox.upgrade() else {
        // Cancellation already synchronously discarded the payload, granting no retry or reconstruction authority.
        return;
    };
    let mut first_note = None;
    let delivered = with_active_native_markdown_surface(|surface| {
        if std::ptr::from_ref(&*surface.controller).addr() != owner_identity
            || !surface.browse_delivery_is_current(generation, accepted_intent)
            || !surface
                .sidebar_results
                .as_ref()
                .is_some_and(|current| std::sync::Arc::ptr_eq(current, &mailbox))
        {
            // Reject changed search mode, owner, input/worker generation, or exact mailbox identity.
            // Reused addresses/scalars cannot let an old notice cancel or consume its replacement mailbox.
            mailbox.cancel();
            return Ok(());
        }
        let Some(results) = mailbox.take() else {
            // A cancelled or consumed mailbox cannot install its page twice.
            return Ok(());
        };
        match results {
            Ok((pager, page)) => {
                // Even a final page requires Core authority; local save may have revoked it while queued.
                if !page
                    .authority
                    .is_current(&surface.session, &surface.collapsed_directories)
                {
                    surface.sidebar_scan_pending = false;
                    return Err("笔记目录已变化，请刷新后继续".into());
                }
                let authority = page.authority.clone();
                let installed =
                    surface.install_workspace_page(page, generation, accepted_intent, mtm)?;
                if !installed
                    || !surface.browse_delivery_is_current(generation, accepted_intent)
                    || !authority.is_current(&surface.session, &surface.collapsed_directories)
                {
                    // After native layout/setter reentry, discard only this local pager without restoring the old page.
                    return Ok(());
                }
                surface.sidebar_pager = Some(pager);
                surface.sidebar_scan_pending = false;
                surface.sidebar_results.take();
                if surface.sidebar_completion_policy.permits_auto_open()
                    && surface.editor.is_none()
                    && surface.current_note_relative_path.is_none()
                    && surface.pending_file_mutation.is_none()
                    && surface.pending_note_switch.is_none()
                {
                    // With no body after initial large-workspace loading or deletion, select the true first note only after sorting completes.
                    first_note = surface
                        .notes
                        .first()
                        .map(|note| (note.relative_path.clone(), authority));
                }
            }
            Err(error) => {
                // Only the same still-current task may report failure, preserving the existing body and user input.
                surface.sidebar_scan_pending = false;
                surface.sidebar_results.take();
                return Err(error.to_string());
            }
        }
        Ok(())
    });
    if let Err(error) = delivered {
        // present_error calls native code; recheck owner/intent first so old failure cannot overwrite a replacement query.
        let _ = with_active_native_markdown_surface(|surface| {
            if std::ptr::from_ref(&*surface.controller).addr() == owner_identity
                && surface.browse_delivery_is_current(generation, accepted_intent)
            {
                // The lease ended the TLS borrow; only this current owner may present the native error.
                surface.controller.present_error(&error);
            }
            Ok(())
        });
    }
    if let Some((path, authority)) = first_note {
        // Use the existing document handoff after leaving the surface lease, without constructing native editors under an Active borrow.
        let current = with_active_native_markdown_surface(|surface| {
            Ok(
                std::ptr::from_ref(&*surface.controller).addr() == owner_identity
                    && surface.browse_delivery_is_current(generation, accepted_intent)
                    && authority.is_current(&surface.session, &surface.collapsed_directories)
                    && surface.sidebar_completion_policy.permits_auto_open()
                    && surface.editor.is_none()
                    && surface.current_note_relative_path.is_none()
                    && surface.pending_file_mutation.is_none()
                    && surface.pending_note_switch.is_none(),
            )
        })
        .unwrap_or(false);
        if current {
            // Existing switch reports its first failure to the current surface; do not repeat a possibly stale outer error.
            let _ = switch_native_markdown_note(&path, None);
        }
    }
}
