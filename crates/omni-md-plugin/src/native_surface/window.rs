//! Owns the mounted window, selected document, and bounded note transitions.
//! The surface is the unique native ownership root; close and release are separate stages.

use super::*;
use objc2_app_kit::NSAppearanceCustomization;

/// The sole foreground root owns the panel, delegate, current editor, bounded sidebar, and search worker.
pub(super) struct NativeMarkdownWindowSurface {
    pub(super) panel: Retained<NativeMarkdownPanel>,
    pub(super) controller: Retained<NativeMarkdownController>,
    pub(super) owned_ledger: Rc<NativeMarkdownOwnedLedger>,
    pub(super) presentation_mode: MarkdownPresentationMode,
    pub(super) session: MarkdownSessionAccess,
    pub(super) lifecycle: Rc<dyn PluginHostLifecycle>,
    pub(super) closing_request: Option<PluginCloseRequest>,
    pub(super) notes: Vec<MarkdownNoteSummary>,
    pub(super) sidebar_pager: Option<omni_markdown_core::MarkdownWorkspacePager>,
    pub(super) collapsed_directories: std::collections::HashSet<String>,
    pub(super) sidebar_has_more: bool,
    pub(super) sidebar_scan_pending: bool,
    pub(super) sidebar_intent: Option<u64>,
    /// Lives with the current pager; after failure, pages permit explicit reselection without auto-opening over the original error.
    pub(super) sidebar_completion_policy:
        super::workspace_browse_policy::NativeMarkdownBrowseCompletionPolicy,
    pub(super) sidebar_results: Option<
        std::sync::Arc<crate::cancelable_mailbox::CancelableMailbox<NativeMarkdownBrowseResult>>,
    >,
    /// Reuses the recorded section-title control for mode feedback without another label or query cache.
    pub(super) sidebar_title: Retained<NSTextField>,
    pub(super) sidebar_rows: Vec<NativeMarkdownSidebarRow>,
    pub(super) sidebar_list: NativeMarkdownSidebarList,
    pub(super) note_title: Retained<NSTextField>,
    /// Displays only the installed document's actual relative path, without duplicating browser selection or document ownership.
    pub(super) note_path: Retained<NSTextField>,
    pub(super) current_note_relative_path: Option<String>,
    pub(super) editor: Option<NativeMarkdownEditorSurface>,
    pub(super) empty_label: Option<Retained<NSTextField>>,
    pub(super) pending_note_switch: Option<PendingNativeMarkdownNoteSwitch>,
    /// Query text and opaque continuation retain bounded metadata only; stop_search and closure take and release them together.
    pub(super) search_query: Option<String>,
    /// Keeps the input intent accepted with the query; save cannot assign a newer input epoch to an old query.
    pub(super) search_intent: Option<u64>,
    pub(super) search_cursor: Option<omni_markdown_core::MarkdownSearchCursor>,
    /// Displayed pages retain independent query authority, so save can revoke even a final page without a cursor.
    pub(super) search_authority: Option<omni_markdown_core::MarkdownSearchAuthority>,
    pub(super) search_generation: u64,
    pub(super) search_cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(super) search_worker: Option<std::thread::JoinHandle<()>>,
    // The foreground owner retains query/results; dispatch carries only a weak mailbox reference.
    pub(super) pending_search_query:
        Option<std::sync::Arc<crate::cancelable_mailbox::CancelableMailbox<String>>>,
    pub(super) search_results: Option<
        std::sync::Arc<crate::cancelable_mailbox::CancelableMailbox<NativeMarkdownSearchResult>>,
    >,
    pub(super) pending_file_mutation: Option<PendingNativeMarkdownFileMutation>,
    pub(super) borrowed_close: Option<NativeMarkdownBorrowedEditorSnapshot>,
    /// Retains the first public modal-cleanup failure; opening another dialog cannot discard incomplete release facts.
    pub(super) modal_failure: Option<String>,
}

/// A deferred note transition preserves only path identities and the old editor release barrier.
pub(super) struct PendingNativeMarkdownNoteSwitch {
    pub(super) relative_path: String,
    pub(super) previous_relative_path: Option<String>,
    pub(super) release_barrier: Option<NativeMarkdownEditorReleaseBarrier>,
    /// Only this link switch carries the decoded fragment; transfers it to destination Body after old-graph release without retaining old document/Scene.
    pub(super) fragment: Option<String>,
}

/// A mutation waits for actual editor release while retaining its original path, saved disk revision, and feedback intent.
pub(super) struct PendingNativeMarkdownFileMutation {
    pub(super) mutation: NativeMarkdownFileMutation,
    pub(super) active_relative_path_before: Option<String>,
    pub(super) expected_revision: Option<String>,
    pub(super) release_barrier: Option<NativeMarkdownEditorReleaseBarrier>,
    /// A modal-only barrier does not prove document release; failure compensation follows the actual editor transition.
    pub(super) editor_was_released: bool,
    /// Captures input intent before begin's first native call; asynchronous release cannot acquire newer query authority at finish.
    pub(super) feedback_intent: u64,
}

/// Only file operations carry their original feedback intent through installation; ordinary callers retain their contract, and mounting cannot renew input authority.
#[derive(Clone, Copy)]
enum NativeMarkdownEditorFeedback {
    ExistingCaller,
    FileMutation(Option<u64>),
}

impl NativeMarkdownEditorFeedback {
    /// Document restoration is necessary compensation; status/focus separately require the original intent to avoid taking newer query input.
    fn permits(self, surface: &NativeMarkdownWindowSurface) -> bool {
        match self {
            // Other document switches retain their caller's handoff authority without a new epoch policy here.
            Self::ExistingCaller => true,
            // None explicitly denotes revoked authority; installation cannot replace it with a newer epoch.
            Self::FileMutation(intent) => {
                intent.is_some_and(|intent| surface.workspace_refresh_intent_is_current(intent))
            }
        }
    }
}

