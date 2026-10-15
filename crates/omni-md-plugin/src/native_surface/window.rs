//! Owns the mounted window, selected document, and bounded note transitions.
//! The surface is the unique native ownership root; close and release are separate stages.

use super::*;

/// The sole foreground root owns the panel, delegate, current editor, bounded sidebar, and search worker.
pub(super) struct NativeMarkdownWindowSurface {
    pub(super) panel: Retained<NativeMarkdownPanel>,
    pub(super) controller: Retained<NativeMarkdownController>,
    pub(super) presentation_mode: MarkdownPresentationMode,
    pub(super) session: MarkdownSessionAccess,
    pub(super) lifecycle: Rc<dyn PluginHostLifecycle>,
    pub(super) closing_request: Option<PluginCloseRequest>,
    pub(super) notes: Vec<MarkdownNoteSummary>,
    pub(super) sidebar_pager: Option<omni_markdown_core::MarkdownWorkspacePager>,
    pub(super) collapsed_directories: std::collections::HashSet<String>,
    pub(super) sidebar_has_more: bool,
    pub(super) sidebar_rows: Vec<NativeMarkdownSidebarRow>,
    pub(super) sidebar_buttons: Vec<Retained<NativeMarkdownSidebarButton>>,
    pub(super) note_title: Retained<NSTextField>,
    pub(super) current_note_relative_path: Option<String>,
    pub(super) editor: Option<NativeMarkdownEditorSurface>,
    pub(super) empty_label: Option<Retained<NSTextField>>,
    pub(super) pending_note_switch: Option<PendingNativeMarkdownNoteSwitch>,
    pub(super) search_generation: u64,
    pub(super) search_cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(super) search_worker: Option<std::thread::JoinHandle<()>>,
    pub(super) pending_file_mutation: Option<PendingNativeMarkdownFileMutation>,
}

/// A deferred note transition preserves only path identities and the old editor release barrier.
pub(super) struct PendingNativeMarkdownNoteSwitch {
    pub(super) relative_path: String,
    pub(super) previous_relative_path: Option<String>,
    pub(super) release_barrier: Option<NativeMarkdownEditorReleaseBarrier>,
}

/// A mutation waits for editor release while retaining the saved authority revision and recovery identity.
pub(super) struct PendingNativeMarkdownFileMutation {
    pub(super) mutation: NativeMarkdownFileMutation,
    pub(super) active_relative_path_before: Option<String>,
    pub(super) expected_revision: Option<String>,
    pub(super) release_barrier: Option<NativeMarkdownEditorReleaseBarrier>,
}

impl NativeMarkdownWindowSurface {
    /// Lend the panel-owned content view to the host until the plugin close lifecycle invalidates it.
    pub(super) fn native_view(&self) -> Option<PluginNativeView> {
        // A missing content view means teardown already removed the host's borrowed mount target.
        let content_view = self.panel.contentView()?;
        // SAFETY: `content_view` is retained by the panel, which is retained by this
        // session surface until the host receives the native close callback.
        unsafe {
            PluginNativeView::from_raw(
                (&*content_view as *const NSView)
                    .cast_mut()
                    .cast::<c_void>(),
            )
        }
    }

