//! Connects foreground workspace commands to the confined core and TextKit adapter.
//!
//! Modal panels never run under a surface or document borrow. Search owns one cancellable
//! foreground worker; queued completions carry a generation and a weak controller only.

use super::*;
use objc2_app_kit::{NSAccessibility, NSOpenPanel};
use std::path::PathBuf;

/// Installs explicitly owned command controls; their weak targets are cleared on close.
pub(super) fn install_native_markdown_workspace_controls(
    controller: &NativeMarkdownController,
    content: &NSView,
    mtm: MainThreadMarker,
) {
    // Keep title/body search in the sidebar and require Return to submit a query.
    let query = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(
            NSPoint::new(
                18.0,
                NATIVE_MARKDOWN_HEIGHT - NATIVE_MARKDOWN_HEADER_HEIGHT - 70.0,
            ),
            NSSize::new(212.0, 24.0),
        ),
    );
    query.setPlaceholderString(Some(ns_string!("Search notes · Return")));
    query.setAccessibilityLabel(Some(ns_string!("Search note titles and contents")));
    query.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
    // SAFETY: this selector belongs to the retained main-thread controller; NSControl borrows it.
    unsafe {
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
    actions.addItemWithTitle(ns_string!("Document"));
    let commands = [
        (1, "Save"),
        (2, "Import Markdown…"),
        (3, "Import attachment…"),
        (4, "Reload from disk…"),
        (5, "Save draft as new note…"),
        (6, "Document information"),
        (7, "Find…"),
        (8, "Find and replace…"),
        (20, "Toggle current block fold"),
        (21, "Expand all folded blocks"),
        (100, "Heading 1"),
        (101, "Heading 2"),
        (102, "Heading 3"),
        (103, "Bold"),
        (104, "Italic"),
        (105, "Underline"),
        (106, "Strikethrough"),
        (107, "Inline code"),
        (108, "Link"),
        (109, "Quote"),
        (110, "Bullet list"),
        (111, "Numbered list"),
        (112, "Task"),
        (113, "Formula"),
        (114, "Superscript"),
        (115, "Horizontal rule"),
        (116, "Code block"),
        (117, "Formula block"),
        (118, "Table"),
    ];
    // Menu tags encode only this controller's finite commands; the surface retains all controls.
    for (tag, title) in commands {
        actions.addItemWithTitle(&NSString::from_str(title));
        if let Some(item) = actions.lastItem() {
            // A newly added item exists; set its action identity before exposing the menu.
            item.setTag(tag);
        }
    }
    actions.setAccessibilityLabel(Some(ns_string!("Markdown document actions and formatting")));
    actions.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
    // SAFETY: the target survives all borrowed actions until detach_controls clears the relation.
    unsafe {
        actions.setTarget(Some(controller));
        actions.setAction(Some(sel!(performMarkdownWorkspaceAction:)));
    }
    content.addSubview(&actions);
    // Explicit tree navigation keeps a constant number of row controls even for large vaults.
    // SAFETY: both fixed selectors belong to the retained main-thread controller.
    let refresh = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Refresh"),
            Some(controller),
            Some(sel!(refreshMarkdownWorkspace:)),
            mtm,
        )
    };
    refresh.setFrame(NSRect::new(
        NSPoint::new(18.0, 6.0),
        NSSize::new(98.0, 24.0),
    ));
    refresh.setAccessibilityLabel(Some(ns_string!(
        "Refresh workspace and return to first page"
    )));
    content.addSubview(&refresh);
    // SAFETY: this fixed selector belongs to the same retained controller and is detached on close.
    let next = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("More notes"),
            Some(controller),
            Some(sel!(nextMarkdownWorkspacePage:)),
            mtm,
        )
    };
    next.setFrame(NSRect::new(
        NSPoint::new(126.0, 6.0),
        NSSize::new(104.0, 24.0),
    ));
    next.setAccessibilityLabel(Some(ns_string!("Show the next bounded workspace page")));
    content.addSubview(&next);
    assert!(
        controller
            .ivars()
            .next_sidebar_page
            .set(next.clone())
            .is_ok()
    );
    let controls = vec![
        query.into_super(),
        actions.into_super().into_super(),
        refresh.into_super(),
        next.into_super(),
    ];
    // Controls install once during surface construction; no second ownership root is created.
    assert!(controller.ivars().workspace_controls.set(controls).is_ok());
}

