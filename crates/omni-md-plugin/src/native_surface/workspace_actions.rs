//! Workspace commands connect confined file operations to the paginated native editor; modal panels span no surface or document borrow.
//!
//! Search owns one cancellable worker. The worker and completion carry only portable Core data, scalar identities, and
//! Rust mailboxes, never native weak wrappers whose destruction synchronously waits for the main queue.

use super::*;
use crate::workspace_ready::{WorkspaceDelivery, WorkspaceReadySignal};
use objc2_app_kit::{NSAccessibility, NSOpenPanel};
use std::path::PathBuf;

/// Only the current epoch's foreground mailbox owns scan results; main-queue closures retain no summary text.
pub(super) type NativeMarkdownSearchResult =
    Result<omni_markdown_core::MarkdownSearchPage, omni_markdown_core::MarkdownWorkspaceError>;

/// The first seven slots retain search, document, paging, and sidebar creation controls; subsequent buttons share the existing controls owner.
pub(super) const NATIVE_MARKDOWN_TOOLBAR_START: usize = 7;

/// Installs existing workspace/sidebar buttons at one composition point; closure disconnects borrowed targets through the same controls ledger.
pub(super) fn install_native_markdown_workspace_controls(
    controller: &NativeMarkdownController,
    content: &NSView,
    ledger: &NativeMarkdownOwnedLedger,
    sidebar: &NativeMarkdownSidebarList,
    has_document: bool,
    mtm: MainThreadMarker,
) -> [Weak<NSView>; 6] {
    // Restores automatic search on input; Return still submits the same bounded field immediately.
    let query = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(
            NSPoint::new(18.0, NATIVE_MARKDOWN_HEIGHT - 91.0),
            NSSize::new(212.0, 28.0),
        ),
    );
    query.setPlaceholderString(Some(ns_string!("搜索标题与正文")));
    query.setAccessibilityLabel(Some(ns_string!("搜索笔记标题和正文")));
    query.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
    // SAFETY: this selector belongs to the retained main-thread controller; NSControl borrows it.
    unsafe {
        query.setDelegate(Some(ProtocolObject::from_ref(controller)));
        query.setTarget(Some(controller));
        query.setAction(Some(sel!(searchMarkdownWorkspace:)));
    }
    content.addSubview(&query);

    // Native commands share one menu without extending the host's plugin-specific API.
    let actions = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(
            NSPoint::new(718.0, NATIVE_MARKDOWN_HEIGHT - 43.0),
            NSSize::new(128.0, 28.0),
        ),
        true,
    );
    actions.addItemWithTitle(ns_string!("文档"));
    install_native_markdown_workspace_menu(&actions, controller, ledger, has_document, mtm)
        .expect("fixed grouped document menu fits owned ledger");
    actions.setAccessibilityLabel(Some(ns_string!("文档操作与格式设置")));
    actions.setRefusesFirstResponder(true);
    actions.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
    // SAFETY: the target survives all borrowed actions until detach_controls clears the relation.
    unsafe {
        // Each leaf sends its own action once; the popup must not send a duplicate command.
        actions.setTarget(None);
        actions.setAction(None);
    }
    content.addSubview(&actions);
    // Explicit tree navigation keeps a constant number of row controls even for large vaults.
    // SAFETY: both fixed selectors belong to the retained main-thread controller.
    let refresh = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("刷新目录"),
            Some(controller),
            Some(sel!(refreshMarkdownWorkspace:)),
            mtm,
        )
    };
    refresh.setFrame(NSRect::new(
        NSPoint::new(18.0, 6.0),
        NSSize::new(98.0, 24.0),
    ));
    refresh.setAccessibilityLabel(Some(ns_string!("刷新笔记目录并返回第一页")));
    content.addSubview(&refresh);
    // SAFETY: this fixed selector belongs to the same retained controller and is detached on close.
    let next = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("下一页"),
            Some(controller),
            Some(sel!(nextMarkdownWorkspacePage:)),
            mtm,
        )
    };
    next.setFrame(NSRect::new(
        NSPoint::new(126.0, 6.0),
        NSSize::new(104.0, 24.0),
    ));
    next.setAccessibilityLabel(Some(ns_string!("显示下一页笔记")));
    content.addSubview(&next);
    assert!(
        controller
            .ivars()
            .next_sidebar_page
            .set(next.clone())
            .is_ok()
    );
    // After construction, records standard controls and actual cells explicitly; no getter spans a ledger borrow.
    for (role, control) in [
        ("workspace.search", query.as_super()),
        ("workspace.actions", actions.as_super().as_super()),
        ("workspace.refresh", refresh.as_super()),
        ("workspace.next", next.as_super()),
    ] {
        // The fixed control count is far below budget; overflow indicates a construction invariant violation.
        ledger
            .include_control(role, control)
            .expect("fixed workspace controls fit owned ledger");
    }
    let mut controls = vec![
        query.into_super(),
        actions.into_super().into_super(),
        refresh.into_super(),
        next.into_super(),
    ];
    for (title, explanation, action, x, width, role) in [
        (
            "+目录",
            "新建目录",
            sel!(createMarkdownDirectoryInSelectedDirectory:),
            80.0,
            44.0,
            "sidebar.create-directory",
        ),
        (
            "+笔记",
            "新建笔记",
            sel!(createMarkdownNoteInSelectedDirectory:),
            130.0,
            44.0,
            "sidebar.create-note",
        ),
        (
            "导入",
            "导入 Markdown 文件",
            sel!(importMarkdownNoteInSelectedDirectory:),
            180.0,
            50.0,
            "sidebar.import-note",
        ),
    ] {
        // The three existing actions occupy their section row's right side without moving search, list, or footer, targeting the same actual sidebar.
        // SAFETY: The current table implements the fixed selector and is surface-owned; controls teardown disconnects action/target first.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(&*sidebar.table),
                Some(action),
                mtm,
            )
        };
        button.setFrame(NSRect::new(
            NSPoint::new(x, NATIVE_MARKDOWN_HEIGHT - 126.0),
            NSSize::new(width, 24.0),
        ));
        button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        button.setToolTip(Some(&NSString::from_str(explanation)));
        button.setAccessibilityLabel(Some(&NSString::from_str(explanation)));
        button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        content.addSubview(&button);
        ledger
            .include_control(role, &button)
            .expect("fixed sidebar actions fit owned ledger");
        controls.push(button.into_super());
    }
    // Toolbar buttons do not take field-editor focus; the original selector still captures document/cell intent before its first getter.
    let button = |title: &str, explanation: &str, tag: isize, action, role: &'static str| {
        // SAFETY: The selector belongs to the same main-thread controller; workspace_controls teardown clears target/action first.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(controller),
                Some(action),
                mtm,
            )
        };
        button.setTag(tag);
        button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        button.setFocusRingType(NSFocusRingType::None);
        button.setRefusesFirstResponder(true);
        // A newly opened document has no history; subsequent Undo/Redo availability follows the original client's actual history notifications.
        button.setEnabled(has_document && !matches!(tag, 22 | 23));
        button.setToolTip(Some(&NSString::from_str(explanation)));
        button.setAccessibilityLabel(Some(&NSString::from_str(explanation)));
        button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        content.addSubview(&button);
        ledger
            .include_control(role, &button)
            .expect("固定文档按钮纳入原控件所有权");
        button.into_super()
    };
    for (title, explanation, tag, action, role) in [
        (
            "撤销",
            "撤销当前笔记的上一步编辑",
            22,
            sel!(performMarkdownWorkspaceAction:),
            "toolbar.undo",
        ),
        (
            "重做",
            "重做当前笔记的编辑",
            23,
            sel!(performMarkdownWorkspaceAction:),
            "toolbar.redo",
        ),
        (
            "查找",
            "在当前笔记中查找",
            7,
            sel!(performMarkdownWorkspaceAction:),
            "toolbar.find",
        ),
        (
            "信息",
            "查看当前笔记信息",
            6,
            sel!(performMarkdownWorkspaceAction:),
            "toolbar.info",
        ),
        (
            "复制全文",
            "将当前笔记的原始Markdown全文复制到系统剪贴板",
            0,
            sel!(copyEntireMarkdownDocument:),
            "toolbar.copy",
        ),
    ] {
        controls.push(button(title, explanation, tag, action, role));
    }

    // Installs existing attachment/format buttons once; their positions belong to actual horizontal scroll content while the header lays out its viewport.
    let strip = super::format_strip::install(
        controller,
        content,
        ledger,
        &mut controls,
        has_document,
        mtm,
    );
    // The five document actions retain their slots; the format strip exposes only its actual scroll-root Weak, not button-local frames as global geometry.
    let toolbar = [
        Weak::new(controls[NATIVE_MARKDOWN_TOOLBAR_START].as_super()),
        Weak::new(controls[NATIVE_MARKDOWN_TOOLBAR_START + 1].as_super()),
        Weak::new(controls[NATIVE_MARKDOWN_TOOLBAR_START + 2].as_super()),
        Weak::new(controls[NATIVE_MARKDOWN_TOOLBAR_START + 3].as_super()),
        Weak::new(controls[NATIVE_MARKDOWN_TOOLBAR_START + 4].as_super()),
        Weak::new(strip.as_super()),
    ];
    // Controls install once during surface construction; no second ownership root is created.
    assert!(controller.ivars().workspace_controls.set(controls).is_ok());
    // The initial document mounts during window construction without install_editor; the same setter must receive
    // actual document availability here, or later history notifications would mistake it for empty state and leave buttons disabled.
    controller.set_copy_document_available(has_document);
    toolbar
}

