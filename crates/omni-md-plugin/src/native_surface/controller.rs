//! Owns native action targets, foreground controls and the single autosave deadline.
//! Delayed work carries weak originating identity; controls and timer detach before session release.

use super::*;

mod footer;
pub(super) use footer::NativeMarkdownFooterDocument;
use footer::{NativeMarkdownDocumentAreaBinding, NativeMarkdownFooterSaveResult};

/// 空窗口按钮只改变下一篇的默认模式；原窗口及输入代次贯穿原生 getter 和队列。
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
            // 任一原输入、窗口或模式身份改变，都不能给排队的旧源码按钮动作补发权限。
            return false;
        }
        surface.editor.is_none() // 旧空态不能取得后来打开文稿的模式权限。
    }

    /// 只在队列交付的Active状态读取纯元数据，不在Native getter后重新捕获票据。
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
            // 类型查询也可能进入原生 runtime，第一次调用前即要求原动作权限。
            return Err("展示模式动作已失效".into());
        }
        let button = sender.downcast_ref::<NSButton>();
        if !self.current_surface(surface) {
            // 类型查询的重入不能把新输入代次贴给旧sender。
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
            return Err("展示模式动作不属于原源码按钮".into()); // 外来按钮没有本窗口的模式发布权限。
        }
        let state = button.state();
        if !self.current_surface(surface) {
            // 实际状态getter返回后仍核同票，不重新捕获文档、选区或窗口权限。
            return Err("展示模式选择期间笔记或输入已变更".into());
        }
        if state == objc2_app_kit::NSControlStateValueOn {
            // 标准双态按钮已按下一次点击开启源码；失败补偿仍显示原已发布模式。
            Ok(MarkdownPresentationMode::Source)
        } else if state == objc2_app_kit::NSControlStateValueOff {
            Ok(MarkdownPresentationMode::InlinePreview) // 同一按钮关闭源码，沿原事务回到实时预览。
        } else {
            Err("展示模式选择无效".into()) // Mixed等未知状态不得猜作默认模式。
        }
    }
}

