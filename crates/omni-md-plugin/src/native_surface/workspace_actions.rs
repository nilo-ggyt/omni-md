//! 工作区命令连接受限文件操作与 TextKit；模态面板不跨越 surface 或文档借用。
//!
//! 搜索仅拥有一个可取消 worker。worker 和完成通知只携带纯 Core 数据、标量身份与
//! Rust 邮箱；不得携带析构时同步等待主队列的原生 weak 包装。

use super::*;
use crate::workspace_ready::{WorkspaceDelivery, WorkspaceReadySignal};
use objc2_app_kit::{NSAccessibility, NSOpenPanel};
use std::path::PathBuf;

/// 扫描结果只能由当前代次的前台邮箱持有，主队列闭包不持有摘要正文。
pub(super) type NativeMarkdownSearchResult =
    Result<omni_markdown_core::MarkdownSearchPage, omni_markdown_core::MarkdownWorkspaceError>;

/// 前七项仍是原搜索、文档、分页与侧栏创建控件；后续按钮统一归原controls拥有。
pub(super) const NATIVE_MARKDOWN_TOOLBAR_START: usize = 7;

/// 同一装配入口安装工作区与侧栏原按钮；关闭沿原controls账本断开全部借用target。
pub(super) fn install_native_markdown_workspace_controls(
    controller: &NativeMarkdownController,
    content: &NSView,
    ledger: &NativeMarkdownOwnedLedger,
    sidebar: &NativeMarkdownSidebarList,
    has_document: bool,
    mtm: MainThreadMarker,
) -> [Weak<NSView>; 6] {
    // 搜索框恢复输入后自动查询，Return 仍允许立即提交同一有限字段。
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
        // 所有leaf自己发送一次action，popup不再发送重复命令。
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
    // 控件构造完成后显式登记标准对象和实际 cell，getter 均不跨越账本借用。
    for (role, control) in [
        ("workspace.search", query.as_super()),
        ("workspace.actions", actions.as_super().as_super()),
        ("workspace.refresh", refresh.as_super()),
        ("workspace.next", next.as_super()),
    ] {
        // 固定控件数量远小于预算，超限是构造不变量错误。
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
        // 三个原入口占原分区行右侧，搜索、列表和底栏不挪位；目标是同一真实sidebar。
        // SAFETY: 固定selector由当前table实现；surface拥有table，原controls拆图先断action/target。
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
    // 工具栏按钮不取得field editor焦点；原selector首个getter前仍捕获同一正文/格内意图。
    let button = |title: &str, explanation: &str, tag: isize, action, role: &'static str| {
        // SAFETY: selector属于同一主线程controller；原workspace_controls拆图先清target/action。
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
        // 新打开的文稿尚无历史；撤销/重做之后只由原客户的真实历史通知更新。
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

    // 原附件与十九项格式按钮只装配一次；局部位置归实际横滚内容，页眉只布局整个视口。
    let strip = super::format_strip::install(
        controller,
        content,
        ledger,
        &mut controls,
        has_document,
        mtm,
    );
    // 五个原文档操作保持历史槽位；格式栏只传实际scroll根的Weak，不把局部按钮当全局frame。
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
    // 首篇正文在窗口构造时已装配，不经过后续 install_editor；同一 setter 必须在这里
    // 接收真实文档存在状态，否则全部历史通知会把首篇误当空态，按钮始终无法更新。
    controller.set_copy_document_available(has_document);
    toolbar
}

impl NativeMarkdownWindowSurface {
    /// 撤销整个查询及续页；刷新、换词、本地文件操作和关闭都走同一释放边界。
    pub(super) fn stop_search(&mut self) {
        self.controller.cancel_workspace_search();
        self.stop_search_attempt();
        self.session.cancel_search();
        // 纯query/cursor也不得跨会话保留；丢弃旧末项不能影响当前正文或历史。
        self.search_cursor.take();
        self.search_authority.take();
        self.search_query.take();
        self.search_intent.take();
        self.sidebar_pager.take();
        self.sidebar_intent.take();
        self.sidebar_completion_policy = Default::default();
    }

    /// 只结束单次worker与邮箱；Next复用当前core查询代次，不能撤销自己的cursor。
    pub(super) fn stop_search_attempt(&mut self) {
        // 同步撤销纯恢复信号；旧worker通知后到也会因邮箱closed而被拒绝。
        cancel_native_markdown_workspace_ready(std::ptr::from_ref(&*self.controller).addr());
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        // 先撤销结果与查询权限并释放已发布载荷，迟到 worker 只能得到拒绝。
        if let Some(mailbox) = self.pending_search_query.take() {
            mailbox.cancel();
        }
        if let Some(mailbox) = self.search_results.take() {
            mailbox.cancel();
        }
        // 目录和搜索复用同一个worker租约；取消同步排空目录结果及其pager句柄。
        if let Some(mailbox) = self.sidebar_results.take() {
            mailbox.cancel();
        }
        self.sidebar_scan_pending = false;
        // Core在有限块之间检查取消。worker只持纯Rust资源，退出/析构不能同步等待主队列；
        // 因而此处join不依赖排队的UI通知，返回后才允许会话或旧正文释放。
        if let Some(worker) = self.search_worker.take() {
            let _ = worker.join();
        }
    }

    /// 实际发布撤销当前搜索投影；单次正常重提使用同一邮箱，不从绘制/计时循环重试。
    pub(super) fn refresh_search_after_save(&mut self, resubmit: bool) -> Result<(), String> {
        // 只取原已接纳查询A的权限；当前控制器代次可能已属于尚未提交的新输入B。
        let original_query = self.search_query.clone();
        let original_intent = self.search_intent;
        // 先取走旧页和末项，随后Native移除回调不会再借到过期metadata。
        self.search_cursor.take();
        self.search_authority.take();
        self.notes = Vec::new();
        self.sidebar_rows = Vec::new();
        self.sidebar_has_more = false;
        self.sidebar_list.clear()?;
        self.controller.set_next_sidebar_page_available(false);
        let (Some(original_query), Some(intent)) = (original_query, original_intent) else {
            // 没有已接纳的旧查询不能创建自动请求；清理旧投影后等待正常输入入口。
            return Ok(());
        };
        if !resubmit || self.controller.workspace_query_intent() != intent {
            // 保存失败保留原首因；新输入B保留原140ms/排队权限，不被保存立即重提。
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
            // 未提交IME不被自动保存读取；真实提交后的原didChange负责新查询。
            return Ok(());
        }
        let query = submitted_workspace_query(field)?;
        if self.controller.workspace_query_intent() != intent || query.trim() != original_query {
            // 字段已变B或读取中重入didChange，均保留新输入调度；不能借它的当前epoch重提。
            return Ok(());
        }
        queue_native_markdown_search_query(self, query, intent, self.panel.mtm())
    }

    /// 只替换当前有界页的metadata；已打开文档仍是唯一正文所有者。
    fn install_search_results(
        &mut self,
        page: omni_markdown_core::MarkdownSearchPage,
        generation: u64,
        intent: u64,
        _mtm: MainThreadMarker,
    ) -> Result<bool, String> {
        if !self.search_delivery_is_current(generation, intent) {
            // 入口保留原worker权限，不得因当前字段已经变B而为旧结果A重新领取票据。
            return Ok(false);
        }
        let query = self.search_query.as_deref().ok_or("搜索查询已结束")?;
        if !page.authority.is_current(&self.session, query) {
            // 本地保存可在worker发布后撤权，即便Native attempt代次未变也不能安装旧页。
            return Err("笔记已变化，请重新搜索".into());
        }
        if !self.search_delivery_is_current(generation, intent) {
            // Native getter重入新输入时保留原目录/页，不继续拆控件或发布旧页。
            return Ok(false);
        }
        // 列表对象保持不变，只撤销旧页和其菜单代次；回调不反借surface。
        self.notes = Vec::new();
        self.sidebar_rows = Vec::new();
        self.sidebar_list.clear()?;
        if !self.search_delivery_is_current(generation, intent) {
            // 只清理刚取走的旧控件；重入输入之后不再创建本次候选。
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
                // 只复制打开所需的路径与标题，不持有正文或常驻索引。
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
        // 最后纯门禁后一次发布metadata与控件；所有权租约阻止后继页并发改写这些字段。
        self.sidebar_pager.take();
        self.sidebar_has_more = next.is_some();
        self.search_cursor = next;
        self.search_authority = Some(authority);
        self.notes = notes;
        self.sidebar_rows = rows;
        self.controller
            .set_next_sidebar_page_available(self.sidebar_has_more);
        if !self.search_delivery_is_current(generation, intent) {
            // setEnabled之后新输入已接管，旧页不再更新模式标题或报告过期错误。
            return Ok(false);
        }
        self.update_sidebar_mode_label(true, intent);
        Ok(self.search_delivery_is_current(generation, intent))
    }

    /// 所有Native阶段共用入口票据；此纯检查不读取字段、不升级原生Weak。
    fn search_delivery_is_current(&self, generation: u64, intent: u64) -> bool {
        self.search_generation == generation
            && self.controller.workspace_query_intent() == intent
            && self.search_intent == Some(intent)
            && self.search_query.is_some()
            && self.closing_request.is_none()
    }
}

/// 查询最多 128 个 scalar；长度先有限检查，避免把无界 NSString 复制进排队工作。
fn submitted_workspace_query(field: &NSTextField) -> Result<String, String> {
    let value = field.stringValue();
    let length = value.length();
    if length > 256 {
        // 128 个 Unicode scalar 最多占 256 UTF-16 units；超长输入不创建正文副本。
        return Err("搜索词最多为 128 个字符".into());
    }
    let mut units = [0_u16; 256];
    // SAFETY: 固定缓冲区足够容纳已检查的 length，NSString 只写入该有限范围。
    unsafe {
        value.getCharacters_range(
            NonNull::new(units.as_mut_ptr()).unwrap(),
            NSRange::new(0, length),
        );
    }
    submitted_workspace_query_from_utf16(&units[..length])
}

/// 有效 Unicode 查询不截断；孤立 surrogate 或超过 scalar 限制直接拒绝。
fn submitted_workspace_query_from_utf16(units: &[u16]) -> Result<String, String> {
    if units.len() > 256 {
        // 与原生读取边界一致，调用纯策略也不能跳过前置预算。
        return Err("搜索词最多为 128 个字符".into());
    }
    let query =
        String::from_utf16(units).map_err(|_| "搜索词包含无效的 Unicode 字符".to_owned())?;
    if query.chars().count() > 128 {
        // BMP 与组合序列遵循 core 同一 scalar 规则，不把组合字符当 grapheme 截断。
        return Err("搜索词最多为 128 个字符".into());
    }
    Ok(query)
}

/// 查询进入当前 surface 的可取消邮箱，主队列只传递弱身份和代次。
pub(super) fn enqueue_native_markdown_workspace_search(
    controller: &NativeMarkdownController,
    field: &NSTextField,
) -> Result<(), String> {
    if !controller.owns_workspace_query(field) {
        // 公开 helper 同样核验字段身份，不能靠伪造 action 搜索其他输入框的内容。
        return Err("搜索输入框已变更，请重新搜索".into());
    }
    let intent = controller.workspace_query_intent();
    if field.currentEditor().is_some_and(|editor| {
        // Return 和 timer 都不得提交组合中的半成品；只查看已经存在的 editor。
        editor
            .downcast_ref::<NSTextView>()
            .is_some_and(|view| view.hasMarkedText())
    }) {
        // 输入仍保留，真实组合提交后的 didChange 会重新安排查询。
        return Ok(());
    }
    let query = submitted_workspace_query(field)?;
    if controller.workspace_query_intent() != intent {
        // 字段getter重入新输入时，旧快照不再有提交权限，也不取消较新的期限。
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

/// 新查询和保存后重提共享一个有界邮箱；不会从surface操作中递归借用Active。
fn queue_native_markdown_search_query(
    surface: &mut NativeMarkdownWindowSurface,
    query: String,
    intent: u64,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    if surface.controller.workspace_query_intent() != intent {
        // 旧动作只能丢弃自己的query，不能清理或重启新输入。
        return Ok(());
    }
    surface.stop_search();
    let accepted_intent = intent.wrapping_add(1);
    if surface.controller.workspace_query_intent() != accepted_intent {
        // cancel timer的原生释放若重入didChange，新输入已取得权限，不能被旧请求覆盖。
        return Ok(());
    }
    let mailbox = std::sync::Arc::new(crate::cancelable_mailbox::CancelableMailbox::new());
    mailbox.publish(query);
    surface.pending_search_query = Some(std::sync::Arc::clone(&mailbox));
    let mailbox = std::sync::Arc::downgrade(&mailbox);
    let generation = surface.search_generation;
    let controller = MainThreadBound::new(Weak::from_retained(&surface.controller), mtm);
    enqueue_native_markdown_main_task(move || {
        // 实际主队列入口重新取得线程证明，不将MainThreadMarker捕获进Send闭包。
        let mtm = MainThreadMarker::new().expect("搜索查询在AppKit主队列执行");
        let Some(controller) = controller.get(mtm).load() else {
            // 通知不保活目标；关闭后不会重建任何查询。
            return;
        };
        let Some(mailbox) = mailbox.upgrade() else {
            // 取消/关闭已释放payload，weak通知没有重试权限。
            return;
        };
        let accepted = with_active_native_markdown_surface(|surface| {
            // 输入意图和worker代次分别核验，包含排队期间的新词及保存撤权。
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
            // 只消费一次同查询请求；start内部再次检查跨Native取消的意图变化。
            if let Err(error) =
                start_native_markdown_search_page(query, None, Some((&controller, accepted_intent)))
            {
                controller.present_error(&error);
            }
        }
    });
    Ok(())
}

/// 同一有界worker接收新查询或opaque续页；排队完成仍只有weak通知。
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
            // 同步Native读取若交接了会话，旧Next不得取消或改写新owner的查询状态。
            return Err("笔记搜索会话已经变化，请重新搜索".into());
        }
        let intent = surface.controller.workspace_query_intent();
        if cursor.is_some() {
            // Next只取消上一attempt，不推进core查询代次；原cursor仍须由core再次核验。
            surface.controller.cancel_workspace_search();
            if surface.controller.workspace_query_intent() != intent.wrapping_add(1) {
                // 原生invalidate/drop重入后，新词已撤销这次Next；不碰它的worker/邮箱。
                return Ok(());
            }
            surface.stop_search_attempt();
        } else {
            // 真正的新输入撤销旧查询、结果、目录续页与旧搜索末项。
            surface.stop_search();
            if surface.controller.workspace_query_intent() != intent.wrapping_add(1) {
                // 新查询的取消阶段同样可能重入新词，旧请求不得随后启动。
                return Ok(());
            }
        }
        let accepted_intent = intent.wrapping_add(1);
        if query.trim().is_empty() {
            // 清空查询回到有限目录投影，不保留扫描 worker 或结果邮箱。
            return surface.refresh_workspace_sidebar(mtm);
        }
        // 第一页扫描期间也标明search模式，AX重复Next不能误推进旧目录cursor。
        surface.search_query = Some(query.trim().to_owned());
        surface.search_intent = Some(accepted_intent);
        surface.search_cursor.take();
        surface.controller.set_next_sidebar_page_available(false);
        if surface.controller.workspace_query_intent() != accepted_intent {
            // setEnabled可重入新输入；固定旧epoch不能被setter后的新值替换来启动旧worker。
            return Ok(());
        }
        surface.search_cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled = std::sync::Arc::clone(&surface.search_cancelled);
        let generation = surface.search_generation;
        let session = surface.session.clone();
        // 只记录非拥有地址，不构造原生Weak：取消时主线程会join，而MainThreadBound的
        // worker侧析构可能同步派发到主线程。精确邮箱身份会在交付时排除地址复用。
        let owner_identity = std::ptr::from_ref(&*surface.controller).addr();
        let mailbox = std::sync::Arc::new(crate::cancelable_mailbox::CancelableMailbox::new());
        surface.search_results = Some(std::sync::Arc::clone(&mailbox));
        let worker = std::thread::Builder::new()
            .name("omni-md-search".into())
            .spawn(move || {
                // worker 仅访问 portable core；取消检查限制扫描并在退出前释放文件快照。
                let results = session.search_notes_page_cancellable(
                    &query,
                    MAX_VISIBLE_NATIVE_MARKDOWN_WORKSPACE_ENTRIES,
                    cursor.as_ref(),
                    &cancelled,
                );
                if !mailbox.publish(results) {
                    // close 已同步排空并封闭邮箱，迟到结果在 worker 栈内释放。
                    return;
                }
                let signal = WorkspaceReadySignal::Search(WorkspaceDelivery {
                    owner: owner_identity,
                    generation,
                    intent: accepted_intent,
                    mailbox: std::sync::Arc::downgrade(&mailbox),
                });
                dispatch2::DispatchQueue::main().exec_async(move || {
                    // 只发纯弱信号；借出owner时等待真实恢复事件，不丢通知或重新扫描。
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

/// Next复用当前查询或目录；不同输入/IME/重复动作不能借旧cursor继续翻页。
fn advance_native_markdown_sidebar_page(
    mtm: MainThreadMarker,
    owner: &NativeMarkdownController,
) -> Result<(), String> {
    let intent = owner.workspace_query_intent();
    let continuation = with_active_native_markdown_surface(|surface| {
        if !std::ptr::eq(owner, &*surface.controller) {
            // 旧菜单/AX操作不能借新会话的同一按钮索引继续翻页。
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
            // 只观察已经存在的字段editor；未提交组合文字没有翻页权限。
            editor
                .downcast_ref::<NSTextView>()
                .is_some_and(|view| view.hasMarkedText())
        }) {
            return Err("请先完成搜索词输入，再查看下一页".into());
        }
        let submitted = submitted_workspace_query(field)?;
        if owner.workspace_query_intent() != intent {
            // 字段Native读取重入新输入后，旧Next既不能取目录游标也不能取消新期限。
            return Ok(None);
        }
        let Some(query) = surface.search_query.as_ref() else {
            if surface.sidebar_intent != Some(intent) {
                // 新词B尚在140ms期限中，旧目录Next不能借其新输入代次重启目录扫描。
                return Err("搜索词已经变化，请等待搜索完成后再翻页".into());
            }
            // 显式刷新进入目录模式后字段可保留原文字；相同意图的Next按目录模式继续。
            surface.install_next_sidebar_page(mtm)?;
            return Ok(None);
        };
        if submitted.trim() != query {
            // 140ms期限尚未提交的新词不能复用旧末项；保留输入与原cursor供正常新查询取代。
            return Err("搜索词已经变化，请等待搜索完成后再翻页".into());
        }
        if owner.workspace_query_intent() != intent {
            // currentEditor/stringValue/UTF16读取任一步重入新输入后，保留新词及其排队权限。
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
        // 离开surface操作后再启动worker，cursor仅作为纯数据移交，不保活Native或正文。
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

/// 当前笔记菜单补足原队列窗口租约：Weak固定真实Doc，路径只取点击时已安装的原身份。
/// 票据不持有Native或正文强引用；侧栏换页、搜索和目录选择不改变这个文件目标。
#[derive(Clone)]
pub(super) struct NativeMarkdownCurrentNoteIntent {
    controller: usize,
    window: usize,
    pub(super) document: WeakMarkdownDocument,
    pub(super) relative_path: String,
}

impl NativeMarkdownCurrentNoteIntent {
    /// composition root只把实际已装配的窗口/Doc/路径绑定为弱票；不要求构造中的窗口已在Active。
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

    /// 首个sender/字段getter之前纯捕获；不读正文、确认IME或分配另一份Native窗口租约。
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
                return None; // 后继窗口或已接纳交接不能从旧菜单重新取得文件权限。
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

    /// Doc Weak保留控制块身份；同名A→B→A打开的新Doc也不能恢复旧票。
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

    /// 只检查原窗口和真实Doc元数据，不升级Weak或查询稍后的侧栏目标。
    pub(super) fn current(&self) -> bool {
        ACTIVE_NATIVE_MARKDOWN_SURFACE
            .with_borrow(|active| active.as_ref().is_some_and(|surface| self.matches(surface)))
    }

    /// 原菜单与冲突条共用具体重载/另存流程；原票跨IME、确认、命名和磁盘副作用持续有效。
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
        if let Ok(view) = active_document_view() {
            // 保留原具体重载/另存前置输入提交；信息卡等只读动作不走这里。
            view.commit_active_table_cell()?;
            if !current() {
                return Ok(());
            }
            view.commit_composition_before_release()?;
        }
        if !current() {
            return Ok(());
        }
        let mtm = MainThreadMarker::new().expect("冲突处理沿原AppKit主队列执行");
        match action {
            // 只有明确确认才放弃原本地草稿；取消保留同一canonical与原外部文件。
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
                    // 只有明确Reload授权丢弃；Cancel保留当前canonical草稿和外部文件。
                    return Ok(());
                }
                if !current() {
                    return Ok(());
                }
                let preparation = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    // 用户已明确确认重新载入；这里只核验磁盘并准备小票据，不发布决定或新模型。
                    let surface = active.as_ref().ok_or("笔记窗口已关闭")?;
                    if !self.matches(surface)
                        || surface.controller.conflict_publication() != revision
                    {
                        return Err("原冲突笔记或保存结果已变更，本次操作已取消".to_owned());
                    }
                    let editor = surface.editor.as_ref().ok_or("当前没有打开的笔记")?;
                    session
                        .prepare_legacy_recovery_after_reload(&editor.document().borrow())
                        .map_err(|error| error.to_string())
                })?;
                let handoff = match preparation {
                    omni_markdown_core::MarkdownRecoveryPreparation::NotPending => {
                        // 普通文档继续既有重新载入，不创建恢复权限。
                        None
                    }
                    omni_markdown_core::MarkdownRecoveryPreparation::Prepared(handoff) => {
                        // 仅当前旧冲突草稿的显式决定获得一次性目标能力。
                        Some(handoff)
                    }
                };
                if !current() {
                    return Ok(());
                }
                replace_active_editor_from_disk(path, handoff, self, revision)
            }
            // 本地草稿先以独占新文件真正保存，再沿原释放屏障更换编辑器。
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
                    // 命名取消保留原草稿和权威文件，不新建或删除任何笔记。
                    return Ok(());
                };
                if !current() {
                    return Ok(());
                }
                let receipt = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    let surface = active.as_ref().ok_or("笔记窗口已关闭")?;
                    if !self.matches(surface)
                        || surface.controller.conflict_publication() != revision
                    {
                        return Err("原冲突笔记或保存结果已变更，本次操作已取消".to_owned());
                    }
                    let editor = surface.editor.as_ref().ok_or("当前没有打开的笔记")?;
                    session
                        .save_document_buffer_as_new(
                            &editor.document().borrow(),
                            &native_markdown_note_parent(&path),
                            &title,
                        )
                        .map_err(|error| error.to_string())
                })?;
                if !current() {
                    return Err(format!(
                        "草稿副本已保存到“{}”，但原笔记操作已撤销",
                        receipt.relative_path
                    ));
                }
                with_active_native_markdown_surface(|surface| {
                    if !self.matches(surface)
                        || surface.controller.conflict_publication() != revision
                    {
                        return Err("草稿副本已保存，但原笔记操作已撤销".to_owned());
                    }
                    surface.refresh_workspace_sidebar(mtm)
                })
                .map_err(|error| {
                    format!(
                        "草稿副本已保存到“{}”，但目录刷新未完成：{error}",
                        receipt.relative_path
                    )
                })?;
                if !current() {
                    return Err(format!(
                        "草稿副本已保存到“{}”，但原笔记操作已撤销",
                        receipt.relative_path
                    ));
                }
                let preparation = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    // 副本已经持久化，重新核验当前 live 草稿与该副本的 receipt；失败仍保留所有文件。
                    let surface = active.as_ref().ok_or("笔记窗口已关闭")?;
                    if !self.matches(surface)
                        || surface.controller.conflict_publication() != revision
                    {
                        return Err("原冲突笔记或保存结果已变更，本次操作已取消".to_owned());
                    }
                    let editor = surface.editor.as_ref().ok_or("当前没有打开的笔记")?;
                    session
                        .prepare_legacy_recovery_after_save_as_new(
                            &editor.document().borrow(),
                            &receipt,
                        )
                        .map_err(|error| format!("副本已保存，但恢复交接尚未准备完成：{error}"))
                })?;
                let handoff = match preparation {
                    omni_markdown_core::MarkdownRecoveryPreparation::NotPending => {
                        // 普通另存副本不生成恢复记录或绕过权限。
                        None
                    }
                    omni_markdown_core::MarkdownRecoveryPreparation::Prepared(handoff) => {
                        // 只把已核验的小票据交给原释放屏障，旧模型与映射不进入 pending transition。
                        Some(handoff)
                    }
                };
                if !current() {
                    return Err(format!(
                        "草稿副本已保存到“{}”，但原笔记操作已撤销",
                        receipt.relative_path
                    ));
                }
                replace_active_editor_from_disk(receipt.relative_path, handoff, self, revision)
            }
            _ => unreachable!("冲突处理只接原重载和另存动作"),
        }
    }

    /// 只收集原笔记动作的有限候选；实际写入仍消费begin_file_mutation的保存/版本/释放流程。
    pub(super) fn perform(&self, action: isize) -> Result<(), String> {
        if !self.current() {
            return Ok(()); // 排队期间原文档已离开，取消旧动作而不向新笔记显示旧错误。
        }
        preflight_native_markdown_modal()?;
        if !self.current() {
            return Ok(()); // 前置边界已撤原窗口时，不捕获后来活动会话。
        }
        let mtm = MainThreadMarker::new().expect("当前笔记菜单只由原AppKit主队列消费");
        let (session, controller, directories) =
            ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                let surface = active.as_ref().expect("上方纯检查固定原Active窗口");
                let directories = if action == 31 {
                    // 页面只提供可编辑目标的有限建议；源笔记始终是原path，不依赖页面行或选中文件夹。
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
                    Vec::new() // 其余动作没有目录选择，不复制与动作无关的有限页面。
                };
                (
                    surface.session.clone(),
                    surface.controller.clone(),
                    directories,
                )
            });
        if action == 33 {
            // Finder不保存或提交草稿；先让原Core重新验证真实Markdown文件，再交系统服务。
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
            // 初值只来自原笔记的真实标题，用户确认的候选仍由原Core名称规则准入。
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
            // 移动目标可手填不在当前有限页的目录，原对话框和Core负责输入及confinement。
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
            }), // Core沿原不覆盖规则生成副本路径，UI不猜文件名或复制正文。
            // 删除必须再次明确确认这篇原笔记，Cancel不提交正文或触发文件删除。
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
            return Ok(()); // modal或局部Native清理已撤销原Doc，候选不转投后来同名笔记。
        }
        let Some(mutation) = mutation else {
            return Ok(()); // 用户取消不保存、不释放正文，也不排队文件效果。
        };
        let accepted = with_active_native_markdown_surface(|surface| {
            if !self.matches(surface) {
                return Ok(false); // 借出Active前后的原身份仍必须相同，不能靠路径补签。
            }
            surface.begin_file_mutation(mutation)?;
            Ok(true)
        })?;
        if !accepted {
            return Ok(());
        }
        with_active_native_markdown_surface(|surface| {
            // 接纳后由原pending拥有一次实际版本/释放收据；不再以已撤销的before重捕Doc或重放。
            let pending = surface
                .pending_file_mutation
                .as_ref()
                .expect("原begin_file_mutation已接纳唯一文件操作");
            controller.enqueue_owned_after_release(pending.release_barrier.as_ref(), || {
                if let Err(error) = finish_native_markdown_file_mutation() {
                    eprintln!("笔记文件操作失败：{error}"); // 原完成入口已保留文件后果与中文状态。
                }
            });
            Ok(())
        })
    }
}