    /// Build an invisible complete foreground graph from a bounded vault snapshot before host mounting.
    pub(super) fn new(
        session: MarkdownSessionAccess,
        lifecycle: Rc<dyn PluginHostLifecycle>,
        mtm: MainThreadMarker,
    ) -> Result<Self, String> {
        // Enumerate metadata under explicit scan/result budgets before opening at most one note.
        let presentation_mode = MarkdownPresentationMode::InlinePreview;
        let collapsed_directories = std::collections::HashSet::new();
        let mut sidebar_pager = session
            .open_workspace_pager()
            .map_err(|error| error.to_string())?;
        let page = sidebar_pager
            .next_page(
                MAX_VISIBLE_NATIVE_MARKDOWN_WORKSPACE_ENTRIES - 1,
                MAX_SCANNED_NATIVE_MARKDOWN_WORKSPACE_ENTRIES,
                &collapsed_directories,
            )
            .map_err(|error| error.to_string())?;
        let sidebar_has_more = page.has_more;
        let snapshot = page.snapshot;
        let (notes, sidebar_rows) = native_markdown_sidebar_rows(snapshot);
        let document_result = notes
            .first()
            .map(|note| {
                // Only the initial selected note acquires a document mapping; sidebar entries stay metadata.
                session.open_document_buffer(&note.relative_path)
            })
            .transpose();

        // Establish the invisible panel/delegate owner before installing any native action targets.
        let panel = NativeMarkdownPanel::new(mtm);
        // SAFETY: Rust Retained ownership lasts through windowWillClose and the explicit finalizer;
        // AppKit must therefore not release this same owning claim automatically during close.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setTitle(ns_string!("Omni MD"));
        panel.setExcludedFromWindowsMenu(true);
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(false);
        panel.setWorksWhenModal(true);
        panel.setHidesOnDeactivate(false);
        panel.setMovableByWindowBackground(true);
        panel.setHasShadow(true);
        panel.setOpaque(true);
        panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
        panel.setLevel(NSFloatingWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        panel.setMinSize(NSSize::new(900.0, 600.0));
        panel.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.055, 0.062, 0.082, 1.0,
        )));

        let controller = NativeMarkdownController::new(mtm);
        panel.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
        let content_view = panel.contentView().unwrap_or_else(|| {
            // A native panel normally supplies its content view. If a platform initializer omits
            // it, install an owned empty root before creating any document/text view, without
            // returning an error that could strand a partially allocated native editing graph.
            let content = NSView::initWithFrame(
                NSView::alloc(mtm),
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(NATIVE_MARKDOWN_WIDTH, NATIVE_MARKDOWN_HEIGHT),
                ),
            );
            content.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            panel.setContentView(Some(&content));
            content
        });

        // The root background resizes with the panel and owns no document or callback state.
        let background = NSBox::initWithFrame(
            NSBox::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(NATIVE_MARKDOWN_WIDTH, NATIVE_MARKDOWN_HEIGHT),
            ),
        );
        background.setBoxType(NSBoxType::Custom);
        background.setTitlePosition(NSTitlePosition::NoTitle);
        background.setBorderWidth(1.0);
        background.setBorderColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.16, 0.18, 0.24, 1.0,
        ));
        background.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.055, 0.062, 0.082, 1.0,
        ));
        background.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content_view.addSubview(&background);

        // Header chrome remains above the editor/sidebar and follows the top edge on resize.
        let header = NSBox::initWithFrame(
            NSBox::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, NATIVE_MARKDOWN_HEIGHT - NATIVE_MARKDOWN_HEADER_HEIGHT),
                NSSize::new(NATIVE_MARKDOWN_WIDTH, NATIVE_MARKDOWN_HEADER_HEIGHT),
            ),
        );
        header.setBoxType(NSBoxType::Custom);
        header.setTitlePosition(NSTitlePosition::NoTitle);
        header.setBorderWidth(0.0);
        header.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.07, 0.078, 0.102, 1.0,
        ));
        header.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
        );
        content_view.addSubview(&header);

        // Static branding retains only its fixed label for this foreground graph.
        let plugin_title = NSTextField::labelWithString(ns_string!("Omni MD"), mtm);
        plugin_title.setFrame(NSRect::new(
            NSPoint::new(18.0, NATIVE_MARKDOWN_HEIGHT - 38.0),
            NSSize::new(76.0, 22.0),
        ));
        plugin_title.setFont(Some(&NSFont::boldSystemFontOfSize(14.0)));
        plugin_title.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.9, 0.92, 0.96, 1.0,
        )));
        plugin_title.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        content_view.addSubview(&plugin_title);

        // The fixed exit selector requests the shared plugin lifecycle rather than destroying the graph in-place.
        // SAFETY: the live main-thread controller implements exitMarkdownPlugin: with the NSButton
        // action ABI and is retained by this surface until targets are detached before its release.
        let exit_button = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("×"),
                Some(&*controller),
                Some(sel!(exitMarkdownPlugin:)),
                mtm,
            )
        };
        exit_button.setFrame(NSRect::new(
            NSPoint::new(94.0, NATIVE_MARKDOWN_HEIGHT - 41.0),
            NSSize::new(28.0, 28.0),
        ));
        exit_button.setBordered(false);
        exit_button.setFocusRingType(NSFocusRingType::None);
        exit_button.setFont(Some(&NSFont::systemFontOfSize(20.0)));
        exit_button.setContentTintColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.58, 0.62, 0.7, 1.0,
        )));
        exit_button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        content_view.addSubview(&exit_button);

        // Publish an initial identity/title only when opening the corresponding authority succeeded.
        let initial_note_relative_path = match (&document_result, notes.first()) {
            // The first snapshot note owns the successfully opened document.
            (Ok(Some(_)), Some(note)) => Some(note.relative_path.clone()),
            // Empty workspaces and open failures expose no active document identity.
            _ => None,
        };
        let current_title = match (&document_result, notes.first()) {
            // A valid editor receives its authoritative snapshot title.
            (Ok(Some(_)), Some(note)) => note.title.clone(),
            // No successful document open means the generic empty/error title is appropriate.
            _ => "Markdown".to_owned(),
        };
        let note_title = NSTextField::labelWithString(&NSString::from_str(&current_title), mtm);
        note_title.setFrame(NSRect::new(
            NSPoint::new(
                NATIVE_MARKDOWN_SIDEBAR_WIDTH + 24.0,
                NATIVE_MARKDOWN_HEIGHT - 38.0,
            ),
            NSSize::new(142.0, 22.0),
        ));
        note_title.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        note_title.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.64, 0.68, 0.76, 1.0,
        )));
        note_title.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewMinYMargin | NSAutoresizingMaskOptions::ViewMaxXMargin,
        );
        content_view.addSubview(&note_title);

        // Table insertion targets the same retained controller and stays disabled when no editor was opened.
        // SAFETY: the live main-thread controller implements insertMarkdownTable: with the NSButton
        // action ABI and is retained by this surface until targets are detached before its release.
        let table_button = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("插入表格"),
                Some(&*controller),
                Some(sel!(insertMarkdownTable:)),
                mtm,
            )
        };
        table_button.setFrame(NSRect::new(
            NSPoint::new(414.0, NATIVE_MARKDOWN_HEIGHT - 40.0),
            NSSize::new(88.0, 26.0),
        ));
        table_button.setBordered(false);
        table_button.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        table_button.setContentTintColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.62, 0.7, 0.88, 1.0,
        )));
        table_button.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewMaxXMargin | NSAutoresizingMaskOptions::ViewMinYMargin,
        );
        table_button.setEnabled(document_result.as_ref().is_ok_and(Option::is_some));
        content_view.addSubview(&table_button);

        // The two fixed modes affect derived native presentation without replacing canonical text.
        let presentation_mode_button = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(
                NSPoint::new(520.0, NATIVE_MARKDOWN_HEIGHT - 42.0),
                NSSize::new(88.0, 28.0),
            ),
            false,
        );
        presentation_mode_button.addItemWithTitle(ns_string!("预览"));
        presentation_mode_button.addItemWithTitle(ns_string!("源码"));
        presentation_mode_button.selectItemAtIndex(presentation_mode.popup_index() as isize);
        presentation_mode_button.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        presentation_mode_button.setBordered(false);
        presentation_mode_button.setFocusRingType(NSFocusRingType::None);
        presentation_mode_button.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMinYMargin,
        );
        // SAFETY: the controller is retained by the window surface until the
        // panel's close finalizer detaches every control target/action.
        unsafe {
            presentation_mode_button.setTarget(Some(&*controller));
            presentation_mode_button.setAction(Some(sel!(changeMarkdownPresentationMode:)));
        }
        content_view.addSubview(&presentation_mode_button);

        // Whole-document copy uses the active piece tree and cannot target an empty editor state.
        // SAFETY: the live main-thread controller implements copyEntireMarkdownDocument: with the NSButton
        // action ABI and is retained by this surface until targets are detached before its release.
        let copy_document_button = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("复制全文"),
                Some(&*controller),
                Some(sel!(copyEntireMarkdownDocument:)),
                mtm,
            )
        };
        copy_document_button.setFrame(NSRect::new(
            NSPoint::new(620.0, NATIVE_MARKDOWN_HEIGHT - 40.0),
            NSSize::new(82.0, 26.0),
        ));
        copy_document_button.setBordered(false);
        copy_document_button.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        copy_document_button.setContentTintColor(Some(
            &NSColor::colorWithSRGBRed_green_blue_alpha(0.62, 0.7, 0.88, 1.0),
        ));
        copy_document_button.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewMaxXMargin | NSAutoresizingMaskOptions::ViewMinYMargin,
        );
        copy_document_button.setEnabled(document_result.as_ref().is_ok_and(Option::is_some));
        content_view.addSubview(&copy_document_button);

        // Status contains bounded operation feedback and is retained by the controller for updates.
        let status_label = NSTextField::labelWithString(ns_string!(""), mtm);
        status_label.setFrame(NSRect::new(
            NSPoint::new(860.0, NATIVE_MARKDOWN_HEIGHT - 37.0),
            NSSize::new(236.0, 20.0),
        ));
        status_label.setAlignment(NSTextAlignment::Right);
        status_label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        status_label.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMinYMargin,
        );
        content_view.addSubview(&status_label);

        // Sidebar chrome is independent of editor mappings; row controls are installed from metadata below.
        let sidebar = NSBox::initWithFrame(
            NSBox::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(
                    NATIVE_MARKDOWN_SIDEBAR_WIDTH,
                    NATIVE_MARKDOWN_HEIGHT - NATIVE_MARKDOWN_HEADER_HEIGHT,
                ),
            ),
        );
        sidebar.setBoxType(NSBoxType::Custom);
        sidebar.setTitlePosition(NSTitlePosition::NoTitle);
        sidebar.setBorderWidth(0.0);
        sidebar.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.062, 0.07, 0.093, 1.0,
        ));
        sidebar.setAutoresizingMask(NSAutoresizingMaskOptions::ViewHeightSizable);
        content_view.addSubview(&sidebar);

        // Keep the metadata section label aligned with the panel's upper sidebar edge.
        let sidebar_title = NSTextField::labelWithString(ns_string!("笔记"), mtm);
        sidebar_title.setFrame(NSRect::new(
            NSPoint::new(
                18.0,
                NATIVE_MARKDOWN_HEIGHT - NATIVE_MARKDOWN_HEADER_HEIGHT - 38.0,
            ),
            NSSize::new(100.0, 20.0),
        ));
        sidebar_title.setFont(Some(&NSFont::boldSystemFontOfSize(12.0)));
        sidebar_title.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.48, 0.53, 0.62, 1.0,
        )));
        sidebar_title.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        content_view.addSubview(&sidebar_title);

        // Each bounded metadata row receives one surface-owned button with a detachable weak target.
        let sidebar_buttons = native_markdown_sidebar_buttons(
            &sidebar_rows,
            initial_note_relative_path.as_deref(),
            &content_view,
            mtm,
        );

        // Construct at most one TextKit graph; an open failure remains a visible empty-state error.
        let editor_frame = native_markdown_editor_frame(&content_view);
        let (editor, current_note_relative_path, document_error) = match document_result {
            // A valid document transfers into the sole native editor surface.
            Ok(Some(document)) => (
                Some(native_markdown_editor_surface(
                    document,
                    editor_frame,
                    presentation_mode,
                    mtm,
                )),
                initial_note_relative_path,
                None,
            ),
            // A legitimately empty workspace mounts no document or mapping.
            Ok(None) => (None, None, None),
            // Keep the open failure for foreground status while retaining metadata/navigation.
            Err(error) => (None, None, Some(error.to_string())),
        };
        let mut empty_label = None;
        if let Some(editor) = &editor {
            // The content view owns the mounted scroll graph while the surface owns its editor root.
            content_view.addSubview(editor.scroll_view());
        } else {
            // Preserve an interactive navigation surface even when no document could be opened.
            let empty_message = if notes.is_empty() {
                // No metadata means the user can create their first note from the workspace controls.
                "这里还没有 Markdown 笔记"
            } else {
                // Existing metadata with no editor signals an authority/open error rather than emptiness.
                "无法打开这个笔记"
            };
            let empty_state_label = native_markdown_empty_label(empty_message, mtm);
            content_view.addSubview(&empty_state_label);
            empty_label = Some(empty_state_label);
        }

        // Install remaining actions before the surface becomes visible or receives responder callbacks.
        install_native_markdown_workspace_controls(&controller, &content_view, mtm);
        controller.set_next_sidebar_page_available(sidebar_has_more);

        controller.install_controls(
            exit_button,
            presentation_mode_button,
            table_button,
            copy_document_button,
            status_label,
        );
        if let Some(document_error) = document_error {
            // Failed open is visible without discarding the navigation needed for recovery.
            controller.present_error(&document_error);
        }
        if let Some(editor) = &editor {
            // Only a live editor can own the initial text responder.
            panel.setInitialFirstResponder(Some(editor.text_view()));
        }

        // Transfer the completed native graph and scalar transition state into the unique surface owner.
        Ok(Self {
            panel,
            controller,
            presentation_mode,
            session,
            lifecycle,
            closing_request: None,
            notes,
            sidebar_pager: Some(sidebar_pager),
            collapsed_directories,
            sidebar_has_more,
            sidebar_rows,
            sidebar_buttons,
            note_title,
            current_note_relative_path,
            editor,
            empty_label,
            search_generation: 0,
            search_cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            search_worker: None,
            pending_note_switch: None,
            pending_file_mutation: None,
        })
    }

    /// Expose the already-owned native graph and request keyboard focus without creating another session.
    pub(super) fn show(&self) {
        // Activate only after the root has retained this fully configured foreground surface.
        self.panel.center();
        let mtm = MainThreadMarker::new()
            .expect("native Markdown show is restricted to the AppKit main thread");
        NSApplication::sharedApplication(mtm).activate();
        self.panel.orderFrontRegardless();
        self.panel.makeKeyWindow();
        if let Some(editor) = &self.editor
            && !self.panel.makeFirstResponder(Some(editor.text_view()))
        {
            // Focus refusal leaves the mounted editor usable through a later explicit click.
            self.controller
                .present_error("编辑器暂时无法获得键盘焦点，请点按正文后继续");
        }
    }

    /// Update bounded sidebar colors from the current relative identity without changing document ownership.
    pub(super) fn update_selected_note(&self, selected_relative_path: Option<&str>) {
        // Precompute four semantic colors before iterating only the bounded current row/button pairs.
        let selected_color = native_markdown_note_color(true);
        let unselected_color = native_markdown_note_color(false);
        let directory_color = NSColor::colorWithSRGBRed_green_blue_alpha(0.58, 0.63, 0.72, 1.0);
        let root_color = NSColor::colorWithSRGBRed_green_blue_alpha(0.82, 0.85, 0.91, 1.0);
        for (row, button) in self.sidebar_rows.iter().zip(&self.sidebar_buttons) {
            // Update appearance from immutable metadata without changing document selection ownership.
            let color = if row.kind == NativeMarkdownSidebarEntryKind::Note
                && selected_relative_path == Some(row.relative_path.as_str())
            {
                // The exact active note identity receives the selection accent.
                &selected_color
            } else if row.kind == NativeMarkdownSidebarEntryKind::Note {
                // Other notes remain selectable without pretending to own the active editor.
                &unselected_color
            } else if row.kind == NativeMarkdownSidebarEntryKind::Directory {
                // Directory actions have a distinct neutral hierarchy color.
                &directory_color
            } else {
                // The virtual root remains visible as the workspace's top-level action target.
                &root_color
            };
            button.setContentTintColor(Some(color));
        }
    }

    /// Install a recoverable empty/error presentation after no document editor remains mounted.
    pub(super) fn show_empty_state(
        &mut self,
        content_view: &NSView,
        message: &str,
        mtm: MainThreadMarker,
    ) {
        if let Some(label) = &self.empty_label {
            // Reuse the existing bounded error/empty control rather than accumulating labels.
            label.setStringValue(&NSString::from_str(message));
        } else {
            // First empty transition installs one owned placeholder in the still-live content view.
            let label = native_markdown_empty_label(message, mtm);
            content_view.addSubview(&label);
            self.empty_label = Some(label);
        }
        // Remove every action/focus affordance that would imply an active document still exists.
        self.panel.setInitialFirstResponder(None);
        self.note_title.setStringValue(ns_string!("Markdown"));
        self.current_note_relative_path = None;
        self.controller.set_copy_document_available(false);
        self.controller.set_table_insertion_available(false);
        self.update_selected_note(None);
    }

    /// Mount one editor only after the previous release barrier and publish its authoritative identity.
    pub(super) fn install_editor(
        &mut self,
        content_view: &NSView,
        editor: NativeMarkdownEditorSurface,
        relative_path: String,
    ) {
        if let Some(empty_label) = self.empty_label.take() {
            // A newly mounted editor supersedes and releases the old empty-state control.
            empty_label.removeFromSuperview();
        }
        // Mount after release verification, then publish the authoritative title/identity together.
        content_view.addSubview(editor.scroll_view());
        let title = self
            .notes
            .iter()
            .find(|note| note.relative_path == relative_path)
            .map_or_else(
                || native_markdown_path_name(&relative_path),
                |note| note.title.clone(),
            );
        self.note_title.setStringValue(&NSString::from_str(&title));
        self.current_note_relative_path = Some(relative_path.clone());
        self.controller.set_copy_document_available(true);
        self.controller.set_table_insertion_available(true);
        self.update_selected_note(Some(&relative_path));
        self.editor = Some(editor);

        // The installed owner now supports focus and user actions without an outstanding old editor.
        let editor = self
            .editor
            .as_ref()
            .expect("native Markdown editor was installed immediately above");
        self.panel
            .setInitialFirstResponder(Some(editor.text_view()));
        self.controller.clear_status();
        if !self.panel.makeFirstResponder(Some(editor.text_view())) {
            // Keep the successfully mounted document visible and report only the focus failure.
            self.controller
                .present_error("编辑器暂时无法获得键盘焦点，请点按正文后继续");
        }
    }

    /// Open a confined authoritative document and create its unique native editor graph.
    pub(super) fn open_note_editor(
        &self,
        relative_path: &str,
        editor_frame: NSRect,
        mtm: MainThreadMarker,
    ) -> Result<NativeMarkdownEditorSurface, String> {
        // The session validates vault confinement and revision identity before acquiring the mapping.
        let document = self
            .session
            .open_document_buffer(relative_path)
            .map_err(|error| error.to_string())?;
        Ok(native_markdown_editor_surface(
            document,
            editor_frame,
            self.presentation_mode,
            mtm,
        ))
    }

    /// Apply the selected source/preview mode to current and subsequently opened editors.
    pub(super) fn set_presentation_mode(
        &mut self,
        mode: MarkdownPresentationMode,
    ) -> Result<(), String> {
        if self.presentation_mode == mode {
            // Re-selecting the current mode needs no text/style invalidation or ownership change.
            self.controller.select_presentation_mode(mode);
            return Ok(());
        }
        if let Some(editor) = &self.editor
            && let Err(error) = editor.set_presentation_mode(mode)
        {
            // Rejected cell input keeps both the old presentation and the native picker coherent.
            self.controller
                .select_presentation_mode(self.presentation_mode);
            return Err(error);
        }
        // Persist the accepted mode for later note opens only after the current editor accepted it.
        self.presentation_mode = mode;
        self.controller.select_presentation_mode(mode);
        Ok(())
    }

    /// Delegate insertion to the active editor while refusing empty-state actions.
    pub(super) fn insert_default_table(&self) -> Result<(), String> {
        // A stale native action cannot insert into a document that has already entered empty state.
        let editor = self
            .editor
            .as_ref()
            .ok_or_else(|| "当前没有可编辑的 Markdown 笔记".to_owned())?;
        editor.insert_default_markdown_table();
        Ok(())
    }

    /// Require actual native responder resignation before releasing any editable document owner.
    pub(super) fn resign_editor_responder(&self) -> Result<(), String> {
        let Some(editor) = &self.editor else {
            // An empty workspace owns no document input responder to resign.
            return Ok(());
        };
        let generation_before = editor.document().borrow().generation();
        if !self.panel.makeFirstResponder(None) {
            // AppKit may reject resignation; preserving the live editor prevents losing an active native draft.
            return Err("The Markdown editor refused to resign its input responder".into());
        }
        if self.panel.firstResponder().is_some_and(|responder| {
            // Compare native responder identity without consulting private field-editor/input-context state.
            std::ptr::eq::<objc2_app_kit::NSResponder>(
                &*responder,
                editor.text_view().as_super().as_super().as_super(),
            )
        }) {
            // A nominal success must also have removed this exact text view as the native input target.
            return Err("The old Markdown editor remained first responder".into());
        }
        if editor.document().borrow().generation() != generation_before {
            // Resignation must not introduce a newly accepted input mutation after the caller's last save.
            // Preserve the live editor for another explicit save/close attempt and restore its input focus.
            self.panel.makeFirstResponder(Some(editor.text_view()));
            return Err(
                "Native input changed during resignation; the draft remains open for saving".into(),
            );
        }
        self.panel.setInitialFirstResponder(None);
        Ok(())
    }

    /// Detach actual input ownership before dropping the editor and returning its complete weak barrier.
    pub(super) fn destroy_current_editor(
        &mut self,
    ) -> Result<Option<NativeMarkdownEditorReleaseBarrier>, String> {
        self.resign_editor_responder()?;
        // A previous document's debounce cannot save through this controller after note ownership changes.
        self.controller.cancel_autosave();
        let Some(editor) = self.editor.take() else {
            // Already-empty teardown clears identity but produces no fictitious release barrier.
            self.current_note_relative_path = None;
            self.update_selected_note(None);
            return Ok(None);
        };
        self.controller.set_copy_document_available(false);
        self.controller.set_table_insertion_available(false);
        // Callers committed and saved composition before this irreversible ownership transition.
        let release_barrier = destroy_native_markdown_editor(editor);
        self.current_note_relative_path = None;
        self.update_selected_note(None);
        Ok(Some(release_barrier))
    }

    /// Save the old authority and stage a single note switch only after safely releasing its editor.
    pub(super) fn begin_note_switch(&mut self, relative_path: &str) -> Result<bool, String> {
        // Sidebar pages are bounded discovery views, not an authorization registry. Imported/new
        // notes may be outside the current page; open_note_editor revalidates their confined path
        // after the old graph releases, and its failure path restores the already-saved original.
        if self.pending_file_mutation.is_some() {
            // A pending mutation owns path/revision authority until its release barrier completes.
            return Err("文件操作仍在完成中".to_owned());
        }
        if let Some(pending_switch) = &self.pending_note_switch {
            // Coalesce the same destination while preserving the first accepted transition.
            return if pending_switch.relative_path == relative_path {
                // The existing deferred finalizer remains responsible for this requested destination.
                Ok(false)
            } else {
                // An opposing note switch cannot steal the in-flight old-editor release boundary.
                Err("另一笔记切换仍在完成中".to_owned())
            };
        }
        if self.current_note_relative_path.as_deref() == Some(relative_path)
            && self.editor.is_some()
        {
            // Selecting the already-open identity only restores focus; it must not reopen its mapping.
            self.controller.clear_status();
            if let Some(editor) = &self.editor {
                // This branch has already established live editor ownership for the current note.
                self.panel.makeFirstResponder(Some(editor.text_view()));
            }
            return Ok(false);
        }

        let previous_relative_path = self.current_note_relative_path.clone();

        // Marked-text commit may produce the final piece-tree edit, so it precedes saving.
        // A failed save leaves the old editor and its in-memory draft intact for user recovery.
        self.commit_marked_text()?;
        self.save_current_document()?;
        let release_barrier = self.destroy_current_editor()?;
        self.pending_note_switch = Some(PendingNativeMarkdownNoteSwitch {
            relative_path: relative_path.to_owned(),
            previous_relative_path,
            release_barrier,
        });
        Ok(true)
    }

    /// Verify old release before opening the destination, recovering the saved original on open failure.
    pub(super) fn finish_note_switch(&mut self, mtm: MainThreadMarker) -> Result<(), String> {
        let Some(pending_switch) = &self.pending_note_switch else {
            // Duplicate deferred callbacks after completion have no transition left to consume.
            return Ok(());
        };
        if let Some(release_barrier) = &pending_switch.release_barrier
            && let Err(error) = release_barrier.verify_released()
        {
            // A failed old-graph barrier forbids opening a successor and preserves pending ownership.
            let content_view = self
                .panel
                .contentView()
                .ok_or_else(|| "原生 Markdown 窗口没有内容视图".to_owned())?;
            self.show_empty_state(&content_view, "笔记切换暂时不可用", mtm);
            return Err(error);
        }

        // Only verified destruction permits taking the pending transition and acquiring a new document.
        let pending_switch = self
            .pending_note_switch
            .take()
            .expect("native Markdown note switch was checked immediately above");
        let content_view = self
            .panel
            .contentView()
            .ok_or_else(|| "原生 Markdown 窗口没有内容视图".to_owned())?;
        let editor_frame = native_markdown_editor_frame(&content_view);

        match self.open_note_editor(&pending_switch.relative_path, editor_frame, mtm) {
            // A valid destination becomes the sole mounted editor after old release verification.
            Ok(editor) => {
                self.install_editor(&content_view, editor, pending_switch.relative_path);
                Ok(())
            }
            // Opening the destination can fail after the old graph has already been safely released.
            Err(open_error) => {
                // The old draft was saved before release. Recovery reads its authoritative file,
                // retaining neither a second complete body nor overlapping document mappings.
                if let Some(previous_relative_path) = pending_switch.previous_relative_path {
                    // Recover the exact previous identity when one existed before this transition.
                    match self.open_note_editor(&previous_relative_path, editor_frame, mtm) {
                        // Successful recovery restores the saved original while reporting the destination error.
                        Ok(editor) => {
                            self.install_editor(&content_view, editor, previous_relative_path);
                            return Err(format!("无法打开所选笔记，已恢复原笔记：{open_error}"));
                        }
                        // Failed recovery mounts no editor and reports both authority failures.
                        Err(recovery_error) => {
                            self.show_empty_state(&content_view, "无法打开或恢复笔记", mtm);
                            return Err(format!(
                                "无法打开所选笔记：{open_error}；原笔记恢复失败：{recovery_error}"
                            ));
                        }
                    }
                }

                // A transition from an originally empty surface has no previous document to recover.
                self.show_empty_state(&content_view, "无法打开这个笔记", mtm);
                Err(open_error)
            }
        }
    }

    /// Cancel stale search, enumerate a bounded authoritative snapshot, and replace detached sidebar controls.
    pub(super) fn refresh_workspace_sidebar(
        &mut self,
        mtm: MainThreadMarker,
    ) -> Result<(), String> {
        // A metadata refresh supersedes search rows; cancel/join before replacing generation-bound results.
        self.stop_search();
        // A fresh foreground cursor reflects current filesystem identities and collapse state.
        self.sidebar_pager = Some(
            self.session
                .open_workspace_pager()
                .map_err(|error| error.to_string())?,
        );
        self.install_next_sidebar_page(mtm)
    }

    /// Advances one bounded metadata page without opening or replacing the active document.
    pub(super) fn install_next_sidebar_page(
        &mut self,
        mtm: MainThreadMarker,
    ) -> Result<(), String> {
        let pager = self
            .sidebar_pager
            .as_mut()
            .ok_or("Refresh the workspace before requesting another page")?;
        let page = pager
            .next_page(
                MAX_VISIBLE_NATIVE_MARKDOWN_WORKSPACE_ENTRIES - 1,
                MAX_SCANNED_NATIVE_MARKDOWN_WORKSPACE_ENTRIES,
                &self.collapsed_directories,
            )
            .map_err(|error| error.to_string())?;
        self.sidebar_has_more = page.has_more;
        self.controller
            .set_next_sidebar_page_available(page.has_more);
        let snapshot = page.snapshot;
        let (notes, mut sidebar_rows) = native_markdown_sidebar_rows(snapshot);
        // Preserve a user's collapsed subtree identity without retaining any note body.
        for row in &mut sidebar_rows {
            row.is_collapsed = self.collapsed_directories.contains(&row.relative_path);
        }
        for button in self.sidebar_buttons.drain(..) {
            // SAFETY: each live surface-owned button has a nullable weak target/action; detach them
            // before removing the view so no native callback can address obsolete row metadata.
            unsafe {
                button.setTarget(None);
                button.setAction(None);
            }
            button.removeFromSuperview();
        }
        let content_view = self
            .panel
            .contentView()
            .ok_or_else(|| "原生 Markdown 窗口没有内容视图".to_owned())?;
        // Each bounded metadata row receives one surface-owned button with a detachable weak target.
        let sidebar_buttons = native_markdown_sidebar_buttons(
            &sidebar_rows,
            self.current_note_relative_path.as_deref(),
            &content_view,
            mtm,
        );
        // Publish one coherent authoritative snapshot only after new controls are successfully constructed.
        self.notes = notes;
        self.sidebar_rows = sidebar_rows;
        self.sidebar_buttons = sidebar_buttons;
        Ok(())
    }

    /// Save/revision-check the active note when required and stage one mutation behind editor teardown.
    pub(super) fn begin_file_mutation(
        &mut self,
        mutation: NativeMarkdownFileMutation,
    ) -> Result<(), String> {
        if native_markdown_operation_is_busy(
            self.pending_note_switch.is_some(),
            self.pending_file_mutation.is_some(),
        ) {
            // A competing transition already owns the document/revision boundary; preserve it unchanged.
            return Err("另一项 Markdown 操作仍在完成中".to_owned());
        }

        // Copy authority/selection metadata before determining whether this operation affects the editor.
        let active_relative_path_before = self.current_note_relative_path.clone();
        let release_editor = native_markdown_mutation_requires_editor_release(
            &mutation,
            active_relative_path_before.as_deref(),
        );
        let expected_revision_required = native_markdown_mutation_requires_expected_revision(
            &mutation,
            active_relative_path_before.as_deref(),
        );
        if release_editor {
            // IME marked text can contain the final piece-tree edit. Commit it before the
            // revision is saved and before any path mutation can invalidate the mmap.
            self.commit_marked_text()?;
            self.save_current_document()?;
        }
        let expected_revision = if expected_revision_required {
            // Direct active-note mutations compare the exact revision just saved above.
            Some(
                self.editor
                    .as_ref()
                    .ok_or_else(|| "当前 Markdown 编辑器已经释放".to_owned())?
                    .document()
                    .borrow()
                    .original_revision()
                    .encode_hex(),
            )
        } else {
            // Independent operations have no active-document revision claim to enforce.
            None
        };
        let release_barrier = if release_editor {
            // The old mapping must be gone before path mutation can invalidate its authority.
            self.destroy_current_editor()?
        } else {
            // Unaffected active editors retain ownership without a fictitious release barrier.
            None
        };
        self.controller.clear_status();
        self.pending_file_mutation = Some(PendingNativeMarkdownFileMutation {
            mutation,
            active_relative_path_before,
            expected_revision,
            release_barrier,
        });
        Ok(())
    }

    /// Apply the confined mutation after release, refresh authority, and recover exactly one editor.
    pub(super) fn finish_file_mutation(&mut self, mtm: MainThreadMarker) -> Result<(), String> {
        let Some(pending_mutation) = &self.pending_file_mutation else {
            // Coalesced or duplicate callbacks do not execute an already-consumed mutation again.
            return Ok(());
        };
        if let Some(release_barrier) = &pending_mutation.release_barrier
            && let Err(error) = release_barrier.verify_released()
        {
            // Failed destruction leaves the mutation pending and forbids touching file authority.
            let content_view = self
                .panel
                .contentView()
                .ok_or_else(|| "原生 Markdown 窗口没有内容视图".to_owned())?;
            self.show_empty_state(&content_view, "文件操作暂时不可用", mtm);
            // Keep the pending mutation installed. A second click cannot bypass a failed
            // mmap/TextKit release barrier; closing the whole native surface is the only
            // remaining recovery boundary.
            return Err(error);
        }

        // Take exactly one verified transition before executing its confined authoritative operation.
        let pending_mutation = self
            .pending_file_mutation
            .take()
            .expect("native Markdown file mutation was checked immediately above");
        let editor_was_released = pending_mutation.release_barrier.is_some();
        let mutation_result = execute_native_markdown_file_mutation(
            &self.session,
            &pending_mutation.mutation,
            pending_mutation.expected_revision.as_deref(),
        );
        let mutation_result = match mutation_result {
            // The receipt is the sole authority for committed destination identities.
            Ok(result) => result,
            // A failed mutation may need to restore the saved original editor after prior release.
            Err(mutation_error) => {
                if editor_was_released
                    && let Some(previous_path) = pending_mutation.active_relative_path_before
                {
                    // Reopen only the saved authority; no stale body mirror is retained across teardown.
                    return match self.install_note_from_authority(&previous_path, mtm) {
                        // Recovery succeeded, so report the mutation error with the original note usable.
                        Ok(()) => Err(mutation_error),
                        // Recovery also failed; retain empty state and report both failures honestly.
                        Err(recovery_error) => Err(format!(
                            "{mutation_error}；原笔记恢复失败：{recovery_error}"
                        )),
                    };
                }
                // Unaffected editors stayed mounted, or no prior note existed to restore.
                return Err(mutation_error);
            }
        };

        let refresh_result = self.refresh_workspace_sidebar(mtm);
        // After deleting the active note/subtree, only a successful authority refresh may choose
        // the next note: the old snapshot could still name the deleted path. Creation/rename/move/
        // copy can instead use the exact committed receipt path even if sidebar refresh fails.
        let first_remaining_note = if refresh_result.is_ok() {
            // Only fresh metadata may identify a surviving fallback after deletion.
            self.notes.first().map(|note| note.relative_path.as_str())
        } else {
            // Suppress stale snapshot fallbacks while preserving authoritative mutation receipts.
            None
        };
        let next_active_path = native_markdown_active_path_after_mutation(
            &pending_mutation.mutation,
            &mutation_result,
            pending_mutation.active_relative_path_before.as_deref(),
            first_remaining_note,
        );
        let editor_identity_changed =
            self.current_note_relative_path.as_deref() != next_active_path.as_deref();
        if editor_identity_changed
            || (self.editor.is_none() && next_active_path.is_some())
            || (editor_was_released && next_active_path.is_none())
        {
            // Identity changes require installing exactly one successor after verified old-editor release.
            if self.editor.is_some() {
                // A live old graph signals a missed release contract; refuse to overlay a second editor.
                return Err("文件操作改变了活动笔记，但旧编辑器没有经过释放屏障".to_owned());
            }
            if let Some(relative_path) = next_active_path {
                // Use the committed or freshly enumerated path to reacquire one authoritative document.
                self.install_note_from_authority(&relative_path, mtm)?;
            } else {
                // No valid remaining identity means empty state, never reopening a deleted old path.
                let content_view = self
                    .panel
                    .contentView()
                    .ok_or_else(|| "原生 Markdown 窗口没有内容视图".to_owned())?;
                self.show_empty_state(&content_view, "这里还没有 Markdown 笔记", mtm);
            }
        } else {
            // The active note was unaffected and keeps its existing editor/mapping.
            self.update_selected_note(self.current_note_relative_path.as_deref());
        }

        if let Err(refresh_error) = refresh_result {
            // The mutation already committed; report the secondary refresh error without replaying it.
            return Err(format!("文件操作已完成，但侧栏刷新失败：{refresh_error}"));
        }
        self.controller.clear_status();
        Ok(())
    }

    /// Reopen the exact committed path rather than restoring a stale in-memory body mirror.
    pub(super) fn install_note_from_authority(
        &mut self,
        relative_path: &str,
        mtm: MainThreadMarker,
    ) -> Result<(), String> {
        // Resolve layout from the live panel and let the session revalidate the requested authority path.
        let content_view = self
            .panel
            .contentView()
            .ok_or_else(|| "原生 Markdown 窗口没有内容视图".to_owned())?;
        let editor_frame = native_markdown_editor_frame(&content_view);
        let editor = self.open_note_editor(relative_path, editor_frame, mtm)?;
        self.install_editor(&content_view, editor, relative_path.to_owned());
        Ok(())
    }

    /// Persist current selection and stream piece-backed changes through the session revision contract.
    pub(super) fn save_current_document(&self) -> Result<(), String> {
        // Explicit/transition saves consume any pending debounce rather than leaving a second write queued.
        self.controller.cancel_autosave();
        let Some(editor) = &self.editor else {
            // An empty surface has no unsaved canonical document or selection to persist.
            return Ok(());
        };
        // Copy native selection before borrowing the document mutably; checked arithmetic rejects overflow.
        let selected_range = editor.text_view().selectedRange();
        let selection_end = selected_range
            .location
            .checked_add(selected_range.length)
            .ok_or_else(|| "当前文本选择范围无效".to_owned())?;
        let mut document = editor.document().borrow_mut();
        document
            .set_selection(MarkdownTextSelection {
                anchor_utf16: selected_range.location,
                head_utf16: selection_end,
            })
            .map_err(|error| error.to_string())?;
        // Revision-aware streaming save preserves the current draft on conflict or IO failure.
        self.session
            .save_document_buffer(&mut document)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Commit table-cell and IME composition before save, copy, or an ownership transition.
    pub(super) fn commit_marked_text(&self) -> Result<(), String> {
        let Some(editor) = &self.editor else {
            // No editor implies no cell or input-method composition can contribute a final edit.
            return Ok(());
        };
        // Commit the table field before ending IME marked text so the piece tree reflects visible input.
        editor.text_view.commit_active_table_cell()?;
        editor.text_view.commit_composition_before_release()?;
        Ok(())
    }

    /// Copy the current committed piece tree only when no competing transition owns the editor.
    pub(super) fn copy_current_document_to_pasteboard(
        &self,
        pasteboard: &NSPasteboard,
    ) -> Result<(), String> {
        if native_markdown_operation_is_busy(
            self.pending_note_switch.is_some(),
            self.pending_file_mutation.is_some(),
        ) {
            // A competing transition already owns the document/revision boundary; preserve it unchanged.
            return Err("另一项 Markdown 操作仍在完成中".to_owned());
        }

        // Table-cell and IME composition are visible user edits; commit them before borrowing
        // the canonical piece tree so copied text agrees with the current editor state.
        self.commit_marked_text()?;
        let editor = self
            .editor
            .as_ref()
            .ok_or_else(|| "当前没有可复制的 Markdown 笔记".to_owned())?;
        let document = editor
            .document()
            .try_borrow()
            .map_err(|_| "Markdown 正文仍在完成本次编辑".to_owned())?;
        write_markdown_document_to_pasteboard(&document, pasteboard)
            .map_err(|error| error.to_string())
    }

    /// Stop foreground search and detach control/responder callbacks before the window close notification.
    pub(super) fn prepare_to_close(&mut self) {
        // Cancel scanning and join the foreground worker before releasing its session.
        self.stop_search();
        // Foreground tree traversal descriptors end before the native surface enters the closing state.
        self.sidebar_pager.take();
        if let Some(editor) = &self.editor {
            // Save and actual responder resignation succeeded; seal both canonical and native input before close callbacks.
            editor.stop_accepting_input();
        }
        // The save/close preflight already committed composition and required actual responder resignation.
        // Do not create another native input transaction after the host accepted its Closing state.
        self.panel.setInitialFirstResponder(None);
        for sidebar_button in &self.sidebar_buttons {
            // SAFETY: surface ownership keeps each main-thread button live; clearing its nullable
            // weak target/action prevents native delivery after the controller is released.
            unsafe {
                sidebar_button.setTarget(None);
                sidebar_button.setAction(None);
            }
        }
        self.controller.detach_controls();
    }

    /// Break the remaining delegate/content edges after the native close callback has completed.
    pub(super) fn release_native_ownership(&self) {
        // This follows windowWillClose, so removing the delegate can no longer lose its close signal.
        self.panel.setDelegate(None);
        self.panel.setContentView(None);
    }
}