impl NativeMarkdownWindowSurface {
    /// Cancels and joins the sole active scan before switching queries or tearing down the session.
    pub(super) fn stop_search(&mut self) {
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        self.session.cancel_search();
        // Core scanners check cancellation between bounded chunks; no worker may outlive teardown.
        if let Some(worker) = self.search_worker.take() {
            let _ = worker.join();
        }
    }

    /// Replaces only bounded sidebar metadata; the selected document remains the sole body owner.
    fn install_search_results(
        &mut self,
        results: Vec<omni_markdown_core::MarkdownSearchResult>,
        mtm: MainThreadMarker,
    ) -> Result<(), String> {
        let content = self
            .panel
            .contentView()
            .ok_or("Markdown content view is unavailable")?;
        // Remove targets before releasing old buttons so delayed menu work cannot retain them.
        for button in self.sidebar_buttons.drain(..) {
            // SAFETY: the main-thread surface still owns each button and its target during detachment.
            unsafe {
                button.setTarget(None);
                button.setAction(None);
            }
            button.removeFromSuperview();
        }
        // Search owns a separate projection; release its obsolete tree cursor before publishing results.
        self.sidebar_pager.take();
        self.sidebar_has_more = false;
        self.controller.set_next_sidebar_page_available(false);
        self.notes = results
            .iter()
            .map(|result| MarkdownNoteSummary {
                // Search carries no body or persistent index, only the identities needed to open notes.
                relative_path: result.relative_path.clone(),
                title: result.title.clone(),
                byte_count: 0,
                modified_at_ms: 0,
            })
            .collect();
        self.sidebar_rows = results
            .into_iter()
            .map(|result| NativeMarkdownSidebarRow {
                kind: NativeMarkdownSidebarEntryKind::Note,
                relative_path: result.relative_path,
                title: result.snippet.map_or(result.title.clone(), |snippet| {
                    // Bound visible snippets independently of the already bounded core search results.
                    format!(
                        "{} · {}",
                        result.title,
                        snippet.chars().take(60).collect::<String>()
                    )
                }),
                depth: 1,
                is_collapsed: false,
            })
            .collect();
        self.sidebar_buttons = native_markdown_sidebar_buttons(
            &self.sidebar_rows,
            self.current_note_relative_path.as_deref(),
            &content,
            mtm,
        );
        Ok(())
    }
}

/// Starts an on-demand scan and ignores completions from old queries or closed surfaces.
pub(super) fn search_native_markdown_workspace(query: String) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("Workspace search requires the AppKit main thread")?;
    with_active_native_markdown_surface(|surface| {
        surface.stop_search();
        // Empty search returns to metadata-only navigation and leaves no scan worker behind.
        if query.trim().is_empty() {
            return surface.refresh_workspace_sidebar(mtm);
        }
        surface.search_cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled = std::sync::Arc::clone(&surface.search_cancelled);
        let generation = surface.search_generation;
        let session = surface.session.clone();
        let controller = MainThreadBound::new(Weak::from_retained(&surface.controller), mtm);
        let worker = std::thread::Builder::new()
            .name("omni-md-search".into())
            .spawn(move || {
                // The worker touches only the portable core and releases each file mapping before the next.
                let results = session.search_notes_cancellable(
                    &query,
                    MAX_VISIBLE_NATIVE_MARKDOWN_WORKSPACE_ENTRIES,
                    &cancelled,
                );
                dispatch2::DispatchQueue::main().exec_async(move || {
                    let mtm = MainThreadMarker::new()
                        .expect("dispatch main queue establishes AppKit thread");
                    // A weak target does not keep the old surface alive until this completion runs.
                    let Some(controller) = controller.get(mtm).load() else {
                        return;
                    };
                    let _ = with_active_native_markdown_surface(|surface| {
                        // Both controller identity and generation must match: reopened sessions reject old work.
                        if !std::ptr::eq(&*surface.controller, &*controller)
                            || surface.search_generation != generation
                        {
                            return Ok(());
                        }
                        match results {
                            // Successful metadata is installed only for the currently requested query.
                            Ok(results) => {
                                // A failed native projection reports status while preserving the live editor.
                                if let Err(error) = surface.install_search_results(results, mtm) {
                                    surface.controller.present_error(&error);
                                }
                            }
                            // A failed scan leaves the live document and existing sidebar usable.
                            Err(error) => surface.controller.present_error(&error.to_string()),
                        }
                        Ok(())
                    });
                });
            })
            .map_err(|error| format!("Cannot start workspace search: {error}"))?;
        surface.search_worker = Some(worker);
        Ok(())
    })
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
        let surface = active.as_ref().ok_or("Markdown is closed")?;
        // A pending handoff owns its release barrier and rejects overlapping document work.
        if native_markdown_operation_is_busy(
            surface.pending_note_switch.is_some(),
            surface.pending_file_mutation.is_some(),
        ) {
            return Err("Another document operation is finishing".into());
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
        let surface = active.as_ref().ok_or("Markdown is closed")?;
        // Reopening a plugin reuses no controller identity, even when the same note path is selected.
        if !std::ptr::eq(&*surface.controller, controller)
            || surface.current_note_relative_path.as_deref() != path
        {
            return Err("The document changed while the dialog was open; retry the action".into());
        }
        Ok(())
    })
}

