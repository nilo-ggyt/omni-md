//! Owns foreground native slots, save/responder preflight and strict editor/window release handoffs.
//! Closing native owners, immediate canonical destruction and actual native-deallocation authority are distinct.

use super::*;
use crate::workspace_ready::{WorkspaceReadySignal, WorkspaceReadySlot};

#[cfg(feature = "native-close-stage-diagnostic")]
thread_local! {
    // 应用退出沿用原诊断；普通返回仅在显式 trace 下加入观察。MD 整个进程最多九条阶段，
    // 角色快照也只输出一次，不因重复返回重置预算或改变原关闭事务。
    static CLOSE_DIAGNOSTIC_ACTIVE: Cell<bool> = const { Cell::new(false) };
    static CLOSE_DIAGNOSTIC_COUNT: Cell<u8> = const { Cell::new(0) };
    static CLOSE_DIAGNOSTIC_ROLES: Cell<bool> = const { Cell::new(false) };
    static CLOSE_DIAGNOSTIC_STARTED: std::time::Instant = std::time::Instant::now();
}

/// 当前mode只作有界分类；不持CF字符串、窗口或业务资源到下一阶段。
#[cfg(feature = "native-close-stage-diagnostic")]
fn diagnostic_close_mode() -> u8 {
    use core_foundation::base::TCFType;
    // SAFETY: 主线程实际CF runloop的Copy规则返回；空值先判断，非空交给唯一create-rule owner释放。
    let raw = unsafe {
        core_foundation::runloop::CFRunLoopCopyCurrentMode(
            core_foundation::runloop::CFRunLoopGetCurrent(),
        )
    };
    if raw.is_null() {
        return 0; // 没有当前mode时不猜测default/modal。
    }
    // SAFETY: raw来自CopyCurrentMode的+1 CFString，恰好包装一次并在本函数内释放。
    let mode: core_foundation::string::CFString = unsafe { TCFType::wrap_under_create_rule(raw) };
    if mode.char_len() > 64 {
        return 4; // 自定义长mode不进入正文或路径日志。
    }
    match mode.to_string().as_str() {
        "kCFRunLoopDefaultMode" => 1,
        "NSModalPanelRunLoopMode" => 2,
        "NSEventTrackingRunLoopMode" => 3,
        _ => 4, // 只记录实际未知mode的分类。
    }
}

/// 仅复制已有状态并输出一次阶段；不改变signals、成功语义、调度或strict nil检查。
#[cfg(feature = "native-close-stage-diagnostic")]
pub(super) fn diagnostic_close_stage(
    stage: &'static str,
    generation: u64,
    facts: std::fmt::Arguments<'_>,
) {
    if !CLOSE_DIAGNOSTIC_ACTIVE.with(Cell::get) {
        return; // 正常输入/换笔记期间的cohort不消费退出观察预算。
    }
    let index = CLOSE_DIAGNOSTIC_COUNT.with(|count| {
        if count.get() == 9 {
            return None; // 固定上限只停止诊断，不制造后续成功或重试。
        }
        let next = count.get() + 1;
        count.set(next);
        Some(next)
    });
    let Some(index) = index else { return };
    let elapsed = CLOSE_DIAGNOSTIC_STARTED.with(|start| start.elapsed().as_micros());
    let mode = diagnostic_close_mode();
    eprintln!(
        "close_stage md index={index} us={elapsed} mode={mode} stage={stage} generation={generation} {facts}"
    );
}

thread_local! {
    pub(super) static ACTIVE_NATIVE_MARKDOWN_SURFACE: RefCell<Option<NativeMarkdownWindowSurface>> = const { RefCell::new(None) };
    pub(super) static CLOSING_NATIVE_MARKDOWN_SURFACE: RefCell<Option<NativeMarkdownWindowSurface>> = const { RefCell::new(None) };
    pub(super) static FINALIZING_NATIVE_MARKDOWN_CLOSE: RefCell<Option<NativeMarkdownCloseCompletion>> = const { RefCell::new(None) };
    pub(super) static NATIVE_MARKDOWN_CLOSE_GENERATION: Cell<u64> = const { Cell::new(0) };
    pub(super) static VERIFYING_NATIVE_MARKDOWN_CLOSE: Cell<Option<(u64, PluginCloseRequest)>> = const { Cell::new(None) };
    static NATIVE_MARKDOWN_CLOSE_CALL_DEPTH: Cell<usize> = const { Cell::new(0) };
    pub(super) static BORROWED_NATIVE_MARKDOWN_OWNER: Cell<Option<usize>> = const { Cell::new(None) };
    // At most one accepted note/file transition exists. A ready completion carries only weak identity
    // and scalar operation metadata while a reentrant callback temporarily observes its owner leased.
    static READY_NATIVE_MARKDOWN_CONTINUATION: RefCell<Option<Box<dyn FnOnce() + Send>>> = const { RefCell::new(None) };
    // 工作区单worker只保一个纯weak信号，与accepted-file continuation完全分离。
    static READY_NATIVE_MARKDOWN_WORKSPACE: RefCell<WorkspaceReadySlot> = RefCell::new(WorkspaceReadySlot::default());
}

/// Run one foreground transaction with the unique surface local, ending all TLS borrows before AppKit.
pub(super) fn with_active_native_markdown_surface<R>(
    operation: impl FnOnce(&mut NativeMarkdownWindowSurface) -> Result<R, String>,
) -> Result<R, String> {
    ACTIVE_NATIVE_MARKDOWN_SURFACE
        .with(|slot| {
            BORROWED_NATIVE_MARKDOWN_OWNER.with(|busy| {
                crate::foreground_owner::with_foreground_owner(
                    slot,
                    busy,
                    |surface| std::ptr::from_ref(&*surface.controller).addr(),
                    operation,
                    enqueue_restored_native_markdown_continuation,
                )
            })
        })
        .map_err(|error| match error {
            // Reentrant native work is rejected with no effects, and the local owner is restored by its lease.
            crate::foreground_owner::ForegroundOwnerError::Busy => {
                "另一笔记原生操作仍在进行中".to_owned()
            }
            // A truly absent surface has no document or native ownership to lend.
            crate::foreground_owner::ForegroundOwnerError::Missing => "笔记窗口已关闭".to_owned(),
        })?
}
/// Preserve accepted handoff work until this exact synchronous foreground lease has been restored.
pub(super) fn defer_native_markdown_continuation_until_restored(
    work: impl FnOnce() + Send + 'static,
) {
    READY_NATIVE_MARKDOWN_CONTINUATION.with_borrow_mut(|pending| {
        // Note/file admission is mutually exclusive. A second accepted completion would violate
        // that invariant rather than authorize overwriting or dropping the first transition.
        assert!(
            pending.is_none(),
            "two accepted Markdown continuations overlapped"
        );
        *pending = Some(Box::new(work));
    });
}

/// Resume from the real owner-restoration event, without polling native deallocation or retrying work.
fn enqueue_restored_native_markdown_continuation() {
    let ready = READY_NATIVE_MARKDOWN_CONTINUATION.with_borrow_mut(Option::take);
    if let Some(ready) = ready {
        // The lease restored its owner/gate and ended the slot borrow before this asynchronous queue call.
        enqueue_native_markdown_main_task(ready);
    }
    // 真正owner恢复且busy已清除后才唤醒结果；此处没有第二轮自排或计时重试。
    enqueue_restored_native_markdown_workspace_ready();
}

/// 取消只取走当前owner的weak信号；载荷仍由既有邮箱cancel同步释放。
pub(super) fn cancel_native_markdown_workspace_ready(owner: usize) {
    let previous = READY_NATIVE_MARKDOWN_WORKSPACE.with_borrow_mut(|slot| slot.cancel_owner(owner));
    drop(previous);
}