impl NativeMarkdownWindowSurface {
    /// Revokes the entire query and continuation; refresh, new terms, local file operations, and closure share this release boundary.
    pub(super) fn stop_search(&mut self) {
        self.controller.cancel_workspace_search();
        self.stop_search_attempt();
        self.session.cancel_search();
        // Pure query/cursor data must not survive the session either; dropping old continuation cannot affect the current document or history.
        self.search_cursor.take();
        self.search_authority.take();
        self.search_query.take();
        self.search_intent.take();
        self.sidebar_pager.take();
        self.sidebar_intent.take();
        self.sidebar_completion_policy = Default::default();
    }

    /// Ends one worker/mailbox attempt only; Next reuses the current Core query epoch and must not revoke its own cursor.
    pub(super) fn stop_search_attempt(&mut self) {
        // Synchronously revokes the pure restoration signal; later old-worker notification is rejected by the closed mailbox.
        cancel_native_markdown_workspace_ready(std::ptr::from_ref(&*self.controller).addr());
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        // Revokes result/query authority and releases published payload first; a late worker can only be rejected.
        if let Some(mailbox) = self.pending_search_query.take() {
            mailbox.cancel();
        }
        if let Some(mailbox) = self.search_results.take() {
            mailbox.cancel();
        }
        // Directory browsing and search share one worker lease; cancellation synchronously drains directory results and pager handles.
        if let Some(mailbox) = self.sidebar_results.take() {
            mailbox.cancel();
        }
        self.sidebar_scan_pending = false;
        // Core checks cancellation between bounded chunks. The worker holds only Rust resources, so exit/destruction cannot synchronously wait for the main queue;
        // join therefore does not depend on queued UI delivery; session or old-document release may proceed only after it returns.
        if let Some(worker) = self.search_worker.take() {
            let _ = worker.join();
        }
    }

    /// Actual publication revokes the current search projection; normal one-shot resubmission uses the same mailbox without paint/timer-loop retries.
    pub(super) fn refresh_search_after_save(&mut self, resubmit: bool) -> Result<(), String> {
        // Takes authority only from the originally accepted query; the current controller epoch may already belong to unsubmitted newer input.
        let original_query = self.search_query.clone();
        let original_intent = self.search_intent;
        // Takes the old page and continuation before native removal callbacks can borrow stale metadata.
        self.search_cursor.take();
        self.search_authority.take();
        self.notes = Vec::new();
        self.sidebar_rows = Vec::new();
        self.sidebar_has_more = false;
        self.sidebar_list.clear()?;
        self.controller.set_next_sidebar_page_available(false);
        let (Some(original_query), Some(intent)) = (original_query, original_intent) else {
            // Without an accepted old query, cannot create automatic work; clears the projection and waits for normal input.
            return Ok(());
        };
        if !resubmit || self.controller.workspace_query_intent() != intent {
            // Preserves save failure and newer input's existing 140 ms/dispatch authority; save cannot resubmit that input immediately.
            return Ok(());
        }
        let field = self
            .controller
            .ivars()
            .workspace_controls
            .get()
            .and_then(|controls| controls.first())
            .and_then(|control| control.downcast_ref::<NSTextField>())
            .ok_or("搜索输入框不可用")?;
        if field.currentEditor().is_some_and(|editor| {
            editor
                .downcast_ref::<NSTextView>()
                .is_some_and(|view| view.hasMarkedText())
        }) {
            // Autosave does not read uncommitted IME text; actual commit's didChange owns new-query scheduling.
            return Ok(());
        }
        let query = submitted_workspace_query(field)?;
        if self.controller.workspace_query_intent() != intent || query.trim() != original_query {
            // A changed field or reentrant didChange preserves newer scheduling; its current epoch cannot authorize the old resubmission.
            return Ok(());
        }
        queue_native_markdown_search_query(self, query, intent, self.panel.mtm())
    }

    /// Replaces only the current bounded metadata page; the opened document remains the sole text owner.
    fn install_search_results(
        &mut self,
        page: omni_markdown_core::MarkdownSearchPage,
        generation: u64,
        intent: u64,
        _mtm: MainThreadMarker,
    ) -> Result<bool, String> {
        if !self.search_delivery_is_current(generation, intent) {
            // Keeps original worker authority at entry; a changed field cannot renew authority for old results.
            return Ok(false);
        }
        let query = self.search_query.as_deref().ok_or("搜索查询已结束")?;
        if !page.authority.is_current(&self.session, query) {
            // Local save may revoke results after worker publication; an unchanged native attempt epoch cannot authorize the old page.
            return Err("笔记已变化，请重新搜索".into());
        }
        if !self.search_delivery_is_current(generation, intent) {
            // Getter reentry into new input preserves the existing directory/page instead of continuing teardown or publishing old results.
            return Ok(false);
        }
        // Retains the list object, revoking only its old page/menu epoch; callbacks do not borrow the surface back.
        self.notes = Vec::new();
        self.sidebar_rows = Vec::new();
        self.sidebar_list.clear()?;
        if !self.search_delivery_is_current(generation, intent) {
            // Cleans only the old controls just removed; no candidate is created after input reentry.
            return Ok(false);
        }
        let omni_markdown_core::MarkdownSearchPage {
            results,
            next,
            authority,
        } = page;
        let notes = results
            .iter()
            .map(|result| MarkdownNoteSummary {
                // Copies only the path/title needed for opening, without document text or a resident index.
                relative_path: result.relative_path.clone(),
                title: result.title.clone(),
                byte_count: 0,
                modified_at_ms: 0,
            })
            .collect();
        let rows: Vec<_> = results
            .into_iter()
            .map(NativeMarkdownSidebarRow::from_search_result)
            .collect();
        self.sidebar_list
            .replace(&rows, self.current_note_relative_path.as_deref())?;
        if !self.search_delivery_is_current(generation, intent)
            || self
                .search_query
                .as_deref()
                .is_none_or(|query| !authority.is_current(&self.session, query))
        {
            self.sidebar_list.clear()?;
            return Ok(false);
        }
        // Publishes metadata and controls together after the final pure check; the owner lease prevents concurrent successor-page writes.
        self.sidebar_pager.take();
        self.sidebar_has_more = next.is_some();
        self.search_cursor = next;
        self.search_authority = Some(authority);
        self.notes = notes;
        self.sidebar_rows = rows;
        self.controller
            .set_next_sidebar_page_available(self.sidebar_has_more);
        if !self.search_delivery_is_current(generation, intent) {
            // If new input takes over during setEnabled, the old page cannot update the mode caption or report stale errors.
            return Ok(false);
        }
        self.update_sidebar_mode_label(true, intent);
        Ok(self.search_delivery_is_current(generation, intent))
    }

    /// All native phases share the entry intent; this pure check reads no field and upgrades no native Weak.
    fn search_delivery_is_current(&self, generation: u64, intent: u64) -> bool {
        self.search_generation == generation
            && self.controller.workspace_query_intent() == intent
            && self.search_intent == Some(intent)
            && self.search_query.is_some()
            && self.closing_request.is_none()
    }
}

/// Allows at most 128 query scalars; checks native length before copying an unbounded NSString into queued work.
fn submitted_workspace_query(field: &NSTextField) -> Result<String, String> {
    let value = field.stringValue();
    let length = value.length();
    if length > 256 {
        // 128 Unicode scalars occupy at most 256 UTF-16 units; oversized input creates no Rust text copy.
        return Err("搜索词最多为 128 个字符".into());
    }
    let mut units = [0_u16; 256];
    // SAFETY: The fixed buffer covers the checked length, and NSString writes only that bounded range.
    unsafe {
        value.getCharacters_range(
            NonNull::new(units.as_mut_ptr()).unwrap(),
            NSRange::new(0, length),
        );
    }
    submitted_workspace_query_from_utf16(&units[..length])
}

/// Never truncates valid Unicode queries; rejects unpaired surrogates or excess scalar count.
fn submitted_workspace_query_from_utf16(units: &[u16]) -> Result<String, String> {
    if units.len() > 256 {
        // The pure conversion follows the native boundary too; callers cannot bypass its preliminary budget.
        return Err("搜索词最多为 128 个字符".into());
    }
    let query =
        String::from_utf16(units).map_err(|_| "搜索词包含无效的 Unicode 字符".to_owned())?;
    if query.chars().count() > 128 {
        // BMP and combining sequences follow Core's scalar rule without truncating by presumed grapheme count.
        return Err("搜索词最多为 128 个字符".into());
    }
    Ok(query)
}

/// Submits the query to the current surface's cancellable mailbox; the main queue carries only weak identity and epochs.
pub(super) fn enqueue_native_markdown_workspace_search(
    controller: &NativeMarkdownController,
    field: &NSTextField,
) -> Result<(), String> {
    if !controller.owns_workspace_query(field) {
        // The shared entry verifies field identity too; an arbitrary action cannot search another field's contents.
        return Err("搜索输入框已变更，请重新搜索".into());
    }
    let intent = controller.workspace_query_intent();
    if field.currentEditor().is_some_and(|editor| {
        // Neither Return nor timer submits unfinished composition; inspects only an existing editor.
        editor
            .downcast_ref::<NSTextView>()
            .is_some_and(|view| view.hasMarkedText())
    }) {
        // Preserves input until didChange after actual composition commit schedules search.
        return Ok(());
    }
    let query = submitted_workspace_query(field)?;
    if controller.workspace_query_intent() != intent {
        // Getter reentry into new input revokes the old snapshot's submission authority without cancelling a newer deadline.
        return Ok(());
    }
    let mtm = MainThreadMarker::new().expect("workspace queries use the main thread");
    with_active_native_markdown_surface(|surface| {
        if !std::ptr::eq(&*surface.controller, controller) {
            return Err("当前笔记搜索会话已变更，请重新搜索".into());
        }
        queue_native_markdown_search_query(surface, query, intent, mtm)
    })
}

