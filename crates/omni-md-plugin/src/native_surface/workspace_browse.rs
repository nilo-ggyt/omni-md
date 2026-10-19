//! 目录完整排序只在一个前台worker执行；主队列通知只持weak邮箱、owner和标量权限。

use super::workspace_browse_policy::{
    NativeMarkdownBrowseCompletionPolicy, NativeMarkdownBrowseTicket,
};
use super::*;
use crate::workspace_ready::{WorkspaceDelivery, WorkspaceReadySignal};
use omni_markdown_core::{
    MarkdownWorkspacePage, MarkdownWorkspacePageProgress, MarkdownWorkspacePager,
};

/// 只有邮箱/worker暂持有限页和pager，关闭可同步排空，不等待通知执行。
pub(super) type NativeMarkdownBrowseResult = Result<
    (MarkdownWorkspacePager, MarkdownWorkspacePage),
    omni_markdown_core::MarkdownWorkspaceError,
>;

/// 只有显式“返回目录/刷新目录”清查询；内部保存、恢复与折叠刷新保留各自原输入合同。
pub(super) fn return_native_markdown_workspace_to_directory(
    owner_identity: usize,
    intent: u64,
) -> Result<(), String> {
    with_active_native_markdown_surface(|surface| {
        if std::ptr::from_ref(&*surface.controller).addr() != owner_identity
            || !surface.workspace_refresh_intent_is_current(intent)
        {
            // 排队期间的新输入或新窗口只撤旧动作，不能取消它的新查询期限。
            return Ok(());
        }
        let mut feedback = Some(intent);
        let result = surface.clear_workspace_query_and_refresh(intent, &mut feedback);
        if let Err(error) = result
            && feedback.is_some_and(|ticket| surface.workspace_refresh_intent_is_current(ticket))
        {
            // 失败只报告给原意图（或本动作确切推进的代次），不能覆盖Native重入的新输入。
            surface.controller.present_error(&error);
        }
        Ok(())
    })
}