/// worker和真实恢复共用同一入口；线程外从不构造/析构MainThreadBound或Native Weak。
pub(super) fn deliver_native_markdown_workspace_signal(signal: WorkspaceReadySignal) {
    if !signal.has_pending() {
        return; // 已消费、取消、poison或销毁的邮箱没有占槽权限。
    }
    if let Some(borrowed) = BORROWED_NATIVE_MARKDOWN_OWNER.get() {
        if borrowed != signal.owner() {
            signal.cancel(); // 外来owner不可等候当前租约，且只取消自己的旧邮箱。
            return;
        }
        let old = READY_NATIVE_MARKDOWN_WORKSPACE.with_borrow(WorkspaceReadySlot::snapshot);
        // Mutex与可能的poison载荷析构都在TLS借用之外；park再核同一弱快照。
        let pending = old.as_ref().is_some_and(WorkspaceReadySignal::has_pending);
        let result = READY_NATIVE_MARKDOWN_WORKSPACE
            .with_borrow_mut(|slot| slot.park(signal, old.as_ref(), pending));
        match result {
            Ok(discarded) => drop(discarded), // 同通知合并或过期弱信号替换，不拥有结果。
            Err(refused) => {
                // 单worker取消顺序保证不会出现两个当前pending；违反时显式失败，绝不覆盖。
                refused.cancel();
                eprintln!("笔记工作区结果交付冲突：已拒绝覆盖仍待处理的结果，请刷新目录或重新搜索");
            }
        }
        return;
    }
    // 没有Native步骤夹在借出检查和实际租约之间；各消费方再次核精确Arc和全部业务权限。
    match signal {
        WorkspaceReadySignal::Directory(delivery) => {
            deliver_native_markdown_directory_result(delivery)
        }
        WorkspaceReadySignal::Search(delivery) => deliver_native_markdown_search_result(delivery),
    }
}

/// 只由ForegroundOwnerLease真正恢复回调调用；先移出TLS槽再检查邮箱或调用dispatch。
fn enqueue_restored_native_markdown_workspace_ready() {
    let signal = READY_NATIVE_MARKDOWN_WORKSPACE.with_borrow_mut(WorkspaceReadySlot::take);
    if let Some(signal) = signal
        && signal.has_pending()
    {
        enqueue_native_markdown_main_task(move || deliver_native_markdown_workspace_signal(signal));
    }
}

/// 最终文稿交接同时等待canonical同步销毁、原映射归还和具体Native图释放。
pub(super) struct NativeMarkdownEditorReleaseBarrier {
    document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    document_released_synchronously: bool,
    document_resources: NativeMarkdownOwnedSnapshot,
    pub(super) borrowed: Option<NativeMarkdownBorrowedEditorSnapshot>,
    native: NativeMarkdownEditorNativeReleaseResources,
}

/// 具体编辑客户端留下的弱资源事实；不拥有或等待外层canonical文稿。
/// 这份收据由最终destroy实际消费，modal-only路径也保留其真实字段与事件证据。
pub(super) struct NativeMarkdownEditorNativeReleaseResources {
    paged_close:
        Option<Result<Rc<super::paged_text_view::NativeMarkdownPagedCloseReceipt>, String>>,
    // 只持固定家族计数，不保正文；原Native copy逃出活动图后也必须等其payload真实析构。
    #[cfg(any(
        test,
        all(
            feature = "native-block-fragment-diagnostic",
            feature = "native-markdown-runtime-probe"
        )
    ))]
    frozen_values: Option<std::sync::Arc<super::frozen_value_family::FrozenValueFamily>>,
    // 只持固定原子计数；框架归还图像 provider 前不能把原生节点销毁等同于像素释放。
    #[cfg(feature = "native-raster-surface-probe")]
    raster_release: Option<super::raster_surface::NativeMarkdownRasterReleaseReceipt>,
    cohort: NativeMarkdownDeallocationCohort,
    owned: NativeMarkdownOwnedSnapshot,
    /// 查找控件属于当前笔记会话，不跟随已释放的上一种排版图结束。
    find_owned: NativeMarkdownOwnedSnapshot,
    find: NativeMarkdownFindResources,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    render: std::rc::Weak<MarkdownStorageRenderPayload>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    text_storage: Weak<PieceBackedMarkdownTextStorage>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    scroll_view: Weak<NSScrollView>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    text_view: Weak<PieceBackedMarkdownTextView>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    gutter: Weak<NativeMarkdownGutter>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    content: Weak<objc2_app_kit::NSTextContentStorage>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    manager: Weak<NSTextLayoutManager>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    container: Weak<NSTextContainer>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    cell: Option<Weak<NSTextField>>,
    session_field_editor: Option<Weak<NativeMarkdownSessionFieldEditor>>,
}

impl NativeMarkdownEditorReleaseBarrier {
    /// 无正文交接仍保留共享客户端事实和自有 field editor 的真实释放事件。
    pub(super) fn for_borrowed_client(
        borrowed: NativeMarkdownBorrowedEditorSnapshot,
        session_field_editor: Option<Weak<NativeMarkdownSessionFieldEditor>>,
        owned: NativeMarkdownOwnedSnapshot,
        mtm: MainThreadMarker,
    ) -> Self {
        // 空工作区的旧输入和modal角色共用既有资源barrier，不等待仍活动的父控件。
        Self::for_non_document_resources(Some(borrowed), session_field_editor, owned, mtm)
    }

    /// 父editor继续存活的文件操作只等待Modal scope，不伪称已销毁正文。
    pub(super) fn for_modal_roles(
        owned: NativeMarkdownOwnedSnapshot,
        mtm: MainThreadMarker,
    ) -> Self {
        Self::for_non_document_resources(None, None, owned, mtm)
    }

    /// 复用既有weak/cohort验收结构，明确表示此资源集合没有canonical document owner。
    fn for_non_document_resources(
        borrowed: Option<NativeMarkdownBorrowedEditorSnapshot>,
        session_field_editor: Option<Weak<NativeMarkdownSessionFieldEditor>>,
        mut owned: NativeMarkdownOwnedSnapshot,
        mtm: MainThreadMarker,
    ) -> Self {
        let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
        owned.seal();
        owned.track_remaining(&mut cohort, mtm);
        if let Some(editor) = session_field_editor.as_ref().and_then(Weak::load) {
            // 临时升级只用于当前登记，收据和排队回调都不保留 editor 强引用。
            cohort.track(&editor, mtm);
        }
        cohort.seal();
        let mut find = NativeMarkdownFindResources::default();
        find.finish_accepted();
        Self {
            document: std::rc::Weak::new(),
            document_released_synchronously: true,
            document_resources: NativeMarkdownOwnedSnapshot::default(),
            borrowed,
            native: NativeMarkdownEditorNativeReleaseResources {
                paged_close: None,
                #[cfg(any(
                    test,
                    all(
                        feature = "native-block-fragment-diagnostic",
                        feature = "native-markdown-runtime-probe"
                    )
                ))]
                frozen_values: None,
                #[cfg(feature = "native-raster-surface-probe")]
                raster_release: None,
                cohort,
                owned,
                find_owned: NativeMarkdownOwnedSnapshot::default(),
                find,
                #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
                render: std::rc::Weak::new(),
                #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
                text_storage: Weak::default(),
                #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
                scroll_view: Weak::default(),
                #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
                text_view: Weak::default(),
                #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
                gutter: Weak::default(),
                #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
                content: Weak::default(),
                #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
                manager: Weak::default(),
                #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
                container: Weak::default(),
                #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
                cell: None,
                session_field_editor,
            },
        }
    }

    /// 实际默认角色事件完成后排队一次验证，具体空壳还需逐字段检查。
    pub(super) fn when_released(&self, work: impl FnOnce() + 'static) {
        self.native.cohort.when_released(work);
    }

    /// 验收只观察同一真实释放端点，不占用已登记的唯一生产完成槽，也不创建第二个关闭过程。
    /// 通知不授予新文稿身份；调用方必须退栈后核原窗口、预声明目标及实际完整帧。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn probe_when_released(&self, wake: impl FnOnce() + 'static) {
        self.native.cohort.probe_when_released(wake);
    }

    /// 默认要求实际销毁的项目根及字段参与事件；具体空壳角色另做字段与公开关系证明。
    pub(super) fn track_surviving_nodes(
        &self,
        cohort: &mut NativeMarkdownDeallocationCohort,
        #[cfg(any(
            test,
            all(
                feature = "native-block-fragment-diagnostic",
                feature = "native-markdown-runtime-probe"
            )
        ))]
        role: super::frozen_value_family::FrozenValueObserverRole,
        mtm: MainThreadMarker,
    ) {
        #[cfg(any(
            test,
            all(
                feature = "native-block-fragment-diagnostic",
                feature = "native-markdown-runtime-probe"
            )
        ))]
        if let Some(family) = &self.native.frozen_values {
            // 完整图与资源边界使用各自原观察槽；不覆盖editor仍在等待的独立角色。
            cohort.track_frozen_values(family, role);
        }
        self.native.owned.track_remaining(cohort, mtm);
        self.native.find_owned.track_remaining(cohort, mtm);
        if let Some(editor) = self
            .native
            .session_field_editor
            .as_ref()
            .and_then(Weak::load)
        {
            // 项目自建 field editor 明确要求实际 nil，不使用 TextKit 空壳例外。
            cohort.track(&editor, mtm);
        }
    }

    /// 只报告销毁栈结束时原canonical强根是否归零；不能单独证明快照alias、mmap或Native已释放。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn canonical_released_synchronously(&self) -> bool {
        self.document_released_synchronously
    }

    /// 返回具体Native cohort的经过时间，完成后固定在最后一个真实释放信号。
    /// 未完成时只是当前耗时，不能代替最终文稿/mmap及Native的严格释放核验。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn native_release_elapsed(&self) -> std::time::Duration {
        self.native.cohort.elapsed()
    }

    /// 最终文稿不能由Native空壳续命；独立检查同一原映射的真实munmap事实。
    pub(super) fn verify_released(&self) -> Result<(), String> {
        if !self.document_released_synchronously || self.document.upgrade().is_some() {
            return Err("关闭后笔记正文仍未释放".into());
        }
        self.document_resources.verify_released()?;
        if self
            .borrowed
            .as_ref()
            .is_some_and(|borrowed| !borrowed.is_detached())
        {
            // 共享客户端由实际公开关系证明已断开，不能用正文Weak-nil替代。
            return Err("关闭后笔记输入控件或查找数据仍有关联".into());
        }
        self.native.verify_released()
    }
}