/// 只监听真实历史完成点；canRedo 自身会发 Checkpoint，故该通知不能用于刷新触发。
fn native_markdown_history_notifications() -> [&'static objc2_foundation::NSNotificationName; 3] {
    // SAFETY: Foundation 导出的通知名称为进程静态 NSString，本文仅在 AppKit 主线程订阅。
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
    /// 只反映窗口已接受的实际模式，Busy只能用它修复picker，不能授权模式动作。
    pub(super) published_presentation_mode: Cell<MarkdownPresentationMode>,
    pub(super) table_menu_item: OnceCell<Retained<NSMenuItem>>,
    pub(super) copy_document_menu_item: OnceCell<Retained<NSMenuItem>>,
    pub(super) status_label: OnceCell<Retained<NSTextField>>,
    /// 三列持久状态与临时消息分别拥有显示值；不持有文稿或保存权限。
    pub(super) footer_labels: OnceCell<[Retained<NSTextField>; 3]>,
    /// 首次显式打开才构造一次；隐藏后只保留本会话的空控件，不持有文稿。
    pub(super) information_panel:
        RefCell<Option<Rc<super::information_panel::NativeMarkdownInformationPanel>>>,
    /// 冲突条只拥有本窗口固定控件和原笔记弱票，不另造保存状态或文稿owner。
    pub(super) conflict_bar: OnceCell<super::conflict_bar::NativeMarkdownConflictBar>,
    footer_document: Cell<Option<NativeMarkdownFooterDocument>>,
    footer_save_result: Cell<NativeMarkdownFooterSaveResult>,
    feedback_visible: Cell<bool>,
    document_area: RefCell<Option<NativeMarkdownDocumentAreaBinding>>,
    document_area_revision: Cell<u64>,
    /// 页眉只观察已有控件；代次使重入resize撤销仍在栈内的旧setter。
    pub(super) header_controls: OnceCell<NativeMarkdownHeaderControls>,
    pub(super) header_layout_revision: Cell<u64>,
    /// 原文档动作与历史按钮共用真实有无文档状态；菜单只读它，不保存状态镜像。
    pub(super) document_actions_available: Cell<bool>,
    history_update_queued: Cell<bool>,
    history_refreshing: Cell<bool>,
    history_observing: Cell<bool>,
    history_manager: RefCell<Option<Weak<objc2_foundation::NSUndoManager>>>,
    /// 原栏只保Rust Weak，输入通知可在Surface借出期间撤权，不持控件或文稿强边。
    find_input_bar: RefCell<std::rc::Weak<NativeMarkdownFindBar>>,
    find_input_intent: Cell<u64>,
    pub(super) close_finalizer_scheduled: Cell<bool>,
    pub(super) workspace_controls: OnceCell<Vec<Retained<NSControl>>>,
    pub(super) next_sidebar_page: OnceCell<Retained<NSButton>>,
    // One foreground-only debounce timer; cancellation precedes note handoff and native close.
    pub(super) autosave_timer: RefCell<Option<Retained<objc2_foundation::NSTimer>>>,
    pub(super) autosave_generation: Cell<u64>,
    /// 搜索仅持有一个前台单次期限；回调弱引用本 controller，不捕获查询正文。
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
        /// 标准 AppKit 客户端使用按需会话 editor，不假定默认 editor 服从窗口历史 delegate。
        fn field_editor(&self, window: &objc2_app_kit::NSWindow, client: Option<&AnyObject>) -> Option<Retained<AnyObject>> {
            if native_markdown_controller_owns_editing_window(self, window) {
                // 预检辞任的 Closing/借出 owner 仍使用同一 provider；accepted freeze 独立禁止新建。
                self.ivars().field_editor_provider.provide(client, self.mtm())
            } else {
                // 旧 delegate 或其他窗口不能借用当前会话的 editor 与撤销权限。
                None
            }
        }

        #[unsafe(method_id(windowWillReturnUndoManager:))]
        /// 原生实际请求历史时才提供本窗口独占 manager，关闭后不再构造。
        fn undo_manager(&self, window: &objc2_app_kit::NSWindow) -> Option<Retained<objc2_foundation::NSUndoManager>> {
            if native_markdown_controller_owns_editing_window(self, window) {
                // 身份相同的实际请求按需创建历史，结束后 helper 保持封闭。
                self.ivars().undo.manager(&self.ivars().owned)
            } else {
                // 不向过期或外来窗口提供本会话 manager。
                None
            }
        }

        #[unsafe(method(windowDidResize:))]
        /// 复用唯一窗口delegate；实际resize只同步修改原控件frame，不排队或重建。
        fn window_did_resize(&self, notification: &NSNotification) {
            let Some(window) = notification.object().and_then(|object| object.downcast::<objc2_app_kit::NSWindow>().ok()) else {
                // 外来或已失去窗口身份的通知不授予布局权限。
                return;
            };
            if !native_markdown_controller_owns_editing_window(self, &window) {
                // 构造前和旧owner通知不进入运行期布局，构造完成另有同一helper调用。
                return;
            }
            if let Some(controls) = self.ivars().header_controls.get() {
                // OnceCell只借不可变weak元数据；helper的所有原生调用均不持TLS/RefMut。
                controls.layout(self, &window, NativeMarkdownHeaderAuthority::Foreground);
            }
            // 页眉完成后同步重排可见查找栏，沿同一delegate而不增加observer/计时器。
            self.layout_document_area(NativeMarkdownHeaderAuthority::Foreground);
        }

        #[unsafe(method(windowShouldClose:))]
        /// 原生关闭请求也只发同一typed intent；真正close由保存和严格屏障批准后执行。
        fn window_should_close(&self, window: &objc2_app_kit::NSWindow) -> bool {
            if native_markdown_controller_owns_editing_window(self, window)
                && !self.ivars().close_finalizer_scheduled.get()
                && let Some(panel) = self.ivars().editing_window.load()
            {
                // 此处绝不自行close；旧/重复请求仍由原生命周期coalescing和weak身份门禁处理。
                panel.enqueue_markdown_window_close();
            }
            // 拒绝AppKit绕过保存而直接关闭；production批准后使用不调用shouldClose的close()。
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

    // SAFETY: 搜索字段借用主线程 controller，关闭先清 delegate 再释放 owner。
    unsafe impl NSControlTextEditingDelegate for NativeMarkdownController {
        #[unsafe(method(controlTextDidBeginEditing:))]
        /// 在真实 begin 回调记录共享 editor 的实际历史归属。
        fn control_text_did_begin_editing(&self, notification: &NSNotification) {
            self.observe_control_editing(notification);
            if let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok()) {
                self.observe_find_control_input(&field, false, false); // 实际 begin 只同步状态跃迁，不把焦点变化当文字改变。
            }
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        /// 只有原生成功结束 client 后才结束该字段历史，失败验证不会收到此通知。
        fn control_text_did_end_editing(&self, notification: &NSNotification) {
            if let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok()) {
                self.observe_find_control_input(&field, false, true); // 只消费通知原字段；协议栈内先记真实结束，不提前刷新。
                // 通知携带原字段身份；迟到或重复结束由共享边界核对，不从当前窗口猜客户。
                let borrowed = self.ivars().borrowed.borrow().clone();
                if let Err(error) = borrowed.finished_control_editing(&field, &self.ivars().undo, &self.ivars().owned) {
                    // 已提交的字段值保留，未知历史不清；通过正常状态提示报告原始失败。
                    self.present_error(&error);
                }
                self.queue_history_availability_refresh();
            }
        }

        #[unsafe(method(controlTextDidChange:))]
        /// 先归并真实 Find 通知，再记录原历史；协议栈结束才刷新，组合候选始终留在原字段。
        fn control_text_did_change(&self, notification: &NSNotification) {
            if let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok())
                && !self.observe_find_control_input(&field, true, false) && self.owns_workspace_query(&field) {
                // 侧栏输入继续撤原Find请求，不把文稿查找接到工作区扫描路径。
                self.ivars().find_input_intent.set(self.find_input_intent().checked_add(1).expect("Find input intent overflow"));
            }
            self.observe_control_editing(notification);
            if let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok()) {
                // 只有本窗口搜索字段有提交权限；其他字段通知不能扫描工作区。
                self.enqueue_workspace_search(&field);
            }
        }
    }
    // SAFETY: NSTextFieldDelegate 继承上述声明的文本回调，生命周期相同。
    unsafe impl NSTextFieldDelegate for NativeMarkdownController {}

    impl NativeMarkdownController {
        #[unsafe(method(markdownFindOptionsDidChange:))]
        /// 只有本栏三个真实选项能撤旧意图；普通按钮仍走原显式导航和替换入口。
        fn markdown_find_options_did_change(&self, sender: &AnyObject) {
            let Some(button) = sender.downcast_ref::<NSButton>() else {
                return; // 非原选项控件没有刷新当前文稿查找的权限。
            };
            let bar = self.ivars().find_input_bar.borrow().upgrade();
            if let Some(bar) = bar.filter(|bar| bar.owns_search_option(button)) {
                self.enqueue_find_input_feedback(&bar, true);
            }
        }

        #[unsafe(method(markdownHistoryDidChange:))]
        /// 仅消费原会话 manager 的完成通知；不在 Undo/分组通知栈内查询 canRedo。
        fn markdown_history_did_change(&self, notification: &NSNotification) {
            let Some(manager) = notification.object().and_then(|object| object.downcast::<objc2_foundation::NSUndoManager>().ok()) else {
                return; // 非原管理器通知不能使工具栏借用其他窗口的历史。
            };
            if self.ivars().undo.owns_observed_manager(&manager) {
                self.queue_history_availability_refresh();
            }
        }

        #[unsafe(method(performMarkdownFindAction:))]
        /// 字段 Return 与按钮共享有限动作表；排队通知只携带当前 controller weak 与 tag。
        fn perform_markdown_find_action(&self, sender: &AnyObject) {
            if let Some(control) = sender.downcast_ref::<NSControl>()
                && let Some(action) = MarkdownFindAction::from_tag(control.tag())
            {
                native_markdown_trace! {
                    // 只记录已取得的有限tag及纯输入代次，用于区分字段原生action与driver显式Next。
                    eprintln!("[probe] find control action queued tag={} intent={}", action as isize, self.find_input_intent());
                }
                // 不把字段字符串或栏 owner 捕获进队列；执行时重新核验实际活笔记。
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
        /// 明确返回目录先清实际查询；排队期间的新输入撤销原按钮意图。
        fn refresh_markdown_workspace(&self, _sender: &AnyObject) {
            // 在Weak构造/排队的任何Native之前固定输入权限，不能执行时重新领取B的代次。
            let intent = self.workspace_query_intent();
            let identity = std::ptr::from_ref(self).addr();
            self.enqueue_owned(move || {
                if let Err(error) = return_native_markdown_workspace_to_directory(identity, intent) {
                    // Busy/缺失owner没有可用状态目标；实际输入失败由helper按原ticket报告。
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
        /// 有限读取查询并交给会话邮箱，排队通知不保留字段文本。
        fn search_markdown_workspace(&self, sender: &AnyObject) {
            if let Some(field) = sender.downcast_ref::<NSTextField>() {
                // Return 撤销尚未到期的自动提交，防止同一输入被迟到 timer 再次提交。
                self.cancel_workspace_search();
                if let Err(error) = enqueue_native_markdown_workspace_search(self, field) {
                    self.present_error(&error);
                }
            }
        }

        #[unsafe(method(performMarkdownWorkspaceAction:))]
        /// 真实自有菜单保留有限 tag；正文编辑跨队列仍须消费首 getter 前的原选择票。
        fn perform_markdown_workspace_action(&self, sender: &AnyObject) {
            // 保存结果的同一发布代次也必须先于sender/格内getter；不能在重入后给旧点击补新权限。
            let resolution_revision = self.conflict_publication();
            // 当前笔记文件动作先固定原Doc/路径；下方格内选择捕获本身也可能调用Native getter。
            let original_note = NativeMarkdownCurrentNoteIntent::capture(self);
            // sender 类型与菜单 getter 都可能重入；先纯捕获原正文票，不从 getter 后的新选择领权。
            let original_paged = NativeMarkdownWorkspacePagedEditIntent::capture(self);
            if let Some(action) = native_markdown_workspace_menu_action(self, sender) {
                if matches!(action, 22 | 23 | 100..=112 | 115..=116 | 118) {
                    if let Some(intent) = original_paged.filter(NativeMarkdownWorkspacePagedEditIntent::current)
                        && let Err(cause) = intent.perform(action)
                    { self.present_error(&cause); }
                    return; // 源码只消费自己的原Input票，不查询预览字段或把它改成正文fallback。
                }
                let original_note = if matches!(action, 4..=6 | 30..=34) {
                    let Some(intent) = original_note.filter(NativeMarkdownCurrentNoteIntent::current) else {
                        return; // 菜单getter已换文档，不向后来相同路径重捕文件动作权限。
                    };
                    Some(MainThreadBound::new(intent, self.mtm()))
                } else {
                    None // 其余原菜单继续各自权限，不附加当前笔记文件票。
                };
                if matches!(action, 4 | 5) && self.conflict_publication() != resolution_revision {
                    return; // 原结果在菜单getter期间已变化，不把同Doc的新冲突补签给旧点击。
                }
                // 只接受当前自有菜单的实际leaf；分组/任意外来tag不能执行文档命令。
                self.enqueue_owned(move || {
                    let mtm = MainThreadMarker::new().expect("正文菜单沿原主队列执行");
                    if original_note.as_ref().is_some_and(|intent| !intent.get(mtm).current()) {
                        return; // 同一窗口的文档A→B→A也不能消费旧Doc文件动作。
                    }
                    // 原票接纳后才进入既有命令事务；模态操作仍在 target callback 和 surface 借用结束后执行。
                    let result = match original_note.as_ref() {
                        // 当前笔记只消费原路径和Doc票，执行时不从侧栏领取目标。
                        Some(intent) if matches!(action, 4 | 5) => intent.get(mtm).perform_resolution(action, resolution_revision),
                        Some(intent) if action == 6 => super::information_panel::toggle(intent.get(mtm)),
                        Some(intent) => intent.get(mtm).perform(action),
                        None => perform_native_markdown_workspace_action(action),
                    };
                    if let Err(error) = result {
                        if matches!(action, 4 | 5) && original_note.as_ref().is_none_or(|intent| !intent.get(mtm).current()) {
                            // 原确认中的A已离开，不能把旧冲突错误显示到同窗口的后继B。
                            eprintln!("原笔记的冲突处理未完成，所属窗口或文稿已经变化");
                            return;
                        }
                        present_native_markdown_error(&error);
                    }
                });
            }
        }

        #[unsafe(method(resolveMarkdownConflict:))]
        /// 原冲突条按钮先捕获显示的Doc弱票和发布代次，再读sender并交原窗口队列。
        fn resolve_markdown_conflict(&self, sender: &AnyObject) {
            super::conflict_bar::enqueue_action(self, sender);
        }

        #[unsafe(method(closeMarkdownInformation:))]
        /// 原信息卡的关闭只移除本卡，不提交输入、不关闭插件或窗口。
        fn close_markdown_information(&self, _sender: &AnyObject) {
            self.hide_information_panel();
        }

        #[unsafe(method(refreshMarkdownInformationWords:))]
        /// 刷新词数只消费已显示卡片的原Doc弱身份，不先辞任正文或格内输入。
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
        /// 在首个原生 getter 前固定模式动作权限，队列只消费该原票而不重领文档代次。
        fn change_markdown_presentation_mode(&self, sender: &AnyObject) {
            if let Some(original) = NativeMarkdownWorkspacePagedEditIntent::capture(self) {
                // 原Input票先于sender getter；Body保留唯一模式意图，真实Ready后才沿原事务辞任/提交。
                // TLS只借出Rust Rc，所有Native getter均在借用结束后执行，Pending不再丢掉这个点击。
                let body = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    active.as_ref()?.editor.as_ref().map(|paged| Rc::clone(&paged.body))
                });
                if let Some(body) = body {
                    // 同步拒绝也由仍持原弱Input/Doc和点击代次的Body反馈，不覆盖后继笔记或按钮。
                    body.accept_presentation_mode_click(self, original, sender);
                }
                return;
            }
            // sender.downcast也可调用Native；先借出原surface并捕获旧源/输入票，不从getter之后领权。
            let feedback = (self.find_input_intent(), self.workspace_query_intent(), self.ivars().header_layout_revision.get());
            let original_note = NativeMarkdownCurrentNoteIntent::capture(self);
            let admitted = with_active_native_markdown_surface(|surface| {
                if !std::ptr::eq(&*surface.controller, self) || !native_markdown_controller_has_foreground_authority(self) {
                    // 外来或正在关闭的窗口不能从 sender 的当前值取得切换权限。
                    return Err("展示模式动作不属于当前窗口".to_owned());
                }
                // 没有文稿时仅改变下次打开的默认模式；不得重捕后来出现的正文。
                if surface.editor.is_some() { return Ok(None); }
                    let ticket = NativeMarkdownEmptyModeAction::empty(self, surface.presentation_mode);
                    let mode = ticket.read_destination(surface, sender)?;
                    Ok((mode != surface.presentation_mode).then_some((mode, ticket)))
            });
            match admitted {
                Ok(Some((mode, ticket))) => {
                    let ticket = MainThreadBound::new(ticket, self.mtm());
                    let original_note = MainThreadBound::new(original_note, self.mtm());
                    // 与既有enqueue_owned相同的main-queue弱controller，只让本回调可做必要picker补偿。
                    let weak = MainThreadBound::new(Weak::from(self), self.mtm());
                    enqueue_native_markdown_main_task(move || {
                        let mtm = MainThreadMarker::new().expect("mode action is delivered on main thread");
                        let ticket = ticket.get(mtm);
                        let Some(controller) = weak.get(mtm).load() else {
                            // 原控制器已释放，纯弱票只能撤销，不能寻找后继窗口执行旧命令。
                            return;
                        };
                        if !ticket.is_current() {
                            // Busy 或后续输入仅撤销原弱票，不排队重放，也不领取新的文档权限。
                            if native_markdown_controller_has_foreground_authority(&controller) {
                                // picker 只恢复该控制器已接受的真值；此标量不能授权切换或正文定位。
                                controller.select_presentation_mode(controller.ivars().published_presentation_mode.get());
                            }
                            return;
                        }
                        if let Err(error) = set_active_native_markdown_presentation_mode(mode) {
                            // 已受权动作仍可能被 IME、草稿提交或挂载拒绝，沿原入口保留真实失败。
                            eprintln!("展示模式切换失败：{error}");
                            if original_note.get(mtm).as_ref().is_some_and(NativeMarkdownCurrentNoteIntent::current)
                                && feedback == (controller.find_input_intent(), controller.workspace_query_intent(), controller.ivars().header_layout_revision.get())
                            {
                                // 原模式票可能因失败已取消；只有原笔记及原反馈仍有效才显示真实原因。
                                controller.present_error(&format!("展示模式未切换：{error}"));
                            }
                        }
                    });
                }
                // 重选模式已经同步清理原票，无需队列或界面补偿。
                Ok(None) => {},
                Err(error) => {
                    eprintln!("展示模式切换未接受：{error}");
                    // Busy不排队读较新surface，也不重放旧命令；仅用此controller已发布真值补偿picker。
                    if native_markdown_controller_has_foreground_authority(self) {
                        // 反馈和 picker 分开：先恢复同 owner 真值，原生重入后再核旧反馈票。
                        self.select_presentation_mode(self.ivars().published_presentation_mode.get());
                        if native_markdown_controller_has_foreground_authority(self)
                            && feedback == (self.find_input_intent(), self.workspace_query_intent(), self.ivars().header_layout_revision.get())
                            && original_note.as_ref().is_some_and(NativeMarkdownCurrentNoteIntent::current) {
                            // 原笔记和反馈均未变时打开错误行；不能只改仍隐藏的label，也不覆盖后继笔记。
                            self.present_error(&format!("展示模式未切换：{error}"));
                        }
                    }
                }
            }
        }
    }
);

impl NativeMarkdownController {
    /// 保存快捷键只排队原 controller 与正文的弱身份，不让迟到按键保存后继笔记。
    pub(super) fn enqueue_document_save(&self, panel: &NativeMarkdownPanel) -> bool {
        // 原生 weak 构造先于 TLS 借用；随后纯校验 getter 重入后的实际 owner。
        let controller = MainThreadBound::new(Weak::from(self), self.mtm());
        let document = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active.as_ref()?;
            if !std::ptr::eq(&*surface.controller, self) || !std::ptr::eq(&*surface.panel, panel) {
                // 外来 delegate 或已替换窗口不能取得当前正文的保存权限。
                return None;
            }
            // 空窗口也绑定 None 状态；等待期间打开的新正文不能被旧按键顺带保存。
            Some(
                surface
                    .editor
                    .as_ref()
                    .map(|editor| Rc::downgrade(editor.document())),
            )
        });
        let Some(document) = document else {
            // 原事件已过期，不排队、不读取正文，也不改变当前状态提示。
            return false;
        };
        let document = MainThreadBound::new(document, self.mtm());
        // 所有 TLS/文档借用已结束；队列仅持 weak，不延长窗口、正文或映射寿命。
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("保存快捷键仅在主队列执行");
            let Some(controller) = controller.get(mtm).load() else {
                // 原 controller 已释放，迟到请求没有可恢复的保存目标。
                return;
            };
            let is_current = || {
                ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    let Some(surface) = active.as_ref() else {
                        // 关闭或暂借 owner 期间不进入另一笔记的保存命令。
                        return false;
                    };
                    if !std::ptr::eq(&*surface.controller, &*controller) {
                        // 同一文件重新打开仍属于新 controller，旧事件不能继承权限。
                        return false;
                    }
                    match (document.get(mtm), surface.editor.as_ref()) {
                        // 活 editor 强拥有当前正文；Weak 比较只核分配身份，不复制或读取文本。
                        (Some(expected), Some(editor)) => {
                            std::rc::Weak::ptr_eq(expected, &Rc::downgrade(editor.document()))
                        }
                        (None, None) => true, // 原空窗口仍为空，沿用现有保存空操作。
                        _ => false,           // 笔记交接或创建改变了目标，必须由新的用户动作保存。
                    }
                })
            };
            if !is_current() {
                // 排队期间的交接撤销原按键，不能保存新目标或对它显示旧错误。
                return;
            }
            if let Err(error) = perform_native_markdown_workspace_action(1) {
                // 同一入口提交表格/IME并检查 revision/pending recovery；失败保留原草稿。
                if is_current() {
                    // 保存期间若重入关闭/交接，旧错误也不能覆盖新正文的状态。
                    controller.present_error(&error);
                }
            }
        });
        true
    }

    /// 输入通知可能来自 native 重入栈，先排 weak 请求，离开原借用后再撤销扫描/创建期限。
    fn enqueue_workspace_search(&self, field: &NSTextField) {
        if !self.owns_workspace_query(field) {
            // 外来通知不得撤销本搜索字段的期限或访问其内容。
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
                // 弱请求不保活原窗口或字段，关闭后不构造任何搜索资源。
                return;
            };
            if controller.ivars().workspace_search_generation.get() != generation {
                // 同一事件中较新的输入或 Return 已替代本请求，不能重启旧期限。
                return;
            }
            if let Err(error) = controller.schedule_workspace_search(&field) {
                // 失败保留原字段文字与正文，状态提示不吞掉输入。
                controller.present_error(&error);
            }
        });
    }

    /// 只登记装配完成的原栏弱身份；换笔记自然撤旧栏，不额外持有字段或正文。
    pub(super) fn register_find_input_bar(&self, bar: &Rc<NativeMarkdownFindBar>) {
        *self.ivars().find_input_bar.borrow_mut() = Rc::downgrade(bar);
    }

    /// 纯Rust身份核验允许反馈setter重入，借用在Native调用前已经结束。
    pub(super) fn owns_find_input_bar(&self, bar: &std::rc::Weak<NativeMarkdownFindBar>) -> bool {
        std::rc::Weak::ptr_eq(&self.ivars().find_input_bar.borrow(), bar)
    }

    /// settled刷新只升级已经登记的原栏，不新建控件或从当前文件名猜查询归属。
    pub(super) fn current_find_input_bar(&self) -> Option<Rc<NativeMarkdownFindBar>> {
        self.ivars().find_input_bar.borrow().upgrade()
    }

    /// 显式动作与真实输入共用原意图；先推进才能撤销尚在队列中的旧反馈。
    pub(super) fn advance_find_input_intent(&self) -> u64 {
        let intent = self
            .find_input_intent()
            .checked_add(1)
            .expect("Find input intent overflow");
        self.ivars().find_input_intent.set(intent);
        intent
    }

    /// 输入先撤原意图并清旧反馈；队列只带原bar/Doc弱票，绝不读取后继笔记重新领票。
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
            return; // 旧栏或已释放Doc只撤原请求，不读取Native输入或恢复反馈。
        };
        feedback.clear(self);
        if !feedback.current(self) {
            return; // 清状态重入的新输入/关闭已经撤掉原票，不能再安排旧查询。
        }
        feedback.schedule_highlights(self);
        if !feedback.current(self) {
            return; // 原 view 的 Native claim 或重绘请求也能重入；不续排已撤销的反馈。
        }
        let controller = MainThreadBound::new(Weak::from(self), self.mtm());
        let feedback = MainThreadBound::new(feedback, self.mtm());
        self.enqueue_owned(move || {
            let mtm = MainThreadMarker::new().expect("Find反馈只在主队列执行");
            if let Some(controller) = controller.get(mtm).load() {
                feedback.get(mtm).run(&controller);
            } // 原controller消失后只丢弱票，不创建替代栏或重试任务。
        });
    }

    /// 纯读取使Borrowed期间的输入也可撤掉旧executor，不依赖Active槽能否借出。
    pub(super) fn find_input_intent(&self) -> u64 {
        self.ivars().find_input_intent.get()
    }

    /// 只读输入意图代次；Native getter重入didChange后，旧Next/查询不能撤销新输入。
    pub(super) fn workspace_query_intent(&self) -> u64 {
        self.ivars().workspace_search_generation.get()
    }

    /// 实际拥有的搜索字段才可提交；不依赖通知发出者或可变显示标签猜身份。
    pub(super) fn owns_workspace_query(&self, field: &NSTextField) -> bool {
        self.ivars()
            .workspace_controls
            .get()
            .and_then(|controls| controls.first())
            .is_some_and(|control| {
                // 控件按构造合同以搜索框为首项，指针相等仅在双方仍活时使用。
                std::ptr::eq(&**control, field.as_super())
            })
    }

    /// 恢复旧界面 140 ms 输入后搜索；只延迟有限字段读取，不持有历史查询副本。
    fn schedule_workspace_search(&self, field: &NSTextField) -> Result<(), String> {
        if !self.owns_workspace_query(field) || !native_markdown_controller_is_current(self) {
            // 关闭、外来字段或旧会话没有取消当前查询/构造 timer 的权限。
            return Ok(());
        }
        self.cancel_workspace_search();
        with_active_native_markdown_surface(|surface| {
            // 新输入立即撤销旧扫描/结果权限，避免 debounce 期间显示过期查询结果。
            surface.stop_search();
            Ok(())
        })?;
        if field.currentEditor().is_some_and(|editor| {
            // 仅查看已存在的实际 editor；不为查询创建另一个 field editor。
            editor
                .downcast_ref::<NSTextView>()
                .is_some_and(|view| view.hasMarkedText())
        }) {
            // 输入法组合文字尚未提交；下一次真实 didChange 再安排搜索。
            return Ok(());
        }
        let generation = self.ivars().workspace_search_generation.get();
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        let field = MainThreadBound::new(Weak::from(field), self.mtm());
        let block = block2::RcBlock::new(move |_timer: NonNull<objc2_foundation::NSTimer>| {
            let mtm = MainThreadMarker::new().expect("workspace search timer uses the main thread");
            let Some(controller) = weak.get(mtm).load() else {
                // 关闭后的 timer 不保活 controller、文档或查询字符串。
                return;
            };
            if controller.ivars().workspace_search_generation.get() != generation
                || !native_markdown_controller_is_current(&controller)
            {
                // 过期期限只返回，不能取消新代次 timer 或扫描。
                return;
            }
            controller.cancel_workspace_search();
            if let Some(field) = field.get(mtm).load() {
                // 当前 owner 重新有限读取实际字段；失败仍保留用户输入并显示原因。
                if let Err(error) = enqueue_native_markdown_workspace_search(&controller, &field) {
                    controller.present_error(&error);
                }
            }
        });
        // SAFETY: 主线程创建单次 timer；闭包仅持 MainThreadBound<Weak> 与标量，不保活业务图。
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                0.14, false, &block,
            )
        };
        if let Err(error) = self.ivars().owned.include("workspace.search.timer", &timer) {
            // 未登记的 candidate 不得留下活 timer；输入和既有正文完全保留。
            timer.invalidate();
            return Err(error);
        }
        if self.ivars().workspace_search_generation.get() != generation
            || !native_markdown_controller_is_current(self)
        {
            // 登记含原生 weak 操作；失效的 candidate 不得覆盖更新输入的期限。
            timer.invalidate();
            return Ok(());
        }
        let previous = self.ivars().workspace_search_timer.replace(Some(timer));
        drop(previous);
        Ok(())
    }

    /// 撤销期限权限后在 RefCell 借用外 invalidate，关闭不留下原生回调或容量。
    pub(super) fn cancel_workspace_search(&self) {
        self.ivars().workspace_search_generation.set(
            self.ivars()
                .workspace_search_generation
                .get()
                .wrapping_add(1),
        );
        let timer = self.ivars().workspace_search_timer.take();
        if let Some(timer) = timer {
            // 显式结束 run loop 所有权；释放 block 时可重入，槽已为空且代次已失效。
            timer.invalidate();
        }
    }

    /// 真实字符提交只重置原850ms单次期限；同一弱笔记票贯穿Native取消、创建和到期保存。
    pub(super) fn schedule_autosave(
        &self,
        original: &NativeMarkdownCurrentNoteIntent,
        current: &impl Fn() -> bool,
    ) {
        self.cancel_autosave();
        if !current() {
            return; // 旧timer释放可重入换笔记或保存；失权后不能占用后继笔记的唯一槽。
        }
        let generation = self.ivars().autosave_generation.get();
        let weak = MainThreadBound::new((Weak::from(self), original.clone()), self.mtm());
        if !current() || self.ivars().autosave_generation.get() != generation {
            return; // 原生Weak构造同样在槽借用之外；重入产生的新期限保留原所有权。
        }
        let block = block2::RcBlock::new(move |_timer: NonNull<objc2_foundation::NSTimer>| {
            let mtm = MainThreadMarker::new().expect("原自动保存期限只在主队列执行");
            let (weak, original) = weak.get(mtm);
            let Some(controller) = weak.load() else {
                return; // 到期闭包不保活窗口、文稿或映射；已结束的会话不再取得保存权限。
            };
            if controller.ivars().autosave_generation.get() != generation
                || !native_markdown_controller_is_current(&controller)
                || !original.current()
            {
                return; // 同控制器的后继笔记或新期限也不能继承这张原笔记票。
            }
            controller.cancel_autosave();
            if !original.current()
                || controller.ivars().autosave_generation.get() != generation.wrapping_add(1)
            {
                return; // invalidate可重入：后继期限或换笔记已撤权，不能继续保存或清它的槽。
            }
            let result = with_active_native_markdown_surface(|surface| {
                if native_markdown_operation_is_busy(
                    surface.pending_note_switch.is_some(),
                    surface.pending_file_mutation.is_some(),
                ) {
                    return Ok(()); // 已接纳的交接独占保存/释放，本期限不重试或与它交叉。
                }
                // 取得原surface租约后只核同一Doc/路径；不在借出期间从Active重捕另一张票。
                if !std::ptr::eq(&*surface.controller, &*controller)
                    || surface.current_note_relative_path.as_deref()
                        != Some(original.relative_path.as_str())
                    || !surface.editor.as_ref().is_some_and(|editor| {
                        std::ptr::eq(original.document.as_ptr(), Rc::as_ptr(editor.document()))
                    })
                {
                    return Ok(()); // 原Doc已离开该具体用例，禁止向另一正文执行磁盘副作用。
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
                    // 唯一到期槽已经归还；格内草稿和系统候选仍归各自输入客户，不能定时强制提交。
                    // 草稿结束或正文真实commit/unmark会沿原observer恢复期限，不轮询、不报普通输入错误。
                    return Ok(());
                }
                surface.save_current_document()
            });
            if let Err(error) = result
                && original.current()
            {
                // 已提交草稿及原保存错误保持原语义；迟到错误不能出现在后继笔记的提示栏。
                controller.present_error(&error);
            }
        });
        // SAFETY: 只捕获MainThreadBound中的原controller/Doc弱票及标量代次，实际回调留在主run loop。
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                0.85, false, &block,
            )
        };
        if !current() || self.ivars().autosave_generation.get() != generation {
            timer.invalidate();
            return; // 只撤本次尚未入槽的局部timer，绝不清除重入产生的后继期限。
        }
        let previous = self.ivars().autosave_timer.replace(Some(timer));
        drop(previous); // Native析构在槽借用外；原代次已核验，不能覆盖其他回调刚发布的timer。
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

    /// accepted handoff 期间封闭窗口级查询输入，避免共享 editor 重新承载新业务而覆盖旧收据。
    pub(super) fn set_workspace_input_enabled(&self, enabled: bool) {
        if let Some(controls) = self.ivars().workspace_controls.get() {
            for control in controls {
                if let Some(field) = control.downcast_ref::<NSTextField>() {
                    // 只配置项目自建搜索字段；不接管 AppKit Find 私有子树。
                    field.setEnabled(enabled);
                    field.setEditable(enabled);
                    field.setSelectable(enabled);
                }
            }
        }
    }

    /// 必要空态补偿只在同一空owner继续；每个setter后检查，避免关闭重入后改后继控件。
    pub(super) fn set_workspace_input_enabled_while(
        &self,
        enabled: bool,
        still_owned: impl Fn() -> bool,
    ) {
        if let Some(controls) = self.ivars().workspace_controls.get() {
            for control in controls {
                // 仍只处理项目自有字段；B输入不会撤补偿权限，因此三个属性一定保持一致。
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

    /// 原生 getter 前复制 tracker，任何 RefCell 借用都不跨实际编辑回调。
    fn observe_control_editing(&self, notification: &NSNotification) {
        if let Some(field) = notification
            .object()
            .and_then(|object| object.downcast::<NSTextField>().ok())
        {
            // 该控制器只作为项目搜索字段 delegate；正文/表格由其自身桥接记录。
            let borrowed = self.ivars().borrowed.borrow().clone();
            if let Err(error) =
                borrowed.observe_editing(&field, &self.ivars().undo, &self.ivars().owned)
            {
                // 归属错误进入后续预检失败，不清空未知历史或丢弃用户输入。
                self.present_error(&error);
            }
            self.queue_history_availability_refresh();
        }
    }

    /// 不替换字段委托；实际窗口事件结束时补组合状态边界并记录共享 editor 的原历史。
    pub(super) fn observe_native_field_editor(&self, editor: &NSTextView) {
        self.observe_find_native_editor(editor); // 事件结束补漏只观察状态；协议 hook 已完成的输入不会再推进意图。
        let borrowed = self.ivars().borrowed.borrow().clone();
        if let Err(error) =
            borrowed.observe_native_editor(editor, &self.ivars().undo, &self.ivars().owned)
        {
            // 只保存有限归属失败，后续关闭会保留原编辑面。
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
            // 紧凑状态栏放不下长错误时，悬停仍可读取同一有界中文消息，不另存正文。
            status_label.setToolTip(Some(&native_message));
            status_label.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                0.96, 0.43, 0.43, 1.0,
            )));
        }
        self.set_feedback_visible(true);
    }

    /// 恢复的旧草稿缺少原保存基线，明确展示可用解决入口而不伪装成普通已保存笔记。
    pub(super) fn present_pending_legacy_recovery(&self) {
        self.present_error("恢复草稿待处理：请另存或重新载入");
        if let Some(status_label) = self.ivars().status_label.get() {
            // 固定提示解释原文件未覆盖；只有活动窗口持有这个界面说明。
            status_label.setToolTip(Some(ns_string!(
                "已恢复旧冲突草稿，原文件未被覆盖。请在“文档”菜单中选择“将草稿另存为新笔记”或“从磁盘重新载入”。"
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

    /// 合并真实编辑/焦点事件与历史完成通知；队列只借原 controller，不捕获文稿或 manager。
    pub(super) fn queue_history_availability_refresh(&self) {
        if !self.ivars().document_actions_available.get()
            || !native_markdown_controller_has_foreground_authority(self)
            || self.ivars().history_update_queued.replace(true)
        {
            return; // 空态、关闭或同一批事件已有更新时，不创建第二任务或任何历史对象。
        }
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("历史按钮更新沿原主队列执行");
            let Some(controller) = weak.get(mtm).load() else {
                return; // 旧任务不保活窗口，也不为后继窗口寻找另一个 controller。
            };
            controller.ivars().history_update_queued.set(false);
            controller.refresh_history_availability();
        });
    }

    /// 显示本按钮实际将操作的历史：表格真实字段使用本地历史，其余仍为唯一 Core 历史。
    fn refresh_history_availability(&self) {
        if !self.ivars().document_actions_available.get()
            || !native_markdown_controller_has_foreground_authority(self)
            || self.ivars().history_refreshing.replace(true)
        {
            return; // canRedo 的 checkpoint 可重入；当前更新栈不再启动第二轮 getter。
        }
        // 原租约将 surface 移到 TLS 借用之外；所有 Native getter 和控件更新均在借用结束后执行。
        let _ = with_active_native_markdown_surface(|surface| {
            if !std::ptr::eq(&*surface.controller, self)
                || native_markdown_operation_is_busy(
                    surface.pending_note_switch.is_some(),
                    surface.pending_file_mutation.is_some(),
                )
            {
                return Ok(()); // 交接中的原文稿不向工具栏发布可编辑状态。
            }
            let Some(editor) = surface.editor.as_ref() else {
                return Ok(()); // 空态按钮已在 set_copy_document_available(false) 中同步停用。
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
                            _ => false, // 查询期间结束或重开原格，不能向新客户发布旧按钮状态。
                        }
                        && target.input_state().is_ok_and(|state| state == before)
                };
                let available = if before.marked.is_some() {
                    [false; 2] // 候选仍归当前输入法；历史按钮不能强制确认候选或改走另一层历史。
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
                Ok(()) // 格内只查真实局部栈，正文只查 canonical，不创建字段 Undo manager。
            }
        });
        self.ivars().history_refreshing.set(false); // 当前同步栈归还后，后续真实通知才可再次更新。
    }

    /// 接受拆图时先移走 Weak，再移除本 controller 的三项原历史观察；不访问或清理历史内容。
    fn remove_history_notifications(&self) {
        let previous = self.ivars().history_manager.take();
        if !self.ivars().history_observing.replace(false) {
            return; // 未订阅过的 controller 没有通知中心清理义务。
        }
        let center = objc2_foundation::NSNotificationCenter::defaultCenter();
        for name in native_markdown_history_notifications() {
            // SAFETY: 只撤本 observer 的准确三个 name；nil object 覆盖本方法已登记的旧 manager，不触碰他人观察。
            unsafe { center.removeObserver_name_object(self, Some(name), None) };
        }
        drop(previous); // Native Weak 的归还在状态借用之外，不持有 manager 或文稿强边。
    }

    /// 仅真实canonical文档存在时启用复制全文菜单，空态不伪装有可导出正文。
    pub(super) fn set_copy_document_available(&self, available: bool) {
        self.ivars().document_actions_available.set(available);
        if let Some(copy_document_item) = self.ivars().copy_document_menu_item.get() {
            // 空工作区无正文可复制，菜单项不能假装可用。
            copy_document_item.setEnabled(available);
        }
        if let Some(controls) = self.ivars().workspace_controls.get() {
            for control in controls.iter().skip(NATIVE_MARKDOWN_TOOLBAR_START + 2) {
                if self.ivars().close_finalizer_scheduled.get() {
                    return;
                }
                // 其余文档动作沿原有文档门槛；撤销/重做另外显示该按钮实际将操作的历史。
                control.setEnabled(available);
            }
        }
        if let Some(button) = self.ivars().presentation_mode_button.get() {
            // 源码切换保持原有文档门槛；空态不能产生没有正文的模式操作。
            button.setEnabled(available);
        }
        set_current_note_actions_available(self, available); // 同一文档有无状态同步原“更多”五项。
        if !available {
            // 接受文档交接后立刻停用原按钮；队列中的旧更新不能跨到后继或空文稿。
            self.remove_history_notifications();
            if let Some(header) = self.ivars().header_controls.get() {
                header.set_history_available([false; 2], &|| {
                    !self.ivars().document_actions_available.get()
                });
            }
        } else {
            self.queue_history_availability_refresh(); // 初次发布完成后读取原 Core；构造期间不创建 manager。
        }
    }

    /// 表格菜单沿原可编辑文档状态启用，不另建常驻工具栏按钮。
    pub(super) fn set_table_insertion_available(&self, available: bool) {
        if let Some(table_item) = self.ivars().table_menu_item.get() {
            // 表格菜单项只在已有canonical文档时可用。
            table_item.setEnabled(available);
        }
    }

    /// Clears transient command feedback after an operation without allocating a status history.
    pub(super) fn clear_status(&self) {
        if let Some(status_label) = self.ivars().status_label.get() {
            // Clear the existing label without creating a background status lifecycle.
            status_label.setStringValue(ns_string!(""));
            // 清空可见状态时同时撤销旧提示，避免下一文档沿用前一个决定的信息。
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
        // 接受关闭后不把路径或恢复错误留在项目自有状态控件中。
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
                    // 本项目配置的搜索 delegate 才可断开，不修改框架 Find 控件委托。
                    // SAFETY: 该主线程字段的项目 delegate 由当前 controller 配置，nil 是有效解除关系。
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
            // SAFETY: 主线程复制菜单项在controller释放前先断开自己的借用回调。
            unsafe {
                copy_document_item.setTarget(None);
                copy_document_item.setAction(None);
            }
        }
        if let Some(table_item) = self.ivars().table_menu_item.get() {
            // SAFETY: 主线程表格菜单项在controller释放前先断开自己的借用回调。
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

    /// 同步已接受的实际模式与原生选择控件，标量只供反馈补偿使用。
    pub(super) fn select_presentation_mode(&self, mode: MarkdownPresentationMode) {
        // caller只传已接受的新模式、原模式或窗口初始化真值；Native setter前先同步纯反馈标量。
        self.ivars().published_presentation_mode.set(mode);
        if let Some(button) = self.ivars().presentation_mode_button.get() {
            // 成功或否决都只显示已接受模式；按钮点击尚未完成的状态不能成为正文真值。
            button.setState(if mode == MarkdownPresentationMode::Source {
                objc2_app_kit::NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
        }
        // 菜单只跟随已接受的实际模式；预览中的折叠项不能被“有文稿”状态重新启用。
        set_current_note_actions_available(self, self.ivars().document_actions_available.get());
        self.render_footer();
    }
}

/// 原字符编辑回调返回后只排弱通知；同一笔记票贯穿状态投影和唯一自动保存期限。
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) fn queue_native_markdown_autosave_for_storage(storage: &PieceBackedMarkdownTextStorage) {
    #[cfg(feature = "native-markdown-runtime-probe")]
    {
        // 先记录实际调用，再交给原队列；失败输入须连这个入口都不触发，不能只比较尚未运行的计时器。
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
            // 原storage已随会话释放，迟到通知不能为后继正文安排磁盘保存。
            return;
        };
        let original = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active.as_ref()?;
            let editor = surface.editor.as_ref()?;
            let document = storage.document()?;
            if !Rc::ptr_eq(editor.document(), &document) {
                return None; // 迟到的旧storage只可释放，不能观察或安排后继笔记的保存。
            }
            let original = NativeMarkdownCurrentNoteIntent::for_document(
                &surface.controller,
                &surface.panel,
                editor.document(),
                surface.current_note_relative_path.as_deref()?,
            );
            Some((surface.controller.clone(), original))
        }); // 仅捕获既有原票与controller claim；所有Doc借用/Native刷新都在TLS借用结束后。
        if let Some((controller, original)) = original {
            publish_native_markdown_document_edit(controller, original);
        }
    });
}

