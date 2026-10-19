//! 实际产品 surface 的具名资源场景；由共享 AppKit 驱动分事件执行，不作物理输入声明。

use super::*;
use objc2_app_kit::NSWindow;
use omni_plugin_sdk::{PluginCloseIntent, PluginError};

/// 每个场景独立激活/关闭，不借用上一场景的窗口、controller 或工作区。
/// 编号固定对应：0 未提交搜索，1 已提交可取消搜索，2 Find，3 Replace，4 表格草稿，
/// 5 旧笔记交接排队后关闭，6 正文保存冲突，7 表格保存冲突，8 分事件 Replace 双字段；
/// case 3 保留同事件双字段的真实回归，case 8 不替换或弱化该场景。
pub(crate) const NATIVE_MARKDOWN_RESOURCE_PROBE_CASES: usize = 9;

/// 只消费产品验证后的精确关闭回执；标量身份和完成次数本身不是资源释放证明。
/// 实际资源、逐角色与 borrowed 状态检查仍由产品 lifecycle 负责，夹具只推进后续事件。
#[derive(Default)]
struct ResourceProbeLifecycle {
    expected_intent: Cell<Option<PluginCloseIntent>>,
    accepted: Cell<Option<PluginCloseRequest>>,
    completed: Cell<Option<PluginCloseRequest>>,
    expected_saved: Cell<Option<omni_markdown_core::MarkdownDocumentRevision>>,
    completion: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl PluginHostLifecycle for ResourceProbeLifecycle {
    fn commit_creation(&self) -> Result<(), PluginError> {
        Ok(())
    }

    fn begin_close(&self, request: PluginCloseRequest) -> Result<(), PluginError> {
        if request.scope != PluginCloseScope::ForegroundTransition {
            // 本夹具不模拟真实待退出 Host，不提供终止 scope 授权。
            return Err(PluginError::new(
                "probe_scope",
                "resource fixture requires foreground close",
            ));
        }
        if self.expected_intent.get() != Some(request.intent) {
            // 窗口关闭必须真正返回启动器；其余显式 SDK 隐藏场景仍核原 DismissAndResume。
            return Err(PluginError::new(
                "probe_close_intent",
                "资源场景没有收到原动作对应的关闭意图",
            ));
        }
        if self
            .accepted
            .get()
            .is_some_and(|accepted| accepted != request)
        {
            // 首次接受后不能换 intent 或 scope，否则后来的回调无法证明属于同一次关闭。
            return Err(PluginError::new(
                "probe_identity",
                "resource fixture close identity changed",
            ));
        }
        if self.accepted.get().is_none() {
            // begin_close 发生在真实保存和辞任之后，只拷贝 canonical revision，不持有正文或路径。
            let expected = ACTIVE_NATIVE_MARKDOWN_SURFACE
                .with_borrow(|surface| {
                    surface
                        .as_ref()
                        .and_then(|surface| surface.editor.as_ref())
                        .map(|editor| editor.document().borrow().current_revision())
                })
                .transpose()
                .map_err(|error| {
                    // 原保存摘要不可读就拒绝接受关闭验收；不能用None绕过随后磁盘核对。
                    eprintln!("native_resource_probe stage=revision_failed cause={error}");
                    PluginError::new("probe_revision", "无法核对已保存正文的摘要")
                })?;
            self.expected_saved.set(expected);
        }
        self.accepted.set(Some(request));
        Ok(())
    }

    fn finish_close(&self, request: PluginCloseRequest) {
        // 产品先完成资源证明才调用这里；这两项断言只核验回执身份和唯一消费，不能替代该证明。
        assert_eq!(
            self.accepted.get(),
            Some(request),
            "resource fixture exact accepted request"
        );
        assert!(
            self.completed.replace(Some(request)).is_none(),
            "resource fixture completion exactly once"
        );
        let completion = self.completion.borrow_mut().take();
        if let Some(completion) = completion {
            // 回调只持 driver weak 和标量；先结束借用再投递真实下一事件。
            completion();
        }
    }
}

struct ResourceProbeHost {
    root: std::path::PathBuf,
    lifecycle: Rc<ResourceProbeLifecycle>,
}

/// case0 先由真实取消终点完成负例，再由真实激活终点完成原正例，最后继续原搜索场景。
#[derive(Clone, Copy, Eq, PartialEq)]
enum ResourceToolbarStage {
    PrepareCancellation,
    AwaitCancellation,
    PrepareInsertion,
    AwaitInsertion,
    Complete,
}

impl OmniBarHostContext for ResourceProbeHost {
    fn workspace_root(&self) -> &Path {
        &self.root
    }
    fn application_data_root(&self) -> &Path {
        &self.root
    }
    fn mount_native_view(&mut self, _view: PluginNativeView) -> Result<(), PluginError> {
        Ok(())
    }
    fn read_clipboard_text(&mut self) -> Result<Option<String>, PluginError> {
        // 资源夹具没有读取真实用户剪贴板的授权。
        Err(PluginError::new(
            "probe_clipboard",
            "resource fixture has no clipboard authority",
        ))
    }
    fn write_clipboard_text(&mut self, _text: &str) -> Result<(), PluginError> {
        Err(PluginError::new(
            "probe_clipboard",
            "resource fixture has no clipboard authority",
        ))
    }
    fn foreground_lifecycle(&self) -> Rc<dyn PluginHostLifecycle> {
        self.lifecycle.clone()
    }
}

/// 仅持 portable 工作区和标量/weak 完成状态；原生唯一 owner 仍在产品前台槽中。
pub(crate) struct NativeMarkdownResourceProbe {
    workspace: omni_markdown_core::MarkdownWorkspace,
    lifecycle: Rc<ResourceProbeLifecycle>,
    directory: std::path::PathBuf,
    case: usize,
    input_started_close: Cell<bool>,
    field_input_step: Cell<usize>,
    field_history: RefCell<Option<ResourceFieldHistoryObservation>>,
    table_activation: RefCell<Option<Rc<NativeMarkdownTableCellReceipt>>>,
    toolbar: RefCell<Option<super::probe::toolbar::NativeMarkdownToolbarProbe>>,
    toolbar_stage: Cell<ResourceToolbarStage>,
}

fn resource_table_four_ready(_: NativeMarkdownTableCellOutcome) {
    crate::runtime_probe_application::post_table_resource_activation(4);
}
fn resource_table_seven_ready(_: NativeMarkdownTableCellOutcome) {
    crate::runtime_probe_application::post_table_resource_activation(7);
}
fn resource_toolbar_ready(_: NativeMarkdownTableCellOutcome) {
    crate::runtime_probe_application::post_table_toolbar_activation();
}

/// 只记最后一次实际字段输入的弱身份；不会延长 editor、历史或字段的生命周期。
#[derive(Clone)]
struct ResourceFieldHistoryObservation {
    field: Weak<NSTextField>,
    editor: Weak<NSTextView>,
    manager: Option<Weak<objc2_foundation::NSUndoManager>>,
    owned_at_input: bool,
}

impl NativeMarkdownResourceProbe {
    /// 私有合成目录采用 exclusive 创建；不会打开生产 vault、设置或剪贴板。
    pub(crate) fn start(
        document_path: &Path,
        case: usize,
        finished: impl FnOnce() + 'static,
        mtm: MainThreadMarker,
    ) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt;
        let parent = document_path
            .parent()
            .ok_or("resource fixture has no parent directory")?;
        let directory = parent.join(format!(
            "native-resource-case-{case}-{}",
            std::process::id()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|error| format!("create exclusive resource fixture: {error}"))?;
        let vault = directory.join("vault");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&vault)
            .map_err(|error| format!("create resource vault: {error}"))?;
        let source = crate::native_markdown_runtime_probe_fixture();
        std::fs::write(vault.join("A.md"), &source)
            .map_err(|error| format!("write private resource note: {error}"))?;
        std::fs::write(vault.join("B.md"), "# Second\n\nnew independent note\n")
            .map_err(|error| format!("write private successor note: {error}"))?;
        let workspace =
            omni_markdown_core::MarkdownWorkspace::new(vault, directory.join("recovery"));
        let session = workspace
            .open_session()
            .map_err(|error| error.to_string())?;
        let lifecycle = Rc::new(ResourceProbeLifecycle {
            expected_intent: Cell::new(Some(if case == 0 {
                // 实际 performClose: 不以私有关闭函数或应用退出替代窗口关闭交付。
                PluginCloseIntent::ExitToLauncher
            } else {
                // 其他场景保留既有 SDK 隐藏请求，验证本次修复没有整批改写共享 intent。
                PluginCloseIntent::DismissAndResume
            })),
            completion: RefCell::new(Some(Box::new(finished))),
            ..ResourceProbeLifecycle::default()
        });
        let mut host = ResourceProbeHost {
            root: directory.clone(),
            lifecycle: Rc::clone(&lifecycle),
        };
        show_native_markdown_window(session, lifecycle.clone(), &mut host)?;
        // 初次展示与后面的具体输入位于不同实际事件，fixture 不在构造栈中立刻关闭。
        let probe = Self {
            workspace,
            lifecycle,
            directory,
            case,
            input_started_close: Cell::new(false),
            field_input_step: Cell::new(0),
            field_history: RefCell::new(None),
            table_activation: RefCell::new(None),
            toolbar: RefCell::new(None),
            toolbar_stage: Cell::new(ResourceToolbarStage::PrepareCancellation),
        };
        probe.assert_case(mtm)?;
        Ok(probe)
    }