impl NativeMarkdownEditorNativeReleaseResources {
    /// 捕获者已真实归还旧owner后才封闭资源集合；不能在Drop之前发送完成。
    pub(super) fn finish_owner_drop(&mut self) {
        self.cohort.seal();
    }

    /// 严格保留具体Native、Find、render、冻结值和字段编辑器的原释放要求。
    pub(super) fn verify_released(&self) -> Result<(), String> {
        if let Some(close) = &self.paged_close {
            close.as_ref().map_err(Clone::clone)?.verify_released()?;
        }
        if !self.cohort.has_completed() {
            // 默认根/字段仍依赖实际事件，排队一轮和时间流逝不构成完成证据。
            return Err("笔记原生资源的释放尚未完成".into());
        }
        #[cfg(any(
            test,
            all(
                feature = "native-block-fragment-diagnostic",
                feature = "native-markdown-runtime-probe"
            )
        ))]
        if let Some(family) = &self.frozen_values {
            let state = family.snapshot();
            if !state.payload_value_destructors_complete {
                // 这是源/值payload析构证明；不声称后台Foundation superclass dealloc栈已经返回。
                return Err(format!("关闭后不可变文本值资源仍未释放：{state:?}"));
            }
        }
        self.owned.verify_released()?;
        self.find_owned.verify_released()?;
        #[cfg(feature = "native-raster-surface-probe")]
        if let Some(raster) = &self.raster_release {
            if !raster.is_released() {
                // 实际 pixel/provider/同步租约或失败状态尚未归还，不能用空 Native 图替代这些事实。
                return Err(format!(
                    "笔记视口光栅资源未完整释放：{:?}",
                    raster.snapshot()
                ));
            }
            let snapshot = raster.snapshot();
            eprintln!(
                "native_raster_surface event=release_verified sticky_failure={} actual_paints={} configured_fragments={} snapshot={snapshot:?} formal=false",
                snapshot.sticky_failure, snapshot.actual_paints, snapshot.configured_fragments,
            ); // 释放事实与绘制功能事实分别报告，不能把已清空的失败绘图当作功能通过。
        }
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        if self.render.upgrade().is_some() {
            // 字典/字体 payload 的项目 Rc 本身也须结束，不能只取走 ivar 而保留栈外副本。
            return Err("关闭后笔记渲染数据仍未释放".into());
        }
        if !self.find.is_detached() {
            // Find的活缓存必须返回公开空事实，不能用父根nil代替。
            return Err("关闭后笔记输入控件或查找数据仍有关联".into());
        }
        if self
            .session_field_editor
            .as_ref()
            .is_some_and(|editor| editor.load().is_some())
        {
            return Err("关闭后笔记会话输入控件仍未释放".into());
        }
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        if self.scroll_view.load().is_some()
            || self.cell.as_ref().is_some_and(|cell| cell.load().is_some())
        {
            // 项目 scroll 根和标准 cell field 未实现空壳边界，不能按类型名称豁免。
            return Err("关闭后笔记滚动容器或表格控件仍未释放".into());
        }
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        if self.text_storage.load().is_some_and(|storage| {
            // 该具体 storage 的文档链接、缓存容量和原生字符串必须同时为空。
            !storage.payload_is_empty() || storage.as_super().string().length() != 0
        }) {
            return Err("关闭后的文本存储仍保留笔记数据".into());
        }
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        if let Some(view) = self.text_view.load()
            && !view.payload_is_empty()
        {
            // sealed 标志只是入口权限之一，不能代替其余逐字段检查。
            #[cfg(feature = "native-markdown-runtime-probe")]
            return Err(format!(
                "关闭后的文本视图仍保留笔记数据：{}",
                view.payload_failure_diagnostic()
            ));
            #[cfg(not(feature = "native-markdown-runtime-probe"))]
            return Err("关闭后的文本视图仍保留笔记数据".into());
        }
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        if self
            .gutter
            .load()
            .is_some_and(|gutter| !gutter.payload_is_empty())
        {
            // ruler 必须释放 Vec 容量、属性和 client 关系。
            return Err("关闭后的行号控件仍保留笔记数据".into());
        }
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        if self.content.load().is_some_and(|content| {
            let observer = ProtocolObject::<dyn NSTextStorageObserving>::from_ref(&*content);
            // AppKit 允许有限 TextKit 组合状态存活，但任何旧文本或项目 delegate/manager 都失败。
            observer.textStorage().is_some()
                || content
                    .attributedString()
                    .is_none_or(|text| text.length() != 0)
                || content.delegate().is_some()
                || !content.textLayoutManagers().is_empty()
                || content.primaryTextLayoutManager().is_some()
        }) {
            return Err("笔记内容存储仍保留正文或回调关联".into());
        }
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        if self.manager.load().is_some_and(|manager| {
            // 仅该构造角色的公开空关系可返回收据，不能忽略未知 TextKit 对象。
            manager.textContentManager().is_some()
                || manager.textContainer().is_some()
                || manager.delegate().is_some()
                || manager.layoutQueue().is_some()
                || manager.textViewportLayoutController().delegate().is_some()
                || !manager.textSelections().is_empty()
        }) {
            return Err("笔记排版管理器仍保留项目关联".into());
        }
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        if self.container.load().is_some_and(|container| {
            // container 已由其拥有者解除 view/manager，不能再连回旧业务。
            container
                .textView(MainThreadMarker::new().expect("release proof uses AppKit main thread"))
                .is_some()
                || container.textLayoutManager().is_some()
        }) {
            return Err("笔记文本容器仍保留项目关联".into());
        }
        Ok(())
    }
}