/// New queries and post-save resubmission share one bounded mailbox without recursively borrowing Active from a surface operation.
fn queue_native_markdown_search_query(
    surface: &mut NativeMarkdownWindowSurface,
    query: String,
    intent: u64,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    if surface.controller.workspace_query_intent() != intent {
        // An old action may drop only its own query, never clear or restart newer input.
        return Ok(());
    }
    surface.stop_search();
    let accepted_intent = intent.wrapping_add(1);
    if surface.controller.workspace_query_intent() != accepted_intent {
        // If native timer release reenters didChange, the newer input owns authority and the old request cannot replace it.
        return Ok(());
    }
    let mailbox = std::sync::Arc::new(crate::cancelable_mailbox::CancelableMailbox::new());
    mailbox.publish(query);
    surface.pending_search_query = Some(std::sync::Arc::clone(&mailbox));
    let mailbox = std::sync::Arc::downgrade(&mailbox);
    let generation = surface.search_generation;
    let controller = MainThreadBound::new(Weak::from_retained(&surface.controller), mtm);
    enqueue_native_markdown_main_task(move || {
        // The actual main-queue entry reacquires thread proof instead of capturing MainThreadMarker in a Send closure.
        let mtm = MainThreadMarker::new().expect("搜索查询在AppKit主队列执行");
        let Some(controller) = controller.get(mtm).load() else {
            // Notification keeps no target alive and reconstructs no query after closure.
            return;
        };
        let Some(mailbox) = mailbox.upgrade() else {
            // Cancellation/closure released the payload; a weak notification has no retry authority.
            return;
        };
        let accepted = with_active_native_markdown_surface(|surface| {
            // Checks input intent and worker epoch separately, covering new terms and save revocation during dispatch.
            Ok(std::ptr::eq(&*surface.controller, &*controller)
                && surface.search_generation == generation
                && controller.workspace_query_intent() == accepted_intent)
        })
        .unwrap_or(false);
        if !accepted {
            mailbox.cancel();
            return;
        }
        if let Some(query) = mailbox.take() {
            // Consumes the same query request once; start rechecks intent changes across native cancellation.
            if let Err(error) =
                start_native_markdown_search_page(query, None, Some((&controller, accepted_intent)))
            {
                controller.present_error(&error);
            }
        }
    });
    Ok(())
}

/// The same bounded worker accepts a new query or opaque continuation; queued completion remains a weak notification only.
fn start_native_markdown_search_page(
    query: String,
    cursor: Option<omni_markdown_core::MarkdownSearchCursor>,
    expected_owner: Option<(&NativeMarkdownController, u64)>,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("笔记搜索必须在 AppKit 主线程执行")?;
    with_active_native_markdown_surface(|surface| {
        if expected_owner.is_some_and(|(owner, intent)| {
            !std::ptr::eq(owner, &*surface.controller) || owner.workspace_query_intent() != intent
        }) {
            // If synchronous native reads hand off the session, old Next cannot cancel or rewrite the new owner's query state.
            return Err("笔记搜索会话已经变化，请重新搜索".into());
        }
        let intent = surface.controller.workspace_query_intent();
        if cursor.is_some() {
            // Next cancels only the prior attempt without advancing Core's query epoch; Core still validates the original cursor.
            surface.controller.cancel_workspace_search();
            if surface.controller.workspace_query_intent() != intent.wrapping_add(1) {
                // New terms during native invalidate/drop revoke this Next; their worker/mailbox remain untouched.
                return Ok(());
            }
            surface.stop_search_attempt();
        } else {
            // Actual new input revokes the old query, results, directory continuation, and search cursor.
            surface.stop_search();
            if surface.controller.workspace_query_intent() != intent.wrapping_add(1) {
                // New-query cancellation may itself reenter another term; the old request cannot start afterward.
                return Ok(());
            }
        }
        let accepted_intent = intent.wrapping_add(1);
        if query.trim().is_empty() {
            // An empty query returns to bounded directory projection without retaining a scan worker or result mailbox.
            return surface.refresh_workspace_sidebar(mtm);
        }
        // Marks search mode during first-page scanning too, so repeated AX Next cannot advance an old directory cursor.
        surface.search_query = Some(query.trim().to_owned());
        surface.search_intent = Some(accepted_intent);
        surface.search_cursor.take();
        surface.controller.set_next_sidebar_page_available(false);
        if surface.controller.workspace_query_intent() != accepted_intent {
            // setEnabled may reenter input; its new epoch cannot replace the captured one to authorize an old worker.
            return Ok(());
        }
        surface.search_cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled = std::sync::Arc::clone(&surface.search_cancelled);
        let generation = surface.search_generation;
        let session = surface.session.clone();
        // Stores a non-owning identity address rather than native Weak: cancellation joins on the main thread, while destruction of MainThreadBound
        // on the worker could synchronously dispatch there. Exact mailbox identity separately rejects address reuse on delivery.
        let owner_identity = std::ptr::from_ref(&*surface.controller).addr();
        let mailbox = std::sync::Arc::new(crate::cancelable_mailbox::CancelableMailbox::new());
        surface.search_results = Some(std::sync::Arc::clone(&mailbox));
        let worker = std::thread::Builder::new()
            .name("omni-md-search".into())
            .spawn(move || {
                // The worker accesses portable Core only; cancellation bounds scanning and releases file snapshots before exit.
                let results = session.search_notes_page_cancellable(
                    &query,
                    MAX_VISIBLE_NATIVE_MARKDOWN_WORKSPACE_ENTRIES,
                    cursor.as_ref(),
                    &cancelled,
                );
                if !mailbox.publish(results) {
                    // Close already drained and sealed the mailbox; late results are released on the worker stack.
                    return;
                }
                let signal = WorkspaceReadySignal::Search(WorkspaceDelivery {
                    owner: owner_identity,
                    generation,
                    intent: accepted_intent,
                    mailbox: std::sync::Arc::downgrade(&mailbox),
                });
                dispatch2::DispatchQueue::main().exec_async(move || {
                    // Sends a pure weak signal; a leased owner waits for actual restoration without losing notification or rescanning.
                    deliver_native_markdown_workspace_signal(signal);
                });
            })
            .map_err(|error| {
                native_markdown_worker_start_error(NativeMarkdownWorker::Search, &error)
            })?;
        surface.search_worker = Some(worker);
        Ok(())
    })
}

/// Next reuses the current query or directory; changed input, IME, and repeated actions cannot borrow an old cursor.
fn advance_native_markdown_sidebar_page(
    mtm: MainThreadMarker,
    owner: &NativeMarkdownController,
) -> Result<(), String> {
    let intent = owner.workspace_query_intent();
    let continuation = with_active_native_markdown_surface(|surface| {
        if !std::ptr::eq(owner, &*surface.controller) {
            // An old menu/AX action cannot continue paging through a new session's same button slot.
            return Err("笔记搜索会话已经变化，请重新搜索".into());
        }
        let field = surface
            .controller
            .ivars()
            .workspace_controls
            .get()
            .and_then(|controls| controls.first())
            .and_then(|control| control.downcast_ref::<NSTextField>())
            .ok_or("搜索输入框不可用")?;
        if field.currentEditor().is_some_and(|editor| {
            // Observes only an existing field editor; uncommitted composition grants no paging authority.
            editor
                .downcast_ref::<NSTextView>()
                .is_some_and(|view| view.hasMarkedText())
        }) {
            return Err("请先完成搜索词输入，再查看下一页".into());
        }
        let submitted = submitted_workspace_query(field)?;
        if owner.workspace_query_intent() != intent {
            // After native field reads reenter new input, old Next can neither take a directory cursor nor cancel the new deadline.
            return Ok(None);
        }
        let Some(query) = surface.search_query.as_ref() else {
            if surface.sidebar_intent != Some(intent) {
                // A term awaiting its 140 ms deadline cannot lend its new input epoch to an old directory Next scan.
                return Err("搜索词已经变化，请等待搜索完成后再翻页".into());
            }
            // Explicit refresh may preserve field text in directory mode; Next under that same intent continues directory paging.
            surface.install_next_sidebar_page(mtm)?;
            return Ok(None);
        };
        if submitted.trim() != query {
            // An unsubmitted term within debounce cannot reuse an old continuation; preserves input/cursor until normal query replacement.
            return Err("搜索词已经变化，请等待搜索完成后再翻页".into());
        }
        if owner.workspace_query_intent() != intent {
            // Reentry during currentEditor, stringValue, or UTF-16 reads preserves new input and its queued authority.
            return Ok(None);
        }
        let query = query.clone();
        let cursor = surface
            .search_cursor
            .take()
            .ok_or("搜索仍在进行或已到最后一页")?;
        Ok(Some((query, cursor)))
    })?;
    if let Some((query, cursor)) = continuation {
        // Starts the worker after the surface operation; cursor transfers as pure data without native or document ownership.
        start_native_markdown_search_page(query, Some(cursor), Some((owner, intent)))?;
    }
    Ok(())
}

/// Snapshot of non-TextKit handles; modal work cannot keep an editor graph alive.
struct DocumentActionContext {
    session: MarkdownSessionAccess,
    controller: Retained<NativeMarkdownController>,
    relative_path: Option<String>,
}

/// Copies action identity after validating that no document handoff is underway.
fn active_action_context() -> Result<DocumentActionContext, String> {
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        let surface = active.as_ref().ok_or("笔记窗口已关闭")?;
        // A pending handoff owns its release barrier and rejects overlapping document work.
        if native_markdown_operation_is_busy(
            surface.pending_note_switch.is_some(),
            surface.pending_file_mutation.is_some(),
        ) {
            return Err("另一项文档操作仍在完成中".into());
        }
        Ok(DocumentActionContext {
            session: surface.session.clone(),
            controller: surface.controller.clone(),
            relative_path: surface.current_note_relative_path.clone(),
        })
    })
}