    fn assert_case(&self, _mtm: MainThreadMarker) -> Result<(), String> {
        if self.case >= NATIVE_MARKDOWN_RESOURCE_PROBE_CASES {
            // 未知编号不能当作空场景进入关闭流程，否则会虚增具名场景的覆盖数。
            return Err("unknown resource probe case".into());
        }
        if self.case == 0 {
            // 读取实际产品面板，先结束前台状态借用再调用原生 getter，避免重入跨越 TLS 借用。
            let panel = with_active_native_markdown_surface(|surface| Ok(surface.panel.clone()))?;
            let style = panel.styleMask();
            let system_chrome = NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable;
            if style.intersects(system_chrome)
                || !style
                    .contains(NSWindowStyleMask::Resizable | NSWindowStyleMask::NonactivatingPanel)
                || !panel.isMovableByWindowBackground()
            {
                // 无边框、可缩放与原有背景拖动必须同时成立，不能仅把系统按钮藏起来。
                return Err("插件面板偏离既有无边框与背景拖动交互".into());
            }
            for button in [
                objc2_app_kit::NSWindowButton::CloseButton,
                objc2_app_kit::NSWindowButton::MiniaturizeButton,
                objc2_app_kit::NSWindowButton::ZoomButton,
            ] {
                if panel.standardWindowButton(button).is_some() {
                    // 验证框架没有为产品创建三色窗口按钮，禁止以不可见替代移除。
                    return Err("插件面板意外创建了系统窗口按钮".into());
                }
            }
            println!(
                "omni_panel_chrome verified=true titled=false standard_buttons=0 background_drag=true resizable=true"
            );
        }
        Ok(())
    }