/// 独立 TextKit 机制夹具归还自己的完整图；正式正文只消费 Paged 的真实关闭收据。
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) fn destroy_native_markdown_textkit_editor(
    mut editor: NativeMarkdownTextKitEditorSurface,
) -> NativeMarkdownEditorReleaseBarrier {
    // 临时持同一allocation，保留原Native局部池排空前的弱读；不是第二Doc或新的长期owner。
    let document_owner = Rc::clone(editor.document());
    let document = Rc::downgrade(&document_owner);
    let mut document_resources = std::mem::take(&mut editor.document_resources);
    // 仍借同一Doc取得原文件及实际已发布编辑文件的观察票；票不保映射，借用在Native拆图前结束。
    // 最终集合按真实资源派生，不在不可逆提交后追加可能失败的登记步骤。
    document_resources.capture_document_backings(document_owner.borrow().backing_observers());
    let mut native = capture_native_markdown_textkit_release(&editor);
    // 先完成会话Find清理、具体后端断图，再按字段顺序归还同一个模型；
    // 原cohort builder仍在，任何提前到达的dealloc不能签发半份资源图的完成事件。
    autoreleasepool(|_| drop(editor));
    drop(document_owner); // Native池真实排空后立刻归还原根，再核Doc弱票与原映射。
    native.cohort.seal();
    let document_released_synchronously = document.upgrade().is_none();
    NativeMarkdownEditorReleaseBarrier {
        document,
        document_released_synchronously,
        document_resources,
        borrowed: None,
        native,
    }
}

/// 源码沿真实Body关闭事件归还像素、字体和源读取；Find/scroll仍由原Native cohort独立核验。
pub(super) fn destroy_native_markdown_editor(
    mut editor: NativeMarkdownEditorSurface,
) -> NativeMarkdownEditorReleaseBarrier {
    let document_owner = Rc::clone(editor.model.document());
    let document = Rc::downgrade(&document_owner);
    let mut document_resources = std::mem::take(&mut editor.document_resources);
    document_resources.capture_document_backings(document_owner.borrow().backing_observers());
    let mut native = capture_native_markdown_paged_release(&editor);
    autoreleasepool(|_| drop(editor));
    drop(document_owner);
    native.finish_owner_drop();
    NativeMarkdownEditorReleaseBarrier {
        document_released_synchronously: document.upgrade().is_none(),
        document,
        document_resources,
        borrowed: None,
        native,
    }
}

/// 模式交换仅捕获原Native图；canonical继续归目标模式，不能把它当作已销毁文稿。
pub(super) fn capture_native_markdown_paged_release(
    editor: &NativeMarkdownEditorSurface,
) -> NativeMarkdownEditorNativeReleaseResources {
    let close = editor.begin_close();
    editor.scroll_view.removeFromSuperview();
    let mtm = editor.scroll_view.mtm();
    let bar = editor.find_bar.borrow().clone();
    let captured = NativeMarkdownFindResources::from_fields(
        bar.as_ref().map(|bar| bar.fields()).into_iter().flatten(),
    );
    let mut find = editor
        .find_resources
        .replace(NativeMarkdownFindResources::default());
    if find.merge(captured).is_err() {
        find.mark_failed(); // 源码模式也不能用空字段集合掩盖真实Find资源观察失败。
    }
    find.finish_accepted();
    let session_field_editor = editor.session_field_editor.borrow_mut().take();
    let mut owned = editor.owned_ledger.snapshot();
    owned.seal();
    let ledger = editor.find_ledger.borrow().clone();
    let mut find_owned = ledger
        .as_ref()
        .map_or_else(NativeMarkdownOwnedSnapshot::default, |ledger| {
            ledger.snapshot()
        });
    find_owned.seal(); // 已交接的旧模式没有Find账本；最终owner必须核同一栏的完整身份清单。
    let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
    owned.track_remaining(&mut cohort, mtm);
    find_owned.track_remaining(&mut cohort, mtm);
    if let Some(field) = session_field_editor.as_ref().and_then(Weak::load) {
        cohort.track(&field, mtm);
    }
    if let Ok(close) = &close {
        let claim = cohort.resource_claim();
        close.when_released(move || claim.release()); // 原像素真实完成才释放父义务，不以Drop或等一轮代替。
    }
    NativeMarkdownEditorNativeReleaseResources {
        paged_close: Some(close),
        #[cfg(any(
            test,
            all(
                feature = "native-block-fragment-diagnostic",
                feature = "native-markdown-runtime-probe"
            )
        ))]
        frozen_values: None,
        #[cfg(feature = "native-raster-surface-probe")]
        raster_release: None,
        cohort,
        owned,
        find_owned,
        find,
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        render: std::rc::Weak::new(),
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        text_storage: Weak::default(),
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        scroll_view: Weak::default(),
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        text_view: Weak::default(),
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        gutter: Weak::default(),
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        content: Weak::default(),
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        manager: Weak::default(),
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        container: Weak::default(),
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        cell: None,
        session_field_editor,
    }
}

/// 原会话仍活时捕获实际TextKit与会话控件的弱收据；本函数不消费模型或封闭cohort builder。
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) fn capture_native_markdown_textkit_release(
    editor: &NativeMarkdownTextKitEditorSurface,
) -> NativeMarkdownEditorNativeReleaseResources {
    let backend = &editor.backend;
    let mtm = MainThreadMarker::new().expect("native Markdown destruction uses the main thread");
    // 移除层级本身可重入responder/container回调；先撤输入权限，禁止拆图中追加编辑。
    backend.stop_accepting_input();
    // 后端仍拥有整张TextKit图时结束挂载；局部池先归还移除层级产生的Native临时claim。
    // SAFETY: 后端在主线程持有此scroll；superview查询只借父关系，不保存父对象。
    if unsafe { backend.scroll_view().superview() }.is_some() {
        // 只有真实挂载才对称移除；回调仍只能看到已撤权的旧后端。
        autoreleasepool(|_| backend.scroll_view().removeFromSuperview());
    }
    // 后续只记录Weak或原子收据，最终等待者不能反向成为Native或文稿owner。
    #[cfg(any(
        test,
        all(
            feature = "native-block-fragment-diagnostic",
            feature = "native-markdown-runtime-probe"
        )
    ))]
    let frozen_values = backend.text_storage.frozen_value_family();
    let render = backend
        .text_storage
        .render_payload()
        .map_or_else(std::rc::Weak::new, |render| Rc::downgrade(&render));
    let text_storage = Weak::from_retained(&backend.text_storage);
    let scroll_view = Weak::from_retained(&backend.scroll_view);
    let text_view = Weak::from_retained(&backend.text_view);
    let gutter = Weak::from_retained(&backend.gutter);
    let content = Weak::from_retained(&backend.text_content_storage);
    let manager = Weak::from_retained(&backend.text_layout_manager);
    let container = Weak::from_retained(&backend.text_container);
    let cell = backend
        .text_view
        .existing_table_cell_editor()
        .map(|cell| Weak::from_retained(&cell));
    // accepted release 再观察一次公开 Find 树，随后只留下 weak 收据，不保留字段强引用。
    let capture_failed = editor.capture_find_resources().is_err();
    let mut find = editor
        .find_resources
        .replace(NativeMarkdownFindResources::default());
    find.finish_accepted();
    if capture_failed {
        // 不完整观察不能被空字段集合掩盖，资源释放仍执行但后继收据保持失败。
        find.mark_failed();
    }
    let session_field_editor = editor.session_field_editor.borrow_mut().take();
    #[cfg(feature = "native-raster-surface-probe")]
    let raster_release = backend.raster_release.clone();
    let mut owned = backend.owned_ledger.snapshot();
    owned.seal();
    let ledger = editor.find_ledger.borrow().clone();
    let mut find_owned = ledger
        .as_ref()
        .map_or_else(NativeMarkdownOwnedSnapshot::default, |ledger| {
            ledger.snapshot()
        });
    find_owned.seal(); // 不按可见性筛选字段；仅真实模式交接可移走整个会话账本。
    // 显式角色 ledger 的默认根/字段使用实际销毁事件；TextKit/custom 对象只在其具体空壳证明通过时放行。
    let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
    #[cfg(any(
        test,
        all(
            feature = "native-block-fragment-diagnostic",
            feature = "native-markdown-runtime-probe"
        )
    ))]
    if let Some(family) = &frozen_values {
        cohort.track_frozen_values(
            family,
            super::frozen_value_family::FrozenValueObserverRole::Editor,
        );
    }
    owned.track_remaining(&mut cohort, mtm);
    find_owned.track_remaining(&mut cohort, mtm);
    if let Some(editor) = session_field_editor.as_ref().and_then(Weak::load) {
        // 在项目 owner 已移出后观察真实 editor 生命周期，不强持有它跨越最终 drop。
        cohort.track(&editor, mtm);
    }
    // 捕获结束只移交Weak/原子事实；外层随后真实Drop完整会话并seal，不能靠排队或轮询补签。
    NativeMarkdownEditorNativeReleaseResources {
        paged_close: None,
        #[cfg(any(
            test,
            all(
                feature = "native-block-fragment-diagnostic",
                feature = "native-markdown-runtime-probe"
            )
        ))]
        frozen_values,
        #[cfg(feature = "native-raster-surface-probe")]
        raster_release,
        cohort,
        owned,
        find_owned,
        find,
        render,
        text_storage,
        scroll_view,
        text_view,
        gutter,
        content,
        manager,
        container,
        cell,
        session_field_editor,
    }
}