/// Rejects modal results for an obsolete surface or a note changed through native reentry.
fn ensure_action_is_current(
    controller: &NativeMarkdownController,
    path: Option<&str>,
) -> Result<(), String> {
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        let surface = active.as_ref().ok_or("笔记窗口已关闭")?;
        // Reopening a plugin reuses no controller identity, even when the same note path is selected.
        if !std::ptr::eq(&*surface.controller, controller)
            || surface.current_note_relative_path.as_deref() != path
        {
            return Err("对话框打开期间文档已变更，请重试".into());
        }
        Ok(())
    })
}

/// Current-note menus supplement the original queued window lease with actual document Weak and the installed path captured at click time.
/// The intent retains no native/document strong owner; sidebar paging, search, and directory selection cannot change this file target.
#[derive(Clone)]
pub(super) struct NativeMarkdownCurrentNoteIntent {
    controller: usize,
    window: usize,
    pub(super) document: WeakMarkdownDocument,
    pub(super) relative_path: String,
}

impl NativeMarkdownCurrentNoteIntent {
    /// The composition root binds actual assembled window/document/path identities weakly without requiring a constructing window to be Active.
    pub(super) fn for_document(
        controller: &NativeMarkdownController,
        panel: &NativeMarkdownPanel,
        document: &SharedMarkdownDocument,
        relative_path: &str,
    ) -> Self {
        Self {
            controller: std::ptr::from_ref(controller).addr(),
            window: std::ptr::from_ref(panel).addr(),
            document: Rc::downgrade(document),
            relative_path: relative_path.to_owned(),
        }
    }

    /// Captures pure identity before sender/field getters, without reading text, confirming IME, or acquiring another native window lease.
    pub(super) fn capture(controller: &NativeMarkdownController) -> Option<Self> {
        ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active.as_ref()?;
            if !std::ptr::eq(&*surface.controller, controller)
                || !native_markdown_controller_has_foreground_authority(controller)
                || native_markdown_operation_is_busy(
                    surface.pending_note_switch.is_some(),
                    surface.pending_file_mutation.is_some(),
                )
            {
                return None; // A successor window or accepted handoff cannot reacquire file authority from the old menu.
            }
            let editor = surface.editor.as_ref()?;
            Some(Self::for_document(
                controller,
                &surface.panel,
                editor.document(),
                surface.current_note_relative_path.as_deref()?,
            ))
        })
    }

    /// Document Weak retains allocation identity; reopening the same path through A-to-B-to-A cannot restore the old intent.
    fn matches(&self, surface: &NativeMarkdownWindowSurface) -> bool {
        self.controller == std::ptr::from_ref(&*surface.controller).addr()
            && self.window == std::ptr::from_ref(&*surface.panel).addr()
            && native_markdown_controller_has_foreground_authority(&surface.controller)
            && surface.current_note_relative_path.as_deref() == Some(self.relative_path.as_str())
            && !native_markdown_operation_is_busy(
                surface.pending_note_switch.is_some(),
                surface.pending_file_mutation.is_some(),
            )
            && surface.editor.as_ref().is_some_and(|editor| {
                std::ptr::eq(self.document.as_ptr(), Rc::as_ptr(editor.document()))
            })
    }

    /// Checks original window/document metadata only, without weak upgrades or querying a later sidebar target.
    pub(super) fn current(&self) -> bool {
        ACTIVE_NATIVE_MARKDOWN_SURFACE
            .with_borrow(|active| active.as_ref().is_some_and(|surface| self.matches(surface)))
    }

    /// Shares the actual reload/save-as flow between the menu and conflict bar, preserving the
    /// original note intent through IME commit, confirmation, naming, and disk side effects.
    pub(super) fn perform_resolution(&self, action: isize, revision: u64) -> Result<(), String> {
        if !self.current() {
            return Ok(());
        }
        let DocumentActionContext {
            session,
            controller,
            ..
        } = active_action_context()?;
        let current = || self.current() && controller.conflict_publication() == revision;
        if !current() {
            return Ok(());
        }
        preflight_native_markdown_modal()?;
        if !current() {
            return Ok(());
        }
        with_active_native_markdown_surface(|surface| {
            // Commit the actual presentation mode's one input client. Source mode still owns
            // Chinese composition even though it does not use an NSTextView.
            surface.commit_marked_text(false)
        })?;
        if !current() {
            return Ok(());
        }
        let mtm = MainThreadMarker::new().expect("冲突处理沿原AppKit主队列执行");
        match action {
            // Discard the local draft only after explicit confirmation; cancellation keeps the
            // same canonical model and the authoritative external file.
            4 => {
                let path = self.relative_path.clone();
                if !run_native_markdown_modal_confirmation(
                    &controller,
                    "重新加载磁盘版本？",
                    "当前本地草稿会被放弃，这项操作无法撤销。",
                    "放弃草稿并重新加载",
                    Some("取消"),
                    mtm,
                )
                .map_err(|error| error.to_string())?
                {
                    // Only explicit reload authorizes discard. Cancel preserves both live and disk versions.
                    return Ok(());
                }
                if !current() {
                    return Ok(());
                }
                // Explicit discard keeps the established reload route and waits for the actual
                // old editor release before acquiring the confined destination Markdown file.
                replace_active_editor_from_disk(path, self, revision, None)
            }
            // Save the live draft under an exclusive new filename before replacing the editor
            // through its original actual-release barrier.
            5 => {
                let path = self.relative_path.clone();
                let title = format!(
                    "{} 本地草稿",
                    Path::new(&path)
                        .file_stem()
                        .and_then(|title| title.to_str())
                        .expect("原Core已验证笔记UTF-8文件名")
                );
                let Some(title) = run_native_markdown_name_dialog(
                    &controller,
                    "保留本地草稿",
                    "创建一篇新笔记并把当前本地草稿保存进去。",
                    &title,
                    "新笔记标题",
                    "另存为新笔记",
                    mtm,
                )
                .map_err(|error| error.to_string())?
                else {
                    // Cancelling naming preserves the live draft and disk file without creating or deleting a note.
                    return Ok(());
                };
                if !current() {
                    return Ok(());
                }
                let (receipt, saved_generation) =
                    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                        // Recheck the original note and conflict publication before the irreversible
                        // save-as. Core streams its actual piece tree and returns a durable-copy receipt.
                        let surface = active.as_ref().ok_or("笔记窗口已关闭")?;
                        if !self.matches(surface)
                            || surface.controller.conflict_publication() != revision
                        {
                            return Err("原冲突笔记或保存结果已变更，本次操作已取消".to_owned());
                        }
                        let editor = surface.editor.as_ref().ok_or("当前没有打开的笔记")?;
                        let document = editor.document().borrow();
                        // Fix the generation in the same original document borrow that owns the actual
                        // save-as. Later Native sidebar work must not recapture a newer live generation.
                        let generation = document.generation();
                        let receipt = session
                            .save_document_buffer_as_new(
                                &document,
                                &native_markdown_note_parent(&path),
                                &title,
                            )
                            .map_err(|error| error.to_string())?;
                        let actual = document.current_revision().map_err(|error| {
                            format!(
                                "草稿副本已保存到“{}”，但原正文校验未完成，原草稿已保留：{error}",
                                receipt.relative_path
                            )
                        })?;
                        if receipt.revision != actual.encode_hex() {
                            // A published copy cannot justify discarding another live value. Keep both
                            // files and the original editor; do not replay the already completed save-as.
                            return Err(format!(
                                "草稿副本已保存到“{}”，但与原正文版本不一致，原草稿已保留",
                                receipt.relative_path
                            ));
                        }
                        Ok((receipt, generation))
                    })?;
                let saved_input_current = || {
                    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                        active.as_ref().is_some_and(|surface| {
                            self.matches(surface)
                                && surface.controller.conflict_publication() == revision
                                && surface.editor.as_ref().is_some_and(|editor| {
                                    editor.document().borrow().generation() == saved_generation
                                })
                        })
                    })
                };
                if !saved_input_current() {
                    return Err(format!(
                        "草稿副本已保存到“{}”，但原笔记已变更，原草稿已保留",
                        receipt.relative_path
                    ));
                }
                with_active_native_markdown_surface(|surface| {
                    // Refresh only the still-original surface after the copy exists. A later failure
                    // reports the saved path; it cannot roll back or hide that real published copy.
                    if !self.matches(surface)
                        || surface.controller.conflict_publication() != revision
                        || surface.editor.as_ref().is_none_or(|editor| {
                            editor.document().borrow().generation() != saved_generation
                        })
                    {
                        return Err("原笔记已变更，原草稿已保留".to_owned());
                    }
                    surface.refresh_workspace_sidebar(mtm)
                })
                .map_err(|error| {
                    format!(
                        "草稿副本已保存到“{}”，但目录刷新未完成，原草稿已保留：{error}",
                        receipt.relative_path
                    )
                })?;
                if !saved_input_current() {
                    return Err(format!(
                        "草稿副本已保存到“{}”，但原笔记已变更，原草稿已保留",
                        receipt.relative_path
                    ));
                }
                // The new copy already passed Core's durable save-as boundary. Transition through
                // the same original editor teardown; failure preserves the copy and current draft.
                replace_active_editor_from_disk(
                    receipt.relative_path,
                    self,
                    revision,
                    Some(saved_generation),
                )
            }
            _ => unreachable!("冲突处理只接原重载和另存动作"),
        }
    }

    /// Collects bounded candidates for the original note; actual writes still use begin_file_mutation's save, revision, and release flow.
    pub(super) fn perform(&self, action: isize) -> Result<(), String> {
        if !self.current() {
            return Ok(()); // The original document departed during dispatch; cancels the action without showing an old error on a new note.
        }
        preflight_native_markdown_modal()?;
        if !self.current() {
            return Ok(()); // A revoked original window cannot capture a later active session after preflight.
        }
        let mtm = MainThreadMarker::new().expect("当前笔记菜单只由原AppKit主队列消费");
        let (session, controller, directories) =
            ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                let surface = active.as_ref().expect("上方纯检查固定原Active窗口");
                let directories = if action == 31 {
                    // The page supplies bounded editable destination suggestions only; the source remains the original path, independent of rows or selected folders.
                    let parent = native_markdown_note_parent(&self.relative_path);
                    std::iter::once(String::new())
                        .chain(
                            surface
                                .sidebar_rows
                                .iter()
                                .filter(|row| row.kind == NativeMarkdownSidebarEntryKind::Directory)
                                .map(|row| row.relative_path.clone()),
                        )
                        .filter(|directory| directory != &parent)
                        .collect::<Vec<_>>()
                } else {
                    Vec::new() // Other actions select no directory and copy no unrelated page metadata.
                };
                (
                    surface.session.clone(),
                    surface.controller.clone(),
                    directories,
                )
            });
        if action == 33 {
            // Finder neither saves nor commits the draft; original Core revalidates the actual Markdown file before invoking the system service.
            let path = session
                .resolve_note_for_system_action(&self.relative_path)
                .map_err(|error| error.to_string())?;
            if !self.current() {
                return Ok(());
            }
            return reveal_markdown_entry_in_finder(&path);
        }
        let title = Path::new(&self.relative_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("已安装笔记路径由Core验证为UTF-8文件名");
        let mutation = match action {
            // Initial text comes only from the original note's actual title; Core naming rules still admit the confirmed candidate.
            30 => run_native_markdown_name_dialog(
                &controller,
                "重命名笔记",
                "输入笔记的新标题。",
                title,
                "笔记标题",
                "重命名",
                mtm,
            )
            .map_err(|error| error.to_string())?
            .map(|title| NativeMarkdownFileMutation::RenameNote {
                relative_path: self.relative_path.clone(),
                title,
            }),
            // Move permits typing a directory outside the bounded page; the existing dialog and Core own input validation and confinement.
            31 => run_native_markdown_move_destination_dialog(&controller, &directories, mtm)
                .map_err(|error| error.to_string())?
                .map(
                    |destination_directory| NativeMarkdownFileMutation::MoveNote {
                        relative_path: self.relative_path.clone(),
                        destination_directory,
                    },
                ),
            32 => Some(NativeMarkdownFileMutation::CopyNote {
                relative_path: self.relative_path.clone(),
            }), // Core chooses the copy path under its no-overwrite rule; UI neither guesses filenames nor copies document text.
            // Delete requires explicit confirmation of this original note; Cancel neither commits its text nor deletes a file.
            34 => run_native_markdown_modal_confirmation(
                &controller,
                &format!("删除笔记“{title}”？"),
                "这篇笔记会从 OMNI 中删除，此操作无法撤销。",
                "删除",
                Some("取消"),
                mtm,
            )
            .map_err(|error| error.to_string())?
            .then(|| NativeMarkdownFileMutation::DeleteNote {
                relative_path: self.relative_path.clone(),
            }),
            _ => unreachable!("原文档菜单只向此票据分派五个当前笔记动作"),
        };
        if !self.current() {
            return Ok(()); // Modal/native cleanup revoked the original document; the candidate cannot target a later note with the same name.
        }
        let Some(mutation) = mutation else {
            return Ok(()); // User cancellation neither saves, releases the document, nor queues file effects.
        };
        let accepted = with_active_native_markdown_surface(|surface| {
            if !self.matches(surface) {
                return Ok(false); // The original identity must match before and after leasing Active; a path cannot renew it.
            }
            surface.begin_file_mutation(mutation)?;
            Ok(true)
        })?;
        if !accepted {
            return Ok(());
        }
        with_active_native_markdown_surface(|surface| {
            // After admission, the original pending operation owns one actual revision/release receipt; revoked before-state cannot recapture a document or replay.
            let pending = surface
                .pending_file_mutation
                .as_ref()
                .expect("原begin_file_mutation已接纳唯一文件操作");
            controller.enqueue_owned_after_release(pending.release_barrier.as_ref(), || {
                if let Err(error) = finish_native_markdown_file_mutation() {
                    eprintln!("笔记文件操作失败：{error}"); // The original completion boundary already retained file consequences and Chinese status feedback.
                }
            });
            Ok(())
        })
    }
}