/// Retains a view only around an immediate text transaction, never around a modal dialog.
pub(super) fn active_document_view() -> Result<Retained<PieceBackedMarkdownTextView>, String> {
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        let surface = active.as_ref().ok_or("笔记窗口已关闭")?;
        surface
            .editor
            .as_ref()
            .map(|editor| editor.backend.text_view.clone())
            .ok_or_else(|| "当前没有打开的笔记".into())
    })
}

/// 系统文件选择器只把明确确认的有限file URL交给core；Cancel与Abort分开。
fn choose_import_file(
    controller: &NativeMarkdownController,
    mtm: MainThreadMarker,
    markdown_only: bool,
) -> Result<Option<PathBuf>, NativeMarkdownModalError> {
    let guard = NativeMarkdownModalUseGuard::enter(controller)?;
    let result = autoreleasepool(|_| {
        let panel = NSOpenPanel::openPanel(mtm);
        // 本地proxy是显式角色并默认actual nil；远端系统UI的进程/内存归属另行验收。
        guard
            .ledger
            .include("modal.open-panel-proxy", &panel)
            .map_err(NativeMarkdownModalError::Boundary)?;
        // 系统选择器也明确使用中文任务名称与确认动作，不依赖系统默认文案。
        // 这里只配置本次模态请求的静态文案，不改文件名、路径或用户的系统语言。
        panel.setPrompt(Some(ns_string!("导入")));
        panel.setTitle(Some(if markdown_only {
            // Markdown 导入复制为工作区笔记，与作为附件插入的入口区分。
            ns_string!("导入 Markdown 文档")
        } else {
            // 附件保留原文件类型，由既有 core 校验与复制流程处理。
            ns_string!("导入附件")
        }));
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(false);
        if markdown_only {
            // 保留既有扩展名筛选；core仍重核descriptor/extension，不把UI过滤当文件authority。
            #[allow(deprecated)]
            panel.setAllowedFileTypes(Some(&NSArray::from_slice(&[
                ns_string!("md"),
                ns_string!("markdown"),
            ])));
        }
        let response = native_markdown_open_panel_response(panel.runModal());
        let candidate = response.and_then(|confirmed| {
            if !confirmed {
                // 用户取消时不读取URL/path，也没有文件系统效果。
                return Ok(None);
            }
            let url = panel.URL().ok_or_else(|| {
                NativeMarkdownModalError::Boundary("文件选择器没有返回选中的文件".into())
            })?;
            if !url.isFileURL() {
                // 不把非文件scheme或缺失路径伪装成普通取消。
                return Err(NativeMarkdownModalError::Boundary(
                    "文件选择器返回的内容不是本地文件".into(),
                ));
            }
            let path = url.path().ok_or_else(|| {
                NativeMarkdownModalError::Boundary("所选文件没有有效的本地路径".into())
            })?;
            if path.length() > 16_384 {
                // Foundation建立UTF-8临时表示之前先限定native输入，拒绝整项而不截断路径。
                return Err(NativeMarkdownModalError::InputTooLong);
            }
            autoreleasepool(|pool| {
                // SAFETY: path和最内层pool覆盖此同步借用与PathBuf构造，期间不调用native setter。
                let path = unsafe { path.to_str(pool) };
                if path.len() > 16_384 {
                    // UTF-8字节边界独立于UTF-16，保留文件名中的前后空格而不trim。
                    return Err(NativeMarkdownModalError::InputTooLong);
                }
                Ok(Some(PathBuf::from(path)))
            })
        });
        // 只清公开自有配置，不遍历远端窗口、清未知field history或更改系统设置。
        panel.setDirectoryURL(None);
        panel.setNameFieldStringValue(ns_string!(""));
        // 共享面板只保系统默认配置，后续模态请求不能继承本次入口的任务文案。
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
    recovery_handoff: Option<omni_markdown_core::MarkdownRecoveryHandoff>,
    original: &NativeMarkdownCurrentNoteIntent,
    revision: u64,
) -> Result<(), String> {
    let controller = with_active_native_markdown_surface(|surface| {
        if !original.matches(surface) || surface.controller.conflict_publication() != revision {
            return Err("原冲突笔记或保存结果已变更，本次操作已取消".to_owned());
        }
        surface.stop_search();
        if let Some(handoff) = &recovery_handoff {
            // 票据只能打开它绑定的唯一目标，不能被另一菜单路径借用为通用绕过入口。
            if handoff.target_relative_path() != relative_path {
                return Err("恢复交接目标已变更，当前草稿已保留".to_owned());
            }
            let editor = surface.editor.as_ref().ok_or("当前没有打开的笔记")?;
            surface
                .session
                .validate_legacy_recovery_handoff(&editor.document().borrow(), handoff)
                .map_err(|error| error.to_string())?;
        }
        let previous_relative_path = surface.current_note_relative_path.clone();
        // 所有可重入的辞任/预检结束后，再在 accepted 清理前核验票据源身份；
        // 晚到输入拒绝释放，票据 drop 不写任何 marker。
        let release_barrier =
            surface.destroy_current_editor_with_recovery(recovery_handoff.as_ref())?;
        surface.pending_note_switch = Some(PendingNativeMarkdownNoteSwitch {
            relative_path,
            previous_relative_path,
            release_barrier,
            recovery_handoff,
        });
        Ok::<_, String>(surface.controller.clone())
    })?;
    // The confirmed reload waits for actual old-graph destruction before acquiring authoritative disk content.
    schedule_native_markdown_note_switch_completion(&controller)
}

/// 原工具栏与菜单共用一次导入：保存原草稿、保原目录来源，再交Core安全复制和原笔记切换。
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
    if let Ok(view) = active_document_view() {
        // 同一原字段和正文先完成IME；不能在面板回来后丢掉用户仍可见的草稿。
        view.commit_active_table_cell()?;
        view.commit_composition_before_release()?;
    }
    ensure_action_is_current(&controller, relative_path.as_deref())?;
    with_active_native_markdown_surface(|surface| {
        if !list.origin_matches_surface(surface, origin) {
            return Err("导入前所选目录已经变化，请重试".into());
        }
        // 恢复原页面flushSave在导入前的顺序；唯一保存入口处理冲突，失败时尚无外部文件复制。
        surface.save_current_document()
    })?;
    if !list.permits_origin(origin) {
        return Err("保存期间所选目录或原笔记会话已经变化，请重新导入".into());
    }
    let Some(path) =
        choose_import_file(&controller, mtm, true).map_err(|error| error.to_string())?
    else {
        return Ok(()); // 取消面板没有目录写入或正文切换，已完成的原文保存仍有效。
    };
    ensure_action_is_current(&controller, relative_path.as_deref())?;
    if !list.permits_origin(origin) {
        return Err("文件选择期间所选目录已经变化，请重试".into());
    }
    // 原Core负责受限目录、实际文件类型和无覆盖复制；目录不在当前页也不改变真实目的地。
    let receipt = session
        .import_note(&path, destination)
        .map_err(|error| error.to_string())?;
    // Core已经提交文件；后续刷新失败必须保留成功路径，不能让重试提示诱发另一份副本。
    with_active_native_markdown_surface(|surface| surface.refresh_workspace_sidebar(mtm)).map_err(
        |error| {
            format!(
                "笔记已导入到“{}”，但目录刷新未完成：{error}",
                receipt.relative_path
            )
        },
    )?;
    // 刷新中的原生重入可以撤销旧窗口动作，但不会撤销已经完成的磁盘导入。
    ensure_action_is_current(&controller, relative_path.as_deref()).map_err(|error| {
        format!(
            "笔记已导入到“{}”，但原窗口动作未完成：{error}",
            receipt.relative_path
        )
    })?;
    // 原生命周期再次保存或打开失败时仍报告真实导入结果，不重放复制或回滚成功文件。
    switch_native_markdown_note(&receipt.relative_path).map_err(|error| {
        format!(
            "笔记已导入到“{}”，但打开未完成：{error}",
            receipt.relative_path
        )
    })
}

/// 分派既有文档动作；文件失败保留草稿，导入与头部按钮共用真实浏览目录和同一流程。
pub(super) fn perform_native_markdown_workspace_action(action: isize) -> Result<(), String> {
    if action == 2 {
        // 文档菜单和头部导入消费同一真实浏览目录，不从当前打开笔记反推目标或默认写根。
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
        // 直接内部调用也固定同一原笔记，不让通用动作重新领取确认之后的Active目标。
        let controller = ACTIVE_NATIVE_MARKDOWN_SURFACE
            .with_borrow(|active| active.as_ref().map(|surface| surface.controller.clone()))
            .ok_or("笔记窗口已关闭")?;
        if let Some(intent) = NativeMarkdownCurrentNoteIntent::capture(&controller) {
            return intent.perform_resolution(action, controller.conflict_publication());
        }
        return Ok(());
    }
    if action == 6 {
        // 信息是只读浮层；不能借用通用文件/格式入口而隐式确认IME或提交单元格。
        let controller = ACTIVE_NATIVE_MARKDOWN_SURFACE
            .with_borrow(|active| active.as_ref().map(|surface| surface.controller.clone()))
            .ok_or("笔记窗口已关闭")?;
        if let Some(intent) = NativeMarkdownCurrentNoteIntent::capture(&controller) {
            super::information_panel::toggle(&intent)?;
        }
        return Ok(());
    }
    let mtm = MainThreadMarker::new().ok_or("文档操作必须在主线程执行")?;
    // nested modal在提交父IME/表格草稿或取得任何native view之前拒绝。
    preflight_native_markdown_modal()?;
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
            let view = active_document_view()?;
            insert_native_markdown_attachment(&session, note, &path, &view)
        }
        // 普通查找直接保留原完整替换面板；字段和真实editor/Undo仍属于本会话。
        7 | 8 => show_native_markdown_find_bar(true),
        // The native menu exposes folding to keyboard and accessibility clients as well as gutter clicks.
        20 => {
            let view = active_document_view()?;
            let offset = view.selectedRange().location;
            let document = view
                .ivars()
                .text_storage
                .load()
                .and_then(|storage| storage.document())
                .ok_or("编辑器已关闭")?;
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
        // 显式按钮的通常入口已固定意图；直接调用也只清本次当前查询，不提交或保存正文。
        9 => return_native_markdown_workspace_to_directory(
            std::ptr::from_ref(&*controller).addr(),
            controller.workspace_query_intent(),
        ),
        // Paging reads another bounded metadata chunk without replacing the editor or preloading bodies.
        10 => advance_native_markdown_sidebar_page(mtm, &controller),
        // 真实菜单和独立插表 action 共用一次提交、明确首格选择及 RequiredRow 激活，不落旧格式器分支。
        118 => insert_table_into_active_native_markdown_editor(),
        // 其余格式仍沿同一已捕获选择和 canonical 事务处理。
        100..=117 => format_active_markdown_selection(action, active_document_view()?),
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
    // 空尾槽先于任何局部 Native claim 声明；提交后接住原 guard，最后析构才清掉同一 pending owner。
    let pending_tail: RefCell<Option<NativeMarkdownPendingSelectionGuard>> = RefCell::new(None);
    with_active_native_markdown_surface(|surface| {
        // Native reentry cannot switch/save/close while the copied asset and canonical text are between commit points.
        let current_view = surface.editor.as_ref().ok_or("当前没有打开的笔记")?;
        if !std::ptr::eq(&*current_view.backend.text_view, view)
            || surface.current_note_relative_path.as_deref() != Some(note)
        {
            // A retained old view or dialog context has no authority to import into a replacement document.
            return Err("插入附件前编辑器已变更，请重试".into());
        }
        if native_markdown_operation_is_busy(
            surface.pending_note_switch.is_some(),
            surface.pending_file_mutation.is_some(),
        ) {
            // An accepted handoff owns the last persistence boundary and forbids further asset/text mutations.
            return Err("另一项文档操作仍在完成中".into());
        }
        let storage = view.ivars().text_storage.load().ok_or("编辑器已关闭")?;
        // 附件插入也由真实ChangeSet映射fold，不先丢弃无关的折叠。
        let range = view.selectedRange();
        let pending = session
            .prepare_attachment_import(note, path)
            .map_err(|error| error.to_string())?;
        let result =
            storage.apply_explicit_replacement(range, &pending.receipt().markdown_insertion);
        match result {
            // The source/history transaction succeeded; retain the asset before publishing native caret changes.
            Ok((_edit, committed, pending_selection)) => {
                drop(pending_tail.replace(pending_selection));
                // 同一原提交的 pending guard 交给早声明尾槽，覆盖附件确认、选择安装及 Native claim 的最后析构。
                // 不能在 storage 返回时先清槽，再让同步通知把中间选择重签为普通新输入。
                // The link is already canonical; no additional asset metadata needs to persist in the UI.
                let _ = pending.commit();
                if view.apply_committed_selection(&committed) {
                    let _ = committed.native_step(|| view.schedule_visible_table_layout_update());
                } // 附件已在canonical内；失效票只阻止旧caret覆盖后继输入。
                Ok(())
            }
            // Text rejection leaves no new history revision; explicitly roll back the still-provisional asset.
            Err(error) => match pending.rollback() {
                // Verified cleanup removes only the unchanged newly created attachment, preserving the live draft.
                Ok(()) => Err(error),
                // A detected external replacement/change must be preserved and reported, never blindly removed.
                Err(cleanup) => Err(format!(
                    "插入附件链接失败：{error}；清理临时附件失败：{cleanup}"
                )),
            },
        }
    })
}

impl NativeMarkdownWorkspaceEditIntent {
    /// 菜单原票直接到实际命令；格内行内格式先于普通前置提交，避免丢掉真实子选区。
    pub(super) fn perform(&self, action: isize) -> Result<(), String> {
        if !self.current() {
            return Err("格式操作的原文档或选择已经改变".into());
        }
        if self.has_table_selection() && matches!(action, 103..=108 | 113..=114) {
            // 原字段自己的 IME 确认、序列化和格式组成一次有限提交；不先走会清字段的通用 preflight。
            preflight_native_markdown_modal()?;
            let view = active_document_view()?;
            view.format_table_cell_for_menu(self, native_markdown_format_action(action)?)
        } else {
            // 非格内动作沿原文件、正文块和插表事务，不添加新的表格块语法或快捷键。
            perform_native_markdown_workspace_action(action)
        }
    }
}

/// 菜单 tag 只映射既有领域命令；正文与真实单格使用同一个映射。
fn native_markdown_format_action(action: isize) -> Result<MarkdownFormatAction, String> {
    Ok(match action {
        // 一级标题归一化当前行原有的标题前缀。
        100 => MarkdownFormatAction::Heading1,
        // 二级标题只替换标题标记，保留所选行正文。
        101 => MarkdownFormatAction::Heading2,
        // 三级标题沿同一次原子前缀替换，使用该级标记宽度。
        102 => MarkdownFormatAction::Heading3,
        // 加粗保留所选源内容；空选择使用原可编辑占位。
        103 => MarkdownFormatAction::Bold,
        // 斜体通过同一领域格式器包裹真实选区或空选择占位。
        104 => MarkdownFormatAction::Italic,
        // 下划线写入 canonical HTML，不维护独立的原生格式正文。
        105 => MarkdownFormatAction::Underline,
        // 删除线保留原成对 Markdown 分隔符语义。
        106 => MarkdownFormatAction::Strikethrough,
        // 行内代码将所选原字节保留在准确的一对反引号内。
        107 => MarkdownFormatAction::InlineCode,
        // 链接沿原格式合同选择可编辑标签，URL 模板留在标签选区之外。
        108 => MarkdownFormatAction::Link,
        // 引用前缀覆盖每条相交行，包括恰在下一行行首的终点。
        109 => MarkdownFormatAction::Blockquote,
        // 无序列表在一次可撤销的 canonical 事务中改动所选各行前缀。
        110 => MarkdownFormatAction::BulletList,
        // 有序列表保留原领域格式器的逐行字面序号。
        111 => MarkdownFormatAction::OrderedList,
        // 任务列表为每条所选行插入未勾选的源复选框。
        112 => MarkdownFormatAction::TaskList,
        // 行内公式的成对标记继续进入唯一 canonical 正文。
        113 => MarkdownFormatAction::InlineFormula,
        // 上标用明确的 canonical HTML 标签表示。
        114 => MarkdownFormatAction::Superscript,
        // 分隔线模板按原默认内容插在真实 head，不删除原选择。
        115 => MarkdownFormatAction::Divider,
        // 代码围栏保留原占位选区与周围换行。
        116 => MarkdownFormatAction::CodeBlock,
        // 块公式以一个历史编辑插入原有限模板。
        117 => MarkdownFormatAction::FormulaBlock,
        // 未知 tag 不具有任何正文修改权限。
        _ => return Err("无法识别的格式命令".into()),
    })
}

/// 有限正文命令进入同一原子格式器，并发布其准确选择。
fn format_active_markdown_selection(
    action: isize,
    view: Retained<PieceBackedMarkdownTextView>,
) -> Result<(), String> {
    // 空槽的声明早于局部原生持有；把参数 owner 移入后声明局部，保证视图最终 release 也先于 guard 清槽。
    let pending_tail: RefCell<Option<NativeMarkdownPendingSelectionGuard>> = RefCell::new(None);
    let command_view = view;
    let view = &*command_view;
    // 主head捕获、Core提交与Native选择发布的整个栈都参与关闭预检，不能只靠Retained保活。
    let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
    let action = native_markdown_format_action(action)?;
    let selection = view
        .selection_for_command()
        .map_err(|error| error.to_string())?;
    let storage = view.ivars().text_storage.load().ok_or("编辑器已关闭")?;
    let (_edit, committed, pending) = storage.apply_format_captured(action, selection)?;
    drop(pending_tail.replace(pending));
    // 原 guard 与同一 Rc 交给早声明尾槽，覆盖安装、布局以及 storage/视图 Native claims 的最后析构。
    // 正文成功不依赖后续选择仍获授权；Native重入后停止旧UI后果，不重放格式事务。
    if view.apply_committed_selection(&committed) {
        let _ = committed.native_step(|| view.schedule_visible_table_layout_update());
    }
    Ok(())
}

/// Reads only explicit file-URL pasteboard items, with a fixed batch limit and no cache.
pub(super) fn native_markdown_pasteboard_file_paths(
    pasteboard: &NSPasteboard,
) -> Result<Vec<PathBuf>, String> {
    // SAFETY: AppKit 导出的文件 URL 类型具有进程寿命；类型查询不物化路径或正文。
    let file_type = unsafe { objc2_app_kit::NSPasteboardTypeFileURL };
    if pasteboard
        .availableTypeFromArray(&NSArray::from_slice(&[file_type]))
        .is_none()
    {
        // 普通文本仍走系统粘贴路径，不因多项文本而触发附件批次策略。
        return Ok(Vec::new());
    }
    let Some(items) = pasteboard.pasteboardItems() else {
        // 已声明文件类型但没有实际 item，不构造伪造路径。
        return Ok(Vec::new());
    };
    if items.count() > 16 {
        // 超出已有批次预算明确拒绝整个请求，不静默只导入前十六个文件。
        return Err("每次粘贴或拖入最多导入 16 个文件".into());
    }
    let mut paths = Vec::new();
    for index in 0..items.count() {
        // 所有 getter 都在有限批次内；URL 和路径在复制进 Rust 之前检查原生长度。
        let item = items.objectAtIndex(index);
        let Some(value) = item.stringForType(file_type) else {
            // 混合批次的非文件 item 不具有附件授权。
            continue;
        };
        if value.length() > 16_384 {
            return Err("附件地址超过导入长度上限".into());
        }
        let url = NSURL::URLWithString(&value).ok_or("附件地址无效")?;
        let path = url.path().ok_or("附件没有本地路径")?;
        if path.length() > 4_096 {
            return Err("附件路径超过导入长度上限".into());
        }
        paths.push(PathBuf::from(path.to_string()));
    }
    Ok(paths)
}

/// 限制待通知业务载荷总量；原生 adapter 与纯测试共享同一入队判定。
fn validate_attachment_queue(pending: usize, paths: &[PathBuf]) -> Result<(), String> {
    if pending >= 4 {
        // 不丢弃旧请求，也不把新请求的路径放到无界 dispatch 队列。
        return Err("已有 4 项附件导入正在等待，请完成后重试".into());
    }
    if paths.is_empty()
        || paths.len() > 16
        || paths.iter().any(|path| path.as_os_str().len() > 16_384)
    {
        // 批次数量和每条路径各自有限，拒绝完整操作而不静默接受前缀。
        return Err("这批附件超过待处理导入的容量上限".into());
    }
    Ok(())
}

/// Defers external-file I/O while carrying no strong native editor ownership across the queue.
pub(super) fn schedule_native_markdown_attachment_paths(
    view: &PieceBackedMarkdownTextView,
    paths: Vec<PathBuf>,
) -> Result<(), String> {
    validate_attachment_queue(
        view.ivars().pending_attachment_imports.borrow().len(),
        &paths,
    )?;
    if view.ivars().textkit_graph_teardown.get() {
        // 关闭后的路径不进入任何邮箱，也不获得新工作权限。
        return Err("笔记编辑器正在关闭".into());
    }
    let mtm = MainThreadMarker::new().expect("native paste/drop callbacks run on the main thread");
    // Preserve the user's exact paste/drop anchor while deferring file I/O outside native callbacks.
    let anchor_range = view.selectedRange();
    let anchor_generation = view
        .ivars()
        .text_storage
        .load()
        .and_then(|storage| storage.document())
        .map(|document| document.borrow().generation());
    let mailbox = std::sync::Arc::new(crate::cancelable_mailbox::CancelableMailbox::new());
    mailbox.publish(paths);
    view.ivars()
        .pending_attachment_imports
        .borrow_mut()
        .push(std::sync::Arc::clone(&mailbox));
    let mailbox = std::sync::Arc::downgrade(&mailbox);
    let weak = MainThreadBound::new(Weak::from(view), mtm);
    enqueue_native_markdown_main_task(move || {
        let mtm = MainThreadMarker::new().expect("native imports execute on the main queue");
        // A closed editor cannot be revived by an earlier drop or paste operation.
        let Some(view) = weak.get(mtm).load() else {
            // The original native editor has deallocated; queued file work owns no successor authority.
            return;
        };
        let Some(mailbox) = mailbox.upgrade() else {
            // close 已同步取消并释放路径；主队列通知不需要执行才能完成清理。
            return;
        };
        let pending = {
            let mut imports = view.ivars().pending_attachment_imports.borrow_mut();
            // 移出本请求的 owner，其他仍待执行请求继续留在当前会话邮箱账本内。
            imports
                .iter()
                .position(|entry| std::sync::Arc::ptr_eq(entry, &mailbox))
                .map(|index| imports.swap_remove(index))
        };
        drop(pending);
        let Some(paths) = mailbox.take() else {
            // 迟到或重复通知没有路径载荷，也不能恢复已关闭编辑器的权限。
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
                return Err("导入附件前编辑器已变更，请重试".into());
            }
            let generation = view
                .ivars()
                .text_storage
                .load()
                .and_then(|storage| storage.document())
                .map(|document| document.borrow().generation());
            if generation != anchor_generation || view.selectedRange() != anchor_range {
                // Intervening input or caret movement invalidates the original physical drop/paste anchor.
                return Err("导入附件前文档或插入位置已变更，请重试".into());
            }
            let note = relative_path.ok_or("当前没有打开的笔记")?;
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
    Ok(())
}

/// Hides native find UI before Escape is allowed to close the document's foreground session.
pub(super) fn dismiss_active_native_markdown_find_bar() -> bool {
    let visible = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        active
            .as_ref()
            .and_then(|surface| surface.editor.as_ref())
            .and_then(|editor| editor.find_bar.borrow().clone())
            .is_some_and(|bar| bar.is_visible())
    });
    if !visible {
        // 没有 transient Find owner 时让调用方执行普通 Escape 策略。
        return false;
    }
    if let Err(error) = perform_native_markdown_find_action(MarkdownFindAction::Hide) {
        // 辞任失败仍消费 Escape 并保留当前字段，不能降级成关闭整个窗口。
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

/// 只有主队列信号门禁允许进入；消费时仍逐项核原owner/代次及精确邮箱身份。
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
        // 被取消的邮箱已销毁；不访问新会话或重建旧查询。
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
            // 地址/计数即使复用，也不能借另一操作的邮箱安装或报告旧结果。
            // 所有检查都在租约内且不调用Native；取消只触碰本通知自己的邮箱。
            mailbox.cancel();
            return Ok(());
        }
        let Some(results) = mailbox.take() else {
            // 取消、poison 或已消费的邮箱没有再次发布 UI 的权限。
            return Ok(());
        };
        match results {
            Ok(results) => {
                // 只安装当前查询的有限摘要，正文 canonical owner 不变。
                if let Err(error) =
                    surface.install_search_results(results, generation, accepted_intent, mtm)
                    && surface.search_delivery_is_current(generation, accepted_intent)
                {
                    // 安装Native阶段若已重入B，旧错误同样不得覆盖其输入反馈。
                    surface.controller.present_error(&error);
                }
            }
            Err(error) => {
                // 错误只在当前前台状态显示，不保存路径或错误到延迟闭包。
                surface.controller.present_error(&error.to_string());
            }
        }
        Ok(())
    });
}