/// Retains a view only around an immediate text transaction, never around a modal dialog.
pub(super) fn active_document_view() -> Result<Retained<PieceBackedMarkdownTextView>, String> {
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        let surface = active.as_ref().ok_or("Markdown is closed")?;
        surface
            .editor
            .as_ref()
            .map(|editor| editor.text_view.clone())
            .ok_or_else(|| "No note is open".into())
    })
}

/// Presents one external-file chooser; cancellation is an ordinary no-op.
fn choose_import_file(mtm: MainThreadMarker, markdown_only: bool) -> Option<PathBuf> {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    // Narrow Markdown selection before the core repeats extension and descriptor validation.
    if markdown_only {
        #[allow(deprecated)]
        panel.setAllowedFileTypes(Some(&NSArray::from_slice(&[
            ns_string!("md"),
            ns_string!("markdown"),
        ])));
    }
    // NSModalResponseOK is 1; every other response preserves the existing editor unchanged.
    if panel.runModal() != 1 {
        return None;
    }
    panel
        .URL()?
        .path()
        .map(|path| PathBuf::from(path.to_string()))
}

/// Starts an explicit discard/reload after the caller obtained confirmation or saved a new draft.
fn replace_active_editor_from_disk(relative_path: String) -> Result<(), String> {
    let controller = with_active_native_markdown_surface(|surface| {
        surface.stop_search();
        let previous_relative_path = surface.current_note_relative_path.clone();
        let release_barrier = surface.destroy_current_editor()?;
        surface.pending_note_switch = Some(PendingNativeMarkdownNoteSwitch {
            relative_path,
            previous_relative_path,
            release_barrier,
        });
        Ok::<_, String>(surface.controller.clone())
    })?;
    // The confirmed reload waits for actual old-graph destruction before acquiring authoritative disk content.
    schedule_native_markdown_note_switch_completion(&controller)
}