/// The plugin preserves the SDK close-intent identity throughout its native lifecycle.
pub(super) type NativeMarkdownCloseIntent = omni_plugin_sdk::PluginCloseIntent;

/// Construct one foreground owner synchronously, then publish its mounted view and input visibility.
pub(crate) fn show_native_markdown_window(
    session: MarkdownSessionAccess,
    lifecycle: Rc<dyn PluginHostLifecycle>,
    host: &mut dyn OmniBarHostContext,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "原生 Markdown 窗口必须在 AppKit 主线程创建".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some())
        || FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow(|completion| completion.is_some())
        || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
        || BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some()
    {
        // A successor cannot allocate while either native close or strict weak release is unfinished.
        return Err("原生 Markdown 窗口仍在关闭".to_owned());
    }

    if ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // Static activation never overlays a second document/window graph on an existing foreground owner.
        return Err("原生 Markdown 窗口已经存在".to_owned());
    }

    let surface = NativeMarkdownWindowSurface::new(session, lifecycle, mtm)?;
    let native_view = surface
        .native_view()
        .ok_or_else(|| "原生 Markdown 窗口没有可挂载的内容视图".to_owned())?;
    host.mount_native_view(native_view)
        .map_err(|error| error.to_string())?;
    if let Err(error) = surface.lifecycle.commit_creation() {
        // Failed host publication drops this still-local surface rather than exposing an orphan active slot.
        return Err(error.to_string());
    }
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow_mut(|active_surface| {
        *active_surface = Some(surface);
    });
    // Native visibility/focus may synchronously call delegates; lend the owner without a state borrow.
    with_active_native_markdown_surface(|surface| {
        surface.show();
        // 首次512项仍未完成时，只在Active已发布后启动唯一后台续扫；小库不产生任务。
        if let Err(error) = surface.continue_initial_sidebar_scan(mtm) {
            // Host已commit_creation，后台启动失败只显示可恢复错误，不能把已挂载窗口当创建失败。
            surface.controller.present_error(&error);
        }
        Ok(())
    })
}

/// Return a copied borrowed-view descriptor; the active surface remains its only native ownership root.
pub(crate) fn active_native_markdown_view() -> Option<PluginNativeView> {
    let panel = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| {
        // 只复制原生 owner，面板 getter 不在 TLS 借用中执行。
        surface.as_ref().map(|surface| surface.panel.clone())
    })?;
    let content = panel.contentView()?;
    // SAFETY: 活跃 surface 拥有此主线程 content；descriptor 不转移或延长其所有权。
    unsafe { PluginNativeView::from_raw(std::ptr::from_ref(&*content).cast_mut().cast()) }
}

/// Updates the current native editor mode while preserving source bytes and session ownership.
pub(super) fn set_active_native_markdown_presentation_mode(
    mode: MarkdownPresentationMode,
) -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "Markdown 展示模式必须在 AppKit 主线程切换".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // Display changes cannot reopen callbacks after close has sealed the current editor's input.
        return Err("原生 Markdown 窗口正在关闭".to_owned());
    }
    with_active_native_markdown_surface(|surface| surface.set_presentation_mode(mode))
}

/// Saves and releases the old note before scheduling installation of the requested successor.
pub(super) fn switch_native_markdown_note(relative_path: &str) -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "原生 Markdown 笔记必须在 AppKit 主线程切换".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // A close owns the last save/release transition and cannot be replaced by a sidebar destination.
        return Err("原生 Markdown 窗口正在关闭".to_owned());
    }

    // 在借出Active owner前拒绝modal；不能先save/flush或触及父responder。
    preflight_native_markdown_modal()?;

    let (switch_started, controller) = autoreleasepool(|_| {
        with_active_native_markdown_surface(|surface| {
            let switch_result = surface.begin_note_switch(relative_path);
            if let Err(error) = &switch_result {
                // Rejected switch/open recovery is visible while the retained original draft stays authoritative.
                surface.controller.present_error(error);
            }
            switch_result.map(|started| (started, surface.controller.clone()))
        })
    })?;
    if switch_started {
        // Register only after the pending transition owns its weak evidence and the local editor owner has dropped.
        schedule_native_markdown_note_switch_completion(&controller)?;
    }
    Ok(())
}

/// Arm one origin-scoped continuation against the pending editor's actual release cohort.
pub(super) fn schedule_native_markdown_note_switch_completion(
    controller: &NativeMarkdownController,
) -> Result<(), String> {
    with_active_native_markdown_surface(|surface| {
        let pending = surface
            .pending_note_switch
            .as_ref()
            .ok_or("当前没有等待切换的笔记")?;
        controller.enqueue_owned_after_release(pending.release_barrier.as_ref(), || {
            if let Err(error) = finish_native_markdown_note_switch() {
                // Preserve the failed graph/open contract and its visible status, without another automatic attempt.
                eprintln!("切换笔记失败：{error}");
            }
        });
        Ok(())
    })
}

/// Installs a pending successor only after strict old-document release verification.
pub(super) fn finish_native_markdown_note_switch() -> Result<(), String> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "原生 Markdown 笔记必须在 AppKit 主线程完成切换".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // The close path has consumed foreground authority, so this old queued switch does nothing.
        return Ok(());
    }

    let result = autoreleasepool(|_| {
        with_active_native_markdown_surface(|surface| {
            let switch_result = surface.finish_note_switch(mtm);
            if let Err(error) = &switch_result {
                // Rejected switch/open recovery is visible while the retained original draft stays authoritative.
                surface.controller.present_error(error);
            }
            switch_result
        })
    });
    #[cfg(all(
        feature = "native-markdown-runtime-probe",
        feature = "native-table-row-append"
    ))]
    if result.is_ok() {
        // 只通知已真实安装的后继；验收不得用队列轮次或旧图关闭请求推断handoff完成。
        NativeMarkdownHostedTableProbe::observe_conflict_note_installed();
    }
    result
}

/// 插件返回和窗口关闭均在保存、完整图释放及 Host 屏障完成后回到启动器。
/// 全局唤出键的隐藏仍由 Host 直接传入 DismissAndResume，不经过本窗口入口。
pub(crate) fn request_native_markdown_window_exit() -> Result<(), String> {
    request_native_markdown_close(PluginCloseRequest {
        intent: NativeMarkdownCloseIntent::ExitToLauncher,
        scope: PluginCloseScope::ForegroundTransition,
    })
}

/// Restore scalar call-stack evidence on both ordinary return and Rust unwinding.
struct NativeMarkdownCloseCallGuard<'a>(&'a Cell<usize>);

impl Drop for NativeMarkdownCloseCallGuard<'_> {
    fn drop(&mut self) {
        // Every successful entry owns one claim; no native value or callback is dropped under this scalar update.
        self.0.set(self.0.get() - 1);
    }
}