/// Foundation already decoded the URL path. Resolve dot segments relative to this note directory; the
/// original session still confines the actual file.
#[cfg(feature = "native-paged-production")]
fn relative_note_link_path(note: &str, path: &str) -> Result<String, String> {
    use std::path::Component;
    let mut target = PathBuf::from(native_markdown_note_parent(note));
    for component in Path::new(path).components() {
        match component {
            Component::Normal(name) => target.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                if !target.pop() {
                    return Err("笔记链接不能越出当前笔记库".into());
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err("笔记链接必须使用库内相对路径".into());
            }
        }
    }
    target
        .to_str()
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "笔记链接没有有效的目标路径".into())
}

/// A relative link queues one weak continuation. Its finite fragment belongs to this switch and cannot
/// be recaptured from the old URI after installing another note.
#[cfg(feature = "native-paged-production")]
pub(super) fn queue_native_markdown_relative_note_link(
    view: &super::paged_text_view::NativeMarkdownPagedTextView,
    key: &super::paged_text_view::NativeMarkdownPagedSceneKey,
    click: &std::rc::Weak<()>,
    path: &str,
    fragment: Option<String>,
) -> Result<(), super::paged_text_view::NativeMarkdownPagedTextViewError> {
    use super::paged_text_view::NativeMarkdownPagedTextViewError as LinkError;
    let note = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        let surface = active.as_ref().ok_or_else(|| {
            LinkError::RelativeNoteLink("当前链接不属于已打开的笔记工作区".into())
        })?;
        let editor = surface.editor.as_ref().ok_or(LinkError::Revoked)?;
        if !std::ptr::eq(editor.input_view(), view.as_super())
            || !std::rc::Weak::ptr_eq(&key.request.document, &Rc::downgrade(editor.document()))
            || !native_markdown_controller_has_foreground_authority(&surface.controller)
            || native_markdown_operation_is_busy(
                surface.pending_note_switch.is_some(),
                surface.pending_file_mutation.is_some(),
            )
        {
            return Err(LinkError::Revoked);
        }
        let relative = surface
            .current_note_relative_path
            .as_deref()
            .ok_or(LinkError::Revoked)?;
        Ok(NativeMarkdownCurrentNoteIntent::for_document(
            &surface.controller,
            &surface.panel,
            editor.document(),
            relative,
        ))
    })?;
    let target =
        relative_note_link_path(&note.relative_path, path).map_err(LinkError::RelativeNoteLink)?;
    if !view.link_intent_current(key, click)? || !note.current() {
        return Err(LinkError::Revoked);
    }
    // Native weak references cannot retain the original body; the main queue carries only original weak
    // key/click, note identity, and finite relative destination.
    let pending = MainThreadBound::new(
        (
            Weak::from(view),
            key.clone(),
            click.clone(),
            note,
            target,
            fragment,
        ),
        view.mtm(),
    );
    enqueue_native_markdown_main_task(move || {
        let mtm = MainThreadMarker::new().expect("相对笔记链接只在原AppKit主队列消费");
        let (weak, key, click, note, target, fragment) = pending.get(mtm);
        let Some(view) = weak.load() else { return };
        let result = (|| {
            let current = match view.link_intent_current(key, click) {
                Ok(current) => current,
                Err(LinkError::Revoked | LinkError::Closed | LinkError::Pending) => false,
                Err(cause) => return Err(cause),
            };
            if !current || !note.current() {
                return Ok(false);
            }
            let session = ACTIVE_NATIVE_MARKDOWN_SURFACE
                .with_borrow(|active| {
                    active
                        .as_ref()
                        .filter(|surface| note.matches(surface))
                        .map(|surface| surface.session.clone())
                })
                .ok_or(LinkError::Revoked)?;
            // Verify the actual Markdown file and symlink confinement before touching the unsaved
            // draft; invalid paths cannot tear down its existing graph.
            session
                .resolve_note_for_system_action(target)
                .map_err(|cause| LinkError::RelativeNoteLink(cause.to_string()))?;
            if !view.link_intent_current(key, click)? || !note.current() {
                return Ok(false);
            }
            view.retire_link_intent(click); // Consume original authority before save/teardown; report failure without replaying an admitted switch.
            Ok(true)
        })();
        match result {
            Ok(true) => {
                drop(view); // Release the temporary Native claim before the old-graph barrier so it cannot block its own actual deallocation.
                if let Err(cause) = switch_native_markdown_note(target, fragment.clone())
                    && let Some(view) = weak.load()
                {
                    view.report_link_failure(key, LinkError::RelativeNoteLink(cause));
                }
            }
            Ok(false) => view.retire_link_intent(click), // Successor input/focus/close revokes only the original click, never granting it authority over the new note.
            Err(cause) => {
                view.retire_link_intent(click);
                view.report_link_failure(key, cause);
            }
        }
    });
    Ok(())
}

/// Report failure only to this actual View/document's original workspace, never a successor window or
/// another open note with the same name.
#[cfg(feature = "native-paged-production")]
pub(super) fn present_native_markdown_paged_link_error(
    view: &super::paged_text_view::NativeMarkdownPagedTextView,
    request: &super::paged_text_view::NativeMarkdownPagedSceneRequest,
    cause: &super::paged_text_view::NativeMarkdownPagedTextViewError,
) {
    let _ = with_active_native_markdown_surface(|surface| {
        if surface.editor.as_ref().is_some_and(|editor| {
            std::ptr::eq(editor.input_view(), view.as_super())
                && std::rc::Weak::ptr_eq(&request.document, &Rc::downgrade(editor.document()))
        }) && native_markdown_controller_has_foreground_authority(&surface.controller)
        {
            surface.controller.present_error(&cause.to_string());
        }
        Ok(())
    });
}