#[cfg(test)]
mod resource_budget_tests {
    use super::{submitted_workspace_query_from_utf16, validate_attachment_queue};
    use std::path::PathBuf;

    #[test]
    fn query_scalar_limit_preserves_pairs_and_rejects_malformed_native_utf16() {
        for count in [127, 128] {
            // 补充平面每 scalar 两 unit，边界内查询必须完整保留而不丢最后一个字符。
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
        // combining mark 按 scalar 计数，不能把可见 grapheme 的数量冒充 core 搜索预算。
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

    #[test]
    fn bounded_attachment_admission_never_discards_an_accepted_prefix() {
        let paths = vec![PathBuf::from("/synthetic/file.md"); 16];
        assert!(validate_attachment_queue(3, &paths).is_ok());
        assert!(validate_attachment_queue(4, &paths).is_err());
        assert!(validate_attachment_queue(0, &[]).is_err());
        let too_many = vec![PathBuf::from("/synthetic/file.md"); 17];
        assert!(validate_attachment_queue(0, &too_many).is_err());
        assert!(validate_attachment_queue(0, &[PathBuf::from("x".repeat(16_384))]).is_ok());
        assert!(validate_attachment_queue(0, &[PathBuf::from("x".repeat(16_385))]).is_err());
        // 验证不消费调用者输入；拒绝后仍可修改批次并明确重试。
        assert_eq!(too_many.len(), 17);
    }
}