/// Routes finite native document actions, preserving unsaved content on every failed operation.
pub(super) fn perform_native_markdown_workspace_action(action: isize) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("Document actions require the main thread")?;
    let DocumentActionContext {
        session,
        controller,
        relative_path,
    } = active_action_context()?;
    // Submit any visible IME/table draft before reading document bytes or launching a modal action.
    if let Ok(view) = active_document_view() {
        view.commit_active_table_cell()?;
        view.commit_composition_before_release()?;
    }
    match action {
        // Saving rechecks the authoritative revision; a conflict leaves the piece tree untouched.
        1 => with_active_native_markdown_surface(|surface| surface.save_current_document()),
        // Note import is collision-safe in the core; the selected note changes only after persistence.
        2 => {
            let Some(path) = choose_import_file(mtm, true) else {
                // Cancelling the external chooser performs no vault mutation or note switch.
                return Ok(());
            };
            ensure_action_is_current(&controller, relative_path.as_deref())?;
            let destination = relative_path
                .as_deref()
                .map(native_markdown_note_parent)
                .unwrap_or_default();
            let receipt = session
                .import_note(&path, &destination)
                .map_err(|error| error.to_string())?;
            with_active_native_markdown_surface(|surface| surface.refresh_workspace_sidebar(mtm))?;
            switch_native_markdown_note(&receipt.relative_path)
        }
        // A provisional confined copy commits only after its canonical link insertion is accepted.
        3 => {
            let note = relative_path
                .as_deref()
                .ok_or("Open a note before importing an attachment")?;
            let Some(path) = choose_import_file(mtm, false) else {
                // Attachment cancellation leaves both text and the reserved asset tree untouched.
                return Ok(());
            };
            ensure_action_is_current(&controller, Some(note))?;
            let view = active_document_view()?;
            insert_native_markdown_attachment(&session, note, &path, &view)
        }
        // Reload is destructive only after explicit confirmation; cancellation preserves the full live draft.
        4 => {
            let path = relative_path.ok_or("No note is open")?;
            let alert = NSAlert::new(mtm);
            alert.setMessageText(ns_string!("Reload from disk?"));
            alert.setInformativeText(ns_string!("Unsaved changes in this editor will be discarded. External disk changes are preserved."));
            alert.addButtonWithTitle(ns_string!("Reload"));
            assign_escape_to_native_markdown_alert_cancel(
                &alert.addButtonWithTitle(ns_string!("Cancel")),
            );
            if alert.runModal() != NSAlertFirstButtonReturn {
                // Only the explicit Reload button authorizes discarding this unsaved draft.
                return Ok(());
            }
            ensure_action_is_current(&controller, Some(&path))?;
            replace_active_editor_from_disk(path)
        }
        // Preserve a conflicted draft as a new, exclusively created note before releasing the old buffer.
        5 => {
            let path = relative_path.ok_or("No note is open")?;
            let title = format!(
                "{} draft",
                native_markdown_path_name(&path).trim_end_matches(".md")
            );
            let Some(title) = run_native_markdown_name_dialog(
                "Save draft as new note",
                "The original file will remain unchanged.",
                &title,
                "Note title",
                "Save new note",
                mtm,
            ) else {
                // Name-dialog cancellation preserves the conflicted draft and authoritative original file.
                return Ok(());
            };
            ensure_action_is_current(&controller, Some(&path))?;
            let receipt = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                let surface = active.as_ref().ok_or("Markdown is closed")?;
                let editor = surface.editor.as_ref().ok_or("No note is open")?;
                session
                    .save_document_buffer_as_new(
                        &editor.document().borrow(),
                        &native_markdown_note_parent(&path),
                        &title,
                    )
                    .map_err(|error| error.to_string())
            })?;
            with_active_native_markdown_surface(|surface| surface.refresh_workspace_sidebar(mtm))?;
            replace_active_editor_from_disk(receipt.relative_path)
        }
        // Piece-tree aggregates provide counts in O(1); metadata is read separately without flattening text.
        6 => {
            let information = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                let surface = active.as_ref().ok_or("Markdown is closed")?;
                let editor = surface.editor.as_ref().ok_or("No note is open")?;
                let document = editor.document().borrow();
                Ok::<_, String>(format!(
                    "Characters: {}\nWords: {}\nLines: {}\nBytes: {}\nModified: {}",
                    document.character_count(),
                    document.word_count(),
                    document.line_count(),
                    document.byte_len(),
                    document
                        .path()
                        .metadata()
                        .and_then(|metadata| metadata.modified())
                        .map(|time| format!("{time:?}"))
                        .unwrap_or_else(|_| "unavailable".into())
                ))
            })?;
            let alert = NSAlert::new(mtm);
            alert.setMessageText(ns_string!("Document information"));
            alert.setInformativeText(&NSString::from_str(&information));
            alert.addButtonWithTitle(ns_string!("OK"));
            alert.runModal();
            Ok(())
        }
        // The public TextKit finder supplies native case modes, next/previous and replace-all semantics.
        7 | 8 => {
            let view = active_document_view()?;
            let item = NSMenuItem::new(mtm);
            let finder_action = if action == 7 {
                // NSTextFinderActionShowFindInterface opens native next/previous and case controls.
                1
            } else {
                // NSTextFinderActionShowReplaceInterface exposes native replacement controls as well.
                12
            };
            item.setTag(finder_action);
            // SAFETY: NSTextView implements the standard responder action; the sender supplies its finite tag.
            unsafe {
                view.performTextFinderAction(Some(&item));
            }
            Ok(())
        }
        // The native menu exposes folding to keyboard and accessibility clients as well as gutter clicks.
        20 => {
            let view = active_document_view()?;
            let offset = view.selectedRange().location;
            let document = view
                .ivars()
                .text_storage
                .load()
                .and_then(|storage| storage.document())
                .ok_or("The editor is closed")?;
            let line = {
                // Resolve canonical source position before invoking a native fold/layout transaction.
                let document = document.borrow();
                document
                    .byte_offset_for_utf16(offset)
                    .and_then(|byte| document.line_for_byte(byte))
                    .map_err(|error| error.to_string())?
            };
            view.toggle_source_fold(line)
        }
        // Expansion is view-local and never touches persistence, history, or document generation.
        21 => {
            active_document_view()?.unfold_source();
            Ok(())
        }
        // Refresh returns to the first metadata page and preserves the current live note.
        9 => with_active_native_markdown_surface(|surface| surface.refresh_workspace_sidebar(mtm)),
        // Paging reads another bounded metadata chunk without replacing the editor or preloading bodies.
        10 => with_active_native_markdown_surface(|surface| surface.install_next_sidebar_page(mtm)),
        // Formatting edits the same selected piece-tree range through the native input transaction.
        100..=118 => format_active_markdown_selection(action, active_document_view()?),
        // Unknown tags cannot trigger arbitrary selectors or mutate document state.
        _ => {
            controller.clear_status();
            Ok(())
        }
    }
}

