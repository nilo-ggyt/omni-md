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
    /// 与当前pager同寿命；失败后各页只供重选，绝不以autoopen覆盖原始错误。
    pub(super) sidebar_completion_policy:
        super::workspace_browse_policy::NativeMarkdownBrowseCompletionPolicy,
    pub(super) sidebar_results: Option<
        std::sync::Arc<crate::cancelable_mailbox::CancelableMailbox<NativeMarkdownBrowseResult>>,
    >,
    /// 复用已登记的分区标题控件，模式提示不需要新label或查询缓存。
    pub(super) sidebar_title: Retained<NSTextField>,
    pub(super) sidebar_rows: Vec<NativeMarkdownSidebarRow>,
    pub(super) sidebar_list: NativeMarkdownSidebarList,
    pub(super) note_title: Retained<NSTextField>,
    /// 只显示当前已安装文档的真实相对路径；不保存第二份浏览选择或正文owner。
    pub(super) note_path: Retained<NSTextField>,
    pub(super) current_note_relative_path: Option<String>,
    pub(super) editor: Option<NativeMarkdownEditorSurface>,
    pub(super) empty_label: Option<Retained<NSTextField>>,
    pub(super) pending_note_switch: Option<PendingNativeMarkdownNoteSwitch>,
    /// 查询文本和opaque续页只保留有限纯元数据；stop_search/关闭时统一take释放。
    pub(super) search_query: Option<String>,
    /// 查询被接纳时的输入意图；保存不得把新输入B的当前代次冒记为旧查询A的权限。
    pub(super) search_intent: Option<u64>,
    pub(super) search_cursor: Option<omni_markdown_core::MarkdownSearchCursor>,
    /// 已展示页的独立查询权限，末页没有cursor也必须能被保存撤销。
    pub(super) search_authority: Option<omni_markdown_core::MarkdownSearchAuthority>,
    pub(super) search_generation: u64,
    pub(super) search_cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(super) search_worker: Option<std::thread::JoinHandle<()>>,
    // 查询与结果由前台所有者持有；排队通知只保存邮箱弱引用。
    pub(super) pending_search_query:
        Option<std::sync::Arc<crate::cancelable_mailbox::CancelableMailbox<String>>>,
    pub(super) search_results: Option<
        std::sync::Arc<crate::cancelable_mailbox::CancelableMailbox<NativeMarkdownSearchResult>>,
    >,
    pub(super) pending_file_mutation: Option<PendingNativeMarkdownFileMutation>,
    pub(super) borrowed_close: Option<NativeMarkdownBorrowedEditorSnapshot>,
    /// modal公开清理失败的首因；不能通过再次打开dialog丢弃不完整资源事实。
    pub(super) modal_failure: Option<String>,
}

/// A deferred note transition preserves only path identities and the old editor release barrier.
pub(super) struct PendingNativeMarkdownNoteSwitch {
    pub(super) relative_path: String,
    pub(super) previous_relative_path: Option<String>,
    pub(super) release_barrier: Option<NativeMarkdownEditorReleaseBarrier>,
    /// 仅明确恢复决定携带一次性小票据；普通切换没有跳过旧草稿的权限。
    pub(super) recovery_handoff: Option<omni_markdown_core::MarkdownRecoveryHandoff>,
    /// 仅该次链接切换携带已解码片段；旧图释放后移交目标Body，不保旧Doc或Scene。
    pub(super) fragment: Option<String>,
}

/// A mutation waits for editor release while retaining the saved authority revision and recovery identity.
pub(super) struct PendingNativeMarkdownFileMutation {
    pub(super) mutation: NativeMarkdownFileMutation,
    pub(super) active_relative_path_before: Option<String>,
    pub(super) expected_revision: Option<String>,
    pub(super) release_barrier: Option<NativeMarkdownEditorReleaseBarrier>,
    /// modal-only barrier不代表正文已释放；失败恢复必须依据真实editor转换。
    pub(super) editor_was_released: bool,
    /// begin首个Native前固定的输入意图；release异步等待不能在finish领新查询权限。
    pub(super) feedback_intent: u64,
}

/// 仅文件操作把原反馈ticket贯穿安装；普通caller保持原合同，挂载不能领取新的输入权限。
#[derive(Clone, Copy)]
enum NativeMarkdownEditorFeedback {
    ExistingCaller,
    FileMutation(Option<u64>),
}

impl NativeMarkdownEditorFeedback {
    /// 正文恢复是必要补偿；状态/聚焦是业务反馈，必须另核原ticket以免抢新查询的输入。
    fn permits(self, surface: &NativeMarkdownWindowSurface) -> bool {
        match self {
            // 其余既有文档切换仍走原调用方的交接权限，不在此引入新代次行为。
            Self::ExistingCaller => true,
            // None明确表示旧权限已撤销，不能改用安装完成后的新epoch。
            Self::FileMutation(intent) => {
                intent.is_some_and(|intent| surface.workspace_refresh_intent_is_current(intent))
            }
        }
    }
}