/// accepted prepare 本地 owner 的 scope 标量；只撤销本栈声明，不拥有原生图。
struct NativeMarkdownAcceptedPreparationGuard;

impl Drop for NativeMarkdownAcceptedPreparationGuard {
    fn drop(&mut self) {
        // 正常返回和 unwind 都恢复 admission gate；同请求只合并，提升请求仍在此阶段拒绝 Busy。
        VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(None));
    }
}

/// Reject a framework-only shell claim while any plugin close request may still own a local panel clone.
pub(crate) fn request_native_markdown_close(request: PluginCloseRequest) -> Result<(), String> {
    NATIVE_MARKDOWN_CLOSE_CALL_DEPTH.with(|depth| {
        let next = depth.get().checked_add(1).ok_or("笔记关闭请求重入过深")?;
        depth.set(next);
        let _guard = NativeMarkdownCloseCallGuard(depth);
        // Cell access holds no state borrow across native callbacks; nested calls own independent balanced claims.
        request_native_markdown_close_inner(request)
    })
}

/// Preflight visible input and persistence, or promote only an already accepted scope.
fn request_native_markdown_close_inner(request: PluginCloseRequest) -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "原生 Markdown 窗口必须在 AppKit 主线程关闭".to_owned())?;
    #[cfg(feature = "native-close-stage-diagnostic")]
    if request.scope == PluginCloseScope::ApplicationTermination
        || (request.intent == omni_plugin_sdk::PluginCloseIntent::ExitToLauncher
            && std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some())
    {
        // 普通返回复用既有 trace 开关，只启用固定预算的阶段与存活角色观察；请求是否获准、
        // 原 scope、保存预检及严格释放门禁仍全部由下方原生产路径决定，不从日志签发完成。
        CLOSE_DIAGNOSTIC_ACTIVE.with(|active| active.set(true));
    }
    let accepted = FINALIZING_NATIVE_MARKDOWN_CLOSE
        .with_borrow(|completion| completion.as_ref().map(|completion| completion.request))
        .or_else(|| {
            VERIFYING_NATIVE_MARKDOWN_CLOSE
                .with(Cell::get)
                .map(|(_, request)| request)
        })
        .or_else(|| {
            CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| {
                // Copy identity before effects; no native owner is lent by close admission.
                surface.as_ref().and_then(|surface| surface.closing_request)
            })
        });
    match crate::close_policy::admit_close(accepted, request).map_err(str::to_owned)? {
        crate::close_policy::CloseAdmission::Coalesced => {
            // An already committed identical request shares its one completion, even during verification.
            return Ok(());
        }
        crate::close_policy::CloseAdmission::Promote => {
            // Promotion never repeats composition, persistence, resignation or the native close selector.
            return promote_native_markdown_close(request);
        }
        crate::close_policy::CloseAdmission::First => {
            // First acceptance still requires every native/data preflight and Host authorization below.
        }
    }
    if BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some() {
        // Unaccepted reentry cannot acquire the temporarily local owner or pretend another close succeeded.
        return Err("另一笔记原生操作仍在进行中".into());
    }
    if ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_none()) {
        // No accepted transaction or live owner exists to supply save/release authority.
        return Err("原生 Markdown 窗口已经关闭".to_owned());
    }
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| {
        // First路径先做纯modal首因/占用检查，accepted coalescing/promotion不改变。
        surface
            .as_ref()
            .ok_or("笔记窗口已关闭")?
            .preflight_modal_boundary()
    })?;
    let save_result = with_active_native_markdown_surface(|surface| {
        surface.commit_marked_text(true)?;
        surface.save_current_document()?;
        // Actual responder refusal vetoes the close before the Host accepts its scope or destination.
        surface.resign_editor_responder()
    });
    if let Err(error) = save_result {
        // A failed save/input/resignation preserves the original usable draft and its ordinary owner.
        present_native_markdown_error(&error);
        return Err(error);
    }
    let lifecycle = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| {
        surface
            .as_ref()
            .expect("the preflight restored its exact owner")
            .lifecycle
            .clone()
    });
    with_active_native_markdown_surface(|surface| {
        // Host admission 前最后一次核对纯提供栈权限；拒绝不改变活动 manager/UndoRedo。
        surface.controller.ivars().undo.preflight()
    })?;
    if let Err(error) = lifecycle.begin_close(request) {
        // Host rejects terminal scope without pending Quit; restore input to the saved foreground owner.
        let message = error.to_string();
        present_native_markdown_error(&message);
        let _ = with_active_native_markdown_surface(|surface| {
            if let Some(editor) = &surface.editor {
                // No close was accepted, so restoring focus does not revive a discarded document.
                surface.panel.makeFirstResponder(Some(editor.input_view()));
            }
            Ok(())
        });
        return Err(message);
    }
    with_active_native_markdown_surface(|surface| {
        // accepted 清理仍可能查询 window delegate；借出身份覆盖整段原生调用且不持 TLS 借用。
        surface.closing_request = Some(request);
        VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(Some((0, request))));
        let _preparing = NativeMarkdownAcceptedPreparationGuard;
        surface.prepare_to_close();
        Ok(())
    })?;
    let Some(surface) = ACTIVE_NATIVE_MARKDOWN_SURFACE.take() else {
        // A reentrant completion already consumed ownership; never create another native close.
        return Ok(());
    };
    let panel = surface.panel.clone();
    CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow_mut(|closing| *closing = Some(surface));
    #[cfg(feature = "native-close-stage-diagnostic")]
    diagnostic_close_stage(
        "close_accepted",
        0,
        format_args!("scope={:?}", request.scope),
    );
    panel.close();
    #[cfg(feature = "native-close-stage-diagnostic")]
    {
        let notified = CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|closing| {
            // 原通知首次即设置此flag并排finalizer；这里只复制标量，不调用native。
            closing
                .as_ref()
                .map(|surface| surface.controller.ivars().close_finalizer_scheduled.get())
        });
        diagnostic_close_stage("close_returned", 0, format_args!("will_close={notified:?}"));
    }
    Ok(())
}

/// Authorize one monotonic scope change while preserving original intent, generation and native effects.
fn promote_native_markdown_close(request: PluginCloseRequest) -> Result<(), String> {
    if VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some() {
        // The completion is local during native verification. Uncommitted reentry rejects Busy with no
        // effects; Host may cancel Quit while the original ordinary transaction continues unchanged.
        return Err("笔记关闭验证仍在进行中".into());
    }
    if let Some(mut completion) = FINALIZING_NATIVE_MARKDOWN_CLOSE.take() {
        let generation = completion.generation;
        VERIFYING_NATIVE_MARKDOWN_CLOSE
            .with(|gate| gate.set(Some((generation, completion.request))));
        // The Host validates its actual pending Quit. No TLS borrow or native graph owner crosses this call.
        let result = completion
            .lifecycle
            .begin_close(request)
            .map_err(|error| error.to_string());
        if result.is_ok() {
            // Only successful Host authorization commits terminal scope; failure preserves the old receipt identity.
            completion.request = request;
        }
        let schedule = result.is_ok() && completion.reserve_terminal_check();
        FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
        VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(None));
        if schedule {
            // A recorded actual resource-release event already occurred. This queues its single
            // terminal verification, not a timer, retry or guessed deallocation boundary.
            enqueue_native_markdown_main_task(move || {
                finish_native_markdown_close_release(generation, CloseBoundary::Resources)
            });
        }
        return result;
    }
    if let Some(mut surface) = CLOSING_NATIVE_MARKDOWN_SURFACE.take() {
        let accepted = surface
            .closing_request
            .expect("a closing surface has an accepted request");
        VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(Some((0, accepted))));
        let result = surface
            .lifecycle
            .begin_close(request)
            .map_err(|error| error.to_string());
        if result.is_ok() {
            // Native windowWillClose/finalization remains the original operation; only its authorized scope changes.
            surface.closing_request = Some(request);
        }
        CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow_mut(|closing| *closing = Some(surface));
        VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(None));
        return result;
    }
    // Admission and effects are synchronous on the main thread; disappearance cannot authorize a guessed promotion.
    Err("已接受的笔记关闭请求不再允许升级".into())
}