    /// Show/activate 与后续输入分开；Find 和表格须经过真实产品的 UI 建立事务。
    pub(crate) fn configure(&self, mtm: MainThreadMarker) -> Result<(), String> {
        if self.toolbar_menu_needed() {
            return self.begin_toolbar_menu(); // 两份原菜单各等一次真实终点，再回201建立原未提交搜索场景。
        }
        match self.case {
            0 | 1 => with_active_native_markdown_surface(|surface| {
                // 两种搜索先建立相同的真实编辑关系，是否提交查询由后续输入阶段区分。
                let field = search_field(surface)?;
                // SAFETY: 活的项目字段在实际主线程窗口中，正常 selectText 建立共享编辑关系。
                unsafe {
                    field.selectText(None);
                }
                Ok(())
            }),
            2 | 3 | 8 => {
                // 直接调用产品具名菜单行为，不复制 Find 构造或覆盖其私有 delegate。
                // action 7 打开 Find，action 8 打开 Replace，后者必须覆盖两个实际输入字段。
                perform_native_markdown_workspace_action(if self.case == 2 { 7 } else { 8 })
            }
            4 | 7 => with_active_native_markdown_surface(|surface| {
                // 两者都先建立表格草稿；场景 7 在关闭阶段额外制造磁盘冲突并核验草稿保留。
                let editor = surface
                    .editor
                    .as_ref()
                    .ok_or("table resource fixture editor missing")?;
                let view = &editor.backend.text_view;
                // Find 全部匹配保留真实 body Multi；后续 cell 草稿关闭必须通过显式格选择归一。
                let identity = Rc::downgrade(editor.document());
                let (generation, revision, length) = {
                    let document = editor.document().borrow();
                    (
                        document.generation(),
                        document
                            .current_revision()
                            .map_err(|error| error.to_string())?,
                        document.utf16_len(),
                    )
                };
                let original = || {
                    !view.ivars().textkit_graph_teardown.get()
                        && identity.upgrade().is_some_and(|document| {
                            let document = document.borrow();
                            document.generation() == generation
                                && document
                                    .current_revision()
                                    .is_ok_and(|actual| actual == revision)
                                && document.utf16_len() == length
                        })
                };
                let bar =
                    prepare_multi_selection_find_probe(editor, &surface.controller, &original)?;
                execute_multi_selection_find_probe(
                    editor,
                    &surface.controller,
                    &bar,
                    surface.controller.find_input_intent(),
                    MarkdownFindAction::SelectAll,
                    None,
                    &original,
                )?;
                finish_list_marker_find_probe(editor, &surface.controller, &original)?;
                if view
                    .native_ordered_selection_state()
                    .map_err(|error| error.to_string())?
                    != NativeMarkdownOrderedSelection::Multiple
                    || view.ivars().folds.known_selection().is_some()
                {
                    return Err("资源表格前置 Find 全选未保留真实 Multi/未知 head".into());
                }
                drop(bar); // 同栏仍由原editor/owned ledger管理，关闭资源门槛不得漏掉它。
                // 使用现有 coalesced 产品事务体准备真实 TextKit 几何，不跑嵌套循环或等待重试。
                view.schedule_visible_table_layout_update();
                view.run_scheduled_table_layout_update();
                let cache = view
                    .ivars()
                    .visible_table_layout_cache
                    .borrow()
                    .as_ref()
                    .map(Rc::clone);
                let cell = cache
                    .as_ref()
                    .and_then(|cache| {
                        cache
                            .tables
                            .iter()
                            .flat_map(|table| &table.cells)
                            .find(|cell| cell.coordinate.row_index == 1)
                            .cloned()
                    })
                    .ok_or("table resource fixture has no actual visible body cell")?;
                drop(cache); // Native属性clone和cache析构均在槽借用外，之后才签发原动作票。
                let callback = if self.case == 4 {
                    resource_table_four_ready
                } else {
                    resource_table_seven_ready
                };
                let receipt = view.probe_prepare_table_cell_receipt(
                    editor.document(),
                    cell.coordinate,
                    callback,
                )?;
                if !view.probe_begin_table_cell_editing(cell, Rc::clone(&receipt)) {
                    return Err("资源表格激活原请求未被接受".into());
                }
                let previous = self.table_activation.replace(Some(receipt));
                drop(previous); // 纯原票在owner放回前发布；嵌套终点由driver暂存。
                Ok(())
            }),
            5 | 6 => {
                // 笔记交接及保存失败在输入事件执行，此阶段保留完整初始图。
                let _ = mtm;
                Ok(())
            }
            // 阶段与固定编号表不一致时拒绝执行，不能略过配置后仍宣称该场景通过。
            _ => Err("unknown resource probe case".into()),
        }
    }