impl NativeMarkdownWindowSurface {
    /// 纯Rust门槛必须早于composition/save/resign/take，失败保留同一可编辑父草稿。
    pub(super) fn preflight_modal_boundary(&self) -> Result<(), String> {
        preflight_native_markdown_modal()?;
        if let Some(error) = &self.modal_failure {
            // 已观察到的清理失败不能因相关window后来不可见而被遗忘。
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
        // 首次只做一个有界步骤；未完成不能用随机前缀决定自动打开的笔记。
        let (snapshot, sidebar_has_more, sidebar_scan_pending) = match progress {
            omni_markdown_core::MarkdownWorkspacePageProgress::Ready(page) => {
                // 小库完整扫描已结束，只有这一确定页可以决定首次打开的笔记。
                (page.snapshot, page.has_more, false)
            }
            omni_markdown_core::MarkdownWorkspacePageProgress::Pending => {
                // 大库没有可发布前缀；根行与加载提示先挂载，之后由后台继续。
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
        // 显示与所有文件操作相同的已验证根；失败直接保留错误，不另建空库。
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
        // 窗口背景固定深色，标准 popup/button/字段也必须使用同一原生 appearance。
        // SAFETY: AppKit 导出的名称具有进程静态生命周期，仅在主线程查询框架共享样式。
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
        // 恢复无边框窗口原有的背景拖动；文本与控件仍先处理各自的输入事件。
        panel.setMovableByWindowBackground(true);
        panel.setHasShadow(true);
        panel.setOpaque(true);
        panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
        panel.setLevel(NSFloatingWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        // 最小尺寸约束实际内容区；无系统标题栏时不再预留额外标题栏高度。
        panel.setContentMinSize(NSSize::new(
            sidebar_geometry::MINIMUM_CONTENT_WIDTH,
            sidebar_geometry::MINIMUM_CONTENT_HEIGHT,
        ));
        panel.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.055, 0.062, 0.082, 1.0,
        )));

        let controller = NativeMarkdownController::new(&owned_ledger, &panel, mtm);
        panel.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
        // 面板构造已建立唯一内容根；缺失属于装配错误，不能另造视图树掩盖它。
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

        // 无系统标题栏仍保留原产品名；品牌不随功能中文化改写，也不增加关闭入口。
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
        // 简短按钮保留完整辅助访问与悬停说明，关闭仍沿原保存与资源屏障执行。
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
        // SAFETY: AppKit公开的固定字重常量在本主线程构造期间有效，不借用文稿或字体缓冲。
        let title_weight = unsafe { NSFontWeightSemibold };
        note_title.setFont(Some(&NSFont::systemFontOfSize_weight(17.0, title_weight)));
        note_title.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.90, 0.92, 0.96, 1.0,
        )));
        // 页眉所有横向位置由同一几何发布，避免独立弹簧在缩放后互相覆盖。
        note_title.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        configure_native_markdown_header_text(&note_title);
        note_title.setAccessibilityLabel(Some(ns_string!("当前笔记")));
        note_title.setToolTip(Some(&NSString::from_str(&current_title)));
        content_view.addSubview(&note_title);

        // 路径来自同一次成功打开的身份；只读标准label保留完整AX值，截断不改用户字符串。
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
        } // 空工作区不显示虚构路径，也不保留上一篇文档的提示。
        content_view.addSubview(&note_path);

        // 原顶部“源码”是同一实际模式事务的双态按钮；不会替换canonical或新增编辑器。
        // SAFETY: 原主线程controller实现固定action，窗口拆图先解除按钮target/action。
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

        // 原三列持久footer独立于临时反馈；每列都是同窗口实际登记的固定控件。
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

        // 笔记库身份使用实际目录末级名称；完整保存位置仍由同一标准标签的提示和AX值提供。
        // 这里只改善长路径的显示，不改会话根、书签、文件身份或打开保存目标。
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
        // 窗口使用固定深色背景；系统浅色 appearance 的 secondaryLabelColor 会变成黑字。
        workspace_location.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.58, 0.63, 0.72, 1.0,
        )));
        workspace_location.setToolTip(Some(&workspace_path));
        workspace_location.setAccessibilityLabel(Some(ns_string!("笔记保存位置")));
        // SAFETY: 该静态标签的无障碍值明确为只读 NSString；AppKit 同步保留这份实际目录文案。
        unsafe {
            workspace_location.setAccessibilityValue(Some(&workspace_path));
        }
        content_view.addSubview(&workspace_location);

        // 原生滚动列表只接收当前有限元数据页，搜索和底栏仍固定在容器外。
        let sidebar_list = NativeMarkdownSidebarList::new(
            &sidebar_rows,
            initial_note_relative_path.as_deref(),
            &content_view,
            &owned_ledger,
            mtm,
        )?;

        // Construct at most one TextKit graph; an open failure remains a visible empty-state error.
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
                // 完整排序还在后台继续，不能把空的暂态误称为用户没有笔记。
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

        // 菜单构造须读实际初始模式；此时没有前台更新权限，只设置既有模式真值。
        // 控件安装后仍由原调用再次同步按钮，不把构造阶段伪装成活会话。
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
            // 构造时直接绑定实际Doc，不等较晚Active getter重领路径；弱票不保活正文。
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
            // 只取纯状态后结束正文借用，再显示恢复提示；原生 setter 不跨越 RefCell 借用。
            let pending_recovery = editor.document().borrow().has_pending_legacy_recovery();
            if pending_recovery {
                // 原记录没有磁盘基线，提示明确解决入口，不把自动恢复说成已经保存。
                controller.present_pending_legacy_recovery();
            }
            // 表格真实编辑只弱引用窗口 controller；正文不增加窗口的强所有权环。
            // Only a live editor can own the initial text responder.
            panel.setInitialFirstResponder(Some(editor.input_view()));
        }

        // 仅登记本构造路径显式创建/配置的原生角色，不把 AppKit 内部子树升级为项目所有。
        for (role, object) in [
            ("window.controller", &*controller as &AnyObject),
            ("window.content", &*content_view as &AnyObject),
            ("window.background", &*background as &AnyObject),
            ("window.header", &*header as &AnyObject),
            ("window.sidebar", &*sidebar as &AnyObject),
        ] {
            // 本构造数量固定且远小于 ledger 预算，失败属于内部构造不变量被破坏。
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
            // 控件及实际 cell 都有独立 weak 证明，不能只依赖 content/controller nil。
            owned_ledger
                .include_control(role, control)
                .expect("fixed Markdown controls fit owned ledger");
        }
        if let Some(label) = &empty_label {
            // 空工作区路径同样登记 placeholder，不能只覆盖正文成功构造。
            owned_ledger
                .include_control("window.empty", label)
                .expect("fixed empty label fits owned ledger");
        }

        // 所有原生控件和菜单已登记；页眉只观察实际两行Weak，唯一strong仍归原controls所有者。
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
        // 构造还未发布全局owner；局部panel/controller身份与尺寸仍须逐setter复核。
        if !header_controls.layout(
            &controller,
            &panel,
            NativeMarkdownHeaderAuthority::Construction,
        ) {
            // 构造期间尺寸/窗口已被重入取代，不能发布只改了一半的工具栏。
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

        // 构造中的Native getter也可能产生输入通知；首个目录任务绑定实际当前意图而非假设0。
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
        // 仅显式探针对照在首次绘制前隐藏既有组件；默认产品与正式功能门禁不变。
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

    /// 只隔离真实窗口的既有组件，不另建正文、布局或持久原生所有者。
    #[cfg(feature = "native-markdown-runtime-probe")]
    fn apply_memory_component_control(&self) {
        let Some(control) = std::env::var_os("OMNI_NATIVE_MEMORY_COMPONENT_CONTROL") else {
            // 没有显式环境值时不查询原生图，也不改变任何组件的可见性。
            return;
        };
        let (label, hide_gutter) = match control.to_str() {
            // 两个固定对照都保留原 scroll_view；只有第二种额外隐藏原行号尺。
            Some("body_only") => ("body_only", false),
            Some("body_only_no_gutter") => ("body_only_no_gutter", true),
            _ => {
                // 未知值不回退为其他对照，也不把原环境内容写入诊断。
                eprintln!(
                    "native_memory_component_control control=invalid formal=false hidden_components=0 invalid=true"
                );
                return;
            }
        };

        let mut hidden_components = 0_usize;
        let current = || native_markdown_controller_has_foreground_authority(&self.controller);
        let result = (|| -> Result<(), ()> {
            // 与既有原生角色账本相同的 1024 上限约束整个快照，超限不截断后继续。
            const MAX_COMPONENT_SUBVIEWS: usize = 1_024;
            if MainThreadMarker::new().is_none() || !current() {
                // 关闭或非主线程不具备隐藏原生视图的权限。
                return Err(());
            }
            let editor = self.editor.as_ref().ok_or(())?;
            let content = self.panel.contentView().ok_or(())?;
            if !current() {
                // getter 可能重入；不能把先前权限沿用到后续视图查询。
                return Err(());
            }
            let subviews = content.subviews();
            if !current() {
                // NSArray 只在本次 show 内临时持有原子视图，失权立即归还快照。
                return Err(());
            }
            let count = subviews.count();
            if count > MAX_COMPONENT_SUBVIEWS {
                // 输入数量超出账本预算时不隐藏任何部分组件。
                return Err(());
            }
            let body: &NSView = editor.scroll_view().as_super();
            let mut body_matches = 0_usize;
            for index in 0..count {
                // 完整预检原 scroll_view 身份；不递归接管 AppKit 内部子树。
                if !current() {
                    return Err(());
                }
                let child = subviews.objectAtIndex(index);
                if !current() {
                    return Err(());
                }
                if std::ptr::eq(&*child, body) {
                    // 只认可实际已挂载正文；缺失或重复身份不能成为有效对照。
                    body_matches += 1;
                }
            }
            if body_matches != 1 {
                // 没有唯一正文时保留原组件状态，不臆造替代视图。
                return Err(());
            }
            for index in 0..count {
                // 只隐藏原快照中的同级组件，保留 frame、层级与原正文所有权。
                if !current() {
                    return Err(());
                }
                let child = subviews.objectAtIndex(index);
                if !current() {
                    return Err(());
                }
                if std::ptr::eq(&*child, body) {
                    // 原 scroll_view 的 hidden 状态也不主动改写。
                    continue;
                }
                child.setHidden(true);
                if !current() || !child.isHidden() || !current() {
                    // setter/getter 任一重入失权都使本次对照无效，不重试或补签。
                    return Err(());
                }
                hidden_components += 1;
            }
            if hide_gutter {
                // 仅改变已有 ruler 视图的 hidden，不修改 rulersVisible 或正文布局。
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
        // 原生快照和逐项临时 claim 已全部归还；对照不能升级为正式功能或内存通过证据。
        let invalid = result.is_err() || !current();
        eprintln!(
            "native_memory_component_control control={label} formal=false hidden_components={hidden_components} invalid={invalid}"
        );
    }

    /// 已安装文档的真实身份只更新列表反馈，不触发另一次文档选择。
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
                // 未完整登记的控件不发布到层级；现有原生 surface 保持可关闭。
                self.controller.present_error(&error);
                return;
            }
            content_view.addSubview(&label);
            self.empty_label = Some(label);
        }
        if self.pending_note_switch.is_none() && self.pending_file_mutation.is_none() {
            // 无待验证旧图的正常空工作区仍允许标题/正文搜索与后续恢复。
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
        // 空工作区不得继续通过悬停暴露上篇笔记的标题。
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

    /// 仅失败且旧正文已释放时使用；每个Native阶段后核同一空owner；B只撤业务发布，不能阻止必要补偿。
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
            // 复用已有placeholder，不创建错误恢复窗口或另一个原生资源图。
            label.setStringValue(&NSString::from_str(message));
        } else {
            let label = native_markdown_empty_label(message, mtm);
            if !self.empty_workspace_compensation_is_current() {
                // 未发布candidate局部drop；关闭/新owner不能被旧错误补建控件。
                return Ok(false);
            }
            self.owned_ledger.include_control("window.empty", &label)?;
            content.addSubview(&label);
            if !self.empty_workspace_compensation_is_current() {
                // 自己尚未发布的candidate只移除自身，不动后继控件。
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
        // 失败补偿只清同一空owner的旧身份，每次Native调用后仍服从原重入撤权。
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
        // pending已消费、旧图已严格释放；只恢复原查询provider，不复活旧field editor。
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

    /// 文档/控件补偿总会完成；仅可见反馈和焦点沿文件操作原ticket门禁。
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
        // Core已成功打开并验证此笔记路径；标题沿同一file_stem规则，不依赖尚在异步刷新的目录页。
        // 目录名仍保留完整文件名规则，碰撞生成的数字后缀及标题内部的点也保持原值。
        let title = std::path::Path::new(&relative_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("Core已验证所安装笔记的UTF-8文件名");
        let native_title = NSString::from_str(title);
        self.note_title.setStringValue(&native_title);
        // 视觉截断只作用于cell，实际字符串、辅助访问值和悬停仍使用完整标题。
        self.note_title.setToolTip(Some(&native_title));
        let native_path = NSString::from_str(&relative_path);
        self.note_path.setStringValue(&native_path);
        self.note_path.setToolTip(Some(&native_path));
        // 普通切换、重命名和移动均从实际安装收据取得身份，不查侧栏页或替换目录名。
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
        // 旧图收据已消费，新 canonical owner 发布后才恢复查询客户端。
        self.controller.set_workspace_input_enabled(true);

        // The installed owner now supports focus and user actions without an outstanding old editor.
        let editor = self
            .editor
            .as_ref()
            .expect("native Markdown editor was installed immediately above");
        if feedback.permits(self) {
            // 文件操作期间已发生新B时不抢查询焦点，正文仍已作为可用补偿发布。
            self.panel
                .setInitialFirstResponder(Some(editor.input_view()));
        }
        if feedback.permits(self) {
            // 上一个Native setter也可能重入；只清仍属于本次原反馈的状态。
            self.controller.clear_status();
        }
        let pending_recovery = editor.document().borrow().has_pending_legacy_recovery();
        if pending_recovery && feedback.permits(self) {
            // 普通重开保留原恢复提示；旧文件操作不覆盖新查询已发布的反馈。
            self.controller.present_pending_legacy_recovery();
        }
        if feedback.permits(self)
            && !self.panel.makeFirstResponder(Some(editor.input_view()))
            && feedback.permits(self)
        {
            // 焦点失败也属于原操作反馈，不能在getter重入B后显示旧错误。
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

    /// 同一正文只更换排版策略；窗口模式、按钮和后续打开默认值都以实际接纳结果为准。
    pub(super) fn set_presentation_mode(
        &mut self,
        mode: MarkdownPresentationMode,
    ) -> Result<(), String> {
        if self.presentation_mode == mode {
            // Re-selecting the current mode needs no text/style invalidation or ownership change.
            self.controller.select_presentation_mode(mode);
            return Ok(());
        }
        // 查找字段也可能正处于中文组合；先走原窗口辞任预检，不把候选迁到另一输入客户。
        let previous = self.panel.firstResponder();
        let find_focus = self.capture_mode_find_focus();
        #[cfg(feature = "native-markdown-runtime-probe")]
        {
            // 只记录原捕获的有限选区与真实客户关系，不读取字段值或以诊断结果授权恢复。
            // 由 capture/restore/verify 三个既有出口区分捕获前变化、当场恢复失败及后继失焦。
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
            // 接纳前的拒绝保留旧模式；接纳后的 Native 重入可能撤销位置恢复，
            // 但已退休的 Producer 不能回滚。始终读取真实正文模式，避免按钮与正文失配。
            if let Some(editor) = &self.editor {
                self.presentation_mode = editor.presentation_mode();
            }
            self.controller
                .select_presentation_mode(self.presentation_mode);
            if self.presentation_mode == mode
                && let Some(editor) = &self.editor
                && let Err(mount) = editor.finish_mode_viewport_mount(&self.controller)
            {
                // 模式可能已接纳后被 Native 输入撤位；仍结束原未绑定票，不留下永远等待的空正文。
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
        // 未打开文稿时只改变下一次打开默认值；当前文稿则必须已经真实接纳目标模式。
        self.presentation_mode = self
            .editor
            .as_ref()
            .map_or(mode, NativeMarkdownEditorSurface::presentation_mode);
        self.controller
            .select_presentation_mode(self.presentation_mode);
        if let Some(editor) = &self.editor {
            // 正文、scroll、Find 栏和字段客户都是原实例；只重算 ruler 显隐后的实际区域。
            // 重复 addSubview 或重绑 Find 会额外改变层级和字段焦点，不属于模式动作。
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
            // 焦点失败也必须交还本次挂载义务；否则已提交模式会永远停在 controller=None。
            // 位置只使用原锚点；绑定失败在 Body 内撤同一票并恢复当前模式的普通排版。
            let mounted = editor.finish_mode_viewport_mount(&self.controller);
            match (focus, mounted) {
                (Err(focus), Err(mount)) => return Err(format!("{focus}；{mount}")),
                (Err(cause), Ok(())) | (Ok(()), Err(cause)) => return Err(cause),
                (Ok(()), Ok(())) => {}
            }
        }
        Ok(())
    }

    /// 模式动作只保原实际Find客户和有限NSRange；不把共享field editor地址当作查询/替换身份。
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
                // 组合候选由下面真实辞任完成；它的内部选择不能作为普通已提交查询选区强行恢复。
                let selected = (!input.hasMarkedText()).then(|| responder.selectedRange());
                return Some((
                    field,
                    selected.filter(|range| range.location != NSNotFound as usize),
                ));
            }
        }
        None
    }

    /// 同一原字段重新获得真实Native客户后恢复其选择，不重建控件或复制查询文字/Undo。
    fn restore_mode_find_focus(
        &self,
        field: &NSTextField,
        selected: Option<NSRange>,
    ) -> Result<(), String> {
        // SAFETY: 模式交接仍持唯一窗口owner，field来自该笔记原Find栏，并未清理或换目标。
        unsafe {
            field.selectText(None);
        }
        let editor = field
            .currentEditor()
            .and_then(|editor| editor.downcast::<NSTextView>().ok())
            .ok_or("展示模式已切换，但原查找字段未恢复输入")?;
        if let Some(selected) = selected {
            let length = editor.string().length(); // 只借Native字段长度，不物化第二份查询或正文。
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
            // 公开 setter 的返回不是焦点或选区收据；这里只观察恢复当场的标量，不补设焦点或重试。
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

    /// 正常辞任所有实际客户端；没有正文的工作区也可能正在搜索字段中组合输入。
    pub(super) fn resign_editor_responder(&self) -> Result<(), String> {
        self.controller.ivars().undo.preflight()?;
        let borrowed = self.controller.ivars().borrowed.borrow().clone();
        borrowed.capture(&self.panel)?;
        if let Some(editor) = &self.editor {
            // Find/表格旧身份先登记，不能依赖辞任后仍能从窗口查到同一客户端。
            editor.capture_find_resources()?;
        }
        let generation_before = self
            .editor
            .as_ref()
            .map(|editor| editor.document().borrow().generation());
        let previous = self.panel.firstResponder();
        if !self.panel.makeFirstResponder(None) {
            // 原生验证拒绝时不进行 accepted 清理，原始字段、草稿与焦点继续有效。
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
                // 名义成功仍不能留下原字段或正文作为实际输入 target。
                return Err("上一笔记的输入仍未结束".to_owned());
            }
            if self
                .editor
                .as_ref()
                .map(|editor| editor.document().borrow().generation())
                != generation_before
            {
                // 辞任期间出现新的 canonical 修改，必须保留该草稿再显式保存。
                return Err("结束输入时正文发生变更，草稿将保持打开以供保存".to_owned());
            }
            borrowed.capture(&self.panel)?;
            borrowed.preflight_unmarked()?;
            // 辞任可触发原生 Undo 提供栈；仍未接受关闭时以 Busy 拒绝而不冻结/清历史。
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
                // 失败恢复也如实返回，不把失焦当成成功关闭或清空权限。
                return Err(format!("{error}；无法恢复原输入焦点"));
            }
            return Err(error);
        }
        self.panel.setInitialFirstResponder(None);
        Ok(())
    }

    /// 通过预检并接受交接后，结束共享编辑器；不在此提交新正文或重新保存。
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
            // 窗口结束才清除查询字段；跨笔记保留窗口级查询值。
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
            // 单元格和 Find 属于即将结束的编辑客户端，不把它们留在下一笔记的共享缓存内。
            let find = editor.find_resources().borrow().clone();
            fields.extend(find.fields());
        }
        let field_refs: Vec<_> = fields.iter().map(|field| &**field).collect();
        // 正式结束客户端前先封闭 provider，清理重入不能分配新的 field editor。
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
            // 原生清空可能重入，不能持 editor RefMut；完成后恢复仅弱元数据收据。
            find.finish_accepted();
            *editor.find_resources().borrow_mut() = find;
            let container =
                ProtocolObject::<dyn NSTextFinderBarContainer>::from_ref(editor.scroll_view());
            container.setFindBarVisible(false);
        }
        (borrowed, released_field_editor)
    }

    /// 正常结束输入后释放当前编辑器，并返回完整的弱释放屏障。
    pub(super) fn destroy_current_editor(
        &mut self,
    ) -> Result<Option<NativeMarkdownEditorReleaseBarrier>, String> {
        // 普通切换没有旧草稿恢复票据，仍共用相同的辞任与实际释放流程。
        self.destroy_current_editor_with_recovery(None)
    }

    /// 在最后一次可失败的输入边界后重验恢复身份，再接受不可逆的编辑器交接。
    pub(super) fn destroy_current_editor_with_recovery(
        &mut self,
        recovery_handoff: Option<&omni_markdown_core::MarkdownRecoveryHandoff>,
    ) -> Result<Option<NativeMarkdownEditorReleaseBarrier>, String> {
        // 弹窗局部owner尚在栈内时，不辞任/清理父输入，也不接受新资源交接。
        self.preflight_modal_boundary()?;
        self.resign_editor_responder()?;
        if let Some(handoff) = recovery_handoff {
            // capture、辞任和末尾原生预检均可能重入；直到这些调用结束，才核对票据的
            // 原路径、旧记录摘要与正文代次。失败时不冻结 provider、不清 Undo、不取走模型，
            // 因而晚到输入继续留在当前草稿中，票据销毁也不会发布恢复决定。
            let editor = self.editor.as_ref().ok_or("当前没有打开的笔记")?;
            self.session
                .validate_legacy_recovery_handoff(&editor.document().borrow(), handoff)
                .map_err(|error| error.to_string())?;
        }
        // 上述纯核验与 accepted 清理之间没有其他原生调用，旧 debounce 此后不能跨正文保存。
        self.controller.cancel_autosave();
        self.controller.set_workspace_input_enabled(false);
        let (borrowed, field_editor) = self.finish_borrowed_editing(false);
        // 信息卡先撤原Doc弱票和全部动态文本，空控件不再挂接旧正文。
        self.controller.hide_information_panel();
        // accepted拆图前先撤纯Weak布局引用，后继反馈不能把尚未析构的旧scroll重新带回布局。
        self.controller.unbind_document_area();
        let Some(editor) = self.editor.take() else {
            // 无正文仍可能有搜索字段的自有 editor；后继必须等它实际释放，不能漏掉该旧图。
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
        // 旧正文已结束；等待后继安装期间不继续公开已撤销的相对路径。
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
        // 即便选择相同笔记也不能抢走当前modal的responder。
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
            recovery_handoff: None,
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

        // 已验证旧图释放后才消费恢复票据；普通打开仍执行旧草稿检查，不共享忽略恢复的开关。
        let opened = if let Some(handoff) = pending_switch.recovery_handoff {
            self.session
                .open_resolved_legacy_recovery(handoff)
                .map_err(|error| error.to_string())
                .and_then(|outcome| {
                    // core 已核验唯一目标并尝试记录决定，只有此时才创建它的原生编辑图。
                    native_markdown_editor_surface(
                        outcome.document,
                        editor_frame,
                        self.presentation_mode,
                        &self.owned_ledger,
                        mtm,
                    )
                    .map(|editor| (editor, Some(outcome.recording)))
                })
        } else {
            // 正常笔记不产生恢复存储或第二正文模型，继续使用 confined 打开入口。
            self.open_note_editor(&pending_switch.relative_path, editor_frame, mtm)
                .map(|editor| (editor, None))
        };
        match opened {
            // A valid destination becomes the sole mounted editor after old release verification.
            Ok((editor, recording)) => {
                // 目标确实打开后才更新目录；下面失败恢复旧正文的分支不能覆盖用户原浏览位置。
                self.sidebar_list
                    .select_directory(&native_markdown_note_parent(&pending_switch.relative_path));
                self.install_editor(&content_view, editor, pending_switch.relative_path);
                if let Some(fragment) = pending_switch.fragment.as_deref() {
                    // 已安装目标才绑定其自己的完整Input；旧屏障和旧Source不能签标题位置。
                    self.editor
                        .as_ref()
                        .expect("刚安装的唯一目标正文")
                        .body
                        .navigate_heading(fragment)
                        .map_err(|cause| cause.to_string())?;
                }
                if let Some(recording) = recording {
                    // 已打开目标与恢复决定的持久化是两个事实，记录故障不能伪装成功。
                    match recording {
                        omni_markdown_core::MarkdownRecoveryRecording::Recorded => {
                            // 完整决定已同步，install_editor 的正常状态已经准确。
                        }
                        omni_markdown_core::MarkdownRecoveryRecording::AlreadyResolved => {
                            // 不覆盖同一旧记录的另一个合法决定，也不声称本次重新写入。
                            self.controller
                                .present_error("已打开笔记，既有恢复决定已保留");
                        }
                        omni_markdown_core::MarkdownRecoveryRecording::NotRecorded(error) => {
                            // 明确保留旧草稿与目标文件；提示可用目标之外仍存在恢复记录故障。
                            self.controller.present_error(&format!(
                                "恢复决定未能完整记录，原草稿已保留，下次可能再次提示：{error}"
                            ));
                        }
                    }
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
        // 先于modal/IME/save/release任何Native阶段固定，只作反馈权限，不阻止必要操作补偿。
        let feedback_intent = self.controller.workspace_query_intent();
        // modal的确认/候选/局部资源未结束前，拒绝任何文件语义或正文效果。
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
                    // 已恢复的旧record没有可写base；即使前序保存已拒绝，类型边界也不能猜测revision。
                    .ok_or_else(|| "旧冲突草稿尚未解决，不能修改原笔记路径".to_owned())?
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
            // 父正文继续使用同一owner，但已结束modal的默认nil角色仍须经过实际事件。
            Some(NativeMarkdownEditorReleaseBarrier::for_modal_roles(
                self.owned_ledger.snapshot_modal(),
                self.panel.mtm(),
            ))
        };
        if self.workspace_refresh_intent_is_current(feedback_intent) {
            // 真实didChange或内部save合法推进代次时都保守不清，不能末尾领取current覆盖新反馈。
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

    /// 完成严格释放后执行一次操作；失败可能已发布，恢复只使用旧authority而不推断磁盘未变。
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
                // 只有旧正文确实释放才展示空状态；modal-only失败保留同一正文、path/title与选择。
                let content_view = self
                    .panel
                    .contentView()
                    .ok_or_else(|| "原生 Markdown 窗口没有内容视图".to_owned())?;
                self.show_empty_state(&content_view, "文件操作暂时不可用", mtm);
            }
            // 保留同一pending与首个失败，caller只展示错误；不重试、不清仍活正文或触碰文件authority。
            return Err(error);
        }

        // Take exactly one verified transition before executing its confined authoritative operation.
        let pending_mutation = self
            .pending_file_mutation
            .take()
            .expect("native Markdown file mutation was checked immediately above");
        // modal-only barrier存在也不授权重建/恢复仍然活着的正文。
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
                    // 仅尝试原path并重新核磁盘；没有body镜像，也不假定失败操作未发生变化。
                    return match self.install_note_from_authority_with_feedback(
                        &previous_path,
                        mtm,
                        NativeMarkdownEditorFeedback::FileMutation(*feedback_intent),
                    ) {
                        // Recovery succeeded, so report the mutation error with the original note usable.
                        Ok(()) => Err(mutation_error),
                        // Err可能发生在真实发布/部分变化之后；旧path不能恢复时让用户检查实际目录。
                        Err(recovery_error) => {
                            let expected_intent = *feedback_intent;
                            *feedback_intent = None;
                            let error = format!(
                                "{mutation_error}；文件操作可能已更改磁盘，持久化状态未确认；原笔记恢复未确认：{recovery_error}；请从目录重新选择笔记并核对当前文件状态"
                            );
                            if let Some(intent) = expected_intent {
                                // 任何Native重入的新输入/关闭都撤销本次旧错误对界面和刷新权限。
                                // 先固定仍可报告的原代次；准备失败也必须携带原mutation首因。
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
                                        // 不重放文件操作；目录重读失败只作为第二事实附在原首因之后。
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

        // 磁盘操作已真正成功；先消费同一目录收据，再刷新有限投影，失败绝不预选或回滚猜测。
        self.sidebar_list
            .apply_directory_mutation(&pending_mutation.mutation, &mutation_result);
        let mut fresh_sidebar = false;
        let refresh_result = if let Some(intent) = *feedback_intent
            && self.workspace_refresh_intent_is_current(intent)
        {
            // 创建已经由 Core 收据确认，且仍属于原目录动作；只展开新项真实祖先，
            // 让折叠目录中的新笔记可见，不改动无关目录或重领后来查询的反馈权限。
            match &mutation_result {
                NativeMarkdownFileMutationResult::NoteCreated(path)
                | NativeMarkdownFileMutationResult::DirectoryCreated(path) => {
                    for parent in Path::new(path).ancestors().skip(1) {
                        // Core 已验证路径为 UTF-8；根的空路径没有折叠标记，删除自然为空操作。
                        self.collapsed_directories
                            .remove(parent.to_str().expect("Core 创建收据的目录必须是 UTF-8"));
                    }
                }
                // 重命名、移动和删除保持用户现有折叠状态；不把所有文件操作都当成新建。
                _ => {}
            }
            // 只能延续begin保存的原ticket；Weak/barrier重入B后不可领取B再cancel它。
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
            // 旧业务刷新被撤销，但真实receipt/editor补偿仍必须完成；旧notes不是新目录证据。
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
                    // 成功的笔记身份变化采用新父目录；目录重命名已经按真实收据保留选中后代。
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
                    // 新目录仍在有界worker扫描，不能把尚未完成的页误称空库。
                    "操作已完成，正在读取笔记目录…"
                } else if fresh_sidebar {
                    // 完整页没有可打开笔记时，仍沿正常目录状态等待后续用户选择。
                    "请选择笔记，或继续查看更多笔记"
                } else {
                    // 跳过旧刷新不能宣称库为空，保留新查询/用户重选的正常入口。
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
            // 必要正文补偿不授予旧操作清理新B反馈的权限。
            self.controller.clear_status();
        }
        Ok(())
    }

    /// 文件操作从最早入口透传反馈ticket，构造/挂载过程不更新它。
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
        // settled选择已由真实Native selection回调同步；保存不能把反向head再次展平为升序NSRange。
        let mut document = editor.document().borrow_mut();
        // 先完成真实保存并结束document可变借用；查询控件清理/重提绝不能跨正文借用。
        let was_dirty = document.is_dirty();
        let saved = self.session.save_document_buffer(&mut document).map(|_| ());
        let footer = NativeMarkdownFooterDocument::capture(&document);
        drop(document);
        // 保存实际结果先按typed原因投影；Native setter不跨canonical可变借用。
        self.controller
            .finish_footer_save(footer, saved.as_ref().err());
        if saved.is_ok() {
            // 真正保存成功后才重读磁盘修改时间；不以失败写入或未确认sync冒充新元数据。
            self.controller.refresh_saved_information(editor.document());
        }
        let saved = saved.map_err(|error| error.to_string());
        let revoked_page = self
            .search_query
            .as_deref()
            .zip(self.search_authority.as_ref())
            .is_some_and(|(query, authority)| !authority.is_current(&self.session, query));
        if self.search_query.is_some() && ((saved.is_ok() && was_dirty) || revoked_page) {
            // 真正发布后的旧页和queued结果失效；no-op保存不重复查询，失败仍保留原保存首因。
            if let Err(error) = self.refresh_search_after_save(saved.is_ok())
                && saved.is_ok()
            {
                // 搜索重提失败不谎报正文保存失败；原保存失败仍由调用者显示其首因。
                self.controller.present_error(&error);
            }
        }
        saved
    }

    /// 同一原组合提交边界区分关闭与保存/模式交接；只有关闭可撤尚未发布的纯显示动作。
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

    /// 显式复制先有界准备独立输出，结束canonical借用后再交付可能重入的系统剪贴板。
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
            // 此阶段只有原Core读取和文件操作；固定scratch不生成全文String或NSString。
            let document = editor
                .document()
                .try_borrow()
                .map_err(|_| "Markdown 正文仍在完成本次编辑".to_owned())?;
            MarkdownPasteboardOutput::prepare(&document).map_err(|error| error.to_string())?
        };
        // Ref已归还且输出不含Doc/Core/FD；Native持有输出不会延长插件或原映射的生命。
        output
            .publish(pasteboard)
            .map_err(|error| error.to_string())
    }

    /// Stop foreground search and detach control/responder callbacks before the window close notification.
    pub(super) fn prepare_to_close(&mut self) {
        self.controller.set_workspace_input_enabled(false);
        let (borrowed, _) = self.finish_borrowed_editing(true);
        // 自有 field editor 已登记窗口账本；即使无正文，最终窗口收据也要求其实际 nil。
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
        // 标签没有回调或Doc；先清真实路径值/悬停，再由原content拆图与surface释放唯一strong。
        self.note_path.setStringValue(ns_string!(""));
        self.note_path.setToolTip(None);
        // This follows windowWillClose, so removing the delegate can no longer lose its close signal.
        self.panel.setDelegate(None);
        self.panel.setContentView(None);
    }
}

/// Computes the current editor area after reserving header and sidebar layout space.
pub(super) fn native_markdown_editor_base_frame(size: NSSize) -> NSRect {
    // 正式Window真实拥有这三块chrome；独立Find embedding不调用本函数。
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

/// 打开选中笔记并建立唯一分页正文；参考 TextKit 图只由独立夹具显式构造。
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

/// 只改变单行显示方式；不截断NSString值，也不重写恢复提示的tooltip。
fn configure_native_markdown_header_text(field: &NSTextField) {
    if let Some(cell) = field.cell() {
        // 标题和状态为已有标准cell；换行关闭后长文字用省略号适配实际frame。
        cell.setUsesSingleLineMode(true);
        cell.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
    }
}