impl NativeMarkdownWindowSurface {
    /// 正常结束已知查询客户端后清字段；拒绝组合/验证/未知历史时不改原输入。
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
            // 这里只查询已存在的实际field editor，不为清空动作新建editor或Undo manager。
            if !self.workspace_refresh_intent_is_current(intent) {
                return Ok(()); // getter重入B后，旧按钮不再拥有辞任或清字段权限。
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
                // 未知输入源和历史完整保留，不清全局manager或借字段路径猜所有权。
                return Err("搜索输入控件不属于当前笔记会话".into());
            }
            if editor.hasMarkedText() {
                // 未提交中文组合不能被程序清空；用户完成或取消组合后可再次点击。
                return Err("请先完成搜索框的输入，再返回目录".into());
            }
            if !self.workspace_refresh_intent_is_current(intent) {
                return Ok(()); // composition getter后仍不能重领新输入代次。
            }
            let responder = self.panel.firstResponder();
            if !self.workspace_refresh_intent_is_current(intent) {
                return Ok(());
            }
            if !responder.as_ref().is_some_and(|value| {
                std::ptr::from_ref(&**value).cast::<()>()
                    == std::ptr::from_ref(&*editor).cast::<()>()
            }) {
                // 名义currentEditor与实际responder不一致时不强制夺走另一输入客户。
                return Err("搜索输入焦点已经变化，请重试".into());
            }
            if !self.panel.makeFirstResponder(None) {
                // 公开验证拒绝正常辞任时，不清值、不清历史，也不切换目录模式。
                return Err("搜索输入尚未结束，请完成输入后重试".into());
            }
            if !self.workspace_refresh_intent_is_current(intent) {
                return Ok(()); // 成功didEnd可能同步重入B；原动作不能清掉它。
            }
            if field.currentEditor().is_some() {
                // 不以makeFirstResponder的返回值代替真实client结束事实。
                return Err("搜索输入仍在编辑，请重试".into());
            }
        }
        if !self.workspace_refresh_intent_is_current(intent) {
            return Ok(()); // 无editor分支的getter同样可能同步重入新输入。
        }
        self.controller.ivars().undo.preflight()?;
        if let Some(error) = borrowed.failure() {
            // didEnd仅沿原owned-client合同结束历史；已记录的未知历史失败不能被清字段掩盖。
            return Err(error);
        }
        field.setStringValue(ns_string!(""));
        if !self.workspace_refresh_intent_is_current(intent) {
            return Ok(()); // setter之后若B接管，只保留B的调度，不继续取消或刷新。
        }
        self.refresh_workspace_sidebar_with_policy(
            self.panel.mtm(),
            intent,
            NativeMarkdownBrowseCompletionPolicy::OpenFirstIfEmpty,
            feedback,
        )
    }

    /// 显式刷新进入目录模式；旧搜索的取消只来自用户刷新动作，不来自Core打开游标。
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

    /// 失败后实际目录只供重选；policy保留在pager上，后续Next不会自动开第一篇。
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

    /// 首个Native取消之前固定意图；timer析构重入B不能被贴到旧刷新继续取消。
    pub(super) fn refresh_workspace_sidebar_with_policy(
        &mut self,
        mtm: MainThreadMarker,
        intent: u64,
        policy: NativeMarkdownBrowseCompletionPolicy,
        feedback_intent: &mut Option<u64>,
    ) -> Result<(), String> {
        *feedback_intent = None;
        if !self.workspace_refresh_intent_is_current(intent) {
            return Ok(());
        }
        self.stop_search();
        let after_cancel = intent.wrapping_add(1);
        if !self.workspace_refresh_intent_is_current(after_cancel) {
            return Ok(());
        }
        // 已观察到自己的取消，若纯打开pager失败，只允许这个确切代次报告错误。
        *feedback_intent = Some(after_cancel);
        let pager = self
            .session
            .open_workspace_pager()
            .map_err(|error| error.to_string())?;
        self.sidebar_pager = Some(pager);
        self.sidebar_intent = Some(after_cancel);
        self.sidebar_completion_policy = policy;
        // 先撤销旧metadata；已删除path不能当成剩余笔记，错误status保持原值。
        self.notes = Vec::new();
        self.sidebar_rows = Vec::new();
        self.sidebar_has_more = false;
        // Next在进入Native取消前固定after_cancel+1；任何重入输入使最终反馈ticket失效。
        *feedback_intent = Some(after_cancel.wrapping_add(1));
        self.install_next_sidebar_page(mtm)
    }

    /// 输入权限和必要空态补偿分开：B撤旧目录/错误权限，但不能让查询控件恢复一半。
    pub(super) fn workspace_refresh_intent_is_current(&self, intent: u64) -> bool {
        self.workspace_surface_owner_is_current()
            && self.controller.workspace_query_intent() == intent
    }

    /// 借用surface的当前栈仍有同owner；只读TLS标量/身份，不执行Weak/Native。
    fn workspace_surface_owner_is_current(&self) -> bool {
        let identity = std::ptr::from_ref(&*self.controller).addr();
        let owner = BORROWED_NATIVE_MARKDOWN_OWNER
            .with(|borrowed| borrowed.get() == Some(identity))
            || super::controller::native_markdown_controller_is_current(&self.controller);
        owner
            && self.closing_request.is_none()
            && !self.controller.ivars().close_finalizer_scheduled.get()
    }

    /// strict释放后必须恢复完整可用的无正文态；只要仍是同一空owner，B不阻止必要补偿。
    pub(super) fn empty_workspace_compensation_is_current(&self) -> bool {
        self.workspace_surface_owner_is_current()
            && self.editor.is_none()
            && self.current_note_relative_path.is_none()
            && self.pending_note_switch.is_none()
            && self.pending_file_mutation.is_none()
    }

    /// 首次挂载完成后继续唯一的未完成扫描；此调用不捕获surface或递归借Active槽。
    pub(super) fn continue_initial_sidebar_scan(
        &mut self,
        mtm: MainThreadMarker,
    ) -> Result<(), String> {
        if !self.sidebar_scan_pending || self.sidebar_pager.is_none() {
            // 小库已经完整Ready；关闭/新query拿走pager后也没有重启权限。
            return Ok(());
        }
        self.install_next_sidebar_page(mtm)
    }

    /// Next只移交一个pager；512步循环留在worker，不在AppKit主线程重扫全库。
    pub(super) fn install_next_sidebar_page(
        &mut self,
        _mtm: MainThreadMarker,
    ) -> Result<(), String> {
        let intent = self.controller.workspace_query_intent();
        if self.search_query.is_some() || self.sidebar_intent != Some(intent) {
            // 新query/IME输入意图已撤销目录；不能把其新epoch贴到旧pager。
            return Ok(());
        }
        let mut pager = self
            .sidebar_pager
            .take()
            .ok_or("目录仍在读取或已经结束，请等待或刷新")?;
        self.controller.cancel_workspace_search();
        let accepted_intent = intent.wrapping_add(1);
        if self.controller.workspace_query_intent() != accepted_intent {
            // timer释放可重入新输入；未发布pager局部释放，不取消新期限。
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
            // Native setter之后再纯核权限；已有恢复/保存反馈不被目录加载覆盖。
            return Ok(());
        }
        self.search_cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled = std::sync::Arc::clone(&self.search_cancelled);
        let generation = self.search_generation;
        let collapsed = self.collapsed_directories.clone();
        // worker绝不持原生Weak或MainThreadBound：主线程取消后同步join，原生包装在
        // worker析构时同步派发会形成互等。非拥有地址须与下方精确邮箱身份共同核验。
        let owner_identity = std::ptr::from_ref(&*self.controller).addr();
        let mailbox = std::sync::Arc::new(crate::cancelable_mailbox::CancelableMailbox::new());
        self.sidebar_results = Some(std::sync::Arc::clone(&mailbox));
        let worker = std::thread::Builder::new()
            .name("omni-md-directory".into())
            .spawn(move || {
                let results = loop {
                    // 每步≤512，Core逐项检查取消和会话权限；Pending不进入UI邮箱。
                    match pager.next_page_cancellable(
                        MAX_VISIBLE_NATIVE_MARKDOWN_WORKSPACE_ENTRIES - 1,
                        MAX_SCANNED_NATIVE_MARKDOWN_WORKSPACE_ENTRIES,
                        &collapsed,
                        &cancelled,
                    ) {
                        Ok(MarkdownWorkspacePageProgress::Pending) => {
                            // 同一个worker继续下一有限步，不向主队列排每帧重试。
                        }
                        Ok(MarkdownWorkspacePageProgress::Ready(page)) => break Ok((pager, page)),
                        Err(error) => break Err(error),
                    }
                };
                if !mailbox.publish(results) {
                    // 关闭/换词已经封闭邮箱，结果和pager在worker栈内释放。
                    return;
                }
                let signal = WorkspaceReadySignal::Directory(WorkspaceDelivery {
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
                // spawn没有建立线程，先释放本次邮箱和等待标记，不遗留永久“正在读取”。
                if let Some(mailbox) = self.sidebar_results.take() {
                    mailbox.cancel();
                }
                self.sidebar_scan_pending = false;
                native_markdown_worker_start_error(NativeMarkdownWorker::Directory, &error)
            })?;
        self.search_worker = Some(worker);
        Ok(())
    }

    /// 纯门禁不调用Native；所有跨Native阶段共用同一个固定输入和任务代次。
    fn browse_delivery_is_current(&self, generation: u64, intent: u64) -> bool {
        NativeMarkdownBrowseTicket { generation, intent }.accepts(
            self.search_generation,
            self.controller.workspace_query_intent(),
            self.sidebar_intent,
            self.search_query.is_some(),
            self.closing_request.is_some(),
        )
    }

    /// 完整页安装只构造固定≤13行；布局和控件调用后再次核验原权限。
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
            // 错模式/旧query/本地发布后不拆除当前有效投影。
            return Ok(false);
        }
        let root = self
            .session
            .resolve_directory_for_system_action("")
            .map_err(|error| error.to_string())?;
        let root_title = native_markdown_path_name(&root.to_string_lossy());
        let (notes, mut rows) = native_markdown_sidebar_rows(page.snapshot, &root_title);
        for row in &mut rows {
            // 只触碰当前有界页；折叠集合是用户状态，不扫描整库来补父行。
            row.is_collapsed = self.collapsed_directories.contains(&row.relative_path);
        }
        self.notes = Vec::new();
        self.sidebar_rows = Vec::new();
        self.sidebar_list.clear()?;
        if !self.browse_delivery_is_current(generation, intent) {
            // 移除过程中重入新输入，旧页不能继续构造控件。
            return Ok(false);
        }
        self.sidebar_list
            .replace(&rows, self.current_note_relative_path.as_deref())?;
        if !self.browse_delivery_is_current(generation, intent)
            || !page
                .authority
                .is_current(&self.session, &self.collapsed_directories)
        {
            self.sidebar_list.clear()?;
            return Ok(false);
        }
        self.notes = notes;
        self.sidebar_rows = rows;
        self.sidebar_has_more = page.has_more;
        self.controller
            .set_next_sidebar_page_available(page.has_more);
        if !self.browse_delivery_is_current(generation, intent) {
            // 最后setEnabled仍可能重入，外层不得随后发布pager或自动选旧笔记。
            return Ok(false);
        }
        if let Some(label) = &self.empty_label {
            // 初始Pending结束但本页没有笔记时，不能永远保留“正在读取”的旧提示。
            label.setStringValue(&NSString::from_str(
                self.sidebar_completion_policy
                    .empty_message(self.sidebar_rows.len() == 1 && !page.has_more),
            ));
        }
        if !self.browse_delivery_is_current(generation, intent) {
            // 标签setter同样是重入边界，不清新输入的状态。
            return Ok(false);
        }
        // 保留活动笔记的恢复/保存反馈；最后Native setter之后仍须核本地发布撤权。
        self.update_sidebar_mode_label(false, intent);
        // 模式标题也为Native setter，仍须通过本次原有的最终权限检查。
        Ok(self.browse_delivery_is_current(generation, intent)
            && page
                .authority
                .is_current(&self.session, &self.collapsed_directories))
    }
}