/// Couple one new confined asset to a positively acknowledged canonical link transaction.
fn insert_native_markdown_attachment(
    session: &MarkdownSessionAccess,
    note: &str,
    path: &Path,
    view: &PieceBackedMarkdownTextView,
) -> Result<(), String> {
    with_active_native_markdown_surface(|surface| {
        // Native reentry cannot switch/save/close while the copied asset and canonical text are between commit points.
        let current_view = surface.editor.as_ref().ok_or("No note is open")?;
        if !std::ptr::eq(&*current_view.text_view, view)
            || surface.current_note_relative_path.as_deref() != Some(note)
        {
            // A retained old view or dialog context has no authority to import into a replacement document.
            return Err("The editor changed before attachment insertion".into());
        }
        if native_markdown_operation_is_busy(
            surface.pending_note_switch.is_some(),
            surface.pending_file_mutation.is_some(),
        ) {
            // An accepted handoff owns the last persistence boundary and forbids further asset/text mutations.
            return Err("Another document operation is finishing".into());
        }
        let storage = view
            .ivars()
            .text_storage
            .load()
            .ok_or("The editor is closed")?;
        // Expand view-local folds before source insertion so TextKit receives the full canonical interval.
        view.unfold_source();
        let range = view.selectedRange();
        let pending = session
            .prepare_attachment_import(note, path)
            .map_err(|error| error.to_string())?;
        let result =
            storage.apply_explicit_replacement(range, &pending.receipt().markdown_insertion);
        match result {
            // The source/history transaction succeeded; retain the asset before publishing native caret changes.
            Ok(edit) => {
                // The link is already canonical; no additional asset metadata needs to persist in the UI.
                let _ = pending.commit();
                view.setSelectedRange(NSRange::new(edit.selection_after.head_utf16, 0));
                view.schedule_visible_table_layout_update();
                Ok(())
            }
            // Text rejection leaves no new history revision; explicitly roll back the still-provisional asset.
            Err(error) => match pending.rollback() {
                // Verified cleanup removes only the unchanged newly created attachment, preserving the live draft.
                Ok(()) => Err(error),
                // A detected external replacement/change must be preserved and reported, never blindly removed.
                Err(cleanup) => Err(format!(
                    "Attachment link insertion failed: {error}; provisional asset cleanup failed: {cleanup}"
                )),
            },
        }
    })
}