/// The system chooser supplies Core only an explicitly confirmed bounded file URL; Cancel remains distinct from Abort.
fn choose_import_file(
    controller: &NativeMarkdownController,
    mtm: MainThreadMarker,
    markdown_only: bool,
) -> Result<Option<PathBuf>, NativeMarkdownModalError> {
    let guard = NativeMarkdownModalUseGuard::enter(controller)?;
    let result = autoreleasepool(|_| {
        let panel = NSOpenPanel::openPanel(mtm);
        // The local proxy is an explicit role with actual nil as its default; remote system UI process/memory ownership is verified separately.
        guard
            .ledger
            .include("modal.open-panel-proxy", &panel)
            .map_err(NativeMarkdownModalError::Boundary)?;
        // The chooser explicitly uses Chinese task and confirmation labels rather than system-default wording.
        // Configures this modal's static labels only, without changing filenames, paths, or the user's system language.
        panel.setPrompt(Some(ns_string!("导入")));
        panel.setTitle(Some(if markdown_only {
            // Markdown import copies a workspace note, distinct from insertion as an attachment.
            ns_string!("导入 Markdown 文档")
        } else {
            // Attachments retain their file types for the existing Core validation/copy path.
            ns_string!("导入附件")
        }));
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(false);
        if markdown_only {
            // Preserves the extension filter; Core still checks descriptor/extension because UI filtering is not file authority.
            #[allow(deprecated)]
            panel.setAllowedFileTypes(Some(&NSArray::from_slice(&[
                ns_string!("md"),
                ns_string!("markdown"),
            ])));
        }
        let response = native_markdown_open_panel_response(panel.runModal());
        let candidate = response.and_then(|confirmed| {
            if !confirmed {
                // Cancellation reads no URL/path and produces no filesystem effect.
                return Ok(None);
            }
            let url = panel.URL().ok_or_else(|| {
                NativeMarkdownModalError::Boundary("文件选择器没有返回选中的文件".into())
            })?;
            if !url.isFileURL() {
                // A non-file scheme or missing path is a failure rather than ordinary cancellation.
                return Err(NativeMarkdownModalError::Boundary(
                    "文件选择器返回的内容不是本地文件".into(),
                ));
            }
            let path = url.path().ok_or_else(|| {
                NativeMarkdownModalError::Boundary("所选文件没有有效的本地路径".into())
            })?;
            if path.length() > 16_384 {
                // Bounds native input before Foundation creates its temporary UTF-8 representation; rejects the whole value without path truncation.
                return Err(NativeMarkdownModalError::InputTooLong);
            }
            autoreleasepool(|pool| {
                // SAFETY: path and the innermost pool cover this synchronous borrow and PathBuf construction, with no native setters.
                let path = unsafe { path.to_str(pool) };
                if path.len() > 16_384 {
                    // Checks UTF-8 bytes independently of UTF-16; preserves leading/trailing filename spaces without trimming.
                    return Err(NativeMarkdownModalError::InputTooLong);
                }
                Ok(Some(PathBuf::from(path)))
            })
        });
        // Clears only owned public configuration, without traversing remote windows, clearing unknown field history, or changing system settings.
        panel.setDirectoryURL(None);
        panel.setNameFieldStringValue(ns_string!(""));
        // Restores the shared panel's default configuration so later modal requests cannot inherit this task's labels.
        panel.setTitle(None);
        panel.setPrompt(None);
        #[allow(deprecated)]
        panel.setAllowedFileTypes(None);
        candidate
    });
    guard.complete(result)
}

/// Starts an explicit discard/reload after the caller obtained confirmation or saved a new draft.
fn replace_active_editor_from_disk(
    relative_path: String,
    original: &NativeMarkdownCurrentNoteIntent,
    revision: u64,
    saved_generation: Option<u64>,
) -> Result<(), String> {
    let controller = with_active_native_markdown_surface(|surface| {
        // Reload explicitly discards the confirmed draft. Save-as instead carries the generation
        // fixed by its real durable copy; neither a sidebar refresh nor search shutdown may renew it.
        let current = |surface: &NativeMarkdownWindowSurface| {
            original.matches(surface)
                && surface.controller.conflict_publication() == revision
                && saved_generation.is_none_or(|generation| {
                    surface
                        .editor
                        .as_ref()
                        .is_some_and(|editor| editor.document().borrow().generation() == generation)
                })
        };
        let changed = || {
            if saved_generation.is_some() {
                format!("草稿副本已保存到“{relative_path}”，但原笔记已变更，原草稿已保留")
            } else {
                "原冲突笔记或保存结果已变更，本次操作已取消".to_owned()
            }
        };
        if !current(surface) {
            return Err(changed());
        }
        surface.stop_search();
        if !current(surface) {
            return Err(changed());
        }
        let previous_relative_path = surface.current_note_relative_path.clone();
        // The actual resign/preflight path rechecks this same document and generation before
        // accepting cleanup. A late canonical edit keeps the original draft open for saving.
        let release_barrier = surface.destroy_current_editor().map_err(|error| {
            if saved_generation.is_some() {
                format!(
                    "草稿副本已保存到“{relative_path}”，但原编辑器未能结束，原草稿已保留：{error}"
                )
            } else {
                error
            }
        })?;
        surface.pending_note_switch = Some(PendingNativeMarkdownNoteSwitch {
            relative_path,
            previous_relative_path,
            release_barrier,
            fragment: None,
        });
        Ok::<_, String>(surface.controller.clone())
    })?;
    // Both replacement routes wait for actual old-graph destruction before acquiring destination disk content.
    schedule_native_markdown_note_switch_completion(&controller)
}

/// Toolbar and menu share one import: saves the original draft, preserves directory origin, then uses Core safe copy and normal note switching.
pub(super) fn import_native_markdown_note_to_directory(
    list: &NativeMarkdownSidebarTable,
    origin: &NativeMarkdownSidebarActionOrigin,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("导入笔记必须在主线程执行")?;
    preflight_native_markdown_modal()?;
    let NativeMarkdownSidebarActionOrigin::SelectedDirectory {
        relative_path: destination,
        ..
    } = origin
    else {
        unreachable!("笔记导入只消费真实浏览目录来源");
    };
    if !list.permits_origin(origin) {
        return Err("导入前所选目录已经变化，请重试".into());
    }
    let DocumentActionContext {
        session,
        controller,
        relative_path,
    } = active_action_context()?;
    with_active_native_markdown_surface(|surface| {
        // Import first commits the actual mode's original client, then uses the existing save entry; paginated input still requires completion without TextKit.
        surface.commit_marked_text(false)
    })?;
    ensure_action_is_current(&controller, relative_path.as_deref())?;
    with_active_native_markdown_surface(|surface| {
        if !list.origin_matches_surface(surface, origin) {
            return Err("导入前所选目录已经变化，请重试".into());
        }
        // Preserves save-before-import ordering; the sole save entry handles conflicts before any external file copy.
        surface.save_current_document()
    })?;
    if !list.permits_origin(origin) {
        return Err("保存期间所选目录或原笔记会话已经变化，请重新导入".into());
    }
    let Some(path) =
        choose_import_file(&controller, mtm, true).map_err(|error| error.to_string())?
    else {
        return Ok(()); // Chooser cancellation writes no directory or switches text; the already completed original save remains valid.
    };
    ensure_action_is_current(&controller, relative_path.as_deref())?;
    if !list.permits_origin(origin) {
        return Err("文件选择期间所选目录已经变化，请重试".into());
    }
    // Core owns confinement, actual file type, and no-overwrite copy; absence from the current page cannot change the real destination.
    let receipt = session
        .import_note(&path, destination)
        .map_err(|error| error.to_string())?;
    // Core already committed the file; refresh failure must report its successful path rather than invite retry creating another copy.
    with_active_native_markdown_surface(|surface| surface.refresh_workspace_sidebar(mtm)).map_err(
        |error| {
            format!(
                "笔记已导入到“{}”，但目录刷新未完成：{error}",
                receipt.relative_path
            )
        },
    )?;
    // Native refresh reentry may revoke the old window action, but cannot undo the completed disk import.
    ensure_action_is_current(&controller, relative_path.as_deref()).map_err(|error| {
        format!(
            "笔记已导入到“{}”，但原窗口动作未完成：{error}",
            receipt.relative_path
        )
    })?;
    // Later lifecycle save/open failure still reports the actual import outcome without replaying copy or removing the successful file.
    switch_native_markdown_note(&receipt.relative_path, None).map_err(|error| {
        format!(
            "笔记已导入到“{}”，但打开未完成：{error}",
            receipt.relative_path
        )
    })
}