/// 正文和单格的真实输入事件共用原历史刷新队列；弱文稿只限定当前业务owner，不保活正文。
pub(super) fn queue_native_markdown_history_for_document(document: &WeakMarkdownDocument) {
    let controller = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        let surface = active.as_ref()?;
        let editor = surface.editor.as_ref()?;
        std::rc::Weak::ptr_eq(document, &Rc::downgrade(editor.document()))
            .then(|| surface.controller.clone())
    });
    if let Some(controller) = controller {
        // 离开TLS借用才创建原Native弱票；原队列执行时读取当前客户，不携带过期canUndo快照。
        controller.queue_history_availability_refresh();
    }
}

/// 草稿结束后恢复原canonical的保存期限；输入中的局部变化只更新历史，不重置正文debounce。
pub(super) fn queue_native_markdown_local_input_for_document(document: &WeakMarkdownDocument) {
    let mtm = MainThreadMarker::new().expect("单格输入反馈沿原主线程排队");
    let document = MainThreadBound::new(document.clone(), mtm);
    enqueue_native_markdown_main_task(move || {
        let mtm = MainThreadMarker::new().expect("单格输入反馈沿原主队列执行");
        let document = document.get(mtm);
        // 复制或切换模式会先借出surface再结束草稿；等原租约归还后才读Active，不能把暂借当作笔记关闭。
        // 队列只带原Doc Weak，重开同路径或换笔记后自然撤销，不保存旧dirty或历史可用性快照。
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
            // Esc取消或未改格的模式切换也可能发生在旧timer到期之后；同Doc在预览仍须恢复原期限。
            // 活动格期间条件为false，不因每次局部键入重置正文保存，也不定时轮询或强制确认候选。
            queue_native_markdown_autosave_for_document(document.clone());
        }
    });
}