/// 只有主队列信号门禁允许进入；消费时仍逐项核原owner/代次及精确邮箱身份。
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
        // cancel已同步释放载荷，没有重试或重建权限。
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
            // 搜索模式、owner、输入/任务代次或精确邮箱任何一个变化均拒绝。
            // 地址与标量复用不能让旧通知取消或消费新邮箱。
            mailbox.cancel();
            return Ok(());
        }
        let Some(results) = mailbox.take() else {
            // 已取消/消费的邮箱不能重复安装。
            return Ok(());
        };
        match results {
            Ok((pager, page)) => {
                // 末页也验证core authority，本地save可能已在排队期间撤权。
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
                    // Native布局/setter重入后仅丢弃本次局部pager，不能拉回旧页。
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
                    // 初始大库/删除后没有正文时，完成排序后才选真正第一条。
                    first_note = surface
                        .notes
                        .first()
                        .map(|note| (note.relative_path.clone(), authority));
                }
            }
            Err(error) => {
                // 只由仍有效的同一任务显示错误，保留现有正文和用户字段。
                surface.sidebar_scan_pending = false;
                surface.sidebar_results.take();
                return Err(error.to_string());
            }
        }
        Ok(())
    });
    if let Err(error) = delivered {
        // present_error是Native调用，先重新只读确认当前owner/意图，旧错误不覆盖新query。
        let _ = with_active_native_markdown_surface(|surface| {
            if std::ptr::from_ref(&*surface.controller).addr() == owner_identity
                && surface.browse_delivery_is_current(generation, accepted_intent)
            {
                // 租约已结束TLS借用；当前owner才可执行原生错误展示。
                surface.controller.present_error(&error);
            }
            Ok(())
        });
    }
    if let Some((path, authority)) = first_note {
        // 离开surface租约后走既有文档转换，不在Active槽借用中构造TextKit。
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
            // 既有switch在当前surface内报告首因；外层不重复显示可能已过期的错误。
            let _ = switch_native_markdown_note(&path);
        }
    }
}