    pub(crate) fn toolbar_waiting(&self) -> bool {
        self.case == 0
            && self.toolbar.borrow().is_some()
            && matches!(
                self.toolbar_stage.get(),
                ResourceToolbarStage::AwaitCancellation | ResourceToolbarStage::AwaitInsertion
            )
    }

    pub(crate) fn toolbar_menu_needed(&self) -> bool {
        self.case == 0
            && matches!(
                self.toolbar_stage.get(),
                ResourceToolbarStage::PrepareCancellation | ResourceToolbarStage::PrepareInsertion
            )
    }

    fn begin_toolbar_menu(&self) -> Result<(), String> {
        let cancelling = match self.toolbar_stage.get() {
            ResourceToolbarStage::PrepareCancellation => true, // 负例必须先取得原正常菜单，再用新明确全选撤销。
            ResourceToolbarStage::PrepareInsertion => false,   // 恢复后新正例重新通过完整原插表链。
            _ => return Err("菜单回归尚未准备或仍在等待原终点".into()),
        };
        let (mut state, capture, menu, index, view) =
            with_active_native_markdown_surface(|surface| {
                let editor = surface.editor.as_ref().ok_or("菜单插表缺少原编辑器")?;
                let mut state = super::probe::toolbar::prepare(editor)?;
                super::probe::toolbar::observe_menu_activation(
                    editor,
                    &mut state,
                    resource_toolbar_ready,
                )?;
                let capture = editor
                    .backend
                    .text_view
                    .ivars()
                    .folds
                    .capture_selection()
                    .map_err(|error| error.to_string())?;
                let item = surface
                    .controller
                    .ivars()
                    .table_menu_item
                    .get()
                    .ok_or("真实插表菜单项缺失")?;
                let valid = selection_native_step(&capture, || {
                    item.tag() == 118
                        && item.action() == Some(sel!(performMarkdownWorkspaceAction:))
                        && item.isEnabled()
                })
                .map_err(|error| error.to_string())?;
                if !valid {
                    return Err("真实插表菜单绑定或启用状态不符".into());
                }
                let menu = selection_native_step(&capture, || {
                    // SAFETY: 活foreground菜单项由原controller/owned ledger持有，公开menu getter不转移业务所有权。
                    unsafe { item.menu() }
                })
                .map_err(|error| error.to_string())?
                .ok_or("插表菜单项没有真实父菜单")?;
                let index = selection_native_step(&capture, || menu.indexOfItem(item))
                    .map_err(|error| error.to_string())?;
                if index < 0 {
                    return Err("插表菜单项没有实际索引".into());
                }
                if !capture.current() {
                    return Err("菜单准备返回时原插表权限已改变".into());
                }
                let view = selection_native_step(&capture, || editor.backend.text_view.clone())
                    .map_err(|error| error.to_string())?;
                Ok((state, capture, menu, index, view))
            })?;
        // 菜单 action 的生产门禁只认实际 ACTIVE；必须先结束借出 scope，让同一 owner 恢复到槽内。
        // 原 capture、实际 menu/index 来自准备前态，恢复后只复核这份票，不重新领取当前文档权限。
        selection_native_step(&capture, || menu.performActionForItemAtIndex(index))
            .map_err(|error| error.to_string())?;
        if cancelling {
            // ACTIVE 已归还且没有先 yield；公开 selectAll 必须位于原菜单返回后的同一个 handler。
            // view/menu 仅是本同步调用的局部 claim，不写入跨事件状态或队列。
            super::probe::toolbar::supersede_menu_with_select_all(&view, &mut state, capture)?;
            self.toolbar_stage
                .set(ResourceToolbarStage::AwaitCancellation);
        } else {
            if !capture.current() {
                return Err("菜单 action 返回时原插表权限已改变".into());
            }
            self.toolbar_stage.set(ResourceToolbarStage::AwaitInsertion);
        }
        drop(menu);
        drop(view); // 最后 Native claims 在本事件返回前结束；205仍重新核原状态与实际选择。
        self.toolbar.replace(Some(state));
        Ok(())
    }