impl NativeMarkdownWindowSurface {
    /// Pure Rust admission precedes composition, save, resignation, or owner removal; failure preserves the same editable parent draft.
    pub(super) fn preflight_modal_boundary(&self) -> Result<(), String> {
        preflight_native_markdown_modal()?;
        if let Some(error) = &self.modal_failure {
            // An observed cleanup failure remains authoritative even if its window later becomes invisible.
            return Err(error.clone());
        }
        Ok(())
    }

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
        let owned_ledger =
            Rc::new(NativeMarkdownOwnedLedger::for_native_session_before_document_open());
        let presentation_mode = MarkdownPresentationMode::InlinePreview;
        let collapsed_directories = std::collections::HashSet::new();
        let mut sidebar_pager = session
            .open_workspace_pager()
            .map_err(|error| error.to_string())?;
        let progress = sidebar_pager
            .next_page_cancellable(
                MAX_VISIBLE_NATIVE_MARKDOWN_WORKSPACE_ENTRIES - 1,
                MAX_SCANNED_NATIVE_MARKDOWN_WORKSPACE_ENTRIES,
                &collapsed_directories,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .map_err(|error| error.to_string())?;
        // Performs one bounded initial step; an incomplete arbitrary prefix cannot select an automatically opened note.
        let (snapshot, sidebar_has_more, sidebar_scan_pending) = match progress {
            omni_markdown_core::MarkdownWorkspacePageProgress::Ready(page) => {
                // A fully scanned small vault has a deterministic page that may choose the initial note.
                (page.snapshot, page.has_more, false)
            }
            omni_markdown_core::MarkdownWorkspacePageProgress::Pending => {
                // An incomplete large vault has no publishable prefix; mounts root/loading UI while background scanning continues.
                (
                    omni_markdown_core::MarkdownWorkspaceSnapshot {
                        directories: Vec::new(),
                        notes: Vec::new(),
                    },
                    false,
                    true,
                )
            }
        };
        // Displays the same validated root used by file operations; failure remains an error instead of creating another empty vault.
        let workspace_root = session
            .resolve_directory_for_system_action("")
            .map_err(|error| error.to_string())?;
        let root_title = native_markdown_path_name(&workspace_root.to_string_lossy());
        let (notes, sidebar_rows) = native_markdown_sidebar_rows(snapshot, &root_title);
        let document_result = notes
            .first()
            .map(|note| {
                // Only the initial selected note acquires a document mapping; sidebar entries stay metadata.
                session.open_document_buffer(&note.relative_path)
            })
            .transpose();

        // Establish the invisible panel/delegate owner before installing any native action targets.
        let panel = NativeMarkdownPanel::new(mtm);
        // The fixed dark window background requires the same native appearance for standard popups, buttons, and fields.
        // SAFETY: AppKit appearance names have process-static lifetime; shared framework appearance is queried only on the main thread.
        let appearance_name = unsafe { objc2_app_kit::NSAppearanceNameDarkAqua };
        let appearance = objc2_app_kit::NSAppearance::appearanceNamed(appearance_name)
            .ok_or("原生深色外观不可用")?;
        panel.setAppearance(Some(&appearance));
        // SAFETY: Rust Retained ownership lasts through windowWillClose and the explicit finalizer;
        // AppKit must therefore not release this same owning claim automatically during close.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setTitle(ns_string!("Omni MD"));
        panel.setExcludedFromWindowsMenu(true);
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(false);
        panel.setWorksWhenModal(true);
        panel.setHidesOnDeactivate(false);
        // Preserves original borderless background dragging while text and controls handle their own input first.
        panel.setMovableByWindowBackground(true);
        panel.setHasShadow(true);
        panel.setOpaque(true);
        panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
        panel.setLevel(NSFloatingWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        // Minimum size constrains actual content; a borderless window reserves no extra title-bar height.
        panel.setContentMinSize(NSSize::new(
            sidebar_geometry::MINIMUM_CONTENT_WIDTH,
            sidebar_geometry::MINIMUM_CONTENT_HEIGHT,
        ));
        panel.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.055, 0.062, 0.082, 1.0,
        )));

        let controller = NativeMarkdownController::new(&owned_ledger, &panel, mtm);
        panel.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
        // Panel construction establishes the sole content root; absence is an assembly bug, not permission to create a replacement tree.
        let content_view = panel
            .contentView()
            .expect("原生 Markdown 面板必须提供内容视图");

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
                NSPoint::new(
                    NATIVE_MARKDOWN_SIDEBAR_WIDTH + 1.0,
                    NATIVE_MARKDOWN_HEIGHT - NATIVE_MARKDOWN_HEADER_HEIGHT,
                ),
                NSSize::new(
                    NATIVE_MARKDOWN_WIDTH - NATIVE_MARKDOWN_SIDEBAR_WIDTH - 1.0,
                    NATIVE_MARKDOWN_HEADER_HEIGHT,
                ),
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

        let sidebar = NSBox::initWithFrame(
            NSBox::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(NATIVE_MARKDOWN_SIDEBAR_WIDTH, NATIVE_MARKDOWN_HEIGHT),
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

        // Keeps the original product name without a system title bar; functional localization neither renames the brand nor adds a close control.
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
                ns_string!("返回"),
                Some(&*controller),
                Some(sel!(exitMarkdownPlugin:)),
                mtm,
            )
        };
        exit_button.setFrame(NSRect::new(
            NSPoint::new(166.0, NATIVE_MARKDOWN_HEIGHT - 42.0),
            NSSize::new(64.0, 28.0),
        ));
        // The compact button retains full accessibility/tooltip text; exit still follows original save and release barriers.
        exit_button.setAccessibilityLabel(Some(ns_string!("返回启动器")));
        exit_button.setToolTip(Some(ns_string!("关闭笔记并返回启动器")));
        exit_button.setBordered(false);
        exit_button.setFocusRingType(NSFocusRingType::None);
        exit_button.setFont(Some(&NSFont::systemFontOfSize(12.0)));
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
            _ => "笔记".to_owned(),
        };
        let note_title = NSTextField::labelWithString(&NSString::from_str(&current_title), mtm);
        note_title.setFrame(NSRect::new(
            NSPoint::new(
                NATIVE_MARKDOWN_SIDEBAR_WIDTH + 24.0,
                NATIVE_MARKDOWN_HEIGHT - 29.0,
            ),
            NSSize::new(248.0, 20.0),
        ));
        // SAFETY: Public AppKit weight constants remain valid during main-thread construction without borrowing document or font buffers.
        let title_weight = unsafe { NSFontWeightSemibold };
        note_title.setFont(Some(&NSFont::systemFontOfSize_weight(17.0, title_weight)));
        note_title.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.90, 0.92, 0.96, 1.0,
        )));
        // One geometry publication owns all header horizontal positions, preventing independent springs from overlapping on resize.
        note_title.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        configure_native_markdown_header_text(&note_title);
        note_title.setAccessibilityLabel(Some(ns_string!("当前笔记")));
        note_title.setToolTip(Some(&NSString::from_str(&current_title)));
        content_view.addSubview(&note_title);

        // The path comes from the same successful open identity; a read-only label preserves full AX text while visual truncation leaves the string intact.
        let initial_path = NSString::from_str(initial_note_relative_path.as_deref().unwrap_or(""));
        let note_path = NSTextField::labelWithString(&initial_path, mtm);
        note_path.setFrame(NSRect::new(
            NSPoint::new(
                NATIVE_MARKDOWN_SIDEBAR_WIDTH + 24.0,
                NATIVE_MARKDOWN_HEIGHT - 46.0,
            ),
            NSSize::new(248.0, 13.0),
        ));
        note_path.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        note_path.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.58, 0.63, 0.72, 1.0,
        )));
        note_path.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        configure_native_markdown_header_text(&note_path);
        note_path.setAccessibilityLabel(Some(ns_string!("当前笔记相对路径")));
        if initial_note_relative_path.is_some() {
            note_path.setToolTip(Some(&initial_path));
        } // Empty state shows no invented path or tooltip from the previous document.
        content_view.addSubview(&note_path);

        // The original Source toggle uses the same actual mode transaction without replacing canonical storage or creating an editor.
        // SAFETY: The original main-thread controller implements the fixed action; teardown clears button target/action first.
        let presentation_mode_button = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("源码"),
                Some(&*controller),
                Some(sel!(changeMarkdownPresentationMode:)),
                mtm,
            )
        };
        presentation_mode_button.setButtonType(objc2_app_kit::NSButtonType::PushOnPushOff);
        presentation_mode_button.setBezelStyle(objc2_app_kit::NSBezelStyle::AccessoryBarAction);
        presentation_mode_button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        presentation_mode_button.setFocusRingType(NSFocusRingType::None);
        presentation_mode_button.setRefusesFirstResponder(true);
        presentation_mode_button.setEnabled(matches!(&document_result, Ok(Some(_))));
        presentation_mode_button.setAccessibilityLabel(Some(ns_string!("切换源码显示")));
        presentation_mode_button.setToolTip(Some(ns_string!("切换实时预览与源码显示")));
        presentation_mode_button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        content_view.addSubview(&presentation_mode_button);

        // Status contains bounded operation feedback and is retained by the controller for updates.
        let status_label = NSTextField::labelWithString(ns_string!(""), mtm);
        status_label.setFrame(NSRect::new(
            NSPoint::new(860.0, NATIVE_MARKDOWN_HEIGHT - 37.0),
            NSSize::new(236.0, 20.0),
        ));
        status_label.setAlignment(NSTextAlignment::Left);
        status_label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        status_label.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
        );
        status_label.setHidden(true);
        configure_native_markdown_header_text(&status_label);
        status_label.setAccessibilityLabel(Some(ns_string!("操作状态")));
        content_view.addSubview(&status_label);

        // The three persistent footer columns are separate from transient feedback; each is a fixed recorded control of this window.
        let footer_labels = ["显示模式", "保存状态", "字符和行数"].map(|accessibility| {
            let label = NSTextField::labelWithString(ns_string!(""), mtm);
            configure_native_markdown_header_text(&label);
            label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            label.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                0.58, 0.63, 0.72, 1.0,
            )));
            label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxYMargin);
            label.setAccessibilityLabel(Some(&NSString::from_str(accessibility)));
            content_view.addSubview(&label);
            label
        });
        footer_labels[1].setAlignment(NSTextAlignment::Center);
        footer_labels[2].setAlignment(NSTextAlignment::Right);

        // Sidebar chrome is independent of editor mappings; row controls are installed from metadata below.
        // Keep the metadata section label aligned with the panel's upper sidebar edge.
        let sidebar_title = NSTextField::labelWithString(ns_string!("笔记目录"), mtm);
        sidebar_title.setFrame(NSRect::new(
            NSPoint::new(18.0, NATIVE_MARKDOWN_HEIGHT - 124.0),
            NSSize::new(56.0, 20.0),
        ));
        sidebar_title.setFont(Some(&NSFont::boldSystemFontOfSize(12.0)));
        sidebar_title.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.48, 0.53, 0.62, 1.0,
        )));
        sidebar_title.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        content_view.addSubview(&sidebar_title);

        // Vault display uses the actual final directory name; the same label exposes its full location through tooltip and AX value.
        // Changes long-path presentation only, preserving session root, bookmarks, file identity, and open/save targets.
        let workspace_path = NSString::from_str(&workspace_root.to_string_lossy());
        let workspace_name = workspace_root
            .file_name()
            .unwrap_or(workspace_root.as_os_str())
            .to_string_lossy();
        let workspace_caption = NSString::from_str(&format!("笔记库 · {workspace_name}"));
        let workspace_location = NSTextField::labelWithString(&workspace_caption, mtm);
        workspace_location.setFrame(NSRect::new(
            NSPoint::new(18.0, 34.0),
            NSSize::new(212.0, 18.0),
        ));
        workspace_location.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        // The fixed dark background needs an explicit compatible label color; the light appearance's secondaryLabelColor becomes black.
        workspace_location.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.58, 0.63, 0.72, 1.0,
        )));
        workspace_location.setToolTip(Some(&workspace_path));
        workspace_location.setAccessibilityLabel(Some(ns_string!("笔记保存位置")));
        // SAFETY: This static label exposes a read-only NSString accessibility value; AppKit synchronously retains the actual directory text.
        unsafe {
            workspace_location.setAccessibilityValue(Some(&workspace_path));
        }
        content_view.addSubview(&workspace_location);

        // The native scrolling list receives only the current bounded metadata page; search and footer stay outside its container.
        let sidebar_list = NativeMarkdownSidebarList::new(
            &sidebar_rows,
            initial_note_relative_path.as_deref(),
            &content_view,
            &owned_ledger,
            mtm,
        )?;

        // Constructs at most one paginated document graph; an open failure remains a visible empty-state error.
        let editor_frame =
            native_markdown_editor_frame(&content_view, controller.feedback_visible());
        let (editor, current_note_relative_path, document_error) = match document_result {
            // A valid document transfers into the sole native editor surface.
            Ok(Some(document)) => (
                Some(native_markdown_editor_surface(
                    document,
                    editor_frame,
                    presentation_mode,
                    &owned_ledger,
                    mtm,
                )?),
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
            let empty_message = if sidebar_scan_pending {
                // Full sorting continues in the background; a temporarily empty page must not claim the user has no notes.
                "正在读取笔记目录…"
            } else if notes.is_empty() {
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

        // Menu construction reads the actual initial mode; before foreground publication it sets only the established mode value.
        // The original caller resynchronizes buttons after installation; construction does not impersonate a live session.
        controller.select_presentation_mode(presentation_mode);
        // Install remaining actions before the surface becomes visible or receives responder callbacks.
        let toolbar_controls = install_native_markdown_workspace_controls(
            &controller,
            &content_view,
            &owned_ledger,
            &sidebar_list,
            editor.is_some(),
            mtm,
        );
        controller.set_next_sidebar_page_available(sidebar_has_more);

        controller.install_controls(
            exit_button,
            presentation_mode_button,
            status_label,
            footer_labels,
        );
        controller.install_conflict_bar()?;
        if let Some(editor) = &editor {
            // Construction binds the actual document directly, without acquiring a path from a later Active getter; its weak intent keeps no text alive.
            controller.bind_conflict_note(NativeMarkdownCurrentNoteIntent::for_document(
                &controller,
                &panel,
                editor.document(),
                current_note_relative_path
                    .as_deref()
                    .expect("已安装正文必有Core验证的相对路径"),
            ));
        }
        controller.select_presentation_mode(presentation_mode);
        let footer = editor
            .as_ref()
            .map(|editor| NativeMarkdownFooterDocument::capture(&editor.document().borrow()));
        controller.install_footer_document(footer);
        if let Some(document_error) = document_error {
            // Failed open is visible without discarding the navigation needed for recovery.
            controller.present_error(&document_error);
        }
        if let Some(editor) = &editor {
            // Actual table editing weakly references the window controller, avoiding a strong document/window cycle.
            // Only a live editor can own the initial text responder.
            panel.setInitialFirstResponder(Some(editor.input_view()));
        }

        // Records only native roles explicitly created/configured here, without claiming ownership of AppKit's internal subtree.
        for (role, object) in [
            ("window.controller", &*controller as &AnyObject),
            ("window.content", &*content_view as &AnyObject),
            ("window.background", &*background as &AnyObject),
            ("window.header", &*header as &AnyObject),
            ("window.sidebar", &*sidebar as &AnyObject),
        ] {
            // This fixed construction is far below the ledger budget; failure indicates a broken construction invariant.
            owned_ledger
                .include(role, object)
                .expect("fixed Markdown chrome fits owned ledger");
        }
        for (role, control) in [
            (
                "header.exit",
                controller.ivars().exit_button.get().unwrap().as_super(),
            ),
            (
                "header.mode",
                controller
                    .ivars()
                    .presentation_mode_button
                    .get()
                    .unwrap()
                    .as_super(),
            ),
            (
                "header.status",
                controller.ivars().status_label.get().unwrap().as_super(),
            ),
            (
                "footer.mode",
                controller.ivars().footer_labels.get().unwrap()[0].as_super(),
            ),
            (
                "footer.save",
                controller.ivars().footer_labels.get().unwrap()[1].as_super(),
            ),
            (
                "footer.counts",
                controller.ivars().footer_labels.get().unwrap()[2].as_super(),
            ),
            ("header.title", note_title.as_super()),
            ("header.path", note_path.as_super()),
            ("sidebar.title", sidebar_title.as_super()),
            ("sidebar.location", workspace_location.as_super()),
        ] {
            // Controls and actual cells each require independent weak release proof; nil content/controller alone is insufficient.
            owned_ledger
                .include_control(role, control)
                .expect("fixed Markdown controls fit owned ledger");
        }
        if let Some(label) = &empty_label {
            // Empty-workspace construction records its placeholder too, rather than covering only successful document construction.
            owned_ledger
                .include_control("window.empty", label)
                .expect("fixed empty label fits owned ledger");
        }

        // All native controls/menus are recorded; the header observes the actual two rows weakly, leaving strong ownership with the original controls.
        let header_controls = NativeMarkdownHeaderControls::new(
            [
                note_title.as_super().as_super(),
                controller
                    .ivars()
                    .presentation_mode_button
                    .get()
                    .unwrap()
                    .as_super()
                    .as_super(),
                controller.ivars().workspace_controls.get().unwrap()[1].as_super(),
                controller
                    .ivars()
                    .status_label
                    .get()
                    .unwrap()
                    .as_super()
                    .as_super(),
                note_path.as_super().as_super(),
                controller.ivars().footer_labels.get().unwrap()[0]
                    .as_super()
                    .as_super(),
                controller.ivars().footer_labels.get().unwrap()[1]
                    .as_super()
                    .as_super(),
                controller.ivars().footer_labels.get().unwrap()[2]
                    .as_super()
                    .as_super(),
            ],
            toolbar_controls,
        );
        // Before global owner publication, each setter still rechecks local panel/controller identity and dimensions.
        if !header_controls.layout(
            &controller,
            &panel,
            NativeMarkdownHeaderAuthority::Construction,
        ) {
            // Reentry replaced the window or dimensions during construction; a partially updated toolbar cannot be published.
            return Err("笔记工具栏布局未能完成，请重新打开笔记窗口".to_owned());
        }
        assert!(
            controller
                .ivars()
                .header_controls
                .set(header_controls)
                .is_ok()
        );

        if let Some(editor) = &editor {
            controller.bind_document_area(editor.scroll_view());
            if !controller.layout_document_area(NativeMarkdownHeaderAuthority::Construction) {
                return Err("笔记正文与反馈布局未能完成，请重新打开笔记窗口".into());
            }
        }

        // Construction getters may emit input notifications; the first directory task binds the actual current intent instead of assuming zero.
        let sidebar_intent = Some(controller.workspace_query_intent());
        // Transfer the completed native graph and scalar transition state into the unique surface owner.
        Ok(Self {
            panel,
            controller,
            owned_ledger,
            presentation_mode,
            session,
            lifecycle,
            closing_request: None,
            notes,
            sidebar_pager: Some(sidebar_pager),
            collapsed_directories,
            sidebar_has_more,
            sidebar_scan_pending,
            sidebar_intent,
            sidebar_completion_policy: Default::default(),
            sidebar_results: None,
            sidebar_title,
            sidebar_rows,
            sidebar_list,
            note_title,
            note_path,
            current_note_relative_path,
            editor,
            empty_label,
            search_query: None,
            search_intent: None,
            search_cursor: None,
            search_authority: None,
            search_generation: 0,
            search_cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            search_worker: None,
            pending_search_query: None,
            search_results: None,
            pending_note_switch: None,
            pending_file_mutation: None,
            borrowed_close: None,
            modal_failure: None,
        })
    }

    /// Expose the already-owned native graph and request keyboard focus without creating another session.
    pub(super) fn show(&self) {
        // Only an explicit diagnostic control hides existing components before first paint; default product and formal feature gates remain unchanged.
        #[cfg(feature = "native-markdown-runtime-probe")]
        self.apply_memory_component_control();
        // Activate only after the root has retained this fully configured foreground surface.
        self.panel.center();
        let mtm = MainThreadMarker::new()
            .expect("native Markdown show is restricted to the AppKit main thread");
        NSApplication::sharedApplication(mtm).activate();
        self.panel.orderFrontRegardless();
        self.panel.makeKeyWindow();
        if let Some(editor) = &self.editor
            && !self.panel.makeFirstResponder(Some(editor.input_view()))
        {
            // Focus refusal leaves the mounted editor usable through a later explicit click.
            self.controller
                .present_error("编辑器暂时无法获得键盘焦点，请点按正文后继续");
        }
    }

    /// Isolates existing components of the actual window without creating document, layout, or persistent native owners.
    #[cfg(feature = "native-markdown-runtime-probe")]
    fn apply_memory_component_control(&self) {
        let Some(control) = std::env::var_os("OMNI_NATIVE_MEMORY_COMPONENT_CONTROL") else {
            // Without an explicit environment value, queries no native graph and changes no component visibility.
            return;
        };
        let (label, hide_gutter) = match control.to_str() {
            // Both fixed controls preserve the original scroll_view; only the second additionally hides its existing line-number ruler.
            Some("body_only") => ("body_only", false),
            Some("body_only_no_gutter") => ("body_only_no_gutter", true),
            _ => {
                // An unknown value selects no alternative control and is never copied into diagnostics.
                eprintln!(
                    "native_memory_component_control control=invalid formal=false hidden_components=0 invalid=true"
                );
                return;
            }
        };

        let mut hidden_components = 0_usize;
        let current = || native_markdown_controller_has_foreground_authority(&self.controller);
        let result = (|| -> Result<(), ()> {
            // The existing ledger's 1024-role bound limits the entire snapshot; over-budget input is rejected without truncation.
            const MAX_COMPONENT_SUBVIEWS: usize = 1_024;
            if MainThreadMarker::new().is_none() || !current() {
                // Closing or a non-main thread has no authority to hide native views.
                return Err(());
            }
            let editor = self.editor.as_ref().ok_or(())?;
            let content = self.panel.contentView().ok_or(())?;
            if !current() {
                // Getters may reenter; prior authority does not automatically authorize later view queries.
                return Err(());
            }
            let subviews = content.subviews();
            if !current() {
                // NSArray temporarily retains original subviews only during this show; revoked authority releases the snapshot immediately.
                return Err(());
            }
            let count = subviews.count();
            if count > MAX_COMPONENT_SUBVIEWS {
                // A snapshot exceeding the ledger budget hides no partial subset.
                return Err(());
            }
            let body: &NSView = editor.scroll_view().as_super();
            let mut body_matches = 0_usize;
            for index in 0..count {
                // Preflights the complete snapshot for the original scroll_view identity without recursively taking over AppKit internals.
                if !current() {
                    return Err(());
                }
                let child = subviews.objectAtIndex(index);
                if !current() {
                    return Err(());
                }
                if std::ptr::eq(&*child, body) {
                    // Only the actually mounted document is valid; a missing or duplicate identity invalidates the control.
                    body_matches += 1;
                }
            }
            if body_matches != 1 {
                // Without a unique document, preserves component state instead of inventing a replacement view.
                return Err(());
            }
            for index in 0..count {
                // Hides only peer components in the original snapshot, preserving frames, hierarchy, and document ownership.
                if !current() {
                    return Err(());
                }
                let child = subviews.objectAtIndex(index);
                if !current() {
                    return Err(());
                }
                if std::ptr::eq(&*child, body) {
                    // Does not change even the original scroll_view's hidden state.
                    continue;
                }
                child.setHidden(true);
                if !current() || !child.isHidden() || !current() {
                    // Revocation during any setter/getter invalidates this control without retry or renewed authority.
                    return Err(());
                }
                hidden_components += 1;
            }
            if hide_gutter {
                // Changes only the existing ruler view's hidden flag, preserving rulersVisible and document layout.
                if !current() {
                    return Err(());
                }
                let gutter = editor.scroll_view().verticalRulerView().ok_or(())?;
                gutter.setHidden(true);
                if !current() || !gutter.isHidden() || !current() {
                    return Err(());
                }
                hidden_components += 1;
            }
            Ok(())
        })();
        // All native snapshots and temporary claims are released; this control is neither formal functionality nor memory-acceptance evidence.
        let invalid = result.is_err() || !current();
        eprintln!(
            "native_memory_component_control control={label} formal=false hidden_components={hidden_components} invalid={invalid}"
        );
    }

    /// The installed document's actual identity updates list feedback only, without triggering another note selection.
    pub(super) fn update_selected_note(&self, selected_relative_path: Option<&str>) {
        self.sidebar_list.select_active(selected_relative_path);
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
            if let Err(error) = self.owned_ledger.include_control("window.empty", &label) {
                // An incompletely recorded control is not mounted; the existing native surface remains closable.
                self.controller.present_error(&error);
                return;
            }
            content_view.addSubview(&label);
            self.empty_label = Some(label);
        }
        if self.pending_note_switch.is_none() && self.pending_file_mutation.is_none() {
            // An ordinary empty workspace without a pending old graph still permits title/body search and later user opening.
            let borrowed = self.controller.ivars().borrowed.borrow().clone();
            self.controller
                .ivars()
                .field_editor_provider
                .resume(&borrowed);
            self.controller.set_workspace_input_enabled(true);
        }
        // Remove every action/focus affordance that would imply an active document still exists.
        self.panel.setInitialFirstResponder(None);
        self.note_title.setStringValue(ns_string!("笔记"));
        // Empty state must not expose the previous note's title through a tooltip.
        self.note_title.setToolTip(Some(ns_string!("笔记")));
        self.note_path.setStringValue(ns_string!(""));
        self.note_path.setToolTip(None);
        self.current_note_relative_path = None;
        self.controller.unbind_document_area();
        self.controller.install_footer_document(None);
        self.controller.set_copy_document_available(false);
        self.controller.set_table_insertion_available(false);
        self.update_selected_note(None);
    }

    /// Used only after failure and actual old-document release; rechecks the same empty owner after each native phase, while newer input revokes feedback but not necessary compensation.
    fn prepare_failed_mutation_reselection(
        &mut self,
        mtm: MainThreadMarker,
    ) -> Result<bool, String> {
        if !self.empty_workspace_compensation_is_current() {
            return Ok(false);
        }
        let content = self
            .panel
            .contentView()
            .ok_or("原生 Markdown 窗口没有内容视图")?;
        if !self.empty_workspace_compensation_is_current() {
            return Ok(false);
        }
        let message = "请从目录重新选择笔记，并核对当前文件状态";
        if let Some(label) = &self.empty_label {
            // Reuses the existing placeholder without creating a recovery window or another native graph.
            label.setStringValue(&NSString::from_str(message));
        } else {
            let label = native_markdown_empty_label(message, mtm);
            if !self.empty_workspace_compensation_is_current() {
                // Drops an unpublished candidate locally; closure or a new owner cannot receive controls recreated by an old error.
                return Ok(false);
            }
            self.owned_ledger.include_control("window.empty", &label)?;
            content.addSubview(&label);
            if !self.empty_workspace_compensation_is_current() {
                // An unpublished candidate removes only itself, leaving successor controls untouched.
                label.removeFromSuperview();
                return Ok(false);
            }
            self.empty_label = Some(label);
        }
        if !self.empty_workspace_compensation_is_current() {
            return Ok(false);
        }
        self.panel.setInitialFirstResponder(None);
        if !self.empty_workspace_compensation_is_current() {
            return Ok(false);
        }
        self.note_title.setStringValue(ns_string!("笔记"));
        if !self.empty_workspace_compensation_is_current() {
            return Ok(false);
        }
        self.note_title.setToolTip(Some(ns_string!("笔记")));
        if !self.empty_workspace_compensation_is_current() {
            return Ok(false);
        }
        // Failure compensation clears only this empty owner's old identity, honoring revocation after each native call.
        self.note_path.setStringValue(ns_string!(""));
        if !self.empty_workspace_compensation_is_current() {
            return Ok(false);
        }
        self.note_path.setToolTip(None);
        if !self.empty_workspace_compensation_is_current() {
            return Ok(false);
        }
        self.current_note_relative_path = None;
        self.controller.unbind_document_area();
        self.controller.install_footer_document(None);
        self.controller.set_copy_document_available(false);
        if !self.empty_workspace_compensation_is_current() {
            return Ok(false);
        }
        self.controller.set_table_insertion_available(false);
        if !self.empty_workspace_compensation_is_current() {
            return Ok(false);
        }
        // Pending work is consumed and the old graph strictly released; resumes the original query provider without reviving an old field editor.
        let borrowed = self.controller.ivars().borrowed.borrow().clone();
        self.controller
            .ivars()
            .field_editor_provider
            .resume(&borrowed);
        self.controller.set_workspace_input_enabled_while(true, || {
            self.empty_workspace_compensation_is_current()
        });
        Ok(self.empty_workspace_compensation_is_current())
    }

    /// Mount one editor only after the previous release barrier and publish its authoritative identity.
    pub(super) fn install_editor(
        &mut self,
        content_view: &NSView,
        editor: NativeMarkdownEditorSurface,
        relative_path: String,
    ) {
        self.install_editor_with_feedback(
            content_view,
            editor,
            relative_path,
            NativeMarkdownEditorFeedback::ExistingCaller,
        );
    }

    /// Document/control compensation still completes; visible feedback and focus alone require the original file-operation intent.
    fn install_editor_with_feedback(
        &mut self,
        content_view: &NSView,
        editor: NativeMarkdownEditorSurface,
        relative_path: String,
        feedback: NativeMarkdownEditorFeedback,
    ) {
        if let Some(empty_label) = self.empty_label.take() {
            // A newly mounted editor supersedes and releases the old empty-state control.
            empty_label.removeFromSuperview();
        }
        // Mount after release verification, then publish the authoritative title/identity together.
        content_view.addSubview(editor.scroll_view());
        // Core opened and validated this note; title uses the same file_stem rule independently of asynchronously refreshed directory pages.
        // The directory name retains full filename semantics, including collision suffixes and interior dots.
        let title = std::path::Path::new(&relative_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("Core已验证所安装笔记的UTF-8文件名");
        let native_title = NSString::from_str(title);
        self.note_title.setStringValue(&native_title);
        // Truncation is cell presentation only; the actual string, accessibility value, and tooltip preserve the full title.
        self.note_title.setToolTip(Some(&native_title));
        let native_path = NSString::from_str(&relative_path);
        self.note_path.setStringValue(&native_path);
        self.note_path.setToolTip(Some(&native_path));
        // Switch, rename, and move derive identity from the actual installation receipt rather than sidebar pages or substituted directory names.
        self.current_note_relative_path = Some(relative_path.clone());
        self.controller.set_copy_document_available(true);
        self.controller.set_table_insertion_available(true);
        self.update_selected_note(Some(&relative_path));
        self.editor = Some(editor);
        let installed = self.editor.as_ref().expect("原editor刚完成唯一安装");
        self.controller.bind_document_area(installed.scroll_view());
        let footer = NativeMarkdownFooterDocument::capture(&installed.document().borrow());
        self.controller
            .bind_conflict_note(NativeMarkdownCurrentNoteIntent::for_document(
                &self.controller,
                &self.panel,
                installed.document(),
                &relative_path,
            ));
        self.controller.install_footer_document(Some(footer));
        self.controller
            .layout_document_area(NativeMarkdownHeaderAuthority::Foreground);
        let borrowed = self.controller.ivars().borrowed.borrow().clone();
        self.controller
            .ivars()
            .field_editor_provider
            .resume(&borrowed);
        // Resumes query clients only after consuming old-graph receipts and publishing the new canonical owner.
        self.controller.set_workspace_input_enabled(true);

        // The installed owner now supports focus and user actions without an outstanding old editor.
        let editor = self
            .editor
            .as_ref()
            .expect("native Markdown editor was installed immediately above");
        if feedback.permits(self) {
            // New query input during the file operation keeps its focus; document compensation still publishes a usable editor.
            self.panel
                .setInitialFirstResponder(Some(editor.input_view()));
        }
        if feedback.permits(self) {
            // The preceding setter may also reenter; clears only status still owned by this original feedback intent.
            self.controller.clear_status();
        }
        if feedback.permits(self)
            && !self.panel.makeFirstResponder(Some(editor.input_view()))
            && feedback.permits(self)
        {
            // Focus failure also belongs to original feedback and cannot be shown after getters reenter newer input.
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
        native_markdown_editor_surface(
            document,
            editor_frame,
            self.presentation_mode,
            &self.owned_ledger,
            mtm,
        )
    }

    /// Changes layout policy on the same document; window mode, buttons, and future-open defaults follow the actual accepted result.
    pub(super) fn set_presentation_mode(
        &mut self,
        mode: MarkdownPresentationMode,
    ) -> Result<(), String> {
        if self.presentation_mode == mode {
            // Re-selecting the current mode needs no text/style invalidation or ownership change.
            self.controller.select_presentation_mode(mode);
            return Ok(());
        }
        // Find fields may hold Chinese composition too; normal window resignation preflight preserves candidates in their original input client.
        let previous = self.panel.firstResponder();
        let find_focus = self.capture_mode_find_focus();
        #[cfg(feature = "native-markdown-runtime-probe")]
        {
            // Records only the originally captured bounded selection and actual client relationship, without reading field values or granting authority from diagnostics.
            // Existing capture/restore/verify exits distinguish pre-capture changes, immediate restoration failure, and later focus loss.
            let actual = find_focus.as_ref().and_then(|(field, _)| {
                field
                    .currentEditor()
                    .and_then(|editor| editor.downcast::<NSTextView>().ok())
            });
            let responder_same = actual.as_ref().is_some_and(|actual| {
                previous.as_ref().is_some_and(|responder| {
                    std::ptr::from_ref(&**responder).addr() == std::ptr::from_ref(&**actual).addr()
                })
            });
            let marked = actual.as_ref().map(|actual| {
                ProtocolObject::<dyn NSTextInputClient>::from_ref(&**actual).hasMarkedText()
            });
            eprintln!(
                "native_mode_find_focus phase=capture from={:?} to={mode:?} captured={} saved_range={:?} responder_same={responder_same} marked={marked:?} key_window={} app_active={}",
                self.presentation_mode,
                find_focus.is_some(),
                find_focus.as_ref().and_then(|(_, selected)| *selected),
                self.panel.isKeyWindow(),
                NSApplication::sharedApplication(self.panel.mtm()).isActive(),
            );
        }
        self.commit_marked_text(false)?;
        self.resign_editor_responder()?;
        if let Some(editor) = &mut self.editor
            && let Err(error) = editor.set_presentation_mode(mode, &self.controller)
        {
            // Pre-admission rejection preserves the old mode; native reentry after acceptance may revoke position restoration,
            // but a retired Producer cannot be rolled back. Reads the actual document mode to keep buttons and content consistent.
            if let Some(editor) = &self.editor {
                self.presentation_mode = editor.presentation_mode();
            }
            self.controller
                .select_presentation_mode(self.presentation_mode);
            if self.presentation_mode == mode
                && let Some(editor) = &self.editor
                && let Err(mount) = editor.finish_mode_viewport_mount(&self.controller)
            {
                // Native input may revoke positioning after mode acceptance; still finalizes the original unbound intent rather than leaving an indefinitely pending document.
                return Err(format!("{error}；{mount}"));
            }
            if let Some((field, range)) = find_focus {
                self.restore_mode_find_focus(&field, range)
                    .map_err(|restore| format!("{error}；{restore}"))?;
            } else if let Some(previous) = previous
                && !self.panel.makeFirstResponder(Some(&previous))
            {
                return Err(format!("{error}；无法恢复原输入焦点"));
            }
            return Err(error);
        }
        // Without a document, changes only the next-open default; an existing document must have actually accepted the target mode.
        self.presentation_mode = self
            .editor
            .as_ref()
            .map_or(mode, NativeMarkdownEditorSurface::presentation_mode);
        self.controller
            .select_presentation_mode(self.presentation_mode);
        if let Some(editor) = &self.editor {
            // Document, scroll, Find bar, and field clients remain the same instances; recomputes only geometry affected by ruler visibility.
            // Repeated addSubview or Find rebinding would alter hierarchy and focus beyond this mode action.
            self.controller
                .layout_document_area(NativeMarkdownHeaderAuthority::Foreground);
            self.panel
                .setInitialFirstResponder(Some(editor.input_view()));
            let focus = if let Some((field, range)) = find_focus {
                self.restore_mode_find_focus(&field, range)
            } else if self.panel.makeFirstResponder(Some(editor.input_view())) {
                Ok(())
            } else {
                Err("展示模式已切换，但正文暂时无法获得键盘焦点".into())
            };
            // Focus failure must still discharge mounting, otherwise an accepted mode would remain at controller=None indefinitely.
            // Position uses only the original anchor; Body revokes that same intent on binding failure and resumes ordinary layout for the current mode.
            let mounted = editor.finish_mode_viewport_mount(&self.controller);
            match (focus, mounted) {
                (Err(focus), Err(mount)) => return Err(format!("{focus}；{mount}")),
                (Err(cause), Ok(())) | (Ok(()), Err(cause)) => return Err(cause),
                (Ok(()), Ok(())) => {}
            }
        }
        Ok(())
    }

    /// Mode actions retain only the original actual Find client and bounded NSRange; a shared field-editor address cannot identify query versus replacement.
    fn capture_mode_find_focus(&self) -> Option<(Retained<NSTextField>, Option<NSRange>)> {
        let bar = self
            .editor
            .as_ref()
            .and_then(|editor| editor.find_bar().borrow().clone())?;
        let responder = self
            .panel
            .firstResponder()
            .and_then(|view| view.downcast::<NSTextView>().ok())?;
        for field in bar.fields() {
            let actual = field.currentEditor();
            if actual.as_ref().is_some_and(|actual| {
                std::ptr::eq::<objc2_app_kit::NSText>(&**actual, responder.as_super())
            }) {
                let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*responder);
                // Actual resignation below completes composition; its internal selection cannot be restored as an ordinary committed query selection.
                let selected = (!input.hasMarkedText()).then(|| responder.selectedRange());
                return Some((
                    field,
                    selected.filter(|range| range.location != NSNotFound as usize),
                ));
            }
        }
        None
    }

    /// Restores selection after the same field reacquires its actual native client, without reconstructing controls or copying query text/Undo.
    fn restore_mode_find_focus(
        &self,
        field: &NSTextField,
        selected: Option<NSRange>,
    ) -> Result<(), String> {
        // SAFETY: Mode handoff retains the sole window owner; field belongs to this note's original Find bar and was neither cleaned nor replaced.
        unsafe {
            field.selectText(None);
        }
        let editor = field
            .currentEditor()
            .and_then(|editor| editor.downcast::<NSTextView>().ok())
            .ok_or("展示模式已切换，但原查找字段未恢复输入")?;
        if let Some(selected) = selected {
            let length = editor.string().length(); // Reads native field length only, without materializing another query or document copy.
            if selected
                .location
                .checked_add(selected.length)
                .is_none_or(|end| end > length)
            {
                return Err("原查找输入在模式交接期间改变，未套用过期选区".into());
            }
            editor.setSelectedRange(selected);
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        {
            // A public setter return is not a focus/selection receipt; observes immediate restoration scalars without forcing focus or retrying.
            let responder_same = self.panel.firstResponder().is_some_and(|responder| {
                std::ptr::from_ref(&*responder).addr() == std::ptr::from_ref(&*editor).addr()
            });
            eprintln!(
                "native_mode_find_focus phase=restore saved_range={selected:?} actual_range={:?} responder_same={responder_same} key_window={} app_active={}",
                editor.selectedRange(),
                self.panel.isKeyWindow(),
                NSApplication::sharedApplication(self.panel.mtm()).isActive(),
            );
        }
        Ok(())
    }

    /// Delegate insertion to the active editor while refusing empty-state actions.
    pub(super) fn insert_default_table(&self) -> Result<(), String> {
        // A stale native action cannot insert into a document that has already entered empty state.
        let editor = self
            .editor
            .as_ref()
            .ok_or_else(|| "当前没有可编辑的 Markdown 笔记".to_owned())?;
        editor
            .text_view
            .insert_default_table()
            .map_err(|cause| cause.to_string())?;
        Ok(())
    }

    /// Normally resigns all actual clients; even a workspace without a document may have composition in its search field.
    pub(super) fn resign_editor_responder(&self) -> Result<(), String> {
        self.controller.ivars().undo.preflight()?;
        let borrowed = self.controller.ivars().borrowed.borrow().clone();
        borrowed.capture(&self.panel)?;
        if let Some(editor) = &self.editor {
            // Records original Find/table clients first rather than assuming the window can still identify them after resignation.
            editor.capture_find_resources()?;
        }
        let generation_before = self
            .editor
            .as_ref()
            .map(|editor| editor.document().borrow().generation());
        let previous = self.panel.firstResponder();
        if !self.panel.makeFirstResponder(None) {
            // Native validation refusal performs no accepted cleanup, preserving original fields, drafts, and focus.
            return Err("笔记输入尚未结束，请完成输入后重试".into());
        }
        let result = (|| {
            if self.panel.firstResponder().is_some_and(|responder| {
                previous
                    .as_ref()
                    .is_some_and(|previous| std::ptr::eq(&**previous, &*responder))
                    && !std::ptr::eq(
                        std::ptr::from_ref(&*responder).cast::<()>(),
                        std::ptr::from_ref(&*self.panel).cast::<()>(),
                    )
            }) {
                // Nominal success must still leave neither the original field nor document as the actual input target.
                return Err("上一笔记的输入仍未结束".to_owned());
            }
            if self
                .editor
                .as_ref()
                .map(|editor| editor.document().borrow().generation())
                != generation_before
            {
                // A canonical edit during resignation must remain as a draft for explicit saving.
                return Err("结束输入时正文发生变更，草稿将保持打开以供保存".to_owned());
            }
            borrowed.capture(&self.panel)?;
            borrowed.preflight_unmarked()?;
            // Resignation may reenter native Undo provision; before close acceptance, Busy rejects without freezing or clearing history.
            self.controller.ivars().undo.preflight()?;
            if let Some(editor) = &self.editor {
                editor.capture_find_resources()?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Some(previous) = previous
                && !self.panel.makeFirstResponder(Some(&previous))
            {
                // Reports restoration failure honestly; focus loss is neither successful closure nor permission to clear input.
                return Err(format!("{error}；无法恢复原输入焦点"));
            }
            return Err(error);
        }
        self.panel.setInitialFirstResponder(None);
        Ok(())
    }

    /// Finishes shared editing after preflight and handoff acceptance, without committing new document text or saving again.
    fn finish_borrowed_editing(
        &mut self,
        close_window: bool,
    ) -> (
        NativeMarkdownBorrowedEditorSnapshot,
        Option<Weak<NativeMarkdownSessionFieldEditor>>,
    ) {
        let borrowed = self
            .controller
            .ivars()
            .borrowed
            .replace(NativeMarkdownBorrowedEditorSnapshot::default());
        let mut fields: Vec<Retained<NSTextField>> = if close_window {
            // Clears query fields only on window closure; note switches preserve window-level query values.
            self.controller
                .ivars()
                .workspace_controls
                .get()
                .into_iter()
                .flatten()
                .filter_map(|control| control.clone().downcast::<NSTextField>().ok())
                .collect()
        } else {
            Vec::new()
        };
        if let Some(editor) = &self.editor {
            // Cell and Find fields are retiring clients and must not leave business data in the next note's shared cache.
            let find = editor.find_resources().borrow().clone();
            fields.extend(find.fields());
        }
        let field_refs: Vec<_> = fields.iter().map(|field| &**field).collect();
        // Freezes the provider before accepted client completion so cleanup reentry cannot allocate another field editor.
        self.controller.ivars().field_editor_provider.freeze();
        let released_field_editor = borrowed.finish_accepted(
            &self.panel,
            &field_refs,
            &self.controller.ivars().undo,
            &self.controller.ivars().field_editor_provider,
            close_window,
        );
        if let Some(editor) = &self.editor {
            *editor.session_field_editor().borrow_mut() = released_field_editor.clone();
            let mut find = editor
                .find_resources()
                .replace(NativeMarkdownFindResources::default());
            // Native clearing may reenter, so no editor RefMut spans it; restores only weak metadata receipts afterward.
            find.finish_accepted();
            *editor.find_resources().borrow_mut() = find;
            let container =
                ProtocolObject::<dyn NSTextFinderBarContainer>::from_ref(editor.scroll_view());
            container.setFindBarVisible(false);
        }
        (borrowed, released_field_editor)
    }

    /// End the actual input client before accepting teardown and return its real release barrier.
    pub(super) fn destroy_current_editor(
        &mut self,
    ) -> Result<Option<NativeMarkdownEditorReleaseBarrier>, String> {
        // Capture the original model before the first reentrant preflight. A later Native getter,
        // resignation, or final responder cleanup cannot silently discard newly committed input.
        let original = self.editor.as_ref().map(|editor| {
            (
                Rc::downgrade(editor.document()),
                editor.document().borrow().generation(),
            )
        });
        // A live modal owner blocks resignation and accepted cleanup of its parent editor.
        self.preflight_modal_boundary()?;
        self.resign_editor_responder()?;
        let unchanged = match (&original, self.editor.as_ref()) {
            (None, None) => true,
            (Some((document, generation)), Some(editor)) => {
                std::rc::Weak::ptr_eq(document, &Rc::downgrade(editor.document()))
                    && editor.document().borrow().generation() == *generation
            }
            _ => false,
        };
        if !unchanged {
            // No provider is frozen and no history/model is taken yet. Keep the actual new input
            // in the original editor; the caller must save or explicitly resolve it again.
            return Err("结束输入时正文发生变更，草稿将保持打开以供保存".to_owned());
        }
        // No Native call separates the final scalar check from accepting teardown. Cancel the
        // original autosave so its delayed work cannot cross into a successor document.
        self.controller.cancel_autosave();
        self.controller.set_workspace_input_enabled(false);
        let (borrowed, field_editor) = self.finish_borrowed_editing(false);
        // Revoke the information panel's original weak document and dynamic values before detaching the graph.
        self.controller.hide_information_panel();
        // Unbind weak layout references before accepted teardown; later feedback cannot remount the old scroll.
        self.controller.unbind_document_area();
        let Some(editor) = self.editor.take() else {
            // An empty body may still own a search field editor. Its actual release remains part
            // of the barrier before the successor can acquire any document graph.
            self.note_path.setStringValue(ns_string!(""));
            self.note_path.setToolTip(None);
            self.current_note_relative_path = None;
            self.controller.unbind_document_area();
            self.controller.install_footer_document(None);
            self.update_selected_note(None);
            return Ok(Some(
                NativeMarkdownEditorReleaseBarrier::for_borrowed_client(
                    borrowed,
                    field_editor,
                    self.owned_ledger.snapshot_modal(),
                    self.panel.mtm(),
                ),
            ));
        };
        self.controller.set_copy_document_available(false);
        self.controller.set_table_insertion_available(false);
        // Callers committed and saved composition before this irreversible ownership transition.
        let mut release_barrier = destroy_native_markdown_editor(editor);
        release_barrier.borrowed = Some(borrowed);
        // The old body has ended; do not expose its revoked relative path while waiting for the successor.
        self.note_path.setStringValue(ns_string!(""));
        self.note_path.setToolTip(None);
        self.current_note_relative_path = None;
        self.controller.unbind_document_area();
        self.controller.install_footer_document(None);
        self.update_selected_note(None);
        Ok(Some(release_barrier))
    }

    /// Save the old authority and stage a single note switch only after safely releasing its editor.
    pub(super) fn begin_note_switch(
        &mut self,
        relative_path: &str,
        fragment: Option<String>,
    ) -> Result<bool, String> {
        // Even selecting the same note cannot take the active modal's responder.
        self.preflight_modal_boundary()?;
        // Sidebar pages are bounded discovery views, not an authorization registry. Imported/new
        // notes may be outside the current page; open_note_editor revalidates their confined path
        // after the old graph releases, and its failure path restores the already-saved original.
        if self.pending_file_mutation.is_some() {
            // A pending mutation owns path/revision authority until its release barrier completes.
            return Err("文件操作仍在完成中".to_owned());
        }
        if let Some(pending_switch) = &self.pending_note_switch {
            // Coalesce the same destination while preserving the first accepted transition.
            return if pending_switch.relative_path == relative_path
                && pending_switch.fragment == fragment
            {
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
                self.panel.makeFirstResponder(Some(editor.input_view()));
                if let Some(fragment) = fragment.as_deref() {
                    editor
                        .body
                        .navigate_heading(fragment)
                        .map_err(|cause| cause.to_string())?;
                }
            }
            return Ok(false);
        }

        let previous_relative_path = self.current_note_relative_path.clone();

        // Marked-text commit may produce the final piece-tree edit, so it precedes saving.
        // A failed save leaves the old editor and its in-memory draft intact for user recovery.
        self.commit_marked_text(false)?;
        self.save_current_document()?;
        let release_barrier = self.destroy_current_editor()?;
        self.pending_note_switch = Some(PendingNativeMarkdownNoteSwitch {
            relative_path: relative_path.to_owned(),
            previous_relative_path,
            release_barrier,
            fragment,
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
        let editor_frame =
            native_markdown_editor_frame(&content_view, self.controller.feedback_visible());

        // The actual old graph has released. Acquire the sole destination through the same
        // confined UTF-8 Markdown opener; no alternate source or historical-record bypass exists.
        let opened = self.open_note_editor(&pending_switch.relative_path, editor_frame, mtm);
        match opened {
            // A valid destination becomes the sole mounted editor after old release verification.
            Ok(editor) => {
                // Update the sidebar only after the target really opens; failure compensation below
                // must not overwrite the user's previous browse location.
                self.sidebar_list
                    .select_directory(&native_markdown_note_parent(&pending_switch.relative_path));
                self.install_editor(&content_view, editor, pending_switch.relative_path);
                if let Some(fragment) = pending_switch.fragment.as_deref() {
                    // Only the installed target's complete Input can authorize heading navigation;
                    // the old source and release barrier grant no target location authority.
                    self.editor
                        .as_ref()
                        .expect("刚安装的唯一目标正文")
                        .body
                        .navigate_heading(fragment)
                        .map_err(|cause| cause.to_string())?;
                }
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

    /// Save/revision-check the active note when required and stage one mutation behind editor teardown.
    pub(super) fn begin_file_mutation(
        &mut self,
        mutation: NativeMarkdownFileMutation,
    ) -> Result<(), String> {
        // Fixes feedback authority before native modal/IME/save/release work; revocation does not block necessary operation compensation.
        let feedback_intent = self.controller.workspace_query_intent();
        // Rejects file and document effects while modal response, candidate, or local resources remain unfinished.
        self.preflight_modal_boundary()?;
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
            self.commit_marked_text(false)?;
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
            // The parent document keeps the same owner, but the finished modal's default nil role still requires actual event completion.
            Some(NativeMarkdownEditorReleaseBarrier::for_modal_roles(
                self.owned_ledger.snapshot_modal(),
                self.panel.mtm(),
            ))
        };
        if self.workspace_refresh_intent_is_current(feedback_intent) {
            // Actual didChange or internal save may legitimately advance the epoch; conservatively preserves feedback instead of recapturing current at the end.
            self.controller.clear_status();
        }
        self.pending_file_mutation = Some(PendingNativeMarkdownFileMutation {
            mutation,
            active_relative_path_before,
            expected_revision,
            release_barrier,
            editor_was_released: release_editor,
            feedback_intent,
        });
        Ok(())
    }

    /// Executes once after strict release; failure may follow publication, so compensation uses original authority without assuming unchanged disk.
    pub(super) fn finish_file_mutation(
        &mut self,
        mtm: MainThreadMarker,
        feedback_intent: &mut Option<u64>,
    ) -> Result<(), String> {
        let Some(pending_mutation) = &self.pending_file_mutation else {
            // Coalesced or duplicate callbacks do not execute an already-consumed mutation again.
            return Ok(());
        };
        if let Some(release_barrier) = &pending_mutation.release_barrier
            && let Err(error) = release_barrier.verify_released()
        {
            if pending_mutation.editor_was_released && self.editor.is_none() {
                // Shows empty state only after actual old-document release; modal-only failure preserves the document, path/title, and selection.
                let content_view = self
                    .panel
                    .contentView()
                    .ok_or_else(|| "原生 Markdown 窗口没有内容视图".to_owned())?;
                self.show_empty_state(&content_view, "文件操作暂时不可用", mtm);
            }
            // Retains the same pending operation and primary failure for feedback, without retry, clearing a live document, or touching file authority.
            return Err(error);
        }

        // Take exactly one verified transition before executing its confined authoritative operation.
        let pending_mutation = self
            .pending_file_mutation
            .take()
            .expect("native Markdown file mutation was checked immediately above");
        // A modal-only barrier grants no authority to reconstruct or restore a document that is still alive.
        let editor_was_released = pending_mutation.editor_was_released;
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
                    // Attempts only the original path with fresh disk validation, without a document mirror or an assumption that failure changed nothing.
                    return match self.install_note_from_authority_with_feedback(
                        &previous_path,
                        mtm,
                        NativeMarkdownEditorFeedback::FileMutation(*feedback_intent),
                    ) {
                        // Recovery succeeded, so report the mutation error with the original note usable.
                        Ok(()) => Err(mutation_error),
                        // Err may follow actual publication or partial changes; if the old path cannot reopen, directs the user to the actual directory.
                        Err(recovery_error) => {
                            let expected_intent = *feedback_intent;
                            *feedback_intent = None;
                            let error = format!(
                                "{mutation_error}；文件操作可能已更改磁盘，持久化状态未确认；原笔记恢复未确认：{recovery_error}；请从目录重新选择笔记并核对当前文件状态"
                            );
                            if let Some(intent) = expected_intent {
                                // Any native reentry delivering new input or closure revokes this old error's UI and refresh authority.
                                // Fixes the still-reportable original epoch first; preparation failure also retains the primary mutation error.
                                *feedback_intent = Some(intent);
                                let prepared = match self.prepare_failed_mutation_reselection(mtm) {
                                    Ok(prepared) => prepared,
                                    Err(preparation_error) => {
                                        return Err(format!(
                                            "{error}；恢复界面未完成：{preparation_error}"
                                        ));
                                    }
                                };
                                if prepared {
                                    let refresh = self.refresh_workspace_sidebar_for_reselection(
                                        mtm,
                                        intent,
                                        feedback_intent,
                                    );
                                    if let Err(refresh_error) = refresh {
                                        // Never replays the file operation; directory reread failure is a secondary fact attached to the primary error.
                                        return Err(format!(
                                            "{error}；目录重新读取失败：{refresh_error}"
                                        ));
                                    }
                                }
                            }
                            Err(error)
                        }
                    };
                }
                // Unaffected editors stayed mounted, or no prior note existed to restore.
                return Err(mutation_error);
            }
        };

        // After actual disk success, consumes the same directory receipt before refreshing bounded projections; failure never guesses selection or rollback.
        self.sidebar_list
            .apply_directory_mutation(&pending_mutation.mutation, &mutation_result);
        let mut fresh_sidebar = false;
        let refresh_result = if let Some(intent) = *feedback_intent
            && self.workspace_refresh_intent_is_current(intent)
        {
            // Core confirms creation under the original directory action; expands only the new item's actual ancestors,
            // making it visible in collapsed directories without changing unrelated folders or acquiring newer query-feedback authority.
            match &mutation_result {
                NativeMarkdownFileMutationResult::NoteCreated(path)
                | NativeMarkdownFileMutationResult::DirectoryCreated(path) => {
                    for parent in Path::new(path).ancestors().skip(1) {
                        // Core validated UTF-8 paths; the empty root path has no collapsed marker, so removing it is naturally a no-op.
                        self.collapsed_directories
                            .remove(parent.to_str().expect("Core 创建收据的目录必须是 UTF-8"));
                    }
                }
                // Rename, move, and delete preserve existing collapsed state rather than treating every operation as creation.
                _ => {}
            }
            // Continues only begin's original intent; Weak/barrier reentry into newer input cannot authorize cancelling that input.
            let result = self.refresh_workspace_sidebar_with_policy(
                mtm,
                intent,
                super::workspace_browse_policy::NativeMarkdownBrowseCompletionPolicy::OpenFirstIfEmpty,
                feedback_intent,
            );
            fresh_sidebar = result.is_ok()
                && feedback_intent
                    .is_some_and(|intent| self.workspace_refresh_intent_is_current(intent));
            result
        } else {
            // Revoked business refresh does not cancel actual receipt/editor compensation; old notes do not prove the new directory state.
            *feedback_intent = None;
            Ok(())
        };
        // After deleting the active note/subtree, only a successful authority refresh may choose
        // the next note: the old snapshot could still name the deleted path. Creation/rename/move/
        // copy can instead use the exact committed receipt path even if sidebar refresh fails.
        let first_remaining_note = if fresh_sidebar {
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
                if !matches!(
                    pending_mutation.mutation,
                    NativeMarkdownFileMutation::RenameDirectory { .. }
                ) {
                    // Successful note identity changes use the new parent; directory rename already preserved selected descendants from its actual receipt.
                    self.sidebar_list
                        .select_directory(&native_markdown_note_parent(&relative_path));
                }
                self.install_note_from_authority_with_feedback(
                    &relative_path,
                    mtm,
                    NativeMarkdownEditorFeedback::FileMutation(*feedback_intent),
                )?;
            } else {
                // No valid remaining identity means empty state, never reopening a deleted old path.
                let content_view = self
                    .panel
                    .contentView()
                    .ok_or_else(|| "原生 Markdown 窗口没有内容视图".to_owned())?;
                let message = if fresh_sidebar && self.sidebar_scan_pending {
                    // The new directory is still scanning in a bounded worker; an incomplete page cannot be called an empty vault.
                    "操作已完成，正在读取笔记目录…"
                } else if fresh_sidebar {
                    // A complete page without an openable note remains in normal directory state awaiting explicit user selection.
                    "请选择笔记，或继续查看更多笔记"
                } else {
                    // Skipping an old refresh cannot imply an empty vault; preserves normal new-query and reselection entry points.
                    "操作已完成，请重新选择笔记"
                };
                self.show_empty_state(&content_view, message, mtm);
            }
        } else {
            // The active note was unaffected and keeps its existing editor/mapping.
            self.update_selected_note(self.current_note_relative_path.as_deref());
        }

        if let Err(refresh_error) = refresh_result {
            // The mutation already committed; report the secondary refresh error without replaying it.
            return Err(format!("文件操作已完成，但侧栏刷新失败：{refresh_error}"));
        }
        if feedback_intent.is_some_and(|intent| self.workspace_refresh_intent_is_current(intent)) {
            // Necessary document compensation does not authorize the old operation to clear newer input feedback.
            self.controller.clear_status();
        }
        Ok(())
    }

    /// File operations carry the original feedback intent from the earliest entry without renewing it during construction or mounting.
    fn install_note_from_authority_with_feedback(
        &mut self,
        relative_path: &str,
        mtm: MainThreadMarker,
        feedback: NativeMarkdownEditorFeedback,
    ) -> Result<(), String> {
        // Resolve layout from the live panel and let the session revalidate the requested authority path.
        let content_view = self
            .panel
            .contentView()
            .ok_or_else(|| "原生 Markdown 窗口没有内容视图".to_owned())?;
        let editor_frame =
            native_markdown_editor_frame(&content_view, self.controller.feedback_visible());
        let editor = self.open_note_editor(relative_path, editor_frame, mtm)?;
        self.install_editor_with_feedback(
            &content_view,
            editor,
            relative_path.to_owned(),
            feedback,
        );
        Ok(())
    }

    /// Persist current selection and stream piece-backed changes through the session revision contract.
    pub(super) fn save_current_document(&mut self) -> Result<(), String> {
        // Explicit/transition saves consume any pending debounce rather than leaving a second write queued.
        self.controller.cancel_autosave();
        let Some(editor) = &self.editor else {
            // An empty surface has no unsaved canonical document or selection to persist.
            return Ok(());
        };
        // Actual native selection callbacks already settled selection; save must not flatten a reversed head into an ascending NSRange.
        let mut document = editor.document().borrow_mut();
        // Completes actual save and releases the mutable document borrow before query-control cleanup or resubmission.
        let was_dirty = document.is_dirty();
        let saved = self.session.save_document_buffer(&mut document).map(|_| ());
        let footer = NativeMarkdownFooterDocument::capture(&document);
        drop(document);
        // Projects the actual typed save result with no canonical mutable borrow spanning native setters.
        self.controller
            .finish_footer_save(footer, saved.as_ref().err());
        if saved.is_ok() {
            // Rereads disk modification time only after actual save success; failed writes or unconfirmed sync do not establish new metadata.
            self.controller.refresh_saved_information(editor.document());
        }
        let saved = saved.map_err(|error| error.to_string());
        let revoked_page = self
            .search_query
            .as_deref()
            .zip(self.search_authority.as_ref())
            .is_some_and(|(query, authority)| !authority.is_current(&self.session, query));
        if self.search_query.is_some() && ((saved.is_ok() && was_dirty) || revoked_page) {
            // Actual publication revokes old pages and queued results; no-op saves do not repeat search, and failure preserves the primary save error.
            if let Err(error) = self.refresh_search_after_save(saved.is_ok())
                && saved.is_ok()
            {
                // Search resubmission failure does not relabel a successful save; callers still report the primary error for actual save failure.
                self.controller.present_error(&error);
            }
        }
        saved
    }

    /// The same original composition boundary distinguishes close from save/mode handoff; only close revokes unpublished presentation work.
    pub(super) fn commit_marked_text(&self, closing: bool) -> Result<(), String> {
        self.controller.ivars().undo.preflight()?;
        let borrowed = self.controller.ivars().borrowed.borrow().clone();
        borrowed.capture(&self.panel)?;
        let Some(editor) = &self.editor else {
            // No editor implies no cell or input-method composition can contribute a final edit.
            return Ok(());
        };
        editor.capture_find_resources()?;
        editor.commit_marked_text(closing)?;
        Ok(())
    }

    /// Explicit copy prepares bounded independent output first, releases the canonical borrow, then delivers to the reentrant system pasteboard.
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
        self.commit_marked_text(false)?;
        let editor = self
            .editor
            .as_ref()
            .ok_or_else(|| "当前没有可复制的 Markdown 笔记".to_owned())?;
        let output = {
            // This phase performs original Core reads/file operations only; fixed scratch never creates a whole-document String or NSString.
            let document = editor
                .document()
                .try_borrow()
                .map_err(|_| "Markdown 正文仍在完成本次编辑".to_owned())?;
            MarkdownPasteboardOutput::prepare(&document).map_err(|error| error.to_string())?
        };
        // The borrow is released and output holds no document/Core/FD; native output ownership does not extend plugin or original mapping lifetime.
        output
            .publish(pasteboard)
            .map_err(|error| error.to_string())
    }

    /// Stop foreground search and detach control/responder callbacks before the window close notification.
    pub(super) fn prepare_to_close(&mut self) {
        self.controller.set_workspace_input_enabled(false);
        let (borrowed, _) = self.finish_borrowed_editing(true);
        // The owned field editor is recorded in the window ledger; even without a document, final window receipts require its actual nil state.
        self.borrowed_close = Some(borrowed);
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
        self.sidebar_list.detach();
        self.controller.detach_controls();
    }

    /// Break the remaining delegate/content edges after the native close callback has completed.
    pub(super) fn release_native_ownership(&self) {
        // Labels hold no callback or document; clears actual paths/tooltips before content teardown and surface release drop sole strong ownership.
        self.note_path.setStringValue(ns_string!(""));
        self.note_path.setToolTip(None);
        // This follows windowWillClose, so removing the delegate can no longer lose its close signal.
        self.panel.setDelegate(None);
        self.panel.setContentView(None);
    }
}

/// Computes the current editor area after reserving header and sidebar layout space.
pub(super) fn native_markdown_editor_base_frame(size: NSSize) -> NSRect {
    // The production window owns these three chrome elements; standalone Find embedding does not call this function.
    NSRect::new(
        NSPoint::new(
            NATIVE_MARKDOWN_SIDEBAR_WIDTH + 1.0,
            sidebar_geometry::STATUS_HEIGHT,
        ),
        NSSize::new(
            (size.width - NATIVE_MARKDOWN_SIDEBAR_WIDTH - 1.0).max(0.0),
            (size.height - NATIVE_MARKDOWN_HEADER_HEIGHT - sidebar_geometry::STATUS_HEIGHT)
                .max(0.0),
        ),
    )
}

pub(super) fn native_markdown_editor_frame(
    content_view: &NSView,
    feedback_visible: bool,
) -> NSRect {
    let base = native_markdown_editor_base_frame(content_view.bounds().size);
    let frames = header_layout::editor_area_frames(base, feedback_visible, 0.0)
        .expect("已声明的窗口最小尺寸保留正文、页眉和footer");
    NSRect::new(
        NSPoint::new(frames.scroll.x, frames.scroll.y),
        NSSize::new(frames.scroll.width, frames.scroll.height),
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

/// Opens the selected note into the sole paginated document; only isolated fixtures explicitly construct reference TextKit graphs.
pub(super) fn native_markdown_editor_surface(
    document: MarkdownDocumentBuffer,
    frame: NSRect,
    presentation_mode: MarkdownPresentationMode,
    resource_root: &NativeMarkdownOwnedLedger,
    mtm: MainThreadMarker,
) -> Result<NativeMarkdownEditorSurface, String> {
    // All fallible file/revision acquisition precedes native allocation at the calling boundary.
    let editor = NativeMarkdownEditorSurface::new_with_mode(
        document,
        frame,
        presentation_mode,
        resource_root,
        mtm,
    )?;
    editor
        .scroll_view()
        .setBackgroundColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.075, 0.084, 0.11, 1.0,
        ));
    Ok(editor)
}

/// Changes single-line presentation only; does not truncate NSString values or rewrite restoration-feedback tooltips.
fn configure_native_markdown_header_text(field: &NSTextField) {
    if let Some(cell) = field.cell() {
        // Existing standard title/status cells disable wrapping and use ellipsis to fit their actual frames.
        cell.setUsesSingleLineMode(true);
        cell.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
    }
}