/// Maps finite menu commands to the shared atomic formatter and publishes its exact selection.
fn format_active_markdown_selection(
    action: isize,
    view: Retained<PieceBackedMarkdownTextView>,
) -> Result<(), String> {
    let action = match action {
        // Heading commands normalize the current line's existing heading prefix.
        100 => MarkdownFormatAction::Heading1,
        // Second-level headings replace the prior heading marker while preserving selected line content.
        101 => MarkdownFormatAction::Heading2,
        // Third-level headings use the same atomic prefix normalization at their own marker width.
        102 => MarkdownFormatAction::Heading3,
        // Inline commands preserve selected source pieces and select editable placeholders when empty.
        103 => MarkdownFormatAction::Bold,
        // Italic wraps the selection or focuses its empty-selection placeholder through the shared formatter.
        104 => MarkdownFormatAction::Italic,
        // Underline inserts canonical HTML tags rather than mutating an independent native attribute model.
        105 => MarkdownFormatAction::Underline,
        // Strikethrough uses the preserved editor's paired Markdown delimiter behavior.
        106 => MarkdownFormatAction::Strikethrough,
        // Inline code retains literal selected source between the formatter's exact backtick pair.
        107 => MarkdownFormatAction::InlineCode,
        // Link insertion selects the editable label/URL range defined by the original editor contract.
        108 => MarkdownFormatAction::Link,
        // Block prefixes apply to every selected line, including an endpoint at the next line start.
        109 => MarkdownFormatAction::Blockquote,
        // Bullet formatting changes each selected line's prefix in one undoable canonical transaction.
        110 => MarkdownFormatAction::BulletList,
        // Ordered formatting numbers the selected lines according to the preserved formatter's sequence.
        111 => MarkdownFormatAction::OrderedList,
        // Task formatting inserts an unchecked source checkbox for each selected line.
        112 => MarkdownFormatAction::TaskList,
        // Math and HTML wrappers remain canonical text rather than independent attributed state.
        113 => MarkdownFormatAction::InlineFormula,
        // Superscript is represented by explicit canonical HTML delimiters.
        114 => MarkdownFormatAction::Superscript,
        // Templates insert at the selection head using the preserved editor's exact default content.
        115 => MarkdownFormatAction::Divider,
        // A fenced code template preserves the original placeholder selection and surrounding line spacing.
        116 => MarkdownFormatAction::CodeBlock,
        // A block formula inserts the original editor's complete finite template as one history edit.
        117 => MarkdownFormatAction::FormulaBlock,
        // Table formatting shares its exact finite default rows and caret placement with the core reference corpus.
        118 => MarkdownFormatAction::Table,
        // An unrecognized tag has no mutation authority.
        _ => return Err("Unknown formatting command".into()),
    };
    let storage = view
        .ivars()
        .text_storage
        .load()
        .ok_or("The editor is closed")?;
    let range = view.selectedRange();
    let selection = MarkdownTextSelection {
        anchor_utf16: range.location,
        head_utf16: range.location.saturating_add(range.length),
    };
    // The core prepares every range and capacity check before publishing one shared history root.
    let edit = storage.apply_format(action, selection)?;
    let start = edit
        .selection_after
        .anchor_utf16
        .min(edit.selection_after.head_utf16);
    let end = edit
        .selection_after
        .anchor_utf16
        .max(edit.selection_after.head_utf16);
    // Canonical mutation and document borrows have ended before native selection/layout can reenter.
    view.setSelectedRange(NSRange::new(start, end - start));
    view.schedule_visible_table_layout_update();
    Ok(())
}