    pub(crate) fn finish_toolbar_menu(&self) -> Result<(), String> {
        if !self.toolbar_waiting() {
            return Err("菜单插表终点缺少原有限阶段".into());
        }
        let state = self
            .toolbar
            .borrow_mut()
            .take()
            .ok_or("菜单插表状态已被替换")?;
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("菜单插表后原编辑器已关闭")?;
            match self.toolbar_stage.get() {
                ResourceToolbarStage::AwaitCancellation => {
                    super::probe::toolbar::finish_cancelled_menu(editor, &state)
                }
                ResourceToolbarStage::AwaitInsertion => {
                    super::probe::toolbar::finish(editor, &state, "workspace_menu_action")
                }
                _ => Err("菜单终点不属于原等待阶段".into()), // 重复或乱序终点不能跳过负例或正例。
            }
        })?;
        self.toolbar_stage.set(match self.toolbar_stage.get() {
            ResourceToolbarStage::AwaitCancellation => ResourceToolbarStage::PrepareInsertion,
            ResourceToolbarStage::AwaitInsertion => ResourceToolbarStage::Complete,
            _ => return Err("菜单终点消费期间阶段已变更".into()),
        });
        Ok(())
    }

    /// 对实际 currentEditor 调用公开输入协议；这是合成 native 输入，不是硬件/系统 IME 验收。
    pub(crate) fn input(&self) -> Result<(), String> {
        match self.case {
            0 | 1 => {
                let (field, controller) = with_active_native_markdown_surface(|surface| {
                    Ok((search_field(surface)?, surface.controller.clone()))
                })?;
                let observation = input_actual_field(&field, &controller, "Probe资源")?;
                self.field_history.replace(Some(observation));
                if self.case == 1 {
                    // 场景 0 保留未提交字段；只有场景 1 在 owner lease 结束后提交到可取消查询邮箱。
                    // SAFETY: 活主线程 controller 实现该精确 selector，字段 sender 在同步调用内保持有效。
                    unsafe {
                        let _: () = msg_send![&*controller, searchMarkdownWorkspace: &*field];
                    }
                }
                Ok(())
            }
            2 | 3 | 8 => {
                let (fields, controller) = with_active_native_markdown_surface(|surface| {
                    let editor = surface
                        .editor
                        .as_ref()
                        .ok_or("Find resource editor missing")?;
                    editor.capture_find_resources()?;
                    let fields: Vec<_> = editor
                        .find_resources
                        .borrow()
                        .fields()
                        .into_iter()
                        .filter(|field| field.isEditable() && !field.isHidden())
                        .collect();
                    Ok((fields, surface.controller.clone()))
                })?;
                let required = if matches!(self.case, 3 | 8) { 2 } else { 1 };
                if fields.len() < required {
                    // replace 场景不能只编辑 query 字段而宣称覆盖了替换输入。
                    return Err("actual Find/Replace editable fields are incomplete".into());
                }
                let fields = if self.case == 8 {
                    let step = self.field_input_step.get();
                    if step >= 2 {
                        return Err(
                            "sequential field probe exceeded its two declared input events".into(),
                        );
                    }
                    if step == 1 {
                        // 第一字段仍拥有焦点：在独立事件实际 Undo/Redo 后，才正常结束它并转第二字段。
                        let observation = self.field_history.borrow().clone();
                        verify_focused_field_history(observation.as_ref())?;
                    }
                    self.field_input_step.set(step + 1);
                    vec![
                        fields
                            .into_iter()
                            .nth(step)
                            .ok_or("sequential field probe is missing its declared field")?,
                    ]
                } else {
                    fields // case3 原同一事件依次编辑两个字段，不能用分事件替代本次异常回归。
                };
                for field in fields {
                    // 每个实际可编辑字段都经历共享 editor 焦点；不依赖私有类名或本地化标签辨认角色。
                    // SAFETY: 字段由活窗口公开 Find bar 得到，正常聚焦不替换框架委托。
                    unsafe {
                        field.selectText(None);
                    }
                    let observation = input_actual_field(&field, &controller, "Probe查找")?;
                    self.field_history.replace(Some(observation));
                }
                Ok(())
            }
            4 | 7 => {
                let receipt = self
                    .table_activation
                    .borrow()
                    .as_ref()
                    .map(Rc::clone)
                    .ok_or("资源表格缺少原激活收据")?;
                let (field, controller) = with_active_native_markdown_surface(|surface| {
                    let editor = surface
                        .editor
                        .as_ref()
                        .ok_or("table resource editor missing")?;
                    let view = &editor.backend.text_view;
                    if receipt.outcome() != Some(NativeMarkdownTableCellOutcome::Activated)
                        || !receipt.current_for(view)
                    {
                        return Err(format!("资源表格原激活没有成功：{:?}", receipt.outcome()));
                    }
                    let field =
                        native_markdown_table_intent_step(&|| receipt.current_for(view), || {
                            view.existing_table_cell_editor()
                        })
                        .map_err(|_| "资源表格激活已撤权")?
                        .ok_or("actual table field did not activate")?;
                    let controller =
                        native_markdown_table_intent_step(&|| receipt.current_for(view), || {
                            surface.controller.clone()
                        })
                        .map_err(|_| "资源表格owner已撤权")?;
                    Ok((field, controller))
                })?;
                let observation = input_actual_field(&field, &controller, "resource-cell-draft")?;
                self.field_history.replace(Some(observation));
                Ok(())
            }
            5 => {
                let index = with_active_native_markdown_surface(|surface| {
                    surface
                        .sidebar_rows
                        .iter()
                        .position(|row| row.relative_path == "B.md")
                        .ok_or_else(|| "successor row missing".to_owned())
                })?;
                // 真正产品侧栏路径注册旧图的 continuation，不制造没有回调的假 pending 状态。
                activate_native_markdown_sidebar_row(index)?;
                let pending = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    active
                        .as_ref()
                        .is_some_and(|surface| surface.pending_note_switch.is_some())
                });
                if !pending {
                    // 没有实际排队的旧图就未覆盖 handoff 关闭边界，不能继续普通关闭冒充该场景。
                    return Err("the old note handoff was not actually pending".into());
                }
                // 同一程序事务可在旧 handoff 回调排队后请求关闭；产品必须纳入该旧资源 ledger。
                self.request_close()?;
                self.input_started_close.set(true);
                Ok(())
            }
            6 => {
                // 正文冲突先建立未保存的 canonical 草稿，与场景 7 的表格客户端草稿分别覆盖。
                let view = active_document_view()?;
                let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(view.as_super());
                // SAFETY: 合成 NSString 和有限插入范围在同步输入调用期间有效。
                unsafe {
                    input.insertText_replacementRange(
                        ns_string!("resource-unsaved-draft\n"),
                        NSRange::new(0, 0),
                    );
                }
                Ok(())
            }
            // 未知输入路径不能退化为无输入关闭；覆盖不完整必须使整个场景失败。
            _ => Err("unknown resource probe case".into()),
        }
    }

    /// 只有新增的分事件场景在固定两步中的第一步之后，再投递一次真实 input 事件。
    pub(crate) fn more_input_events(&self) -> bool {
        self.case == 8 && self.field_input_step.get() == 1
    }

    /// 驱动只对预先声明的 handoff 场景跳过另一个 close 事件，不重复关闭或更改等待阶段。
    pub(crate) fn input_started_close(&self) -> bool {
        self.input_started_close.get()
    }

    /// 失败预检必须保留原生 owner 和 canonical 草稿，然后才进行允许成功的资源清理。
    pub(crate) fn request_close(&self) -> Result<(), String> {
        if matches!(self.case, 0..=4 | 7 | 8) {
            // 输入事件已经返回；在独立事件验证实际客户端 Undo/Redo，再开始可失败关闭。
            let observation = self.field_history.borrow().clone();
            verify_focused_field_history(observation.as_ref())?;
            if matches!(self.case, 3 | 8) {
                // 第一字段已经正常提交/辞任；第二字段 Undo/Redo 不能误改它的值或重新接管其客户端。
                let first = with_active_native_markdown_surface(|surface| {
                    surface
                        .editor
                        .as_ref()
                        .and_then(|editor| {
                            editor.find_resources.borrow().fields().into_iter().next()
                        })
                        .ok_or_else(|| "the prior Find field disappeared".to_owned())
                })?;
                if first.currentEditor().is_some()
                    || !first.stringValue().isEqualToString(ns_string!("Probe查找"))
                {
                    return Err("the ended Find field changed while undoing the next client".into());
                }
            }
        }
        let request = PluginCloseRequest {
            intent: PluginCloseIntent::DismissAndResume,
            scope: PluginCloseScope::ForegroundTransition,
        };
        if matches!(self.case, 6 | 7) {
            let (controller, view, generation) = with_active_native_markdown_surface(|surface| {
                let editor = surface
                    .editor
                    .as_ref()
                    .ok_or("rejected-close editor missing")?;
                Ok((
                    Weak::from_retained(&surface.controller),
                    Weak::from_retained(&editor.backend.text_view),
                    editor.document().borrow().generation(),
                ))
            })?;
            // 私有夹具故意改变磁盘版本；新文档私有 snapshot 不受 truncate/重写影响。
            let path = self.directory.join("vault/A.md");
            std::fs::write(&path, "# External resource-probe revision\n")
                .map_err(|error| error.to_string())?;
            if request_native_markdown_close(request).is_ok()
                || self.lifecycle.accepted.get().is_some()
            {
                // 磁盘冲突必须在 Host admission 前拒绝；accepted 后的 Closing 不能冒充可恢复失败。
                return Err("save conflict incorrectly accepted the resource close".into());
            }
            with_active_native_markdown_surface(|surface| {
                let current = surface
                    .editor
                    .as_ref()
                    .ok_or("failed save removed the editor")?;
                // 必须保留同一可编辑图和草稿；表格辞任可先提交 canonical，因此只禁止代次倒退。
                if !controller
                    .load()
                    .is_some_and(|expected| std::ptr::eq(&*expected, &*surface.controller))
                    || !view.load().is_some_and(|expected| {
                        std::ptr::eq(&*expected, &*current.backend.text_view)
                    })
                    || !current.backend.text_view.isEditable()
                    || !current.backend.text_storage.ivars().accepts_edits.get()
                    || current.document().borrow().generation() < generation
                    || current
                        .document()
                        .borrow()
                        .find_utf16(
                            if self.case == 7 {
                                "resource-cell-draft"
                            } else {
                                "resource-unsaved-draft"
                            },
                            0,
                            false,
                            true,
                        )
                        .map_err(|error| error.to_string())?
                        .is_none()
                {
                    return Err("failed save did not preserve the original editable draft".into());
                }
                Ok(())
            })?;
            // 恢复私有 fixture 的原始基线字节，下一次显式保存按真实冲突契约重试。
            std::fs::write(&path, crate::native_markdown_runtime_probe_fixture())
                .map_err(|error| error.to_string())?;
        }
        if self.case == 0 {
            // TLS 借用先结束；公开原生窗口动作自行弱排队，局部 panel 在队列执行前正常释放。
            let panel = with_active_native_markdown_surface(|surface| Ok(surface.panel.clone()))?;
            panel.performClose(None);
            Ok(())
        } else {
            // 显式 SDK 隐藏保持其原请求，不能借窗口入口改变全局唤出键的切换语义。
            request_native_markdown_close(request)
        }
    }

    /// 仅在产品实际发出一次精确资源收据后清理 portable 会话和私有测试目录。
    pub(crate) fn finish(self) -> Result<(), String> {
        // 此处消费产品完成验证后的通知，不从两个相等标量重新推导 native 或业务 payload 已释放。
        if self.lifecycle.completed.get() != self.lifecycle.accepted.get()
            || self.lifecycle.completed.get().is_none()
        {
            return Err("resource fixture did not receive its exact lifecycle receipt".into());
        }
        // 任何产品 owner 槽仍被占用都不能结束夹具；清理测试目录不能替代产品自己的资源收据。
        if ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow(Option::is_some)
        {
            return Err("resource fixture retained a native product owner after receipt".into());
        }
        if let Some(expected) = self.lifecycle.expected_saved.get() {
            // 旧业务 owner 已结束后再独立重开私有文件，比较实际磁盘字节的内容 revision。
            let saved =
                MarkdownDocumentBuffer::open(&self.directory.join("vault/A.md"), 2 * 1_024 * 1_024)
                    .map_err(|error| error.to_string())?;
            if saved
                .current_revision()
                .map_err(|error| error.to_string())?
                != expected
            {
                return Err("resource close did not persist the accepted canonical bytes".into());
            }
            drop(saved);
        }
        self.workspace.close_session();
        if self.case == 0 {
            // 仅在产品严格资源回执及保存字节检查通过后记录真实窗口关闭的具体意图。
            println!(
                "omni_window_close verified=true intent=ExitToLauncher strict_release=true application_quit=false"
            );
        }
        println!("markdown_resource_case_{}_passed=true", self.case);
        std::fs::remove_dir_all(&self.directory)
            .map_err(|error| format!("remove private resource fixture: {error}"))
    }

    /// 失败只报告标量及静态角色，不读取或输出用户正文或路径。
    pub(crate) fn failure_diagnostic(&self) -> String {
        let pending = FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow(|completion| {
            completion.as_ref().map(|completion| {
                (
                    completion.owned.clone(),
                    completion.cohort.diagnostic(),
                    completion.resources.diagnostic(),
                )
            })
        });
        let pending = pending.map(|(owned, full, resources)| {
            format!(
                "full=[{full}] resources=[{resources}] owned={:?}",
                owned.verify_released().err()
            )
        });
        format!(
            "resource_case={} accepted={} completed={} pending={}",
            self.case,
            self.lifecycle.accepted.get().is_some(),
            self.lifecycle.completed.get().is_some(),
            pending.as_deref().unwrap_or("none")
        )
    }
}