/// Actual-release authority remains distinct for ordinary graph completion and terminal payload completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseBoundary {
    FullGraph,
    Resources,
}

/// A successful proof reports its real boundary, never relabelling an OS-held panel as deallocated.
enum NativeMarkdownCloseReceipt {
    ForegroundResourcesReleased,
    ApplicationResourcesReleased(crate::terminal_close::FrameworkWindowLeaseReceipt),
}

/// Holds weak observations and copied scope identity after every project/native owning root was dropped.
pub(super) struct NativeMarkdownCloseCompletion {
    generation: u64,
    request: PluginCloseRequest,
    lifecycle: Rc<dyn PluginHostLifecycle>,
    editors: Vec<NativeMarkdownEditorReleaseBarrier>,
    pub(super) cohort: NativeMarkdownDeallocationCohort,
    pub(super) resources: NativeMarkdownDeallocationCohort,
    terminal_authority: crate::close_policy::TerminalReleaseAuthority,
    close_notified: bool,
    project_owner_released: bool,
    panel: Weak<NativeMarkdownPanel>,
    controller: Weak<NativeMarkdownController>,
    content: Option<Weak<NSView>>,
    pub(super) owned: NativeMarkdownOwnedSnapshot,
    borrowed: NativeMarkdownBorrowedEditorSnapshot,
}

impl NativeMarkdownCloseCompletion {
    /// Coalesce an already recorded actual event into exactly one terminal verification authority.
    fn reserve_terminal_check(&mut self) -> bool {
        // A completed counter is set only by final marker destruction; it also covers promotion
        // before the resource callback itself runs. No elapsed-time or queue-boundary inference exists.
        self.terminal_authority
            .reserve(self.request.scope, self.resources.has_completed())
    }

    /// Require every predecessor/editor and window payload identity to be actually nil.
    fn verify_resources_released(&self) -> Result<(), String> {
        if !self.resources.has_completed() || !self.project_owner_released {
            // A resource signal and synchronous unique-owner destruction are independent requirements.
            return Err("笔记项目资源尚未完全释放".into());
        }
        self.owned.verify_released()?;
        if !self.borrowed.is_detached() {
            // 完整窗口关闭不能遗漏 search/Find/table 共享编辑器与独占历史的公开边界。
            return Err("笔记共享输入控件尚未完成清理".into());
        }
        for editor in &self.editors {
            // Interrupted handoffs retain their original strict canonical/TextKit barrier without exceptions.
            editor.verify_released()?;
        }
        if self.controller.load().is_some()
            || self
                .content
                .as_ref()
                .is_some_and(|content| content.load().is_some())
        {
            // Terminal scope permits only the panel shell; controller/content survival is still a strict failure.
            return Err("项目资源释放后笔记控制器或内容仍未释放".into());
        }
        Ok(())
    }

    /// Produce one scope-specific typed receipt after actual release, with no retained window escaping its pool.
    fn verify_released(&mut self) -> Result<NativeMarkdownCloseReceipt, String> {
        self.verify_resources_released()?;
        match self.request.scope {
            PluginCloseScope::ForegroundTransition => {
                // Ordinary dismissal/back/resume still require the unchanged all-node cohort and exact nil panel.
                if !self.cohort.has_completed() || self.panel.load().is_some() {
                    return Err("笔记前台窗口对象尚未完全释放".into());
                }
                Ok(NativeMarkdownCloseReceipt::ForegroundResourcesReleased)
            }
            PluginCloseScope::ApplicationTermination => {
                // All state getters below copy only ownership-presence flags; native queries occur after their borrows end.
                let project_owner_present = ACTIVE_NATIVE_MARKDOWN_SURFACE
                    .with_borrow(Option::is_some)
                    || CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
                    || BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some()
                    || NATIVE_MARKDOWN_CLOSE_CALL_DEPTH.with(Cell::get) != 0;
                let receipt = autoreleasepool(|_| {
                    let panel = self.panel.load();
                    let mut facts = crate::terminal_close::TerminalWindowFacts {
                        close_notified: self.close_notified,
                        project_owner_present,
                        panel_present: panel.is_some(),
                        visible: false,
                        key: false,
                        content_attached: false,
                        delegate_attached: false,
                        initial_responder_attached: false,
                        payload_responder_attached: false,
                        sheet_attached: false,
                    };
                    if let Some(panel) = &panel {
                        // This is the sole transient panel upgrade. Public getters prove a closed/detached
                        // shell; no retainCount, private ownership mutation or caller-pool operation is used.
                        facts.visible = panel.isVisible();
                        facts.key = panel.isKeyWindow();
                        facts.content_attached = panel.contentView().is_some();
                        facts.delegate_attached = panel.delegate().is_some();
                        facts.initial_responder_attached = panel.initialFirstResponder().is_some();
                        facts.payload_responder_attached =
                            panel.firstResponder().is_some_and(|responder| {
                                // NSWindow may itself be its inert responder; any different responder remains a forbidden payload edge.
                                !std::ptr::eq(
                                    std::ptr::from_ref(&*responder).cast::<()>(),
                                    std::ptr::from_ref(&**panel).cast::<()>(),
                                )
                            });
                        facts.sheet_attached = panel.attachedSheet().is_some();
                    }
                    let receipt = facts.verify().map_err(str::to_owned)?;
                    // Revoke full-graph callback authority before removing only its own association.
                    // Marker removal is cancelled observation, not deallocation; no project callback
                    // or marker metadata remains attached to the AppKit-owned terminal panel shell.
                    self.cohort.cancel_observation(panel.as_deref());
                    Ok::<_, String>(receipt)
                })?;
                // The weak upgrade, all getter temporaries and their local pool ended before receipt use.
                Ok(NativeMarkdownCloseReceipt::ApplicationResourcesReleased(
                    receipt,
                ))
            }
        }
    }
}