/// Reads only explicit file-URL pasteboard items, with a fixed batch limit and no cache.
pub(super) fn native_markdown_pasteboard_file_paths(pasteboard: &NSPasteboard) -> Vec<PathBuf> {
    // No file payload means ordinary text remains eligible for inherited native paste.
    let Some(items) = pasteboard.pasteboardItems() else {
        // A plain-text or empty pasteboard has no explicit file batch to import.
        return Vec::new();
    };
    // SAFETY: AppKit exports this immutable type identifier for the process lifetime.
    let file_type = unsafe { objc2_app_kit::NSPasteboardTypeFileURL };
    (0..items.count().min(16))
        .filter_map(|index| {
            // Access only the capped batch; no unbounded native array is copied into Rust.
            let item = items.objectAtIndex(index);
            // NSURL parses escaping; the core repeats regular-file and symlink validation on open.
            let value = item.stringForType(file_type)?;
            let url = NSURL::URLWithString(&value)?;
            url.path().map(|path| PathBuf::from(path.to_string()))
        })
        .collect()
}

/// Defers external-file I/O while carrying no strong native editor ownership across the queue.
pub(super) fn schedule_native_markdown_attachment_paths(
    view: &PieceBackedMarkdownTextView,
    paths: Vec<PathBuf>,
) {
    let mtm = MainThreadMarker::new().expect("native paste/drop callbacks run on the main thread");
    // Preserve the user's exact paste/drop anchor while deferring file I/O outside native callbacks.
    let anchor_range = view.selectedRange();
    let anchor_generation = view
        .ivars()
        .text_storage
        .load()
        .and_then(|storage| storage.document())
        .map(|document| document.borrow().generation());
    let weak = MainThreadBound::new(Weak::from(view), mtm);
    enqueue_native_markdown_main_task(move || {
        let mtm = MainThreadMarker::new().expect("native imports execute on the main queue");
        // A closed editor cannot be revived by an earlier drop or paste operation.
        let Some(view) = weak.get(mtm).load() else {
            // The original native editor has deallocated; queued file work owns no successor authority.
            return;
        };
        let result = (|| {
            let DocumentActionContext {
                session,
                controller: _,
                relative_path,
            } = active_action_context()?;
            let current = active_document_view()?;
            // Session replacement invalidates old file actions even when their weak view still exists.
            if !std::ptr::eq(&*view, &*current) {
                // A still-alive old native shell cannot import files into the newly selected document.
                return Err("The editor changed before attachment import".into());
            }
            let generation = view
                .ivars()
                .text_storage
                .load()
                .and_then(|storage| storage.document())
                .map(|document| document.borrow().generation());
            if generation != anchor_generation || view.selectedRange() != anchor_range {
                // Intervening input or caret movement invalidates the original physical drop/paste anchor.
                return Err("The document or insertion point changed before attachment import; retry the action".into());
            }
            let note = relative_path.ok_or("No note is open")?;
            // Commit composition before using the current selection for explicit attachment links.
            view.commit_active_table_cell()?;
            view.commit_composition_before_release()?;
            for path in paths {
                // Each accepted file has one canonical undo transaction; failure rolls back only that provisional copy.
                insert_native_markdown_attachment(&session, &note, &path, &view)?;
            }
            Ok::<(), String>(())
        })();
        // Report bounded import failures without closing the note or losing its live draft.
        if let Err(error) = result {
            // Display only the bounded failure message; the already committed draft remains editable.
            present_native_markdown_error(&error);
        }
    });
}

/// Hides native find UI before Escape is allowed to close the document's foreground session.
pub(super) fn dismiss_active_native_markdown_find_bar() -> bool {
    // Clone the scroll owner, then end the surface borrow before a native responder transition.
    let scroll = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        active.as_ref().and_then(|surface| {
            surface
                .editor
                .as_ref()
                .map(|editor| editor.scroll_view.clone())
        })
    });
    if let Some(scroll) = scroll
        && scroll.isFindBarVisible()
    {
        // Only the transient finder closes; the document and its unsaved state remain active.
        scroll.setFindBarVisible(false);
        true
    } else {
        // No transient finder owns Escape, so the caller can apply the normal session-close policy.
        false
    }
}

/// Exports current unsaved canonical text only for an explicit foreground copy action.
pub(super) fn copy_active_markdown_document_to_general_pasteboard() -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "Markdown 全文复制必须在 AppKit 主线程执行".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
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