/// 分页正文只转运原canonical Weak；迟到提交不能为已切换的笔记重排自动保存。
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

/// 两个真实编辑后端共用既有footer、冲突结果与唯一自动保存期限；不增加另一保存策略。
fn publish_native_markdown_document_edit(
    controller: Retained<NativeMarkdownController>,
    original: NativeMarkdownCurrentNoteIntent,
) {
    if !original.current() {
        return; // 原storage claim的释放或已接纳交接不能给旧通知恢复权限。
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
    drop(document); // 只保O(1)显示标量；后续Native可重入，不能跨canonical借用或强Doc claim。
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
    }; // 同Doc重入编辑/保存或冲突结果也撤旧聚合，避免取消新期限或重排刚暂停的保存。
    if !current() {
        return;
    }
    controller.refresh_footer_document(Some(footer));
    if !current() {
        return; // footer布局Native setter已换笔记或改变同Doc，后续Info/timer均停止。
    }
    controller.refresh_information_panel(false);
    if !current() {
        return; // 信息卡只更新字符等聚合；其Native回调不能把保存决策带到后继文稿。
    }
    if footer.is_dirty() {
        controller.schedule_autosave(&original, &current);
    } else {
        controller.cancel_autosave(); // 真正保存或Undo到clean只取消该原票仍拥有的期限。
    }
    if !current() {
        return; // timer创建/取消也可重入；原真实历史刷新不能借旧通知访问新笔记。
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

/// 正常界面变更只认Active或确切借出owner；accepted准备与Closing不授予frame/输入发布权。
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
    // 此处只读TLS身份/标量，没有Weak升级或Native调用跨借用；不查询Closing槽。
    foreground_flags_allow_mutation(active, borrowed, preparing_close, close_scheduled)
}

/// show借出态与resize Active态共享同一纯门槛，不能把活对象或旧revision当成关闭后权限。
fn foreground_flags_allow_mutation(
    active: bool,
    borrowed: bool,
    preparing_close: bool,
    close_scheduled: bool,
) -> bool {
    (active || borrowed) && !preparing_close && !close_scheduled
}

/// 窗口和 controller 同时匹配实际 Active、Closing 或暂借 owner；全部 TLS 借用只读取身份。
pub(super) fn native_markdown_controller_owns_editing_window(
    controller: &NativeMarkdownController,
    window: &objc2_app_kit::NSWindow,
) -> bool {
    let Some(panel) = controller.ivars().editing_window.load() else {
        // controller 不拥有窗口强引用；窗口结束后不恢复输入能力。
        return false;
    };
    let owned_window: &objc2_app_kit::NSWindow = &panel;
    let exact_window = std::ptr::eq(owned_window, window);
    drop(panel);
    if !exact_window {
        // 先结束 weak 临时升级，再读全局 owner；外来窗口不会授予权限。
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

#[cfg(test)]
mod foreground_authority_tests {
    use super::foreground_flags_allow_mutation;

    #[test]
    /// 借出中的show可发布；accepted close尚未撤借出标量时也须立即停止普通setter。
    fn borrowed_show_and_active_resize_stop_at_accepted_or_native_close() {
        assert!(foreground_flags_allow_mutation(true, false, false, false));
        assert!(foreground_flags_allow_mutation(false, true, false, false));
        assert!(!foreground_flags_allow_mutation(false, false, false, false));
        assert!(!foreground_flags_allow_mutation(false, true, true, false));
        assert!(!foreground_flags_allow_mutation(true, false, false, true));
    }
}