fn search_field(surface: &NativeMarkdownWindowSurface) -> Result<Retained<NSTextField>, String> {
    surface
        .controller
        .ivars()
        .workspace_controls
        .get()
        .into_iter()
        .flatten()
        .find_map(|control| control.clone().downcast::<NSTextField>().ok())
        .ok_or_else(|| "actual workspace search field is missing".into())
}

fn input_actual_field(
    field: &NSTextField,
    controller: &NativeMarkdownController,
    text: &str,
) -> Result<ResourceFieldHistoryObservation, String> {
    let editor = field
        .currentEditor()
        .and_then(|editor| editor.downcast::<NSTextView>().ok())
        .ok_or("the focused field has no actual native editor")?;
    let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
    let text = NSString::from_str(text);
    // SAFETY: 主线程真实 field editor 实现该公开输入协议，合成字符串在同步调用中保持有效。
    unsafe {
        input.insertText_replacementRange(&text, NSRange::new(NSNotFound as usize, 0));
    }
    if editor.string().length() == 0 {
        return Err("the actual field editor did not accept synthetic input".into());
    }
    // 这是实际输入事务后的历史初始化路径，不是在关闭观测中创建 manager。
    controller.observe_native_field_editor(&editor);
    let observation = ResourceFieldHistoryObservation {
        field: Weak::from(field),
        editor: Weak::from_retained(&editor),
        // 此处属于刚完成的真实输入功能路径；失败诊断只升级此弱引用，不再查询潜在创建式 getter。
        manager: editor.undoManager().as_ref().map(Weak::from_retained),
        owned_at_input: editor
            .downcast_ref::<NativeMarkdownSessionFieldEditor>()
            .is_some(),
    };
    let borrowed = controller.ivars().borrowed.borrow().clone();
    if borrowed.failure().is_some() || controller.ivars().undo.preflight().is_err() {
        // observe 的 Objective-C 回调通道没有 Result；夹具须传播已保存失败，不能把输入接受当成归属证明。
        let panel = field.window();
        return Err(format!(
            "resource field input failed its session ownership checks; {}",
            field_history_failure_diagnostic(Some(&observation), panel.as_deref(), controller)
        ));
    }
    Ok(observation)
}