/// Dispatches existing document actions; file failure preserves drafts, and import shares the header's actual browsing directory and flow.
pub(super) fn perform_native_markdown_workspace_action(action: isize) -> Result<(), String> {
    if action == 2 {
        // Menu and header import use the same actual browsing directory rather than inferring from the open note or defaulting to root.
        let (list, origin) = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active.as_ref().ok_or("笔记窗口已关闭")?;
            Ok::<_, String>((
                surface.sidebar_list.table.clone(),
                surface.sidebar_list.table.directory_origin(),
            ))
        })?;
        return import_native_markdown_note_to_directory(&list, &origin);
    }
    if matches!(action, 4 | 5) {
        // Direct internal calls also capture the original note, preventing generic actions from acquiring an Active target after confirmation.
        let controller = ACTIVE_NATIVE_MARKDOWN_SURFACE
            .with_borrow(|active| active.as_ref().map(|surface| surface.controller.clone()))
            .ok_or("笔记窗口已关闭")?;
        if let Some(intent) = NativeMarkdownCurrentNoteIntent::capture(&controller) {
            return intent.perform_resolution(action, controller.conflict_publication());
        }
        return Ok(());
    }
    if action == 6 {
        // Information is a read-only overlay; it cannot implicitly confirm IME or commit cells through generic file/format entry points.
        let controller = ACTIVE_NATIVE_MARKDOWN_SURFACE
            .with_borrow(|active| active.as_ref().map(|surface| surface.controller.clone()))
            .ok_or("笔记窗口已关闭")?;
        if let Some(intent) = NativeMarkdownCurrentNoteIntent::capture(&controller) {
            super::information_panel::toggle(&intent)?;
        }
        return Ok(());
    }
    let mtm = MainThreadMarker::new().ok_or("文档操作必须在主线程执行")?;
    // Rejects nested modal work before committing parent IME/table drafts or acquiring any native view.
    preflight_native_markdown_modal()?;
    let DocumentActionContext {
        session,
        controller,
        relative_path,
    } = active_action_context()?;
    // Submit any visible IME/table draft before reading document bytes or launching a modal action.
    with_active_native_markdown_surface(|surface| surface.commit_marked_text(false))?;
    match action {
        // Saving rechecks the authoritative revision; a conflict leaves the piece tree untouched.
        1 => with_active_native_markdown_surface(|surface| surface.save_current_document()),
        // A provisional confined copy commits only after its canonical link insertion is accepted.
        3 => {
            let note = relative_path.as_deref().ok_or("请先打开笔记，再导入附件")?;
            let Some(path) =
                choose_import_file(&controller, mtm, false).map_err(|error| error.to_string())?
            else {
                // Attachment cancellation leaves both text and the reserved asset tree untouched.
                return Ok(());
            };
            ensure_action_is_current(&controller, Some(note))?;
            let view = ACTIVE_NATIVE_MARKDOWN_SURFACE
                .with_borrow(|active| {
                    active
                        .as_ref()?
                        .editor
                        .as_ref()
                        .map(|editor| Rc::clone(&editor.text_view))
                })
                .ok_or("当前没有打开的笔记")?;
            insert_native_markdown_paged_attachment(&session, note, &path, &view, false)
                .map_err(|cause| cause.to_string())
        }
        // Ordinary Find keeps replacement collapsed; its separate menu action shares this session's existing bar and field history.
        7 => show_native_markdown_find_bar(false),
        8 => show_native_markdown_find_bar(true),
        // Folding changes only the current paginated client's presentation without committing characters or rewriting history.
        20 | 21 => {
            let view = ACTIVE_NATIVE_MARKDOWN_SURFACE
                .with_borrow(|active| {
                    active
                        .as_ref()?
                        .editor
                        .as_ref()
                        .map(|editor| Rc::clone(&editor.text_view))
                })
                .ok_or("当前没有打开的笔记")?;
            if action == 20 {
                view.toggle_current_source_fold()
            } else {
                view.expand_source_folds()
            }
            .map_err(|cause| cause.to_string())
        }
        // The normal button entry already fixes intent; direct calls clear only this current query without committing or saving text.
        9 => return_native_markdown_workspace_to_directory(
            std::ptr::from_ref(&*controller).addr(),
            controller.workspace_query_intent(),
        ),
        // Paging reads another bounded metadata chunk without replacing the editor or preloading bodies.
        10 => advance_native_markdown_sidebar_page(mtm, &controller),
        // Menu and dedicated table action share one commit, explicit first-cell selection, and RequiredRow activation without an obsolete formatter branch.
        118 => insert_table_into_active_native_markdown_editor(),
        // Other formatting uses the same captured selection and canonical transaction.
        100..=112 | 115..=116 | 22 | 23 => {
            let intent = NativeMarkdownWorkspacePagedEditIntent::capture(&controller)
                .ok_or("格式操作的原文档或选择已经改变")?;
            intent.perform(action)
        }
        // Unknown tags cannot trigger arbitrary selectors or mutate document state.
        _ => {
            controller.clear_status();
            Ok(())
        }
    }
}

/// Connects the same provisional attachment to actual Input commit; later display failure preserves a file already referenced by canonical text.
fn insert_native_markdown_paged_attachment(
    session: &MarkdownSessionAccess,
    note: &str,
    path: &Path,
    view: &super::paged_text_view::NativeMarkdownPagedTextView,
    interpreted: bool,
) -> Result<(), super::paged_text_view::NativeMarkdownPagedTextViewError> {
    use super::paged_text_view::NativeMarkdownPagedTextViewError as ViewError;
    with_active_native_markdown_surface(|surface| {
        let source = surface.editor.as_ref().ok_or("原源码文稿已经关闭")?;
        if !std::ptr::eq(&**source.text_view, view)
            || surface.current_note_relative_path.as_deref() != Some(note)
        {
            return Err("插入附件前原源码文稿已经改变".into());
        }
        if native_markdown_operation_is_busy(
            surface.pending_note_switch.is_some(),
            surface.pending_file_mutation.is_some(),
        ) {
            return Err("另一项文档操作仍在完成中".into());
        }
        if !interpreted {
            surface.commit_marked_text(false)?; // Queue-head input already completed composition on the original NSTextInputClient stack; it must not be committed again as an external close precondition.
        }
        let before = match view.input_state() {
            Ok(before) => before,
            Err(cause) => return Ok(Err(cause)), // Preserves the typed primary Input/Source failure rather than downgrading it to external-file rejection.
        };
        let provisional = session
            .prepare_attachment_import(note, path)
            .map_err(|cause| cause.to_string())?;
        let text = provisional.receipt().markdown_insertion.clone(); // Copies only this bounded filename/link, never original document or attachment contents.
        let mut provisional = Some(provisional);
        let inserted = view.insert_attachment_link(before, &text, interpreted, || {
            let _ = provisional
                .take()
                .expect("原Core提交只确认一次同一附件")
                .commit();
        });
        let result = match (inserted, provisional) {
            (Ok(()), _) => Ok(()),
            (Err(cause), None) => Err(cause), // Core already committed; a subsequent native presentation failure cannot delete the attachment.
            (Err(cause), Some(provisional)) => match provisional.rollback() {
                Ok(()) => Err(cause),
                Err(cleanup) => Err(ViewError::AttachmentRollback {
                    cause: Box::new(cause),
                    cleanup: cleanup.to_string(),
                }),
            },
        };
        Ok(result) // Separates external-file rejection from actual Core/native consequence errors, retaining the latter's typed cause.
    })
    .map_err(ViewError::AttachmentImport)?
}

/// The original paginated queue head performs confined copies one by one, rechecking the same Source view and preserving committed files after later failure.
pub(super) fn insert_native_markdown_paged_attachments(
    view: &super::paged_text_view::NativeMarkdownPagedTextView,
    paths: &[PathBuf],
) -> Result<(), super::paged_text_view::NativeMarkdownPagedTextViewError> {
    use super::paged_text_view::NativeMarkdownPagedTextViewError as ViewError;
    let DocumentActionContext {
        session,
        controller: _,
        relative_path,
    } = active_action_context().map_err(ViewError::AttachmentImport)?;
    let note =
        relative_path.ok_or_else(|| ViewError::AttachmentImport("当前没有打开的笔记".into()))?;
    for path in paths {
        // At most sixteen bounded paths are admitted; Core streams each file and inserts only its bounded link, never copying the entire note.
        insert_native_markdown_paged_attachment(&session, &note, path, view, true)?;
    }
    Ok(())
}

/// Reports after the original queue head returns its surface lease; old file requests cannot overwrite a successor note's status.
pub(super) fn present_native_markdown_paged_attachment_error(
    view: &super::paged_text_view::NativeMarkdownPagedTextView,
    message: &str,
) {
    let current = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        active.as_ref().is_some_and(|surface| {
            surface
                .editor
                .as_ref()
                .is_some_and(|source| std::ptr::eq(&**source.text_view, view))
                && native_markdown_controller_has_foreground_authority(&surface.controller)
        })
    });
    if current {
        present_native_markdown_error(message); // Shows only this current original document's bounded error without closing it or clearing unsaved text.
    }
}

/// Maps menu tags only to existing domain commands, shared by document text and the actual editable cell.
pub(super) fn native_markdown_format_action(action: isize) -> Result<MarkdownFormatAction, String> {
    Ok(match action {
        // Heading 1 normalizes the current line's existing heading prefix.
        100 => MarkdownFormatAction::Heading1,
        // Heading 2 replaces heading markers while preserving selected line text.
        101 => MarkdownFormatAction::Heading2,
        // Heading 3 uses the same atomic prefix replacement with its own marker width.
        102 => MarkdownFormatAction::Heading3,
        // Bold preserves selected source content; an empty selection uses the established editable placeholder.
        103 => MarkdownFormatAction::Bold,
        // Italic wraps the actual selection or empty-selection placeholder through the same domain formatter.
        104 => MarkdownFormatAction::Italic,
        // Underline writes canonical HTML without a separate native formatted-text model.
        105 => MarkdownFormatAction::Underline,
        // Strikethrough retains paired Markdown delimiter semantics.
        106 => MarkdownFormatAction::Strikethrough,
        // Inline code preserves selected source bytes inside the domain formatter's code delimiters.
        107 => MarkdownFormatAction::InlineCode,
        // Link formatting selects the editable label under the existing contract, leaving the URL template outside that selection.
        108 => MarkdownFormatAction::Link,
        // Blockquote prefixes each intersecting line under the formatter's existing endpoint policy.
        109 => MarkdownFormatAction::Blockquote,
        // Bullet lists modify selected line prefixes in one undoable canonical transaction.
        110 => MarkdownFormatAction::BulletList,
        // Ordered lists retain the domain formatter's literal per-line numbering.
        111 => MarkdownFormatAction::OrderedList,
        // Task lists insert unchecked source checkboxes on selected lines.
        112 => MarkdownFormatAction::TaskList,
        // Divider inserts the established template at the actual head without deleting the original selection.
        115 => MarkdownFormatAction::Divider,
        // Code fences preserve the established placeholder selection and surrounding newlines.
        116 => MarkdownFormatAction::CodeBlock,
        // An unknown tag grants no document mutation authority.
        _ => return Err("无法识别的格式命令".into()),
    })
}