/// Computes the current editor area after reserving header and sidebar layout space.
pub(super) fn native_markdown_editor_frame(content_view: &NSView) -> NSRect {
    let content_size = content_view.bounds().size;
    NSRect::new(
        NSPoint::new(NATIVE_MARKDOWN_SIDEBAR_WIDTH + 1.0, 0.0),
        NSSize::new(
            (content_size.width - NATIVE_MARKDOWN_SIDEBAR_WIDTH - 1.0).max(0.0),
            (content_size.height - NATIVE_MARKDOWN_HEADER_HEIGHT).max(0.0),
        ),
    )
}

/// Creates a noneditable placeholder for an empty workspace without opening a document.
pub(super) fn native_markdown_empty_label(
    message: &str,
    mtm: MainThreadMarker,
) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(message), mtm);
    label.setFrame(NSRect::new(
        NSPoint::new(NATIVE_MARKDOWN_SIDEBAR_WIDTH + 36.0, 310.0),
        NSSize::new(420.0, 28.0),
    ));
    label.setFont(Some(&NSFont::systemFontOfSize(15.0)));
    label.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.5, 0.54, 0.62, 1.0,
    )));
    label.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewMaxXMargin
            | NSAutoresizingMaskOptions::ViewMinYMargin
            | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );
    label
}

/// Opens one selected note and assembles the actual TextKit surface for its foreground lifetime.
pub(super) fn native_markdown_editor_surface(
    document: MarkdownDocumentBuffer,
    frame: NSRect,
    presentation_mode: MarkdownPresentationMode,
    mtm: MainThreadMarker,
) -> NativeMarkdownEditorSurface {
    // All fallible file/revision acquisition precedes native allocation at the calling boundary.
    let editor =
        NativeMarkdownEditorSurface::new_with_mode(document, frame, presentation_mode, mtm);
    editor
        .scroll_view()
        .setBackgroundColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.075, 0.084, 0.11, 1.0,
        ));
    editor
}