/// 验证当前真实 field editor 的历史往返；不冒充硬件 Cmd-Z 或系统 IME 验收。
fn verify_focused_field_history(
    observation: Option<&ResourceFieldHistoryObservation>,
) -> Result<(), String> {
    let (panel, controller) = with_active_native_markdown_surface(|surface| {
        Ok((surface.panel.clone(), surface.controller.clone()))
    })?;
    let editor = panel
        .firstResponder()
        .and_then(|responder| {
            responder
                .downcast::<NativeMarkdownSessionFieldEditor>()
                .ok()
        })
        .ok_or_else(|| {
            // 不重新聚焦或改用另一个输入对象重试；保留跨事件真实 responder 失败与原字段弱身份。
            format!(
                "resource field history lost its owned editor responder; {}",
                field_history_failure_diagnostic(observation, Some(&panel), &controller)
            )
        })?;
    controller.observe_native_field_editor(&editor);
    controller.ivars().undo.preflight()?;
    let after = editor.string();
    if after.length() > 1_024 {
        // 合成字段有固定长度；不为测试读取或复制任意大的用户 native 字符串。
        return Err("resource field history exceeds its synthetic text budget".into());
    }
    // NSText.string 可能指向可变 backing string；有限复制期望，避免 undo 同时改变比较基准。
    let after = NSString::from_str(&after.to_string());
    let manager = editor
        .undoManager()
        .ok_or("resource field history has no actual undo manager")?;
    if !manager.canUndo() {
        // 活动阶段禁止通过关闭 Undo 换取清理成功；这应作为功能失败保留。
        return Err("resource field history did not register native input undo".into());
    }
    manager.undo();
    if editor.string().isEqualToString(&after) || !manager.canRedo() {
        return Err("resource field undo did not restore previous editable data".into());
    }
    manager.redo();
    if !editor.string().isEqualToString(&after) {
        return Err("resource field redo did not restore accepted input".into());
    }
    Ok(())
}