/// Reads only explicit file-URL pasteboard items, with a fixed batch limit and no cache.
pub(super) fn native_markdown_pasteboard_file_paths(
    pasteboard: &NSPasteboard,
) -> Result<Vec<PathBuf>, String> {
    // SAFETY: AppKit's exported file-URL type has process lifetime; type queries materialize no path or document text.
    let file_type = unsafe { objc2_app_kit::NSPasteboardTypeFileURL };
    if pasteboard
        .availableTypeFromArray(&NSArray::from_slice(&[file_type]))
        .is_none()
    {
        // Ordinary text follows standard paste rather than triggering attachment batch policy for multiple text items.
        return Ok(Vec::new());
    }
    let Some(items) = pasteboard.pasteboardItems() else {
        // A declared file type without actual items supplies no fabricated paths.
        return Ok(Vec::new());
    };
    if items.count() > 16 {
        // Rejects the whole over-budget batch instead of silently importing only its first sixteen files.
        return Err("每次粘贴或拖入最多导入 16 个文件".into());
    }
    let mut paths = Vec::new();
    for index in 0..items.count() {
        // All getters run within the bounded batch; native URL/path lengths are checked before Rust copies.
        let item = items.objectAtIndex(index);
        let Some(value) = item.stringForType(file_type) else {
            // A non-file item in a mixed batch grants no attachment authority.
            continue;
        };
        if value.length() > 16_384 {
            return Err("附件地址超过导入长度上限".into());
        }
        let url = NSURL::URLWithString(&value).ok_or("附件地址无效")?;
        if !url.isFileURL() {
            return Err("附件地址必须是本地文件地址".into()); // A network URL path cannot authorize reading an external local file.
        }
        if let Some(host) = url.host()
            && host.length() != 0
            && !host.to_string().eq_ignore_ascii_case("localhost")
        {
            return Err("附件地址不能包含远端文件主机".into()); // Preserves file://server authority semantics instead of reading a similarly named local path.
        }
        let path = url.path().ok_or("附件没有本地路径")?;
        if path.length() > 4_096 {
            return Err("附件路径超过导入长度上限".into());
        }
        paths.push(PathBuf::from(path.to_string()));
    }
    Ok(paths)
}

/// Bounds pending attachment payload before the native adapter admits a queued request.
pub(super) fn admit_attachment_paths(pending: usize, paths: &[PathBuf]) -> Result<(), String> {
    if pending >= 4 {
        // Neither discards older requests nor puts new paths on an unbounded dispatch queue.
        return Err("已有 4 项附件导入正在等待，请完成后重试".into());
    }
    if paths.is_empty()
        || paths.len() > 16
        || paths.iter().any(|path| path.as_os_str().len() > 16_384)
    {
        // Bounds both batch size and each path; rejects the entire operation without silently accepting a prefix.
        return Err("这批附件超过待处理导入的容量上限".into());
    }
    Ok(())
}

/// Hides native find UI before Escape is allowed to close the document's foreground session.
pub(super) fn dismiss_active_native_markdown_find_bar() -> bool {
    let visible = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        active
            .as_ref()
            .and_then(|surface| surface.editor.as_ref())
            .and_then(|editor| editor.find_bar().borrow().clone())
            .is_some_and(|bar| bar.is_visible())
    });
    if !visible {
        // Without a transient Find owner, leaves ordinary Escape policy to the caller.
        return false;
    }
    if let Err(error) = perform_native_markdown_find_action(MarkdownFindAction::Hide) {
        // Resignation failure still consumes Escape and preserves the field; it must not fall through to closing the window.
        present_native_markdown_error(&error);
    }
    true
}

/// Exports current unsaved canonical text only for an explicit foreground copy action.
pub(super) fn copy_active_markdown_document_to_general_pasteboard() -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "Markdown 全文复制必须在 AppKit 主线程执行".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // Accepted closing has already consumed foreground copy authority; do not replace the
        // system pasteboard from a delayed action while the old document is being released.
        return Err("原生 Markdown 窗口正在关闭".to_owned());
    }

    autoreleasepool(|_| {
        with_active_native_markdown_surface(|surface| {
            let pasteboard = NSPasteboard::generalPasteboard();
            surface.copy_current_document_to_pasteboard(&pasteboard)
        })
    })
}

/// Routes an explicit table command to the sole editable foreground document.
pub(super) fn insert_table_into_active_native_markdown_editor() -> Result<(), String> {
    MainThreadMarker::new().ok_or_else(|| "Markdown 表格只能在 AppKit 主线程插入".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        return Err("原生 Markdown 窗口正在关闭".to_owned());
    }
    with_active_native_markdown_surface(|surface| surface.insert_default_table())
}

/// Runs only through the main-queue signal gate, then checks the original owner, epochs, and exact mailbox identity before consumption.
pub(super) fn deliver_native_markdown_search_result(
    delivery: WorkspaceDelivery<NativeMarkdownSearchResult>,
) {
    let WorkspaceDelivery {
        owner: owner_identity,
        generation,
        intent: accepted_intent,
        mailbox,
    } = delivery;

    let mtm = MainThreadMarker::new().expect("dispatch main queue establishes AppKit thread");
    let Some(mailbox) = mailbox.upgrade() else {
        // An expired cancelled mailbox cannot access a new session or reconstruct the old query.
        return;
    };
    let _ = with_active_native_markdown_surface(|surface| {
        if std::ptr::from_ref(&*surface.controller).addr() != owner_identity
            || !surface.search_delivery_is_current(generation, accepted_intent)
            || !surface
                .search_results
                .as_ref()
                .is_some_and(|current| std::sync::Arc::ptr_eq(current, &mailbox))
        {
            // Even reused addresses/epochs cannot borrow another operation's mailbox to install or report old results.
            // Checks are pure within the lease; cancellation touches only this notification's mailbox.
            mailbox.cancel();
            return Ok(());
        }
        let Some(results) = mailbox.take() else {
            // A cancelled, poisoned, or consumed mailbox grants no further UI publication.
            return Ok(());
        };
        match results {
            Ok(results) => {
                // Installs only bounded summaries for the current query, preserving the canonical document owner.
                if let Err(error) =
                    surface.install_search_results(results, generation, accepted_intent, mtm)
                    && surface.search_delivery_is_current(generation, accepted_intent)
                {
                    // If native installation reenters newer input, the old error cannot overwrite that input's feedback.
                    surface.controller.present_error(&error);
                }
            }
            Err(error) => {
                // Reports errors only in current foreground state, without retaining paths or error text in delayed closures.
                surface.controller.present_error(&error.to_string());
            }
        }
        Ok(())
    });
}

#[cfg(test)]
mod resource_budget_tests {
    use super::submitted_workspace_query_from_utf16;

    #[test]
    #[cfg(feature = "native-paged-production")]
    /// Relative-note normalization permits parent directories within the vault and rejects root escape or absolute paths before save/teardown.
    fn relative_note_links_keep_the_origin_directory_and_vault_boundary() {
        let resolve = super::relative_note_link_path;
        assert_eq!(
            resolve("guide/start.md", "next.md").unwrap(),
            "guide/next.md"
        );
        assert_eq!(
            resolve("guide/start.md", "../other.md").unwrap(),
            "other.md"
        );
        assert_eq!(
            resolve("guide/start.md", "./子目录/中文 笔记.md").unwrap(),
            "guide/子目录/中文 笔记.md"
        );
        assert_eq!(resolve("start.md", "nested/../next.md").unwrap(), "next.md");
        assert!(resolve("start.md", "../outside.md").is_err());
        assert!(resolve("guide/start.md", "../../outside.md").is_err());
        assert!(resolve("guide/start.md", "/absolute.md").is_err());
        assert!(resolve("start.md", "").is_err());
    }

    #[test]
    fn query_scalar_limit_preserves_pairs_and_rejects_malformed_native_utf16() {
        for count in [127, 128] {
            // Supplementary-plane scalars use two UTF-16 units; in-budget queries must preserve every character, including the final one.
            let query = "😀".repeat(count);
            assert_eq!(
                submitted_workspace_query_from_utf16(&query.encode_utf16().collect::<Vec<_>>())
                    .unwrap(),
                query
            );
        }
        assert!(
            submitted_workspace_query_from_utf16(
                &"a".repeat(129).encode_utf16().collect::<Vec<_>>()
            )
            .is_err()
        );
        assert!(
            submitted_workspace_query_from_utf16(
                &"😀".repeat(129).encode_utf16().collect::<Vec<_>>()
            )
            .is_err()
        );
        assert!(submitted_workspace_query_from_utf16(&[0xd800]).is_err());
        assert!(submitted_workspace_query_from_utf16(&[0xdc00]).is_err());
        // Combining marks count as scalars; visible grapheme count cannot substitute for Core's search budget.
        let combining = format!("{}e\u{301}", "a".repeat(126));
        assert!(
            submitted_workspace_query_from_utf16(&combining.encode_utf16().collect::<Vec<_>>())
                .is_ok()
        );
        let too_many = format!("{}e\u{301}", "a".repeat(127));
        assert!(
            submitted_workspace_query_from_utf16(&too_many.encode_utf16().collect::<Vec<_>>())
                .is_err()
        );
    }
}