/// Drop foreground owners after the real windowWillClose and install independent actual-release authorities.
pub(super) fn finalize_native_markdown_close() {
    #[cfg(feature = "native-close-stage-diagnostic")]
    diagnostic_close_stage("finalizer_enter", 0, format_args!("entered=true"));
    let completion = autoreleasepool(|_| {
        let mut surface = CLOSING_NATIVE_MARKDOWN_SURFACE.take()?;
        let request = surface
            .closing_request
            .expect("closing Markdown preserves its accepted request");
        let close_notified = surface.controller.ivars().close_finalizer_scheduled.get();
        let generation = NATIVE_MARKDOWN_CLOSE_GENERATION.with(|generation| {
            // An operation generation prevents stale callbacks from observing or completing a successor.
            let next = generation.get().wrapping_add(1);
            generation.set(next);
            next
        });
        let mtm = MainThreadMarker::new().expect("native close finalization uses the main thread");
        let mut editors = Vec::new();
        if let Some(pending) = surface.pending_note_switch.take()
            && let Some(barrier) = pending.release_barrier
        {
            // Closing includes an earlier graph whose handoff has not yet finished.
            editors.push(barrier);
        }
        if let Some(pending) = surface.pending_file_mutation.take()
            && let Some(barrier) = pending.release_barrier
        {
            // Filesystem handoff cannot exempt a preceding canonical/native editor from terminal or ordinary release.
            editors.push(barrier);
        }
        if let Some(editor) = surface.editor.take() {
            // Preflight already saved and resigned; teardown synchronously drops canonical/history/mapping ownership.
            editors.push(destroy_native_markdown_editor(editor));
        }
        let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
        let mut resources = NativeMarkdownDeallocationCohort::new(mtm);
        for editor in &editors {
            // Independent keys preserve the ordinary full graph while allowing terminal resource release to complete.
            editor.track_surviving_nodes(
                &mut cohort,
                #[cfg(any(
                    test,
                    all(
                        feature = "native-block-fragment-diagnostic",
                        feature = "native-markdown-runtime-probe"
                    )
                ))]
                super::frozen_value_family::FrozenValueObserverRole::FullGraph,
                mtm,
            );
            editor.track_surviving_nodes(
                &mut resources,
                #[cfg(any(
                    test,
                    all(
                        feature = "native-block-fragment-diagnostic",
                        feature = "native-markdown-runtime-probe"
                    )
                ))]
                super::frozen_value_family::FrozenValueObserverRole::Resources,
                mtm,
            );
        }
        cohort.track(&surface.panel, mtm);
        cohort.track(&surface.controller, mtm);
        resources.track(&surface.controller, mtm);
        let content = surface.panel.contentView().map(|content| {
            // Root content is a payload identity under both scopes, never part of the permitted framework shell.
            cohort.track(&content, mtm);
            resources.track(&content, mtm);
            Weak::from_retained(&content)
        });
        let mut owned = surface.owned_ledger.snapshot();
        owned.seal();
        owned.track_remaining(&mut cohort, mtm);
        owned.track_remaining(&mut resources, mtm);
        let mut completion = NativeMarkdownCloseCompletion {
            generation,
            request,
            lifecycle: surface.lifecycle.clone(),
            editors,
            cohort,
            resources,
            terminal_authority: crate::close_policy::TerminalReleaseAuthority::default(),
            close_notified,
            project_owner_released: false,
            panel: Weak::from_retained(&surface.panel),
            controller: Weak::from_retained(&surface.controller),
            content,
            owned,
            borrowed: surface
                .borrowed_close
                .take()
                .expect("accepted close prepared borrowed editor boundary"),
        };
        surface.release_native_ownership();
        drop(surface);
        // The unique surface/model/search owner has ended synchronously; callbacks retain only scalar/weak evidence.
        completion.project_owner_released = true;
        completion.cohort.seal();
        completion.resources.seal();
        Some(completion)
    });
    let Some(completion) = completion else {
        // Duplicate notifications cannot consume another native owner or register another close authority.
        return;
    };
    #[cfg(feature = "native-close-stage-diagnostic")]
    if CLOSE_DIAGNOSTIC_ACTIVE.with(Cell::get)
        && !CLOSE_DIAGNOSTIC_ROLES.with(|reported| reported.replace(true))
    {
        // 原finalizer本地pool已排空；只做一次完整既有Weak角色统计，观察pool也先结束再输出。
        let (roles, identities, borrowed, sidebar) = autoreleasepool(|_| {
            (
                completion.owned.diagnostic_alive_roles(),
                completion.owned.diagnostic_raster_close_identities(),
                completion.borrowed.failure_diagnostic(),
                completion.owned.diagnostic_sidebar_relationships(),
            )
        }); // 所有Weak升级、类名/长度读取与临时Native claim先在同一观察池内结束。
        let resources = completion.resources.diagnostic_state();
        let full = completion.cohort.diagnostic_state();
        // Rust weak只读强计数，不临时升级正文或渲染owner；这仍是诊断事实，不推进资源门禁。
        let documents_released = completion.editors.iter().all(|editor| {
            editor.document_released_synchronously && editor.document.strong_count() == 0
        });
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        let renders_released = Some(
            completion
                .editors
                .iter()
                .all(|editor| editor.native.render.strong_count() == 0),
        );
        // 退出诊断可独立编译；没有原render观察字段时明确未观察，不能将缺票输出为已释放。
        #[cfg(not(any(test, feature = "native-markdown-runtime-probe")))]
        let renders_released = None::<bool>;
        diagnostic_close_stage(
            "finalizer_pool_drained",
            completion.generation,
            format_args!(
                "owner_released={} documents_released={documents_released} renders_released={renders_released:?} resources={resources:?} full={full:?} roles_complete={} roles={:?} raster_identities={:?} raster_identities_truncated={} borrowed=[{borrowed}] sidebar=[{sidebar}]",
                completion.project_owner_released, roles.0, roles.1, identities.0, identities.1
            ),
        );
    }
    let generation = completion.generation;
    completion.cohort.when_released(move || {
        finish_native_markdown_close_release(generation, CloseBoundary::FullGraph)
    });
    completion
        .resources
        .when_released(move || observe_native_markdown_resource_release(generation));
    FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
}

/// Preserve the real payload-deallocation event while an ordinary close may still await its panel.
fn observe_native_markdown_resource_release(generation: u64) {
    #[cfg(feature = "native-close-stage-diagnostic")]
    diagnostic_close_stage(
        "resources_observed",
        generation,
        format_args!("entered=true"),
    );
    let Some(mut completion) = FINALIZING_NATIVE_MARKDOWN_CLOSE.take() else {
        // A later completed transaction has no resource event left to consume.
        return;
    };
    if completion.generation != generation {
        // An earlier event cannot annotate a later operation's readiness.
        FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
        return;
    }
    let finish = completion.reserve_terminal_check();
    FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
    if finish {
        // This is already the actual-release relay after the final native destruction stack, not an added wait phase.
        finish_native_markdown_close_release(generation, CloseBoundary::Resources);
    }
}

/// Consume exactly one matching actual boundary; a failed proof retains honest Closing state.
fn finish_native_markdown_close_release(generation: u64, boundary: CloseBoundary) {
    let Some(mut completion) = FINALIZING_NATIVE_MARKDOWN_CLOSE.take() else {
        // Late callbacks retain no native owners and cannot emit a second Host receipt.
        return;
    };
    if completion.generation != generation
        || (completion.request.scope == PluginCloseScope::ForegroundTransition
            && boundary != CloseBoundary::FullGraph)
        || (completion.request.scope == PluginCloseScope::ApplicationTermination
            && boundary != CloseBoundary::Resources)
    {
        // Scope promotion invalidates ordinary completion authority without changing original identity or destination.
        FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
        return;
    }
    VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(Some((generation, completion.request))));
    let verified = autoreleasepool(|_| completion.verify_released());
    #[cfg(feature = "native-close-stage-diagnostic")]
    diagnostic_close_stage(
        "verify_result",
        generation,
        format_args!(
            "scope={:?} verified={}",
            completion.request.scope,
            verified.is_ok()
        ),
    );
    match verified {
        Ok(receipt) => {
            // Only the typed proof authorizes its scope. Terminal diagnostics explicitly report a
            // framework lease instead of claiming the surviving panel's native deallocation.
            match receipt {
                NativeMarkdownCloseReceipt::ForegroundResourcesReleased => {
                    native_markdown_trace! { eprintln!("[probe] Markdown foreground resources released after_us={}", completion.cohort.elapsed().as_micros()); }
                }
                NativeMarkdownCloseReceipt::ApplicationResourcesReleased(lease) => {
                    native_markdown_trace! { eprintln!("[probe] Markdown terminal resources released after_us={} framework_panel_pending={}", completion.resources.elapsed().as_micros(), lease.framework_panel_pending); }
                }
            }
            VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(None));
            #[cfg(feature = "native-close-stage-diagnostic")]
            diagnostic_close_stage("plugin_finish", generation, format_args!("verified=true"));
            completion.lifecycle.finish_close(completion.request);
        }
        Err(error) => {
            // A failed resource/shell proof cannot retry, downgrade or announce a successful terminal receipt.
            FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow_mut(|pending| *pending = Some(completion));
            VERIFYING_NATIVE_MARKDOWN_CLOSE.with(|gate| gate.set(None));
            eprintln!("Markdown close release failed: {error}");
        }
    }
}

#[cfg(test)]
mod close_stack_tests {
    use super::NativeMarkdownCloseCallGuard;
    use std::cell::Cell;

    /// Nested close work and rejected/unwinding effects cannot leave a false framework-only ownership claim.
    #[test]
    fn close_call_stack_claims_restore_after_nested_unwind() {
        let depth = Cell::new(1);
        let outer = NativeMarkdownCloseCallGuard(&depth);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            depth.set(depth.get() + 1);
            let _inner = NativeMarkdownCloseCallGuard(&depth);
            assert_eq!(depth.get(), 2);
            panic!("synthetic close effect rejected by unwinding");
        }));
        assert!(unwind.is_err());
        assert_eq!(depth.get(), 1);
        drop(outer);
        assert_eq!(depth.get(), 0);
    }
}