/// 失败只报告有限弱角色的公开事实和身份关系；不读出正文、不打印类名、地址、路径或任意错误文本。
fn field_history_failure_diagnostic(
    observation: Option<&ResourceFieldHistoryObservation>,
    panel: Option<&NSWindow>,
    controller: &NativeMarkdownController,
) -> String {
    let field = observation.and_then(|value| value.field.load());
    let editor = observation.and_then(|value| value.editor.load());
    let manager = observation
        .and_then(|value| value.manager.as_ref())
        .and_then(Weak::load);
    let responder = panel.and_then(NSWindow::firstResponder);
    let current_editor = field.as_ref().and_then(|field| field.currentEditor());
    let body = active_document_view().ok();
    let borrowed = controller.ivars().borrowed.borrow().clone();
    // 指针只在本次同步观测中比较活对象身份，从不输出或跨阶段缓存地址。
    let same = |left: *const (), right: *const ()| left == right;
    let responder_is_editor = responder.as_ref().is_some_and(|responder| {
        editor.as_ref().is_some_and(|editor| {
            same(
                std::ptr::from_ref(&**responder).cast(),
                std::ptr::from_ref(&**editor).cast(),
            )
        })
    });
    let responder_is_field = responder.as_ref().is_some_and(|responder| {
        field.as_ref().is_some_and(|field| {
            same(
                std::ptr::from_ref(&**responder).cast(),
                std::ptr::from_ref(&**field).cast(),
            )
        })
    });
    let responder_is_body = responder.as_ref().is_some_and(|responder| {
        body.as_ref().is_some_and(|body| {
            same(
                std::ptr::from_ref(&**responder).cast(),
                std::ptr::from_ref(&**body).cast(),
            )
        })
    });
    let current_is_editor = current_editor.as_ref().is_some_and(|current| {
        editor.as_ref().is_some_and(|editor| {
            same(
                std::ptr::from_ref(&**current).cast(),
                std::ptr::from_ref(&**editor).cast(),
            )
        })
    });
    let field_in_panel = field
        .as_ref()
        .and_then(|field| field.window())
        .is_some_and(|window| panel.is_some_and(|panel| std::ptr::eq(&*window, panel)));
    let editor_in_panel = editor
        .as_ref()
        .and_then(|editor| editor.window())
        .is_some_and(|window| panel.is_some_and(|panel| std::ptr::eq(&*window, panel)));
    format!(
        "input_observed={} input_owned={} field_live={} editor_live={} manager_live={} manager_owned={} responder_present={} responder_recorded_editor={} responder_recorded_field={} responder_body={} current_editor_present={} current_recorded_editor={} current_owned={} field_in_panel={} editor_in_panel={} field_editable={} field_enabled={} field_units={} editor_field={} editor_units={} undo={} redo={} undo_failed={} borrowed_failed={}",
        observation.is_some(),
        observation.is_some_and(|value| value.owned_at_input),
        field.is_some(),
        editor.is_some(),
        manager.is_some(),
        manager
            .as_ref()
            .is_some_and(|manager| { controller.ivars().undo.owns_observed_manager(manager) }),
        responder.is_some(),
        responder_is_editor,
        responder_is_field,
        responder_is_body,
        current_editor.is_some(),
        current_is_editor,
        current_editor.as_ref().is_some_and(|editor| editor
            .downcast_ref::<NativeMarkdownSessionFieldEditor>()
            .is_some()),
        field_in_panel,
        editor_in_panel,
        field.as_ref().is_some_and(|field| field.isEditable()),
        field.as_ref().is_some_and(|field| field.isEnabled()),
        field
            .as_ref()
            .map_or(0, |field| field.stringValue().length()),
        editor.as_ref().is_some_and(|editor| editor.isFieldEditor()),
        editor.as_ref().map_or(0, |editor| editor.string().length()),
        manager.as_ref().is_some_and(|manager| manager.canUndo()),
        manager.as_ref().is_some_and(|manager| manager.canRedo()),
        controller.ivars().undo.preflight().is_err(),
        borrowed.failure().is_some(),
    )
}
