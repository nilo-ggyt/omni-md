//! 正式分页工作负载与独立原生参考验收；只观察各自真实输入、完整帧和资源释放。

use super::*;
use crate::native_surface::NativeMarkdownTextKitEditorSurface;
use crate::native_surface::destroy_native_markdown_textkit_editor as destroy_native_markdown_editor;
use crate::native_surface::paged_body::{
    NativeMarkdownPagedBody, NativeMarkdownPagedBodyPaintObserver,
};
use crate::native_surface::paged_input::{NativeMarkdownPagedInput, NativeMarkdownPagedInputState};
use crate::native_surface::paged_text_view::NativeMarkdownPagedSceneKey;
use std::time::{Duration, Instant};

mod clipboard;
pub use clipboard::{
    read_native_markdown_clipboard_control, run_native_markdown_clipboard_control,
};
#[cfg(feature = "native-block-fragment-diagnostic")]
mod code_paint;
mod find_highlights;
mod hosted_workload;
mod input_capacity;
mod list_markers;
mod multi_selection;
mod paged_cell_history;
pub(super) mod pending_fault;
mod pending_selection;
pub use paged_cell_history::run_native_markdown_cell_history_oracle;
#[cfg(feature = "native-block-fragment-diagnostic")]
mod regex_streaming;
#[cfg(feature = "native-block-fragment-diagnostic")]
pub use regex_streaming::run_native_markdown_long_regex_control;
#[path = "probe/source_mode_viewport.rs"]
mod mode_viewport;
mod selection_actions;
mod semantic_snapshot;
mod source_session;
mod subscript;
#[cfg(feature = "native-table-row-append")]
mod table_append;
mod terminal;
pub(super) mod toolbar;
pub use hosted_workload::{
    NativeMarkdownHostedFlow, NativeMarkdownHostedStep, NativeMarkdownHostedWorkload,
    write_native_markdown_giant_paragraph_fixture, write_native_markdown_memory_fixture,
};
#[cfg(feature = "native-table-row-append")]
pub(in crate::native_surface) use table_append::wait_for_paged_cell;
#[cfg(feature = "native-table-row-append")]
pub use table_append::{
    NativeMarkdownHostedTableCase, NativeMarkdownHostedTableProbe, NativeMarkdownHostedTableStep,
};
pub(super) use terminal::ProbeTerminalObserver;

/// Host工作负载只保原弱正文、有限动作票和固定marker坐标；真实会话完全由Host拥有。
/// 不创建workspace或窗口，也不保整篇期待String或另一份原文图。
struct NativeMarkdownMemoryWorkload {
    flow: NativeMarkdownHostedFlow,
    path: std::path::PathBuf,
    facts: RefCell<Option<MemoryWorkloadFacts>>,
    cpu_frame: RefCell<Option<Rc<MemoryFrameObservation>>>,
    pending_mode: RefCell<Option<MemoryModeObservation>>,
    pending_find: RefCell<Option<Rc<MemoryFindObservation>>>,
    pending_query: Cell<Option<MemoryQueryObservation>>,
    verified_query: Cell<Option<(bool, u64)>>,
    find_highlights: RefCell<Option<find_highlights::FindHighlightsProbe>>,
    source_session: RefCell<Option<source_session::SourceSession>>,
    // 相对链接的通知/目标FD在Return前归还；原独立inode仅留到423流式核保存，随后关闭。
    relative_link: RefCell<Option<Rc<source_session::RelativeLink>>>,
}

struct MemoryModeObservation {
    request: Rc<mode_viewport::ModeViewportExpectation>,
    epoch: MemoryActionEpoch,
    // 仅原输入已验证时延续相同字面查询；模式的真实 Find+1 不能让未知查询重新取得权限。
    verified_query: Option<bool>,
    field: Option<(std::rc::Weak<NativeMarkdownFindBar>, NSRange)>,
}

struct MemoryFindObservation {
    execution: NativeMarkdownFindExecution,
    input: NativeMarkdownPagedInputState,
    mode: MarkdownPresentationMode,
    frame: Rc<MemoryFrameObservation>,
    epoch: MemoryActionEpoch,
    before: std::ops::Range<usize>,
    expected: std::ops::Range<usize>,
    code: bool,
    #[cfg(feature = "native-block-fragment-diagnostic")]
    code_paint: Option<Rc<code_paint::ExpectedCodePaint>>,
}

#[derive(Clone, Copy)]
/// 一次明确查询输入的原正文选区和预声明输入代次；不持Native对象，不在下一事件重签。
struct MemoryQueryObservation {
    epoch: MemoryActionEpoch,
    before: MarkdownTextSelection,
    code: bool,
}

/// 首Native前复制原动作输入/窗口代次；原请求完成后观察也不能借新输入的权限。
#[derive(Clone, Copy)]
struct MemoryActionEpoch {
    find: u64,
    workspace: u64,
    header: u64,
}

impl MemoryActionEpoch {
    fn capture(surface: &NativeMarkdownWindowSurface) -> Self {
        Self {
            find: surface.controller.find_input_intent(),
            workspace: surface.controller.workspace_query_intent(),
            header: surface.controller.ivars().header_layout_revision.get(),
        }
    }

    fn permits(self, surface: &NativeMarkdownWindowSurface) -> bool {
        self.find == surface.controller.find_input_intent()
            && self.workspace == surface.controller.workspace_query_intent()
            && self.header == surface.controller.ivars().header_layout_revision.get()
    }
}

#[derive(Clone)]
struct MemoryWorkloadFacts {
    owner: usize,
    document: WeakMarkdownDocument,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    input: std::rc::Weak<NativeMarkdownPagedInput>,
    generation: u64,
    length: usize,
    revision: omni_markdown_core::MarkdownDocumentRevision,
    disk: (u64, u64, u64, i64, i64),
    opening_selection: MarkdownTextSelection,
    opening_input: NativeMarkdownPagedInputState,
    opening_epoch: MemoryActionEpoch,
    table_match: std::ops::Range<usize>,
    code_matches: [std::ops::Range<usize>; 3],
}

type MemoryFrameContinuation = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

/// 一次实际动作的完整 CPU 帧终点；只持原弱身份、完整输入和发布 key，不持 Scene、Reader 或像素。
struct MemoryFrameObservation {
    facts: MemoryWorkloadFacts,
    controller: Weak<NativeMarkdownController>,
    epoch: MemoryActionEpoch,
    input: NativeMarkdownPagedInputState,
    mode: MarkdownPresentationMode,
    started: Instant,
    registered: Cell<bool>,
    published: RefCell<Option<(NativeMarkdownPagedSceneKey, usize)>>,
    signal: RefCell<Option<Box<dyn FnOnce()>>>,
    failure: RefCell<Option<String>>,
}

impl MemoryFrameObservation {
    /// 输入必须是调用者已核验的本次后态；这里只固定原 owner，不重新发现窗口或改正文选择。
    /// 只读 Find 与 Hosted 真编辑共用实际发布机制，是否允许 dirty 由各自原动作验证负责；
    /// 不能拿只读 g0 的 dirty 规则拒绝合法 g+1，也不能让后继合法输入成为本动作的新期待。
    fn capture(
        surface: &NativeMarkdownWindowSurface,
        facts: &MemoryWorkloadFacts,
        epoch: MemoryActionEpoch,
        input: NativeMarkdownPagedInputState,
    ) -> Result<Rc<Self>, String> {
        let paged = surface.editor.as_ref().ok_or("完整帧观察没有原分页正文")?;
        if !memory_workload_identity_current(surface, facts)
            || !epoch.permits(surface)
            || paged
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?
                != input
        {
            return Err("完整帧观察不属于原窗口和完整 Input".into());
        }
        // 这里只创建有限观察载荷。弱 Doc/Body/Input 不保留 canonical root、reader 或字体，
        // 原 foreground controller 的 weak 仅用于其既有 owned queue 登记这一次通知。
        Ok(Rc::new(Self {
            facts: facts.clone(),
            controller: Weak::from_retained(&surface.controller),
            epoch,
            input,
            mode: paged.body.presentation_mode(),
            started: Instant::now(),
            registered: Cell::new(false),
            published: RefCell::new(None),
            signal: RefCell::new(None),
            failure: RefCell::new(None),
        }))
    }

    /// 发布检查只读同一 Body/View 的现成元数据；没有当前整帧就等待原生产者，不主动绘图。
    fn current_publication(&self) -> Result<Option<(NativeMarkdownPagedSceneKey, usize)>, String> {
        with_active_native_markdown_surface(|surface| self.current_publication_at(surface))
    }

    /// 同一 Active 租约中的调用不得再次借全局槽；完整 Input 允许原真实编辑后的 dirty 文稿。
    fn current_publication_at(
        &self,
        surface: &NativeMarkdownWindowSurface,
    ) -> Result<Option<(NativeMarkdownPagedSceneKey, usize)>, String> {
        // 原弱 Input 身份排除同文稿重挂另一客户；Find/workspace/header 代次则撤销旧动作。
        // 两者都不是布局权限：下方仍须向当前实际 View 取得 Ready 和原完整发布 key。
        if !memory_workload_identity_current(surface, &self.facts) || !self.epoch.permits(surface) {
            return Err("CPU 帧原文稿或动作代次已撤销".into());
        }
        let paged = surface.editor.as_ref().ok_or("CPU 帧原分页客户已释放")?;
        let actual_mode = paged.body.presentation_mode();
        let actual_input = paged
            .text_view
            .input_state()
            .map_err(|cause| cause.to_string())?;
        if actual_mode != self.mode || actual_input != self.input {
            // 只在原终点拒绝处留下同一次读取的模式和输入元数据；不读正文、不另采样，
            // 也不接纳后继输入替换原票。这样可区分模式、选择、组合或文稿代次的真实变化。
            #[cfg(feature = "native-markdown-runtime-probe")]
            eprintln!(
                "native_memory_frame_replaced expected_mode={:?} actual_mode={:?} expected_input={:?} actual_input={:?}",
                self.mode, actual_mode, self.input, actual_input
            );
            return Err("CPU 帧模式或完整 Input 已被后继动作替换".into());
        }
        let ready = match paged.text_view.capture_ready_scene_key() {
            Ok(key) => key,
            Err(
                crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending,
            ) => return Ok(None),
            Err(cause) => return Err(cause.to_string()),
        };
        // 两种展示模式不会推进 generation；单凭同 g 可误接旧 Source 帧，因此模式与完整
        // Input 一并核验，包括有向选择、marked、多选、fold 和 selection serial。
        if ready.presentation_mode != self.mode
            || ready.request.input != self.input
            || !WeakMarkdownDocument::ptr_eq(&ready.request.document, &self.facts.document)
        {
            return Err("CPU 帧没有原输入与模式的真实当前 Ready".into());
        }
        let Some((key, paints)) = paged.text_view.probe_cpu_published_frame() else {
            return Ok(None);
        };
        // CPU 的所有可见槽完整提交后才发布此 key。排队、局部 tile 或旧 paints 均不授予
        // 采样权限；这里只读原 surface 标量，绝不主动 paint 出一个测试专用画面。
        if paints == 0
            || !key.same(&ready)
            || paged.text_view.cpu_paints() != Some(paints)
            || paged.text_view.cpu_paint_pending() != Some(false)
        {
            return Ok(None); // Ready 不替代已经完整发布的相同 key 像素。
        }
        Ok(Some((key, paints)))
    }

    fn reject(&self, cause: String) {
        let mut failure = self.failure.borrow_mut();
        if failure.is_none() {
            *failure = Some(cause); // 缺帧或取消后的次级观察不能覆盖本动作已保存的原失败。
        }
    }

    fn signal(&self) {
        let signal = self.signal.borrow_mut().take();
        if let Some(signal) = signal {
            signal();
        }
    }

    /// 注册和已完成交付都离开动作的 Active 租约；队列只保本观察 Weak，不使测试延长正文寿命。
    fn observe(self: &Rc<Self>, observer: impl FnOnce() + 'static) -> Result<(), String> {
        if self.registered.replace(true) {
            return Err("原完整帧终点不能重复观察".into());
        }
        *self.signal.borrow_mut() = Some(Box::new(observer));
        // 观察者在原 workload/probe 槽保活，生产 Body 只保 std::Weak。关闭会先取走这些
        // 槽再严格归还 Native 图；本队列不会因一个未完成测试而延长正文生命周期。
        let controller = self
            .controller
            .load()
            .ok_or("完整帧观察原 controller 已释放")?;
        let weak = MainThreadBound::new(Rc::downgrade(self), controller.mtm());
        controller.enqueue_owned(move || {
            let mtm = MainThreadMarker::new().expect("完整帧观察只在原主队列运行");
            let Some(observation) = weak.into_inner(mtm).upgrade() else {
                return;
            };
            let result = (|| {
                if let Some((key, paints)) = observation.current_publication()? {
                    // 原完整帧已在登记之前完成也可以接纳；依旧执行同一次当前 key 认证，
                    // 不要求为了通知额外制造一个没有实际动作来源的新帧。
                    observation.frame_published(&key, paints);
                    return Ok(());
                }
                let body = observation
                    .facts
                    .body
                    .upgrade()
                    .ok_or("完整帧原 Body 已释放")?;
                let erased: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = observation.clone();
                // 唯一实际 Body 已有的一次发布通知承担等待；Busy/关闭是注册失败，不能另建
                // timer 或扫描视图树来寻找一个“看起来完成”的替代终点。
                body.observe_next_cpu_frame(Rc::downgrade(&erased))
                    .map_err(|cause| cause.to_string())
            })();
            if let Err(cause) = result {
                observation.reject(cause);
                observation.signal();
            }
        });
        Ok(())
    }

    fn completed(&self) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| self.completed_at(surface))
    }

    fn completed_at(&self, surface: &NativeMarkdownWindowSurface) -> Result<(), String> {
        if let Some(cause) = self.failure.borrow().as_ref() {
            return Err(cause.clone());
        }
        let published = self
            .published
            .borrow()
            .clone()
            .ok_or("原动作没有真实完整 CPU 帧终点")?;
        let actual = self
            .current_publication_at(surface)?
            .ok_or("原动作 CPU 帧仍在准备")?;
        // 通知只能给出排下一验证事件的权限；验证时还必须仍是当初那份完整发布，
        // 后继滚动/输入生成的合法 frame 不能倒填到原观察记录里。
        if !published.0.same(&actual.0) || published.1 != actual.1 {
            return Err("原完整 CPU 帧被后继发布替换".into());
        }
        Ok(())
    }
}

impl NativeMarkdownPagedBodyPaintObserver for MemoryFrameObservation {
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        let checked = (|| {
            let Some((actual, count)) = self.current_publication()? else {
                return Err("CPU 发布回调没有真实当前完整帧".into());
            };
            if !actual.same(key)
                || count != paints
                || self.started.elapsed() > Duration::from_secs(3)
            {
                return Err("CPU 发布不是原票完整帧或超过原三秒期限".into());
            }
            // 仅保存源/输入弱票与 paints 标量。回调返回不保 Canvas、整幅 bitmap 或
            // 任何页目录，故正常关闭仍由产品自己的严格资源收据证明。
            *self.published.borrow_mut() = Some((key.clone(), paints));
            Ok(())
        })();
        if let Err(cause) = checked {
            self.reject(cause);
        }
        self.signal(); // 归还全部 RefCell/Active 借用后才交外部下一事件。
    }
}

impl MemoryFindObservation {
    fn find_completed(&self) -> Result<(), String> {
        // Complete 是本次生产调用同步成功，Pending 则必须消费其原终态；二者都不能
        // 由栏文案或 Body.pending_find 已清空代替。失败沿原请求保留已提交后果。
        match &self.execution {
            NativeMarkdownFindExecution::Complete => Ok(()),
            NativeMarkdownFindExecution::Pending(request) => match request.probe_completed() {
                Some(true) => Ok(()),
                Some(false) => Err(request.take_failure().map_or_else(
                    || "原 Find 请求已取消或失败".to_owned(),
                    |cause| cause.to_string(),
                )),
                None => Err("原 Find 请求尚未完成可见性认证".into()),
            },
        }
    }

    fn completed(&self) -> Result<(), String> {
        self.find_completed()?;
        self.frame.completed()
    }

    fn completed_at(&self, surface: &NativeMarkdownWindowSurface) -> Result<(), String> {
        self.find_completed()?;
        self.frame.completed_at(surface)
    }

    /// 同一实际 Pending 先结束 Find，再等原输入的完整 CPU 帧；没有新的查询、票或布局执行。
    fn observe_terminal(self: &Rc<Self>, observer: impl FnOnce() + 'static) -> Result<(), String> {
        let continuation: MemoryFrameContinuation = Rc::new(RefCell::new(Some(Box::new(observer))));
        let after = |observation: Rc<Self>, continuation: MemoryFrameContinuation| {
            let observer = continuation.borrow_mut().take();
            if let Some(observer) = observer {
                if let Err(cause) = observation.find_completed() {
                    observation.frame.reject(cause);
                    observer();
                } else if let Err(cause) = observation.frame.observe(observer) {
                    observation.frame.reject(cause);
                    observation.frame.signal();
                }
            }
        };
        let terminal = match &self.execution {
            NativeMarkdownFindExecution::Complete => true,
            NativeMarkdownFindExecution::Pending(request) => {
                // 请求可能持有此通知，而 workload 又持原请求；回调只保观察者 Weak，
                // 避免形成 request→observer→request 环。续体本身只有一次外部事件闭包。
                let weak = Rc::downgrade(self);
                let continuation = continuation.clone();
                request.observe_probe_terminal(move || {
                    if let Some(observation) = weak.upgrade() {
                        after(observation, continuation);
                    }
                })?
            }
        };
        if terminal {
            after(self.clone(), continuation);
        }
        Ok(())
    }
}

impl NativeMarkdownMemoryWorkload {
    /// 固定当前已经打开的真实路径与弱正文身份；不创建窗口、workspace或第二个正文。
    fn capture_open_facts(&self) -> Result<MemoryWorkloadFacts, String> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(&self.path).map_err(|_| "无法读取合成笔记元数据")?;
        let expected_path = self
            .path
            .canonicalize()
            .map_err(|_| "无法解析合成笔记路径")?;
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("工作负载没有打开实际笔记")?;
            let paged = editor;
            let opening_input = paged
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            let document = editor.document().borrow();
            if document.path().canonicalize().ok().as_ref() != Some(&expected_path)
                || !document.original_file_identity_is_unchanged()
            {
                // /var与/private/var别名先canonicalize；其他笔记即使内容相同也不能替代目标。
                return Err("当前实际笔记路径或原始磁盘身份与工作负载不一致".into());
            }
            let table = memory_marker_ranges(&document, "第115行")?;
            let code = memory_marker_ranges(&document, "巨段落可见区域绘制检查")?;
            if table.len() != 1 || code.len() != 3 || document.is_dirty() {
                // code只收前三处供相邻Next证明，不缓存全部命中；不同seed不能换小夹具继续。
                return Err("合成seed缺少预定的唯一115行或重复长代码标记".into());
            }
            Ok(MemoryWorkloadFacts {
                owner: std::ptr::from_ref(&*surface.controller).addr(),
                document: Rc::downgrade(editor.document()),
                body: Rc::downgrade(&paged.body),
                input: paged.body.probe_find_input_weak(),
                generation: document.generation(),
                length: document.utf16_len(),
                revision: document
                    .original_revision()
                    .ok_or("合成笔记没有已验证的初始磁盘版本")?,
                disk: (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.len(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                ),
                opening_selection: document.selection(),
                opening_input,
                opening_epoch: MemoryActionEpoch::capture(surface),
                table_match: table[0].clone(),
                code_matches: [code[0].clone(), code[1].clone(), code[2].clone()],
            })
        })
    }

    /// 首次身份已在原真实帧观察前固定；实际 Ready 与同 key 的完整 CPU 发布缺一不可。
    pub(crate) fn verify_open(&self) -> Result<(), String> {
        let facts = self.facts()?;
        let frame = self
            .cpu_frame
            .borrow()
            .clone()
            .ok_or("首屏没有原完整帧观察")?;
        frame.completed()?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts)
                || !facts.opening_epoch.permits(surface)
                || !surface.editor.as_ref().is_some_and(|editor| {
                    editor.document().borrow().selection() == facts.opening_selection
                })
            {
                return Err("初次显示前原笔记、完整输入或窗口已失效".into());
            }
            let paged = surface.editor.as_ref().ok_or("首屏没有唯一分页正文")?;
            let key = paged
                .text_view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            println!(
                "native_memory_open mounted=true viewport_width={} viewport_height={} actual_cpu_mode={:?} actual_cpu_paints={:?}",
                key.request.viewport.size.width,
                key.request.viewport.size.height,
                key.presentation_mode,
                paged.text_view.cpu_paints(),
            );
            Ok(())
        })?;
        println!(
            "native_memory_fixture bytes={} utf16_units={} core_revision={}",
            facts.disk.2,
            facts.length,
            facts.revision.encode_hex()
        );
        self.verify_unchanged()
    }

    fn facts(&self) -> Result<MemoryWorkloadFacts, String> {
        self.facts
            .borrow()
            .clone()
            .ok_or_else(|| "工作负载原正文身份尚未建立".to_owned())
    }

    /// 原三秒期限已经失败后，只观察最初那一正文的现成进度和发布事实；
    /// 不推进队列、不读取正文，也不消费 Body/View 的失败槽，避免诊断改变首因。
    pub(crate) fn diagnose_frame_deadline(&self, stage: i16) {
        if stage == 573 {
            // 链接已合法切换时旧Body本来就必须消失；先结束场景槽借用，再只读原预声明目标
            // 的安装/完整帧事实，不能沿旧facts的Weak失败推断链接是否成功。
            let relative = self.relative_link.borrow().clone();
            if let Some(relative) = relative {
                relative.diagnose_deadline();
            }
            return;
        }
        #[cfg(feature = "native-block-fragment-diagnostic")]
        if stage == 421 {
            // 先结束原Find槽借用；仅打印已存失败，期限后不能再做一次采样来替换原结果。
            let paint = self
                .pending_find
                .borrow()
                .as_ref()
                .and_then(|request| request.code_paint.clone());
            if let Some(paint) = paint {
                paint.diagnose_deadline();
            }
        }
        let facts = self.facts.borrow().clone();
        let Some(facts) = facts else {
            eprintln!("native_memory_frame_deadline stage={stage} original_facts=false");
            return;
        };
        let Some(body) = facts.body.upgrade() else {
            eprintln!("native_memory_frame_deadline stage={stage} original_body=false");
            return;
        };
        // 工作负载可能已完成真实编辑；这里核原弱身份，不把只读 g0 的规则错用于 g+1。
        // Progress 只含原生产者的标量/计费事实；错误仅输出枚举判别值，不展开外部载荷。
        eprintln!(
            "native_memory_frame_deadline stage={stage} original_body=true original_input={} mode={:?} progress={:?}",
            std::rc::Weak::ptr_eq(&body.probe_find_input_weak(), &facts.input),
            body.presentation_mode(),
            body.progress()
                .map_err(|cause| std::mem::discriminant(&cause)),
        );
        let Ok(view) = body.view() else {
            eprintln!("native_memory_frame_deadline stage={stage} original_view=false");
            return;
        };
        // 先结束所有 workload 槽借用，再调用可能核验 Native 几何的 Ready 入口。
        // 两张 key 仍只是原弱票；既不保留 Scene/Source，也不把旧 CPU 帧当当前成功帧。
        let input = view
            .input_state()
            .map_err(|cause| std::mem::discriminant(&cause));
        // Ready 尚未建立时仍需原 View 的实际滚动坐标，才能区分准备不足与两次真实 reveal 互相覆盖。
        // 这里只捕现成请求，不申请 Scene、移动 clip，或以新请求补签原超时动作。
        match view.capture_scene_request() {
            Ok(request) => eprintln!(
                "native_memory_frame_deadline_request stage={stage} input={:?} viewport={:?} bounds_width={} geometry_epoch={} virtual_viewport={:?}",
                request.input,
                request.viewport,
                request.bounds_width,
                request.geometry_epoch,
                request.virtual_viewport,
            ),
            Err(cause) => eprintln!(
                "native_memory_frame_deadline_request stage={stage} error={:?}",
                std::mem::discriminant(&cause),
            ),
        }
        let ready = view.capture_ready_scene_key();
        let published = view.probe_cpu_published_frame();
        let ready_status = match &ready {
            Ok(_) => "ready",
            Err(
                crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending,
            ) => "pending",
            Err(
                crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Revoked,
            ) => "revoked",
            Err(
                crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Closed,
            ) => "closed",
            Err(crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Busy) => {
                "busy"
            }
            Err(_) => "other_error",
        };
        eprintln!(
            "native_memory_frame_deadline stage={stage} input={input:?} ready_status={ready_status} cpu_published={} cpu_paints={:?} cpu_pending={:?} cpu_steps={:?} ready_cpu_same={}",
            published.is_some(),
            view.cpu_paints(),
            view.cpu_paint_pending(),
            view.cpu_paint_steps(),
            ready.as_ref().is_ok_and(|key| published
                .as_ref()
                .is_some_and(|(actual, _)| key.same(actual))),
        );
        // 仅输出 key 中的有限坐标和完整输入，不输出弱对象地址、路径、查询或 Source 字节。
        for (kind, key, paints) in ready
            .as_ref()
            .ok()
            .map(|key| ("ready", key, None))
            .into_iter()
            .chain(
                published
                    .as_ref()
                    .map(|(key, paints)| ("cpu", key, Some(*paints))),
            )
        {
            eprintln!(
                "native_memory_frame_deadline_key stage={stage} kind={kind} mode={:?} input={:?} viewport={:?} bounds_width={} geometry_epoch={} virtual_viewport={:?} paints={paints:?}",
                key.presentation_mode,
                key.request.input,
                key.request.viewport,
                key.request.bounds_width,
                key.request.geometry_epoch,
                key.request.virtual_viewport,
            );
        }
    }

    fn current(&self, surface: &NativeMarkdownWindowSurface, facts: &MemoryWorkloadFacts) -> bool {
        memory_workload_facts_current(surface, facts)
    }
}

/// 原文件与正文身份的纯检查可供一次终点闭包复用；不让闭包持有整个工作负载owner。
fn memory_workload_identity_current(
    surface: &NativeMarkdownWindowSurface,
    facts: &MemoryWorkloadFacts,
) -> bool {
    if !native_markdown_controller_has_foreground_authority(&surface.controller)
        || std::ptr::from_ref(&*surface.controller).addr() != facts.owner
    {
        return false;
    }
    let Some(editor) = surface.editor.as_ref() else {
        return false;
    };
    let paged = editor;
    if !std::rc::Weak::ptr_eq(&Rc::downgrade(editor.document()), &facts.document)
        || !std::rc::Weak::ptr_eq(&Rc::downgrade(&paged.body), &facts.body)
        || !std::rc::Weak::ptr_eq(&paged.body.probe_find_input_weak(), &facts.input)
    {
        return false;
    }
    true
}

fn memory_workload_facts_current(
    surface: &NativeMarkdownWindowSurface,
    facts: &MemoryWorkloadFacts,
) -> bool {
    if !memory_workload_identity_current(surface, facts) {
        return false;
    }
    let editor = surface.editor.as_ref().expect("原身份已核唯一 editor");
    let document = editor.document().borrow();
    // 每个Native边界只核原弱身份/代次/长度/dirty；不为观察重复整篇hash，关闭后独立核真实字节。
    document.generation() == facts.generation
        && document.utf16_len() == facts.length
        && !document.is_dirty()
}

impl NativeMarkdownMemoryWorkload {
    /// 原工作负载的真实终点入口；动作已执行，这里只安装一次原请求/CPU 发布观察。
    /// 每个阶段的三秒由外部原 driver deadline 约束，不在这里轮询、延时或补发输入。
    pub(crate) fn observe_memory_terminal(
        &self,
        stage: i16,
        observer: impl FnOnce() + 'static,
    ) -> Result<(), String> {
        if matches!(stage, 403 | 411 | 413 | 419 | 426 | 436) {
            let expected = self
                .pending_mode
                .borrow()
                .as_ref()
                .map(|receipt| receipt.request.clone())
                .ok_or("模式终点缺少原动作期望")?;
            return with_active_native_markdown_surface(|surface| {
                expected.observe_preview_terminal(&surface.controller, observer)
            });
        }
        if matches!(stage, 409 | 417) {
            let receipt = self
                .pending_find
                .borrow()
                .clone()
                .ok_or("Find 终点缺少原动作请求")?;
            return receipt.observe_terminal(observer);
        }
        if stage == 421 {
            #[cfg(feature = "native-block-fragment-diagnostic")]
            return self.observe_preview_find_paint(observer);
            #[cfg(not(feature = "native-block-fragment-diagnostic"))]
            {
                let receipt = self
                    .pending_find
                    .borrow()
                    .clone()
                    .ok_or("Find 终点缺少原动作请求")?;
                return receipt.observe_terminal(observer);
            }
        }
        if stage != 401 && !matches!(stage, 427..=435 | 437..=438) {
            return Err("未声明的 Memory 完整帧终点".into());
        }
        let captured = self.facts.borrow().clone();
        let facts = match captured {
            Some(facts) => facts,
            None if stage == 401 => {
                let facts = self.capture_open_facts()?;
                *self.facts.borrow_mut() = Some(facts.clone());
                facts
            }
            None => return Err("Memory 完整帧观察没有原打开身份".into()),
        };
        let frame = with_active_native_markdown_surface(|surface| {
            let paged = surface.editor.as_ref().ok_or("Memory 帧没有唯一分页正文")?;
            let input = if stage == 401 {
                facts.opening_input
            } else {
                paged
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
            };
            let epoch = if stage == 401 {
                facts.opening_epoch
            } else {
                MemoryActionEpoch::capture(surface)
            };
            MemoryFrameObservation::capture(surface, &facts, epoch, input)
        })?;
        let previous = self.cpu_frame.replace(Some(frame.clone()));
        drop(previous);
        frame.observe(observer)
    }

    /// 后继像素动作先消费同一次观察的当前完整帧；失败通知只能唤醒检查，不能冒充通过。
    fn verify_memory_frame(&self) -> Result<(), String> {
        let frame = self
            .cpu_frame
            .borrow()
            .clone()
            .ok_or("Memory 缺少原 CPU 帧观察")?;
        frame.completed()
    }

    /// 正文选择也属于原查询动作：即便正文未改，意外Next仍必须使该输入阶段失败。
    fn query_current(
        &self,
        surface: &NativeMarkdownWindowSurface,
        facts: &MemoryWorkloadFacts,
        query: MemoryQueryObservation,
    ) -> bool {
        self.current(surface, facts)
            && query.epoch.permits(surface)
            && surface
                .editor
                .as_ref()
                .is_some_and(|editor| editor.document().borrow().selection() == query.before)
    }

    /// 每个检查都要求原owner/根/代次和未修改状态；动作返回不代表present或采样峰值已稳定。
    pub(crate) fn verify_unchanged(&self) -> Result<(), String> {
        let facts = self.facts()?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts) {
                return Err("工作负载的原正文或窗口已变化".into());
            }
            Ok(())
        })
    }

    /// 沿实际模式按钮 selector 变更同一 Body；动作前固定完整 Input 与首 scalar 屏幕位置。
    pub(crate) fn change_mode(&self, source: bool) -> Result<(), String> {
        let facts = self.facts()?;
        let (controller, button, receipt) = with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts) {
                return Err("模式工作负载原窗口已失效".into());
            }
            let before = MemoryActionEpoch::capture(surface);
            let editor = surface.editor.as_ref().ok_or("模式入口缺少原文稿")?;
            let target = if source {
                MarkdownPresentationMode::Source
            } else {
                MarkdownPresentationMode::InlinePreview
            };
            let epoch = MemoryActionEpoch {
                find: if editor.presentation_mode() != target {
                    before.find.checked_add(1).ok_or("模式 Find 代次耗尽")?
                } else {
                    before.find
                },
                ..before
            };
            let verified_query = self
                .verified_query
                .get()
                .filter(|(_, intent)| *intent == before.find)
                .map(|(code, _)| code);
            let request = Rc::new(mode_viewport::ModeViewportExpectation::capture(
                surface, source,
            )?);
            let original_bar = editor.find_bar().borrow().clone();
            let field = if let Some(bar) = original_bar.filter(|bar| bar.is_visible()) {
                let fields = bar.fields();
                let input = fields[0]
                    .currentEditor()
                    .ok_or("模式前没有原 Find 字段 editor")?;
                let selection = input.selectedRange();
                if !self.current(surface, &facts) || !before.permits(surface) {
                    return Err("模式捕获原字段时输入已撤销".into());
                }
                Some((Rc::downgrade(&bar), selection))
            } else {
                None
            };
            let button = surface
                .controller
                .ivars()
                .presentation_mode_button
                .get()
                .expect("正式窗口已装配原模式按钮")
                .clone();
            Ok((
                surface.controller.clone(),
                button,
                MemoryModeObservation {
                    request,
                    epoch,
                    verified_query,
                    field,
                },
            ))
        })?;
        // 归还 Active 租约后才交真实 target/action；不能在借出槽中让产品按钮被 Busy 拒绝。
        button.setState(if source {
            objc2_app_kit::NSControlStateValueOn
        } else {
            objc2_app_kit::NSControlStateValueOff
        });
        // SAFETY: 原 controller 与同窗口真实模式按钮均存活，selector 与生产按钮 ABI 相同。
        unsafe {
            let _: () = msg_send![&*controller, changeMarkdownPresentationMode: &*button];
        }
        let previous = self.pending_mode.replace(Some(receipt));
        drop(previous); // 原观察只持弱身份；Native weak 的析构不跨工作负载 RefMut。
        Ok(())
    }

    pub(crate) fn verify_mode(&self, source: bool) -> Result<(), String> {
        let receipt = self
            .pending_mode
            .borrow_mut()
            .take()
            .ok_or("模式工作负载没有原收据")?;
        let facts = self.facts()?;
        with_active_native_markdown_surface(|surface| {
            let mode = if source {
                MarkdownPresentationMode::Source
            } else {
                MarkdownPresentationMode::InlinePreview
            };
            if !self.current(surface, &facts)
                || !receipt.epoch.permits(surface)
                || surface.presentation_mode != mode
            {
                return Err("模式工作负载实际模式或原正文不一致".into());
            }
            receipt.request.verify(surface)?;
            let editor = surface.editor.as_ref().ok_or("模式验证缺少正文")?;
            let paged = editor;
            let current = || self.current(surface, &facts) && receipt.epoch.permits(surface);
            let ruler =
                native_markdown_table_intent_step(&current, || paged.scroll_view.rulersVisible())
                    .map_err(|_| "模式 ruler 检查已撤权")?;
            if ruler != source {
                return Err("实际模式与 ruler 展示不一致".into());
            }
            let bar = editor.find_bar().borrow().clone();
            if receipt.field.is_some() && bar.as_ref().is_none_or(|bar| !bar.is_visible()) {
                return Err("模式交接隐藏或释放了原可见 Find 栏".into());
            }
            if let Some(bar) = bar.filter(|bar| bar.is_visible()) {
                // 同栏真实 field editor 与窗口 responder 必须仍是同一对象，查询不借新字段补签。
                let fields = native_markdown_table_intent_step(&current, || bar.fields())
                    .map_err(|_| "模式字段检查已撤权")?;
                let input =
                    native_markdown_table_intent_step(&current, || fields[0].currentEditor())
                        .map_err(|_| "模式输入焦点已撤权")?
                        .ok_or("模式切换丢失 Find 编辑关系")?;
                let Some((original_bar, field_selection)) = &receipt.field else {
                    return Err("模式后出现了动作前不存在的 Find 字段".into());
                };
                if !std::rc::Weak::ptr_eq(original_bar, &Rc::downgrade(&bar))
                    || input.selectedRange() != *field_selection
                {
                    return Err("模式交接替换了原 Find 栏或字段选择".into());
                }
                if let Some(code) = receipt.verified_query {
                    let value = fields[0].stringValue();
                    let expected = NSString::from_str(if code {
                        "巨段落可见区域绘制检查"
                    } else {
                        "第115行"
                    });
                    if !value.isEqualToString(&expected) || !current() {
                        return Err("模式交接改变了原已验证查询".into());
                    }
                }
                let responder =
                    native_markdown_table_intent_step(&current, || surface.panel.firstResponder())
                        .map_err(|_| "模式窗口焦点已撤权")?
                        .ok_or("模式切换丢失窗口焦点")?;
                if std::ptr::from_ref(&*input).cast::<c_void>()
                    != std::ptr::from_ref(&*responder).cast::<c_void>()
                {
                    return Err("模式切换没有保持真实 Find 字段焦点".into());
                }
            }
            if !current() {
                return Err("模式焦点对象释放后原权限已撤销".into());
            }
            if let Some(code) = receipt.verified_query {
                self.verified_query.set(Some((code, receipt.epoch.find)));
            }
            Ok(())
        })
    }

    /// 与插件Cmd-F使用同一具名处理器；不合成键盘事件、不访问全局Find剪贴板。
    pub(crate) fn show_find(&self) -> Result<(), String> {
        self.verify_unchanged()?;
        dispatch_markdown_find_key_command(MarkdownFindKeyCommand::Show(false))
    }

    pub(crate) fn verify_find_shown(&self) -> Result<(), String> {
        let facts = self.facts()?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts) {
                return Err("Find栏原窗口已失效".into());
            }
            let editor = surface.editor.as_ref().ok_or("Find栏缺少编辑器")?;
            let bar = editor.find_bar().borrow().clone().ok_or("Find栏尚未建立")?;
            if !bar.is_visible() {
                return Err("Find栏没有实际显示".into());
            }
            let fields = bar.fields();
            let input = fields[0].currentEditor();
            if !self.current(surface, &facts) || input.is_none() {
                return Err("Find查询没有真实字段编辑器".into());
            }
            Ok(())
        })
    }

    /// 实际自有field editor执行公开输入协议；不以setValue/正文插入代替查询输入。
    pub(crate) fn enter_query(&self, code: bool) -> Result<(), String> {
        let query = if code {
            "巨段落可见区域绘制检查"
        } else {
            "第115行"
        };
        let facts = self.facts()?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts) {
                return Err("查询输入原窗口已失效".into());
            }
            let editor = surface.editor.as_ref().ok_or("查询输入缺少正文")?;
            // 在currentEditor/selectText等首Native前固定方向完整的正文选择与全部原输入代次。
            let original = MemoryQueryObservation {
                epoch: MemoryActionEpoch::capture(surface),
                before: editor.document().borrow().selection(),
                code,
            };
            if self.pending_query.get().is_some() {
                return Err("前一次查询输入尚未完成验证".into());
            }
            let mut expected = original;
            // 本driver这一次exact insertText声明恰好一次didChange，不是平台IME的一般通知计数。
            // 不在返回后采样代次：零次、多次或其他字段输入都必须失败，不能冒充本次原请求。
            expected.epoch.find = original
                .epoch
                .find
                .checked_add(1)
                .ok_or("查询工作负载输入代次溢出")?;
            self.verified_query.set(None);
            (|| -> Result<(), String> {
                let current = || self.query_current(surface, &facts, original);
                let bar = editor
                    .find_bar()
                    .borrow()
                    .clone()
                    .ok_or("查询输入缺少Find栏")?;
                let fields = bar.fields();
                let actual =
                    native_markdown_table_intent_step(&current, || fields[0].currentEditor())
                        .map_err(|_| "查询字段读取期间原输入已撤权")?;
                let actual = match actual {
                    Some(actual) => actual, // 同客户已有editor时不能重复selectText触发结束编辑。
                    None => {
                        native_markdown_table_intent_step(&current, || {
                            // SAFETY: 仅无实际editor时以公开入口建立当前字段客户关系。
                            unsafe { fields[0].selectText(None) };
                        })
                        .map_err(|_| "查询字段激活期间原输入已撤权")?;
                        native_markdown_table_intent_step(&current, || fields[0].currentEditor())
                            .map_err(|_| "查询编辑器取得期间原输入已撤权")?
                            .ok_or("查询字段未取得实际编辑器")?
                    }
                };
                let field_editor = actual
                    .downcast::<NSTextView>()
                    .map_err(|_| "查询字段的编辑器不是实际NSTextView")?;
                if !current()
                    || field_editor
                        .downcast_ref::<NativeMarkdownSessionFieldEditor>()
                        .is_none()
                {
                    return Err("查询编辑器不属于当前产品会话".into());
                }
                let value = native_markdown_table_intent_step(&current, || field_editor.string())
                    .map_err(|_| "查询文本读取期间原输入已撤权")?;
                let units = native_markdown_table_intent_step(&current, || value.length())
                    .map_err(|_| "查询长度读取期间原输入已撤权")?;
                drop(value); // 仅读长度，既不复制查询正文，也不保一个跨输入的Native字符串。
                if !current() || units > 256 {
                    return Err("查询字段已变更或超过有限工作负载预算".into());
                }
                native_markdown_table_intent_step(&current, || {
                    field_editor.setSelectedRange(NSRange::new(0, units));
                })
                .map_err(|_| "查询选区设置期间原输入已撤权")?;
                let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*field_editor);
                let text =
                    native_markdown_table_intent_step(&current, || NSString::from_str(query))
                        .map_err(|_| "查询输入对象构造期间原输入已撤权")?;
                // SAFETY: 原票仍有效的自有field editor接受一次有限替换；该明确输入合法推进一次Find意图。
                unsafe {
                    input.insertText_replacementRange(&text, NSRange::new(0, units));
                }
                let after = || self.query_current(surface, &facts, expected);
                native_markdown_table_intent_step(&after, || {
                    surface
                        .controller
                        .observe_native_field_editor(&field_editor);
                })
                .map_err(|_| "查询输入通知次数、正文选择或原窗口已变更")?;
                Ok(())
            })()?;
            // 结束所有局部Native claim后再核同一原票，不能让析构重入的Next成为新合法基线。
            if !self.query_current(surface, &facts, expected) {
                return Err("查询输入结束后原正文选择或输入权限已变更".into());
            }
            self.pending_query.set(Some(expected));
            Ok(())
        })
    }

    pub(crate) fn verify_query(&self, code: bool) -> Result<(), String> {
        let query = self.pending_query.take().ok_or("查询检查缺少原输入票据")?;
        if query.code != code {
            return Err("查询检查阶段不属于原输入票据".into());
        }
        let facts = self.facts()?;
        with_active_native_markdown_surface(|surface| {
            let current = || self.query_current(surface, &facts, query);
            if !current() {
                return Err(format!(
                    "查询输入或切焦点已改变正文选择或原权限：before_anchor={} before_head={}",
                    query.before.anchor_utf16, query.before.head_utf16
                ));
            }
            let editor = surface.editor.as_ref().ok_or("查询检查缺少正文")?;
            let bar = editor
                .find_bar()
                .borrow()
                .clone()
                .ok_or("查询检查缺少Find栏")?;
            let fields = bar.fields();
            let expected = native_markdown_table_intent_step(&current, || {
                NSString::from_str(if code {
                    "巨段落可见区域绘制检查"
                } else {
                    "第115行"
                })
            })
            .map_err(|_| "查询检查期待值构造期间已撤权")?;
            let value = native_markdown_table_intent_step(&current, || fields[0].stringValue())
                .map_err(|_| "查询检查读取期间已撤权")?;
            let matches =
                native_markdown_table_intent_step(&current, || value.isEqualToString(&expected))
                    .map_err(|_| "查询检查比较期间已撤权")?;
            drop(value); // 查询临时Native值释放后仍只接受读取前的原输入意图。
            drop(expected);
            if !current() || !matches {
                return Err("真实查询值与预定工作负载不同".into());
            }
            // 只记已经核对的有限查询种类与原代次；Next不能把后继未知输入贴给旧期望。
            self.verified_query.set(Some((code, query.epoch.find)));
            Ok(())
        })
    }

    /// Next使用原生产匹配/粗定位/异步executor；只观察同步返回时仍存在的同一请求。
    pub(crate) fn find_next(&self) -> Result<(), String> {
        let facts = self.facts()?;
        #[cfg(feature = "native-block-fragment-diagnostic")]
        let mut code_paint = None;
        let (epoch, before, expected, code, old_feedback) = with_active_native_markdown_surface(
            |surface| {
                if !self.current(surface, &facts) {
                    return Err("Next 入口原窗口已失效".into());
                }
                let input_epoch = MemoryActionEpoch::capture(surface);
                let epoch = MemoryActionEpoch {
                    find: input_epoch
                        .find
                        .checked_add(1)
                        .ok_or("Next 原输入代次耗尽")?,
                    ..input_epoch
                };
                let (code, intent) = self.verified_query.get().ok_or("Next 没有已验证的查询")?;
                if intent != input_epoch.find {
                    return Err("Next 原查询输入已变化".into());
                }
                let editor = surface.editor.as_ref().ok_or("Next 没有实际正文")?;
                let paged = editor;
                let bar = editor
                    .find_bar()
                    .borrow()
                    .clone()
                    .ok_or("Next 没有原 Find 栏")?;
                let old_feedback = NativeMarkdownFindViewportFeedback::capture(
                    &bar,
                    &surface.controller,
                    input_epoch.find,
                    true,
                )
                .ok_or("Next 前无法捕获原排队反馈")?;
                let input = paged
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?;
                let selection = editor.document().borrow().selection();
                if input.marked.is_some() || input.directed_selection() != Some(selection) {
                    return Err("Next 不能跳过原组字或未知方向选区".into());
                }
                let before = selection.anchor_utf16.min(selection.head_utf16)
                    ..selection.anchor_utf16.max(selection.head_utf16);
                let candidates = if code {
                    facts.code_matches.as_slice()
                } else {
                    std::slice::from_ref(&facts.table_match)
                };
                let expected = memory_expected_next_match(candidates, !code, &before)
                    .ok_or_else(|| format!("Next 旧选择超出有限标记域：preaction={before:?}"))?;
                let current = || self.current(surface, &facts) && input_epoch.permits(surface);
                #[cfg(feature = "native-block-fragment-diagnostic")]
                if code && surface.presentation_mode == MarkdownPresentationMode::InlinePreview {
                    code_paint = Some(Rc::new(code_paint::ExpectedCodePaint::capture(
                        &paged.text_view,
                        &facts,
                        epoch,
                        &expected,
                        &current,
                    )?));
                }
                if !current() || input.primary_range != NSRange::new(before.start, before.len()) {
                    return Err("Next 前完整 Input 与原 canonical 选择不一致".into());
                }
                println!(
                    "native_memory_find_before preaction={before:?} expected={expected:?} code={code}"
                );
                Ok((epoch, before, expected, code, old_feedback))
            },
        )?;
        // 原生产入口返回本次真实 Complete/Pending；不在后继从可变槽发现另一份请求补票。
        let execution = perform_native_markdown_find_action(MarkdownFindAction::Next)?;
        let receipt = with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts) || !epoch.permits(surface) {
                return Err("Next 返回时原正文已失效".into());
            }
            let editor = surface.editor.as_ref().ok_or("Next 返回缺少原正文")?;
            let paged = editor;
            let bar = editor
                .find_bar()
                .borrow()
                .clone()
                .ok_or("Next 返回缺少原栏")?;
            let input = match &execution {
                NativeMarkdownFindExecution::Complete => paged
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?,
                NativeMarkdownFindExecution::Pending(request) => request.input_state(),
            };
            if input.primary_range != NSRange::new(expected.start, expected.len())
                || paged
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    != input
                || old_feedback.current(&surface.controller)
            {
                return Err("Next 没有提交原预期选择或没有撤销旧输入反馈".into());
            }
            let current = || self.current(surface, &facts) && epoch.permits(surface);
            let status =
                native_markdown_table_intent_step(&current, || bar.probe_feedback_status())
                    .map_err(|_| "Next 状态读取时失权")?;
            // 旧反馈的原 clear/run 必须保持新 Find 的完整输入、状态和实际原请求，不能取消新定位。
            old_feedback.clear(&surface.controller);
            old_feedback.run(&surface.controller);
            let after = native_markdown_table_intent_step(&current, || bar.probe_feedback_status())
                .map_err(|_| "旧反馈交付后原 Next 已失权")?;
            let unchanged =
                native_markdown_table_intent_step(&current, || status.isEqualToString(&after))
                    .map_err(|_| "Next 状态比较时失权")?;
            drop((status, after));
            if !unchanged
                || !current()
                || paged
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    != input
                || matches!(&execution, NativeMarkdownFindExecution::Pending(request)
                    if request.probe_completed() == Some(false))
            {
                return Err("旧输入反馈覆盖了原 Next 请求、选择或状态".into());
            }
            self.verified_query.set(Some((code, epoch.find)));
            let frame = MemoryFrameObservation::capture(surface, &facts, epoch, input)?;
            Ok(Rc::new(MemoryFindObservation {
                execution,
                input,
                mode: paged.body.presentation_mode(),
                frame,
                epoch,
                before,
                expected,
                code,
                #[cfg(feature = "native-block-fragment-diagnostic")]
                code_paint,
            }))
        })?;
        let previous = self.pending_find.replace(Some(receipt));
        drop(previous);
        Ok(())
    }

    #[cfg(feature = "native-block-fragment-diagnostic")]
    /// 原 Find 和完整 CPU 帧已终结后才核同一实际代码背景；不另注册没有生产者的第二帧。
    pub(crate) fn observe_preview_find_paint(
        &self,
        observer: impl FnOnce() + 'static,
    ) -> Result<(), String> {
        let receipt = self
            .pending_find
            .borrow()
            .clone()
            .ok_or("背景观察缺少原 Find 请求")?;
        let paint = receipt
            .code_paint
            .clone()
            .ok_or("背景观察缺少原 Preview 代码动作")?;
        let weak = Rc::downgrade(&receipt);
        receipt.observe_terminal(move || {
            if let Some(receipt) = weak.upgrade() {
                paint.after_find_terminal(receipt, Box::new(observer));
            }
        })
    }

    pub(crate) fn verify_find(&self, code: bool, second: bool) -> Result<(), String> {
        let receipt = self
            .pending_find
            .borrow_mut()
            .take()
            .ok_or("Find 工作负载缺少原请求")?;
        receipt.completed()?;
        let facts = self.facts()?;
        if receipt.code != code {
            return Err("Find 检查阶段与原查询不一致".into());
        }
        with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts) && receipt.epoch.permits(surface);
            if !current() {
                return Err("Find 检查原窗口已失效".into());
            }
            let editor = surface.editor.as_ref().ok_or("Find 检查缺少正文")?;
            let paged = editor;
            let bar = editor
                .find_bar()
                .borrow()
                .clone()
                .ok_or("Find 检查没有原栏")?;
            let input = paged
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            let expected = &receipt.expected;
            if input != receipt.input
                || input.primary_range != NSRange::new(expected.start, expected.len())
                || paged.body.presentation_mode() != receipt.mode
            {
                return Err(format!(
                    "Find 原完整 Input 或选择错误：preaction={:?} expected={expected:?} actual={:?}",
                    receipt.before, input.primary_range
                ));
            }
            let key = paged
                .text_view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let proof = paged
                .text_view
                .verify_find_target_visible(&key, NSRange::new(expected.start, expected.len()))
                .map_err(|cause| cause.to_string())?;
            let frame = proof.first_rect();
            let visible = key.request.viewport;
            let feedback =
                native_markdown_table_intent_step(&current, || bar.probe_found_feedback())
                    .map_err(|_| "Find 反馈检查已失权")?;
            if !feedback
                || !current()
                || !paged
                    .text_view
                    .capture_ready_scene_key()
                    .map_err(|cause| cause.to_string())?
                    .same(&key)
                || paged
                    .text_view
                    .probe_cpu_published_frame()
                    .is_none_or(|(actual, paints)| !actual.same(&key) || paints == 0)
                || paged.text_view.cpu_paint_pending() != Some(false)
            {
                return Err("原 Find 成功终点与实际当前字形、完整帧或反馈不一致".into());
            }
            if code && second {
                #[cfg(feature = "native-block-fragment-diagnostic")]
                receipt
                    .code_paint
                    .as_ref()
                    .ok_or("Preview 代码背景没有原动作观察")?
                    .verify(surface, &current)?;
                #[cfg(not(feature = "native-block-fragment-diagnostic"))]
                println!(
                    "native_memory_code_background status=unavailable actual_paint_verified=false"
                );
            }
            println!(
                "native_memory_find verified=true selected_start={} selected_units={} glyph_screen_y={} preview_next={second} actual_cpu_mode={:?}",
                input.primary_range.location,
                input.primary_range.length,
                frame.origin.y - visible.origin.y,
                key.presentation_mode,
            );
            if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
                eprintln!(
                    "native_memory_paged_frame generation={} mode={:?} viewport={:?} actual_paints={:?}",
                    input.generation,
                    key.presentation_mode,
                    visible,
                    paged.text_view.cpu_paints()
                );
            }
            Ok(())
        })
    }

    /// 合成“返回”只进入正式typed退出，不调用NSApplication.terminate或跳过严格释放。
    pub(crate) fn request_return(&self) -> Result<(), String> {
        let facts = self.facts()?;
        let relative = self.relative_link.borrow().clone();
        // 关闭只核原Host窗口与真实输入客户，不拿只读g0阻止已接纳的编辑或历史退出。
        // 相对链接的唯一目标仍交原弱票核验；后来打开的同名Doc不能接管失败清理。
        with_active_native_markdown_surface(|surface| {
            if let Some(relative) = relative.as_ref() {
                relative.require_return_current(surface)
            } else if memory_workload_identity_current(surface, &facts) {
                Ok(())
            } else {
                Err("原Host工作负载会话已经失效，不能关闭后继笔记".into())
            }
        })?;
        if let Some(relative) = relative {
            relative.prepare_return(); // Native注销与FD归还均在工作负载槽借用之外完成。
        }
        let frame = self.cpu_frame.borrow_mut().take();
        let mode = self.pending_mode.borrow_mut().take();
        let find = self.pending_find.borrow_mut().take();
        let highlights = self.find_highlights.borrow_mut().take();
        let source = self.source_session.borrow_mut().take();
        drop((frame, mode, find, highlights, source)); // 原动作票和有限颜色样本先离开借用，再释放其弱观察载荷。
        request_native_markdown_window_exit()
    }

    /// Hosted仅在已知四次字符变化恢复原revision并真正保存后结算旧只读收据，不重新发现新基线。
    fn settle_known_hosted_save(
        &self,
        original: &MemoryWorkloadFacts,
        expected_generation: u64,
        expected_selection: MarkdownTextSelection,
        saved_disk: (u64, u64, u64, i64, i64),
    ) -> Result<(), String> {
        let stored = self.facts()?;
        if stored.owner != original.owner
            || !WeakMarkdownDocument::ptr_eq(&stored.document, &original.document)
            || stored.generation != original.generation
            || stored.length != original.length
            || stored.revision != original.revision
            || stored.disk != original.disk
            || original.generation.checked_add(4) != Some(expected_generation)
        {
            return Err("Hosted恢复保存结算不属于原只读收据或精确四次变化".into());
        }
        with_active_native_markdown_surface(|surface| {
            if !native_markdown_controller_has_foreground_authority(&surface.controller)
                || std::ptr::from_ref(&*surface.controller).addr() != original.owner
            {
                return Err("Hosted恢复保存结算的原窗口已失效".into());
            }
            let editor = surface
                .editor
                .as_ref()
                .ok_or("Hosted恢复保存结算缺少正文")?;
            if !WeakMarkdownDocument::ptr_eq(&original.document, &Rc::downgrade(editor.document()))
            {
                return Err("Hosted恢复保存结算的原弱正文已替换".into());
            }
            let document = editor.document().borrow();
            if document.generation() != expected_generation
                || document.utf16_len() != original.length
                || document.selection() != expected_selection
                || document
                    .current_revision()
                    .map_err(|error| format!("无法读取原文档revision：{error}"))?
                    != original.revision
                || document.original_revision() != Some(original.revision)
                || document.is_dirty()
                || !document.original_file_identity_is_unchanged()
            {
                return Err("Hosted恢复保存结算不符合原全文摘要、代次与完整选择".into());
            }
            Ok(())
        })?;
        let mut facts = self.facts.borrow_mut();
        let facts = facts.as_mut().ok_or("Hosted恢复保存结算的原收据已释放")?;
        facts.generation = expected_generation; // 只接纳原g+4，不能回采任意当前g。
        facts.disk = saved_disk; // 只接纳本次已验证真实保存的metadata，revision与原弱身份保持原值。
        Ok(())
    }

    /// 只核Native完整空owner、原正文死亡与磁盘未变；不推断Host收据、session Drop或启动器状态。
    fn verify_native_closed(&self) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;
        if ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow(Option::is_some)
            || BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some()
            || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
        {
            return Err("工作负载原Native前台、关闭或验证owner仍未释放".into());
        }
        let facts = self.facts()?;
        if facts.document.upgrade().is_some()
            || facts.body.upgrade().is_some()
            || facts.input.upgrade().is_some()
        {
            return Err("工作负载原Doc/Body/Input尚未释放".into());
        }
        let relative = self.relative_link.borrow().clone();
        if let Some(relative) = relative {
            // 相对链接具有预声明的真实未保存编辑，Return会保存它；只读facts仍保持原值。
            // 独立FD和原编辑revision核实际落盘及资源，driver保存的功能首因不受此清理结果改写。
            relative.verify_closed()?;
            return Ok(());
        }
        let metadata = std::fs::metadata(&self.path).map_err(|_| "关闭后无法核对合成文件")?;
        let disk = (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
        );
        if disk != facts.disk {
            return Err("只读工作负载改变了文件元数据".into());
        }
        let saved = MarkdownDocumentBuffer::open(&self.path, 2 * 1_024 * 1_024)
            .map_err(|_| "关闭后无法验证合成文件内容")?;
        if saved
            .current_revision()
            .map_err(|error| format!("无法读取原文档revision：{error}"))?
            != facts.revision
        {
            return Err("只读工作负载改变了合成文件字节".into());
        }
        drop(saved);
        Ok(())
    }
}

fn memory_marker_ranges(
    document: &MarkdownDocumentBuffer,
    query: &str,
) -> Result<Vec<std::ops::Range<usize>>, String> {
    let mut result = Vec::with_capacity(3);
    omni_markdown_core::markdown_find::visit_markdown_literal_matches_cancellable(
        document,
        query,
        0..document.utf16_len(),
        omni_markdown_core::markdown_find::MarkdownLiteralFindOptions {
            case_sensitive: true,
            whole_word: false,
            overlapping: false,
        },
        || true,
        |found| {
            result.push(found.range_utf16);
            Ok(result.len() < 3)
        },
    )
    .map_err(|_| "无法流式核对合成seed标记")?;
    Ok(result)
}

/// 与生产Next相同地排除当前项并从旧选择末端前进；不完整前缀不能证明回绕。
fn memory_expected_next_match(
    candidates: &[std::ops::Range<usize>],
    complete: bool,
    selection: &std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    if let Some(found) = candidates
        .iter()
        .find(|found| *found != selection && found.start >= selection.end)
    {
        return Some(found.clone()); // 升序前缀已覆盖至候选，故不存在尚未记录的更早Next。
    }
    if !complete {
        return None; // 三处之外仍可能有后续匹配，不能猜测已到EOF或回绕第一项。
    }
    candidates
        .iter()
        .find(|found| *found != selection)
        .or_else(|| candidates.iter().find(|found| !found.is_empty()))
        .cloned() // 完整唯一非空匹配仍允许生产合同的原项回绕。
}

/// 独立探针也借出明确的本地 owner 身份，不绕过产品 provider 的窗口/会话校验。
pub(super) struct ProbeEditingOwnerLease(usize);

impl ProbeEditingOwnerLease {
    pub(super) fn new(controller: &NativeMarkdownController) -> Result<Self, String> {
        let identity = std::ptr::from_ref(controller).addr();
        let occupied = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || BORROWED_NATIVE_MARKDOWN_OWNER.with(|owner| owner.get().is_some());
        if occupied {
            // 不能覆盖真实产品 owner 或嵌套借出；探针不创建第二份业务 authority。
            return Err("The native Markdown probe cannot borrow another active owner".into());
        }
        BORROWED_NATIVE_MARKDOWN_OWNER.with(|owner| owner.set(Some(identity)));
        Ok(Self(identity))
    }
}

impl Drop for ProbeEditingOwnerLease {
    fn drop(&mut self) {
        // 仅撤销本栈的 Copy 身份，不触碰原生对象；错误和 unwind 路径同样归还。
        BORROWED_NATIVE_MARKDOWN_OWNER.with(|owner| {
            assert_eq!(owner.get(), Some(self.0), "the probe owner lease changed");
            owner.set(None);
        });
    }
}

/// 两个真实系统值的弱收据；Rc只便于在短Rust借用外执行Native weak操作。
struct ProbeParagraphSample {
    paragraph: Weak<objc2_app_kit::NSTextParagraph>,
    attributed: Weak<NSAttributedString>,
    utf16_length: usize,
}

/// 此状态只证明本次最多两个样本，不声称遍历了全部paragraph或所有历史版本。
struct ProbeParagraphObservations {
    samples: [Option<Rc<ProbeParagraphSample>>; 2],
    visited: usize,
    incomplete: bool,
}

impl ProbeParagraphObservations {
    fn not_sampled() -> Self {
        Self {
            samples: [None, None],
            visited: 0,
            incomplete: true,
        }
    }

    /// 只在真实释放后的独立事件观察一次；存活即失败，不按类名/空值/延时豁免。
    fn verify_released(&self) -> Result<(), String> {
        let count = self.samples.iter().flatten().count();
        println!(
            "native_paragraph_samples count={count} visited={} incomplete={}",
            self.visited, self.incomplete
        );
        let mut survived = false;
        for (index, sample) in self.samples.iter().flatten().enumerate() {
            let (paragraph_alive, attributed_alive, current_units) = autoreleasepool(|_| {
                let paragraph = sample.paragraph.load();
                let attributed = sample.attributed.load();
                let values = (
                    paragraph.is_some(),
                    attributed.is_some(),
                    attributed.as_ref().map_or(0, |value| value.length()),
                );
                drop(attributed);
                drop(paragraph); // 临时+1全部在本地pool结束，不能变成下一样本的存活原因。
                values
            });
            println!(
                "native_paragraph_sample index={index} sampled_units={} paragraph_alive={paragraph_alive} attributed_alive={attributed_alive} attributed_current_units={current_units}",
                sample.utf16_length
            );
            survived |= paragraph_alive || attributed_alive;
        }
        if self.incomplete {
            return Err("系统paragraph弱观察不完整，不能确认本次抽样释放".into());
        }
        if count == 0 {
            return Err("系统paragraph弱观察没有取得真实非空样本".into());
        }
        if survived {
            return Err("系统paragraph或其原始attributedString在实际关闭后仍存活".into());
        }
        Ok(())
    }
}

/// 只读已经完成的可见布局；空options不要求ensure，不创建paragraph或替代delegate。
fn capture_probe_paragraph_observations(
    surface: &NativeMarkdownTextKitEditorSurface,
) -> ProbeParagraphObservations {
    const MAX_VISITED: usize = 32;
    let _payload = PayloadUseGuard::enter(&surface.backend.text_view.ivars().payload_use_depth);
    autoreleasepool(|_| {
        let Some((visible, origin)) = surface.backend.text_view.bounded_paint_viewport() else {
            return ProbeParagraphObservations::not_sampled();
        };
        let manager = &surface.backend.text_layout_manager;
        let content = &surface.backend.text_content_storage;
        let controller = manager.textViewportLayoutController();
        let Some(viewport) = controller.viewportRange() else {
            return ProbeParagraphObservations::not_sampled();
        };
        let start = viewport.location();
        let samples: RefCell<[Option<Rc<ProbeParagraphSample>>; 2]> = RefCell::new([None, None]);
        let visited = Cell::new(0usize);
        let incomplete = Cell::new(false);
        let block =
            block2::RcBlock::new(|pointer: NonNull<objc2_app_kit::NSTextLayoutFragment>| {
                visited.set(visited.get() + 1); // 上个callback达到32时已停止，不检查第33个fragment。
                let proceed = || -> objc2::runtime::Bool {
                    (visited.get() < MAX_VISITED && samples.borrow().iter().flatten().count() < 2)
                        .into()
                };
                // SAFETY: 空options的同步枚举只在callback期间借出该活fragment，不向状态保存强claim。
                let fragment = unsafe { pointer.as_ref() };
                if fragment.state() != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable {
                    incomplete.set(true);
                    return objc2::runtime::Bool::NO;
                }
                let frame = fragment.layoutFragmentFrame();
                if ![
                    frame.origin.x,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                    frame.origin.x + frame.size.width,
                    frame.origin.y + frame.size.height,
                ]
                .iter()
                .all(|value| value.is_finite())
                    || frame.size.width < 0.0
                    || frame.size.height < 0.0
                {
                    incomplete.set(true);
                    return objc2::runtime::Bool::NO;
                }
                if frame.origin.y + origin.y >= visible.origin.y + visible.size.height {
                    return objc2::runtime::Bool::NO; // 实际可见区结束；不是宣称完整文档已枚举。
                }
                let displayed = NSRect::new(
                    NSPoint::new(frame.origin.x + origin.x, frame.origin.y + origin.y),
                    frame.size,
                );
                if ![
                    displayed.origin.x,
                    displayed.origin.y,
                    displayed.origin.x + displayed.size.width,
                    displayed.origin.y + displayed.size.height,
                ]
                .iter()
                .all(|value| value.is_finite())
                {
                    incomplete.set(true);
                    return objc2::runtime::Bool::NO; // 有限分量相加也可能溢出，不能用非法坐标证明可见。
                }
                if !NSIntersectsRect(displayed, visible) {
                    return proceed();
                }
                let Some(element) = fragment.textElement() else {
                    incomplete.set(true);
                    return objc2::runtime::Bool::NO;
                };
                let Ok(paragraph) = element.downcast::<objc2_app_kit::NSTextParagraph>() else {
                    return proceed(); // 非paragraph结构不伪造成新的样本。
                };
                let previous = samples.borrow().clone(); // 只clone Rc，借用内不调用objc_copyWeak/load/drop。
                for previous in previous.into_iter().flatten() {
                    if previous
                        .paragraph
                        .load()
                        .is_some_and(|value| std::ptr::eq(&*value, &*paragraph))
                    {
                        return proceed(); // 一个paragraph可有多个fragment，只登记同一真实身份一次。
                    }
                }
                // SAFETY: 公开readonly attributedString返回对象，Option接收异常nil而不改变+0返回ABI。
                let attributed: Option<Retained<NSAttributedString>> =
                    unsafe { msg_send![&*paragraph, attributedString] };
                let Some(attributed) = attributed else {
                    incomplete.set(true);
                    return objc2::runtime::Bool::NO;
                };
                let units = attributed.length();
                if units == 0 {
                    return proceed();
                } // 空系统paragraph没有正文样本，继续有限查找。
                let Some(range) = paragraph.elementRange() else {
                    incomplete.set(true);
                    return objc2::runtime::Bool::NO;
                };
                let actual_units =
                    content.offsetFromLocation_toLocation(&range.location(), &range.endLocation());
                if usize::try_from(actual_units) != Ok(units)
                    || fragment.state() != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable
                {
                    incomplete.set(true);
                    return objc2::runtime::Bool::NO; // 长度或布局变化使本次强readonly来源证据不完整。
                }
                let index = samples.borrow().iter().flatten().count();
                if index == 2 {
                    return objc2::runtime::Bool::NO; // 样本已满便主动结束，不追加或替换先前真实身份。
                }
                let candidate = Rc::new(ProbeParagraphSample {
                    paragraph: Weak::from_retained(&paragraph),
                    attributed: Weak::from_retained(&attributed),
                    utf16_length: units,
                });
                samples.borrow_mut()[index] = Some(candidate); // 只移动本地已建弱收据，不跨借用执行Native。
                proceed()
            });
        manager.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
            Some(&start),
            objc2_app_kit::NSTextLayoutFragmentEnumerationOptions::empty(),
            &block,
        );
        drop(block);
        let samples = samples.into_inner();
        if visited.get() == MAX_VISITED && samples.iter().all(Option::is_none) {
            incomplete.set(true); // 预算用完仍无样本时，不能把未覆盖的后续片段当成已确认不存在。
        }
        ProbeParagraphObservations {
            incomplete: incomplete.get(),
            samples,
            visited: visited.get(),
        }
    }) // manager/paragraph/attr/element/range临时claim及本地pool均先结束，返回值只有Weak和标量。
}

/// Weak-only release evidence consumed after actual tracked native deallocation, without polling.
///
/// The token owns no native graph, document, map, worker, or timer. Verification runs once on the
/// first queued callback after the final actual deallocation stack, matching product handoff/close.
/// 具体 TextKit 空壳使用产品的逐字段收据；默认项目根及控制器仍实际 nil，不轮询。
#[must_use = "native input success requires actual-deallocation completion and strict verification"]
pub struct NativeMarkdownRuntimeProbeCompletion {
    cohort: NativeMarkdownDeallocationCohort,
    owned: NativeMarkdownOwnedSnapshot,
    panel: Weak<NativeMarkdownPanel>,
    container_view: Weak<NSView>,
    release_barrier: NativeMarkdownEditorReleaseBarrier,
    document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    scroll_view: Weak<NSScrollView>,
    gutter: Weak<NativeMarkdownGutter>,
    text_view: Weak<PieceBackedMarkdownTextView>,
    text_storage: Weak<PieceBackedMarkdownTextStorage>,
    text_content_storage: Weak<objc2_app_kit::NSTextContentStorage>,
    text_layout_manager: Weak<NSTextLayoutManager>,
    text_container: Weak<NSTextContainer>,
    table_cell_editor: Option<Weak<NSTextField>>,
    paragraph_observations: ProbeParagraphObservations,
}

impl NativeMarkdownRuntimeProbeCompletion {
    /// Queue one strict verification event only after every tracked native role actually deallocates.
    pub fn when_native_objects_released(&self, work: impl FnOnce() + Send + 'static) {
        self.cohort.when_released(work);
    }

    /// Observe bounded role booleans once when the standalone driver's missing-signal deadline fails.
    pub fn failure_diagnostic(&self, _mtm: MainThreadMarker) -> String {
        // Every temporary native upgrade immediately becomes a boolean. This diagnostic cannot
        // accept a survivor, schedule another check or convert missing completion into success.
        let owned = self.owned.verify_released().err();
        // 只在原失败出口检查同一懒建字段；全部临时Native claim在本地池归还，
        // 只带出公开关系和长度，不读取草稿正文，也不改变仍存活控件的任何关系。
        let cell = autoreleasepool(|_| {
            self.table_cell_editor
                .as_ref()
                .and_then(Weak::load)
                .map(|field| {
                    // SAFETY: 失败观察仍在AppKit主线程，局部Retained保护同一活字段；
                    // superview仅查询当前公开层级，不沿已释放的旧父图保存或发送后继消息。
                    let parent = unsafe { field.superview() }.is_some();
                    format!(
                        "hidden={} parent={} window={} current_editor={} delegate={} target={} units={}",
                        field.isHidden(),
                        parent,
                        field.window().is_some(),
                        field.currentEditor().is_some(),
                        field.delegate().is_some(),
                        field.target().is_some(),
                        field.stringValue().length(),
                    )
                })
        });
        format!(
            "cohort=[{}] canonical_released_synchronously={} panel={} root={} document={} scroll={} gutter={} storage={} view={} content={} manager={} container={} cell={} owned={owned:?} cell_relations={cell:?}",
            self.cohort.diagnostic(),
            self.release_barrier.canonical_released_synchronously(),
            self.panel.load().is_some(),
            self.container_view.load().is_some(),
            self.document.upgrade().is_some(),
            self.scroll_view.load().is_some(),
            self.gutter.load().is_some(),
            self.text_storage.load().is_some(),
            self.text_view.load().is_some(),
            self.text_content_storage.load().is_some(),
            self.text_layout_manager.load().is_some(),
            self.text_container.load().is_some(),
            self.table_cell_editor
                .as_ref()
                .is_some_and(|cell| cell.load().is_some()),
        )
    }

    /// Require every old weak node/resource to be released before any successor may become accepted.
    pub fn verify_released(self, _mtm: MainThreadMarker) -> Result<(), String> {
        if !self.cohort.has_completed() {
            // Calling the old guessed next-queue observation cannot bypass actual native deallocation authority.
            return Err("the native Markdown probe deallocation cohort has not completed".into());
        }
        let paragraph_result = self.paragraph_observations.verify_released();
        // Distinguish actual native completion from synchronous canonical/history/mapping destruction.
        println!(
            "markdown_canonical_released_synchronously={}",
            self.release_barrier.canonical_released_synchronously()
        );
        println!(
            "markdown_project_native_release_after_us={}",
            self.cohort.elapsed().as_micros()
        );
        println!(
            "markdown_editor_resource_release_after_us={}",
            self.release_barrier.native_release_elapsed().as_micros()
        );
        if self.panel.load().is_some() || self.container_view.load().is_some() {
            // A closed fixture window/content owner is part of the graph, not an allowed harness survivor.
            return Err(
                "the native Markdown probe window/content survived actual completion".into(),
            );
        }
        // The product barrier checks canonical document/storage and scroll ownership first.
        if let Err(error) = self.release_barrier.verify_released() {
            // Keep the strict product failure while exposing every independent weak observation;
            // only booleans escape these temporary loads, with no addresses or document contents.
            return Err(format!(
                "{error}; graph: document={} scroll={} storage={} view={} content={} manager={} container={} cell={}",
                self.document.upgrade().is_some(),
                self.scroll_view.load().is_some(),
                self.text_storage.load().is_some(),
                self.text_view.load().is_some(),
                self.text_content_storage.load().is_some(),
                self.text_layout_manager.load().is_some(),
                self.text_container.load().is_some(),
                self.table_cell_editor.as_ref().is_some_and(|weak| {
                    // A cell absent before first activation cannot be a leaked native editor.
                    weak.load().is_some()
                })
            ));
        }
        if self.document.upgrade().is_some() {
            // A surviving canonical document implies its history/mapping owner escaped surface teardown.
            return Err(
                "the Rust Markdown document survived the native release transaction".to_owned(),
            );
        }
        if self.scroll_view.load().is_some() {
            // An old mounted scroll graph cannot coexist with a successor document surface.
            return Err(
                "the native Markdown scroll view survived the native release transaction"
                    .to_owned(),
            );
        }
        // 具体 TextKit/custom 角色已由产品 barrier 检查所有 payload 与公开空关系。
        // 原始 weak 存活标量仍保留于 failure_diagnostic，不能当作项目数据仍存活的替代证据。
        self.owned.verify_released()?;
        if self.table_cell_editor.is_some_and(|weak| {
            // Lazy allocation is optional, but any actually-created editor must be fully released.
            weak.load().is_some()
        }) {
            // Hidden reusable controls still count as live ownership and cannot satisfy teardown.
            return Err(
                "the reusable table cell editor survived the native release transaction".to_owned(),
            );
        }
        paragraph_result
    }
}

/// Establish that constructing only the real storage/model has no persistent owner before TextKit mounts it.
fn verify_unmounted_native_markdown_model_release(
    path: &Path,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    let document = MarkdownDocumentBuffer::open(path, 2 * 1_024 * 1_024)
        .map_err(|error| format!("failed to open storage countertest fixture: {error}"))?;
    let (cohort, document_weak, storage_weak) = autoreleasepool(|_| {
        let model = NativeMarkdownTextModel::new(document);
        let document_weak = Rc::downgrade(&model.document);
        // 单独申请旧后端适配器，模型本身不会隐式建立TextKit图或第二canonical根。
        let storage = PieceBackedMarkdownTextStorage::new(Rc::clone(model.document()), mtm);
        let storage_weak = Weak::from_retained(&storage);
        let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
        cohort.track(&storage, mtm);
        cohort.seal();
        // 先归还明确的Native owner，再归还文稿根；没有view/manager/事件可替它们续命。
        drop(storage);
        drop(model);
        (cohort, document_weak, storage_weak)
    });
    if document_weak.upgrade().is_some() || storage_weak.load().is_some() || !cohort.has_completed()
    {
        // Construction alone must not create a persistent native/storage or canonical ownership cycle.
        return Err(format!(
            "unmounted Markdown model survived: {}",
            cohort.diagnostic()
        ));
    }
    println!("native_unmounted_model_release=passed canonical_and_storage");
    Ok(())
}

/// Owns the mounted synthetic fixture across actual native event transactions.
///
/// Opening, exercising and closing in separate callbacks lets AppKit service initialization and
/// input work as it does for a user session. The fixture retains no production vault or user text.
#[must_use = "the mounted probe must be exercised and then closed before release acceptance"]
pub struct NativeMarkdownRuntimeProbeSession {
    // 测试图真实跨事件持有同一panel/controller；异常drop先撤此lease，再释放下面的Native图。
    table_owner_lease: RefCell<Option<ProbeEditingOwnerLease>>,
    application: Retained<NSApplication>,
    surface: NativeMarkdownTextKitEditorSurface,
    panel: Retained<NativeMarkdownPanel>,
    container_view: Retained<NSView>,
    controller: Retained<NativeMarkdownController>,
    owned: Rc<NativeMarkdownOwnedLedger>,
    initial_text: String,
    fixture_suffix: String,
    paragraph_observations: RefCell<Option<ProbeParagraphObservations>>,
    table_cell_exercise: RefCell<Option<ProbeTableCellExercise>>,
}

/// Mounts the actual editor; the caller returns from this callback before exercising its input.
#[cfg(feature = "native-markdown-runtime-probe")]
pub fn run_native_markdown_runtime_probe(
    document_path: &Path,
    mtm: MainThreadMarker,
) -> Result<NativeMarkdownRuntimeProbeSession, String> {
    verify_native_markdown_deallocation_accounting(mtm)?;
    let application = NSApplication::sharedApplication(mtm);
    // A real NSApplication.run harness already completed launch; standalone callers must finish
    // launch once so native main-queue sources share the application's scheduling boundary.
    if !application.isRunning() {
        // Avoid repeating AppKit launch notifications when the probe runs from the real app delegate.
        application.finishLaunching();
    }
    verify_unmounted_native_markdown_model_release(document_path, mtm)?;
    // Open only the caller-owned bounded synthetic document; no production session or vault is consulted.
    let document = MarkdownDocumentBuffer::open(document_path, 2 * 1_024 * 1_024)
        .map_err(|error| format!("failed to open runtime-probe document: {error}"))?;

    // This bounded fixture copy supports independently constructed expectations for subsequent edits.
    let initial_text = std::fs::read_to_string(document_path)
        .map_err(|error| format!("failed to read runtime-probe fixture: {error}"))?;
    let heading_prefix = "# Probe";
    let fixture_suffix = initial_text
        .strip_prefix(heading_prefix)
        .ok_or_else(|| "runtime-probe fixture must start with the Probe heading".to_owned())?
        .to_owned();

    // Mount the same actual TextKit surface as the product, with no alternate minimal/skip path.
    // 独立夹具显式建立唯一 session root；editor 使用同一个 root，不另开身份预算。
    let owned = Rc::new(NativeMarkdownOwnedLedger::default());
    // 首次构造Native编辑器前固定唯一夹具尺寸，后面的parent不再使用另一套默认宽度。
    let fixture_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(720.0, 520.0));
    let surface = NativeMarkdownTextKitEditorSurface::new_with_mode(
        document,
        fixture_frame,
        MarkdownPresentationMode::InlinePreview,
        &owned,
        mtm,
    )?;
    let panel = NativeMarkdownPanel::new(mtm);
    // 独立夹具的editor与实际parent共用预声明尺寸，Find首次show不能把已捕获的720正文扩为1120。
    panel.setContentSize(fixture_frame.size);
    let controller = NativeMarkdownController::new(&owned, &panel, mtm);
    let _owner_lease = ProbeEditingOwnerLease::new(&controller)?;
    panel.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    let _ = surface
        .backend
        .text_view
        .ivars()
        .controller
        .set(Weak::from_retained(&controller));
    // 与正式窗口挂载一致，将纯fold权限绑定到本夹具的唯一controller；
    // ProbeEditingOwnerLease只借出同一身份，不允许owner为0的未挂载视图接收编辑。
    surface
        .backend
        .text_view
        .ivars()
        .folds
        .bind_owner(std::ptr::from_ref(&*controller).addr());
    // 读取实际三种原生类的公开滚动兼容结果；覆写scrollWheel可能已禁用并发滚动，
    // 不能把再加一个相同开关当作有效性能对照。仅输出固定布尔值，不读取正文或新增缓存。
    let (document_responsive, scroll_responsive, clip_responsive): (bool, bool, bool) = {
        let clip = surface.backend.scroll_view().contentView();
        // SAFETY: 这三个主线程活对象的类均继承NSView；公开类方法以BOOL返回兼容标志。
        unsafe {
            (
                msg_send![
                    PieceBackedMarkdownTextView::class(),
                    isCompatibleWithResponsiveScrolling
                ],
                msg_send![
                    surface.backend.scroll_view().class(),
                    isCompatibleWithResponsiveScrolling
                ],
                msg_send![clip.class(), isCompatibleWithResponsiveScrolling],
            )
        }
    };
    println!(
        "native_responsive_scrolling document={document_responsive} scroll={scroll_responsive} clip={clip_responsive}"
    );
    owned.include("probe.controller", &controller)?;
    // SAFETY: the probe owns this retained panel through its explicit native close boundary;
    // automatic AppKit release must not consume the Rust owner's independent reference.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
    let container_view = panel
        .contentView()
        .ok_or_else(|| "runtime-probe panel has no content view".to_owned())?;
    container_view.addSubview(surface.backend.scroll_view());
    panel.setInitialFirstResponder(Some(surface.backend.text_view()));
    application.activate();
    panel.orderFrontRegardless();
    panel.makeKeyWindow();
    if !panel.makeFirstResponder(Some(surface.backend.text_view())) {
        // Native input exercises require the actual text responder; focus refusal invalidates the test setup.
        return Err("runtime-probe editor did not become first responder".to_owned());
    }
    complete_native_markdown_probe_layout_transaction(
        &application,
        &panel,
        &container_view,
        &surface,
    )?;
    Ok(NativeMarkdownRuntimeProbeSession {
        table_owner_lease: RefCell::new(None),
        application,
        surface,
        panel,
        container_view,
        controller,
        owned,
        initial_text,
        fixture_suffix,
        paragraph_observations: RefCell::new(None),
        table_cell_exercise: RefCell::new(None),
    })
}

impl NativeMarkdownRuntimeProbeSession {
    /// 长夹具单独验证真实属性换页；原短夹具的同步编辑阶段不借用这份不同的排版前提。
    pub(crate) fn exercise_semantic_snapshot(&self) -> Result<(), String> {
        let _owner_lease = ProbeEditingOwnerLease::new(&self.controller)?;
        // 仍使用同一已挂载产品图、原 controller 和实际属性 getter，不创建替代语义缓存。
        semantic_snapshot::verify(&self.surface, &self.controller, &self.initial_text)?;
        // 与完整场景相同，关闭前采集至多两个实际可见段落的弱引用；未取到或不完整仍由原释放门禁拒绝。
        let observations = capture_probe_paragraph_observations(&self.surface);
        let previous = self.paragraph_observations.replace(Some(observations));
        drop(previous); // 仅替换弱样本，不让原段落或 attributedString 跨后续关闭事件保活。
        Ok(())
    }

    /// 错误期限或driver终止也归还同一测试图lease；不改变产品owner或领取新动作权限。
    pub(crate) fn cancel_table_exercise(&self) {
        self.surface
            .backend
            .text_view
            .ivars()
            .pending_table_cell_activation
            .set(None);
        let previous = self.table_owner_lease.borrow_mut().take();
        drop(previous);
    }

    /// Runs every input/style/table assertion against the already-mounted product graph.
    pub fn exercise(&self) -> Result<(), String> {
        super::probe_call_diagnostic::allocation_audit_point("exercise_begin");
        let _owner_lease = ProbeEditingOwnerLease::new(&self.controller)?;
        super::sidebar::verify_search_rows(&self.container_view, &self.owned, self.panel.mtm())?;
        // Borrow immutable ownership roots only; reentrant AppKit calls never cross a document borrow.
        let Self {
            application,
            surface,
            panel,
            container_view,
            initial_text,
            fixture_suffix,
            ..
        } = self;
        let heading_prefix = "# Probe";
        // Check identity through TextKit 2's observer protocol, avoiding legacy layout-manager accessors.
        let text_storage_observer = ProtocolObject::<dyn NSTextStorageObserving>::from_ref(
            &*surface.backend.text_content_storage,
        );
        let installed_storage = text_storage_observer
            .textStorage()
            .ok_or_else(|| "TextKit 2 did not retain the piece-backed text storage".to_owned())?;
        if !std::ptr::eq::<NSTextStorage>(&*installed_storage, surface.backend.text_storage()) {
            // A substitute native storage would bypass the canonical piece tree and conceal a duplicate body owner.
            return Err("TextKit 2 installed a different text storage".to_owned());
        }
        drop(installed_storage);

        if surface.backend.text_view().allowsUndo() {
            // Native undo would retain an independent edit history instead of the bounded structural document history.
            return Err("AppKit undo storage remained enabled".to_owned());
        }
        if surface.backend.text_view().enabledTextCheckingTypes() != 0 {
            // Automatic checking could retain text or start unaccounted foreground work outside the declared editor model.
            return Err("AppKit text checking remained enabled".to_owned());
        }
        if surface.backend.scroll_view().documentView().is_none() {
            // A detached scroll hierarchy cannot establish visible layout or native input acceptance.
            return Err("the native Markdown scroll view has no editor document view".to_owned());
        }
        if surface.backend.text_storage.presentation_mode()
            != MarkdownPresentationMode::InlinePreview
        {
            // Initial user-visible presentation must match the product default before semantic style assertions apply.
            return Err("the native Markdown surface did not start in preview mode".to_owned());
        }
        subscript::verify(surface, &self.controller, initial_text)?;
        super::probe_call_diagnostic::allocation_audit_point("exercise_subscript_done");
        list_markers::verify(surface, &self.controller, initial_text)?;
        super::probe_call_diagnostic::allocation_audit_point("exercise_list_done");
        selection_actions::verify(surface, &self.controller, initial_text)?;
        super::probe_call_diagnostic::allocation_audit_point("exercise_selection_done");
        multi_selection::verify(surface, &self.controller, initial_text)?;
        super::probe_call_diagnostic::allocation_audit_point("exercise_multi_done");
        if std::env::var_os("OMNI_NATIVE_PROBE_MULTI_DELETE_BASELINE").is_some() {
            // 只在明确的旧图对照进程中观察真实delete:及Core历史；不切换默认输入后端或生命周期。
            multi_selection::observe_delete_baseline(surface, &self.controller, initial_text)?;
        }

        // Exercise view-local fold ownership before style/input checks, then expand the original source.
        surface
            .backend
            .set_presentation_mode(MarkdownPresentationMode::Source, &self.controller)?;
        complete_native_markdown_probe_table_layout(&surface.backend.text_view)?;
        surface.backend.text_view.refresh_source_gutter();
        surface.backend.gutter.verify_visible_labels_for_probe()?;
        let fold_generation = surface.document().borrow().generation();
        let fold_length = surface.document().borrow().utf16_len();
        surface.backend.text_view.toggle_source_fold(0)?;
        if surface.backend.text_view.ivars().folds.borrow().is_empty()
            || surface.document().borrow().generation() != fold_generation
            || surface.document().borrow().utf16_len() != fold_length
            || surface.document().borrow().is_dirty()
        {
            // Folding must omit presentation paragraphs without mutating source, history, or saved identity.
            return Err("native source folding changed canonical text or failed to retain its fold interval".into());
        }
        // 保留折叠时走真实输入协议：标题前的空格只平移隐藏锚点，不能因编辑而展开全文。
        // 有限fold元数据在Native调用前复制完毕，不跨可重入输入持有RefCell借用。
        let original_folds = surface.backend.text_view.ivars().folds.borrow().clone();
        surface
            .backend
            .text_view
            .setSelectedRange(NSRange::new(0, 0));
        let input_client =
            ProtocolObject::<dyn NSTextInputClient>::from_ref(surface.backend.text_view());
        // SAFETY: 当前主线程上的活NSTextView接受NSString；范围0..0是已验证正文的有效起点。
        unsafe {
            input_client.insertText_replacementRange(&NSString::from_str(" "), NSRange::new(0, 0));
        }
        let expected_folds: Vec<_> = original_folds
            .iter()
            .map(
                |fold| omni_markdown_core::markdown_folding::MarkdownFoldRegion {
                    // 插入不含换行，标题仍处于原逻辑行，两个真实UTF-16端点各向后移动一位。
                    header_line: fold.header_line,
                    hidden_utf16_range: fold.hidden_utf16_range.start + 1
                        ..fold.hidden_utf16_range.end + 1,
                },
            )
            .collect();
        if *surface.backend.text_view.ivars().folds.borrow() != expected_folds
            || surface.document().borrow().utf16_len() != fold_length + 1
            || surface.backend.text_view.ivars().folds.generation.get()
                != surface.document().borrow().generation()
        {
            // 正文提交与fold发布必须属于同一新代次；只核长度不能发现意外展开或旧锚点。
            return Err("折叠状态下的原生输入未同步平移隐藏锚点".into());
        }
        surface
            .backend
            .text_view
            .restore_history(MarkdownHistoryDirection::Undo);
        if *surface.backend.text_view.ivars().folds.borrow() != original_folds
            || surface.document().borrow().utf16_len() != fold_length
            || surface.document().borrow().is_dirty()
            || surface.backend.text_view.ivars().folds.generation.get()
                != surface.document().borrow().generation()
        {
            // 真实Undo须恢复同一保存root及原折叠坐标，不能只恢复文字后清空折叠掩盖错误。
            return Err("折叠状态下的原生Undo未恢复正文及隐藏锚点".into());
        }
        surface.backend.text_view.unfold_source();
        if !surface.backend.text_view.ivars().folds.borrow().is_empty() {
            // Expansion must synchronously release every hidden interval before native editing begins.
            return Err("native source expansion retained a hidden interval".into());
        }
        complete_native_markdown_probe_table_layout(&surface.backend.text_view)?;

        surface
            .backend
            .set_presentation_mode(MarkdownPresentationMode::InlinePreview, &self.controller)?;
        // 模式切换可能改变正文的实际有序选择；先走产品的明确选择路径，再捕获本次格式意图。
        // 不能只向纯存储入口传一个范围，让通知中的真实 before 与原 Core 选择脱节。
        let format_selection = MarkdownTextSelection {
            anchor_utf16: 2,
            head_utf16: 7,
        };
        surface
            .backend
            .text_view
            .set_semantic_native_selection(format_selection);
        let captured = surface
            .backend
            .text_view
            .selection_for_command()
            .map_err(|error| error.to_string())?;
        let before = surface
            .backend
            .text_view
            .native_ordered_selection_state_for_capture(captured.native_capture())
            .map_err(|error| error.to_string())?;
        let NativeMarkdownOrderedSelection::Single(before) = before else {
            return Err("格式验收的明确选择没有安装为单一原生范围".into());
        };
        if captured.selection != format_selection
            || !captured.proven_head
            || before != NSRange::new(2, 5)
            || surface.backend.text_view.ivars().folds.known_selection() != Some(format_selection)
        {
            return Err("格式验收的原选择或完整方向已改变".into());
        }
        // 真实 getter 仍沿首个 capture 认证；同一 observed before 随 observer 进入原提交收据。
        let captured = captured
            .with_observed_native_before(before)
            .map_err(|error| error.to_string())?;
        let (format_edit, committed, pending) = surface
            .backend
            .text_storage
            .apply_format_captured(MarkdownFormatAction::Bold, captured)?;
        // 验收也消费 storage 返回的同一 Rc；pending 覆盖安装期间的真实通知，不用裸 setter 绕开原收据。
        if committed.selection != format_edit.selection_after
            || !surface
                .backend
                .text_view
                .apply_committed_selection(&committed)
        {
            return Err("原格式提交的选择没有通过同一收据实际安装".into());
        }
        if surface.document().borrow().utf16_len() != fold_length + 4 {
            // Paired strong delimiters add exactly four code units around the selected fixture label.
            return Err(
                "native format storage notification did not commit the canonical wrapper".into(),
            );
        }
        // 原格式的实际安装与正文核验已结束；后继 Undo 是新动作，不能继续悬挂旧 pending owner。
        drop(pending);
        surface
            .backend
            .text_view
            .restore_history(MarkdownHistoryDirection::Undo);
        if surface.document().borrow().utf16_len() != fold_length
            || surface.document().borrow().is_dirty()
        {
            // A single undo restores the original saved root before the remaining independent assertions.
            return Err(
                "native formatting did not restore its exact saved root in one undo".into(),
            );
        }
        complete_native_markdown_probe_table_layout(&surface.backend.text_view)?;

        // Heading rendering must add heading semantics without accidentally introducing underline.
        let heading_index = markdown_probe_utf16_offset(
            initial_text,
            initial_text
                .find("Probe")
                .ok_or_else(|| "runtime-probe fixture has no heading text".to_owned())?,
        )?;
        assert_native_markdown_attributes(surface, heading_index, false, "heading")?;
        assert_native_markdown_styles(
            surface,
            heading_index,
            &[MarkdownPresentationStyles::HEADING_1],
            &[MarkdownPresentationStyles::UNDERLINE],
            "heading",
        )?;

        // Table delimiters own table semantics even when their dash syntax resembles a horizontal rule.
        let table_delimiter_index = markdown_probe_utf16_offset(
            initial_text,
            initial_text
                .find(":---")
                .ok_or_else(|| "runtime-probe fixture has no table delimiter".to_owned())?,
        )?;
        assert_native_markdown_attributes(
            surface,
            table_delimiter_index,
            false,
            "table delimiter",
        )?;
        assert_native_markdown_styles(
            surface,
            table_delimiter_index,
            &[
                MarkdownPresentationStyles::TABLE,
                MarkdownPresentationStyles::TABLE_DELIMITER,
            ],
            &[
                MarkdownPresentationStyles::HORIZONTAL_RULE,
                MarkdownPresentationStyles::UNDERLINE,
                MarkdownPresentationStyles::HEADING_1,
                MarkdownPresentationStyles::HEADING_2,
                MarkdownPresentationStyles::HEADING_3,
                MarkdownPresentationStyles::HEADING_4,
                MarkdownPresentationStyles::HEADING_5,
                MarkdownPresentationStyles::HEADING_6,
            ],
            "table delimiter",
        )?;
        assert_native_markdown_table_recovered(surface, table_delimiter_index)?;
        assert_native_markdown_table_layout(surface)?;
        assert_native_markdown_table_input_guards(surface)?;

        // Scroll to the final table and invalidate derived geometry before checking EOF visibility.
        let eof_table_header_index = markdown_probe_utf16_offset(
            initial_text,
            initial_text
                .rfind("| 阶段 | 交付核心物料 | 周期 | 风险预案 |")
                .ok_or_else(|| "runtime-probe fixture has no EOF table".to_owned())?,
        )?;
        let clip_view = surface.backend.scroll_view.contentView();
        let document_height = surface
            .backend
            .text_view()
            .frame()
            .size
            .height
            .max(surface.backend.text_view().bounds().size.height);
        let viewport_height = clip_view.bounds().size.height;
        clip_view.scrollToPoint(NSPoint::new(
            0.0,
            (document_height - viewport_height).max(0.0),
        ));
        surface
            .backend
            .scroll_view
            .reflectScrolledClipView(&clip_view);
        surface
            .backend
            .text_view
            .invalidate_visible_table_layout_cache();
        surface
            .backend
            .text_view
            .ivars()
            .table_layout_update_scheduled
            .set(false);
        surface.backend.text_view().setNeedsDisplay(true);
        complete_native_markdown_probe_layout_transaction(
            application,
            panel,
            container_view,
            surface,
        )?;

        assert_native_markdown_eof_table_layout(surface, eof_table_header_index)?;

        // 原 baseline CSS 与现产品合同都要求可见链接带下划线；此 label 不是隐藏目标，
        // LINK 已授权该属性，后续仍单独断言它不带显式 UNDERLINE 语义。
        let link_index = markdown_probe_utf16_offset(
            initial_text,
            initial_text
                .find("link")
                .ok_or_else(|| "runtime-probe fixture has no link text".to_owned())?,
        )?;
        assert_native_markdown_attributes(surface, link_index, true, "link")?;
        assert_native_markdown_styles(
            surface,
            link_index,
            &[MarkdownPresentationStyles::LINK],
            &[MarkdownPresentationStyles::UNDERLINE],
            "link",
        )?;

        // The explicit underline fixture distinguishes product style rules from incidental native defaults.
        let underline_index = markdown_probe_utf16_offset(
            initial_text,
            initial_text
                .find("underlined")
                .ok_or_else(|| "runtime-probe fixture has no explicit underline".to_owned())?,
        )?;
        assert_native_markdown_attributes(surface, underline_index, true, "explicit underline")?;
        assert_native_markdown_styles(
            surface,
            underline_index,
            &[MarkdownPresentationStyles::UNDERLINE],
            &[],
            "explicit underline",
        )?;

        // A true rule must remain distinct from both table delimiters and heading syntax.
        let horizontal_rule_byte = initial_text
            .find("\n---\n")
            .map(|offset| offset + 1)
            .ok_or_else(|| "runtime-probe fixture has no horizontal rule".to_owned())?;
        let horizontal_rule_index =
            markdown_probe_utf16_offset(initial_text, horizontal_rule_byte)?;
        assert_native_markdown_attributes(
            surface,
            horizontal_rule_index,
            false,
            "horizontal rule",
        )?;
        assert_native_markdown_styles(
            surface,
            horizontal_rule_index,
            &[MarkdownPresentationStyles::HORIZONTAL_RULE],
            &[
                MarkdownPresentationStyles::UNDERLINE,
                MarkdownPresentationStyles::HEADING_1,
                MarkdownPresentationStyles::HEADING_2,
            ],
            "horizontal rule",
        )?;

        // Source mode preserves every delimiter while using bounded syntax coloring and uniform typography.
        surface
            .backend
            .set_presentation_mode(MarkdownPresentationMode::Source, &self.controller)?;
        if surface.backend.text_storage.presentation_mode() != MarkdownPresentationMode::Source {
            // Source mode must actually remove preview geometry/concealment without changing canonical text.
            return Err("the native Markdown surface did not enter source mode".to_owned());
        }
        assert_native_markdown_attributes(surface, heading_index, false, "source heading")?;
        assert_native_markdown_styles(
            surface,
            heading_index,
            &[MarkdownPresentationStyles::HEADING_1],
            &[MarkdownPresentationStyles::UNDERLINE],
            "source heading",
        )?;
        // Literal source markers and headings share a readable uniform baseline rather than preview concealment.
        let (marker_attributes, _) = surface.backend.text_storage.attributes_at_index(0);
        let (heading_attributes, _) = surface
            .backend
            .text_storage
            .attributes_at_index(heading_index);
        // SAFETY: AppKit exports the immutable font key, and this module creates NSFont values for it.
        let font_key = unsafe { NSFontAttributeName };
        let marker_font = marker_attributes
            .objectForKey(font_key)
            .and_then(|font| font.downcast::<NSFont>().ok())
            .ok_or("source marker has no native font")?;
        let heading_font = heading_attributes
            .objectForKey(font_key)
            .and_then(|font| font.downcast::<NSFont>().ok())
            .ok_or("source heading has no native font")?;
        if marker_font.pointSize() != 14.0
            || heading_font.pointSize() != 14.0
            || !surface.backend.scroll_view.rulersVisible()
        {
            // Source styling must not accidentally shrink punctuation or retain preview heading heights.
            return Err(
                "native Source mode did not preserve literal marker geometry and visible gutter"
                    .into(),
            );
        }
        assert_native_markdown_attributes(surface, underline_index, false, "source underline")?;
        surface
            .backend
            .set_presentation_mode(MarkdownPresentationMode::InlinePreview, &self.controller)?;
        if surface.backend.text_storage.presentation_mode()
            != MarkdownPresentationMode::InlinePreview
        {
            // Preview re-entry must restore derived semantics rather than leaving the source-mode presentation active.
            return Err("the native Markdown surface did not return to preview mode".to_owned());
        }
        assert_native_markdown_attributes(
            surface,
            underline_index,
            true,
            "preview underline after mode switch",
        )?;

        // An out-of-range NSString bulk read must initialize the caller's buffer deterministically.
        let mut invalid_bulk_read = [0xdead_u16; 2];
        let invalid_bulk_offset = surface.document().borrow().utf16_len().saturating_add(1);
        copy_piece_backed_utf16_range(
            &surface.document().borrow(),
            invalid_bulk_offset,
            &mut invalid_bulk_read,
        );
        if invalid_bulk_read != [0xfffd_u16; 2] {
            // Even rejected native string reads must initialize all requested output units deterministically.
            return Err("an invalid bulk NSString read left output bytes uninitialized".to_owned());
        }
        assert_textkit_matches_piece_tree(surface)?;

        // Drive the public input-client contract, including AppKit's current-selection sentinel.
        let input_client =
            ProtocolObject::<dyn NSTextInputClient>::from_ref(surface.backend.text_view());
        let no_explicit_replacement = NSRange::new(NSNotFound as usize, 0);

        surface
            .backend
            .text_view()
            .setSelectedRange(NSRange::new(heading_prefix.encode_utf16().count(), 0));
        let ascii = NSString::from_str(" A");
        // SAFETY: this live NSString is an accepted NSTextInputClient dynamic input value;
        // the main-thread view and its valid current selection outlive the synchronous call.
        unsafe {
            input_client.insertText_replacementRange(&ascii, no_explicit_replacement);
        }
        expect_document_text(surface, &format!("{heading_prefix} A{fixture_suffix}"))?;
        let selection_after_ascii = surface.backend.text_view().selectedRange();
        if selection_after_ascii != NSRange::new(heading_prefix.encode_utf16().count() + 2, 0) {
            // The accepted insertion must advance by its UTF-16 length without losing the native selection.
            return Err(format!(
                "TextKit moved the caret after ASCII insertion: location={}, length={}",
                selection_after_ascii.location, selection_after_ascii.length
            ));
        }
        assert_textkit_matches_piece_tree(surface)?;

        // Two successive marked-text updates emulate IME composition without duplicating the first value.
        let marked_start = heading_prefix.encode_utf16().count() + 2;
        let marked_ascii = NSString::from_str("zh");
        // SAFETY: the live NSString satisfies the dynamic marked-text argument contract;
        // the selected UTF-16 range lies within it and NSNotFound requests the current selection.
        unsafe {
            input_client.setMarkedText_selectedRange_replacementRange(
                &marked_ascii,
                NSRange::new(2, 0),
                no_explicit_replacement,
            );
        }
        let first_marked_range = input_client.markedRange();
        if !input_client.hasMarkedText()
            || first_marked_range != NSRange::new(marked_start, marked_ascii.len_utf16())
        {
            // The initial IME candidate must own exactly the inserted composition span, not an unrelated document range.
            return Err(format!(
                "TextKit did not establish the first marked-text range: location={}, length={}",
                first_marked_range.location, first_marked_range.length
            ));
        }
        expect_document_text(surface, &format!("{heading_prefix} Azh{fixture_suffix}"))?;

        // Attributed Chinese replacement must update the same marked span before explicit commit.
        let marked_chinese = NSString::from_str("中文");
        let attributed_marked =
            NSAttributedString::initWithString(NSAttributedString::alloc(), &marked_chinese);
        // SAFETY: this live NSAttributedString is a supported dynamic marked-text input;
        // its internal UTF-16 selection is valid and the main-thread view owns the replacement state.
        unsafe {
            input_client.setMarkedText_selectedRange_replacementRange(
                &attributed_marked,
                NSRange::new(2, 0),
                no_explicit_replacement,
            );
        }
        let chinese_marked_range = input_client.markedRange();
        if !input_client.hasMarkedText()
            || chinese_marked_range != NSRange::new(marked_start, marked_chinese.len_utf16())
        {
            // Replacing a marked candidate must preserve one composition identity while updating its Unicode payload.
            return Err(format!(
                "TextKit lost marked text during the Chinese update: location={}, length={}",
                chinese_marked_range.location, chinese_marked_range.length
            ));
        }
        expect_document_text(surface, &format!("{heading_prefix} A中文{fixture_suffix}"))?;

        // 非空相对选择与 primitive 默认末端 caret 不同，必须由同一 observed 事务发布完整方向。
        let nonempty_candidate = NSString::from_str("中😀文");
        let nonempty_generation = surface.document().borrow().generation();
        let chinese_revision = surface
            .document()
            .borrow()
            .current_revision()
            .map_err(|error| format!("无法捕获原文档revision：{error}"))?;
        let nonempty_commits = surface
            .backend
            .text_storage
            .ivars()
            .input_commit_count
            .get();
        let nonempty_callbacks = surface
            .backend
            .text_storage
            .ivars()
            .input_callback_success_count
            .get();
        let nonempty_autosave_enqueues = surface
            .backend
            .text_storage
            .ivars()
            .autosave_enqueue_count
            .get();
        // SAFETY: 1..3 恰好覆盖候选中的完整 emoji，输入对象仍由本同步作用域持有。
        unsafe {
            input_client.setMarkedText_selectedRange_replacementRange(
                &nonempty_candidate,
                NSRange::new(1, 2),
                no_explicit_replacement,
            );
        }
        let whole_selection = MarkdownTextSelection {
            anchor_utf16: marked_start + 1,
            head_utf16: marked_start + 3,
        };
        if surface.document().borrow().selection() != whole_selection
            || surface.backend.text_view.ivars().folds.known_selection() != Some(whole_selection)
            || input_client.selectedRange() != NSRange::new(marked_start + 1, 2)
            || surface
                .backend
                .text_view
                .native_ordered_selection_range()
                .map_err(|error| error.to_string())?
                != NSRange::new(marked_start + 1, 2)
            || input_client.markedRange() != NSRange::new(marked_start, 4)
            || surface.document().borrow().generation() != nonempty_generation + 1
            || surface.document().borrow().has_prepared_text_input()
            || surface
                .backend
                .text_view
                .ivars()
                .input_callback
                .borrow()
                .is_some()
            || surface
                .backend
                .text_storage
                .ivars()
                .input_commit_count
                .get()
                != nonempty_commits + 1
            || surface
                .backend
                .text_storage
                .ivars()
                .input_callback_success_count
                .get()
                != nonempty_callbacks + 1
            || surface
                .backend
                .text_storage
                .ivars()
                .autosave_enqueue_count
                .get()
                != nonempty_autosave_enqueues + 1
        {
            return Err("候选非空选择未由同一事务发布完整 Core、Known 和实际 Native 范围".into());
        }
        expect_document_text(
            surface,
            &format!("{heading_prefix} A中😀文{fixture_suffix}"),
        )
        .map_err(|error| format!("输入阶段 nonempty_candidate_commit：{error}"))?;
        let preserved = {
            let document = surface.document().borrow();
            (
                document.generation(),
                document
                    .current_revision()
                    .map_err(|error| format!("无法读取原文档revision：{error}"))?,
                document.byte_len(),
                document.utf16_len(),
                document.is_dirty(),
                document.can_undo(),
                document.can_redo(),
                document.selection(),
            )
        };
        let native_marked_before = surface.backend.text_view.native_marked_state_for_probe();
        let processed_before = surface
            .backend
            .text_storage
            .ivars()
            .character_process_count
            .get();
        let commits_before = surface
            .backend
            .text_storage
            .ivars()
            .input_commit_count
            .get();
        let callbacks_before = surface
            .backend
            .text_storage
            .ivars()
            .input_callback_success_count
            .get();
        let autosave_enqueues_before = surface
            .backend
            .text_storage
            .ivars()
            .autosave_enqueue_count
            .get();
        let autosave_before = self.controller.ivars().autosave_generation.get();
        let autosave_timer_before = self
            .controller
            .ivars()
            .autosave_timer
            .borrow()
            .as_ref()
            .map(|timer| std::ptr::from_ref(&**timer).addr());
        let rejected = NSString::from_str("新");
        // SAFETY: 这是显式错误范围的有限拒绝回归；整数落在真实文档内，客户端必须在 super 前拒绝低 surrogate。
        unsafe {
            input_client.setMarkedText_selectedRange_replacementRange(
                &rejected,
                NSRange::new(1, 0),
                NSRange::new(marked_start + 2, 1),
            );
        }
        let after_rejection = {
            let document = surface.document().borrow();
            (
                document.generation(),
                document
                    .current_revision()
                    .map_err(|error| format!("无法读取原文档revision：{error}"))?,
                document.byte_len(),
                document.utf16_len(),
                document.is_dirty(),
                document.can_undo(),
                document.can_redo(),
                document.selection(),
            )
        };
        if preserved != after_rejection
            || surface.backend.text_view.ivars().folds.known_selection() != Some(whole_selection)
            || input_client.markedRange() != NSRange::new(marked_start, 4)
            || !input_client.hasMarkedText()
            || surface.backend.text_view.native_marked_state_for_probe() != native_marked_before
            || !surface
                .backend
                .text_storage
                .ivars()
                .composition_active
                .get()
            || surface
                .backend
                .text_storage
                .ivars()
                .character_process_count
                .get()
                != processed_before
            || surface
                .backend
                .text_storage
                .ivars()
                .input_commit_count
                .get()
                != commits_before
            || surface
                .backend
                .text_storage
                .ivars()
                .input_callback_success_count
                .get()
                != callbacks_before
            || surface
                .backend
                .text_storage
                .ivars()
                .autosave_enqueue_count
                .get()
                != autosave_enqueues_before
            || surface
                .backend
                .text_storage
                .ivars()
                .input_transaction
                .borrow()
                .is_some()
            || surface.document().borrow().has_prepared_text_input()
            || surface
                .backend
                .text_view
                .ivars()
                .input_callback
                .borrow()
                .is_some()
            || self.controller.ivars().autosave_generation.get() != autosave_before
            || self
                .controller
                .ivars()
                .autosave_timer
                .borrow()
                .as_ref()
                .map(|timer| std::ptr::from_ref(&**timer).addr())
                != autosave_timer_before
        {
            return Err("非法候选替换没有完整保留原组合、正文、选择、历史或零字符通知边界".into());
        }
        expect_document_text(
            surface,
            &format!("{heading_prefix} A中😀文{fixture_suffix}"),
        )
        .map_err(|error| format!("输入阶段 invalid_surrogate_preserved：{error}"))?;
        if std::env::var_os("OMNI_NATIVE_PROBE_INPUT_CAPACITY_REJECTION").is_some() {
            // 此显式隔离场景真正进入 Core 容量拒绝；失败保留现场，不能通过修补 Native 状态冒充事务成功。
            input_capacity::verify(surface)?;
        }
        let unmark_capture = surface
            .backend
            .text_view
            .ivars()
            .folds
            .capture_selection()
            .map_err(|error| format!("输入阶段 candidate_unmark_capture：{error}"))?;
        let unmark_counts = [
            surface
                .backend
                .text_storage
                .ivars()
                .character_process_count
                .get(),
            surface
                .backend
                .text_storage
                .ivars()
                .input_commit_count
                .get(),
            surface
                .backend
                .text_storage
                .ivars()
                .input_callback_success_count
                .get(),
            surface
                .backend
                .text_storage
                .ivars()
                .autosave_enqueue_count
                .get(),
        ];
        input_client.unmarkText();
        // unmark 只解除组合，不修改正文或选择；若这一步失败，不能把后续 Undo 的未执行误报成文本差异。
        let unmark_native = selection_native_step(&unmark_capture, || surface.backend.text_view.native_marked_state_for_probe())
            .map_err(|error| format!("输入阶段 candidate_unmark_super 已撤权：{error}; composing={} selection={:?} known={:?}",
                surface.backend.text_storage.ivars().composition_active.get(),
                surface.document().borrow().selection(), surface.backend.text_view.ivars().folds.known_selection()))?;
        let unmark_semantic_marked =
            selection_native_step(&unmark_capture, || input_client.hasMarkedText())
                .map_err(|error| format!("输入阶段 candidate_unmark_semantic 已撤权：{error}"))?;
        let unmark_ordered = surface
            .backend
            .text_view
            .native_ordered_selection_state_for_capture(&unmark_capture);
        let unmark_ordered_ok = matches!(&unmark_ordered, Ok(NativeMarkdownOrderedSelection::Single(range)) if *range == NSRange::new(marked_start + 1, 2));
        let unmark_selection = surface.document().borrow().selection();
        if !unmark_capture.current()
            || unmark_semantic_marked
            || unmark_native.0
            || unmark_native.1.location != NSNotFound as usize && unmark_native.1.length != 0
            || surface
                .backend
                .text_storage
                .ivars()
                .composition_active
                .get()
            || surface
                .backend
                .text_storage
                .ivars()
                .input_transaction
                .borrow()
                .is_some()
            || surface.document().borrow().has_prepared_text_input()
            || surface
                .backend
                .text_view
                .ivars()
                .input_callback
                .borrow()
                .is_some()
            || unmark_counts
                != [
                    surface
                        .backend
                        .text_storage
                        .ivars()
                        .character_process_count
                        .get(),
                    surface
                        .backend
                        .text_storage
                        .ivars()
                        .input_commit_count
                        .get(),
                    surface
                        .backend
                        .text_storage
                        .ivars()
                        .input_callback_success_count
                        .get(),
                    surface
                        .backend
                        .text_storage
                        .ivars()
                        .autosave_enqueue_count
                        .get(),
                ]
            || unmark_selection != whole_selection
            || surface.backend.text_view.ivars().folds.known_selection() != Some(whole_selection)
            || !unmark_ordered_ok
        {
            return Err(format!(
                "输入阶段 candidate_unmark 未完成：capture={} semantic_marked={} super_marked={unmark_native:?} composing={} selection={unmark_selection:?} known={:?} ordered_ok={}",
                unmark_capture.current(),
                unmark_semantic_marked,
                surface
                    .backend
                    .text_storage
                    .ivars()
                    .composition_active
                    .get(),
                surface.backend.text_view.ivars().folds.known_selection(),
                unmark_ordered_ok,
            ));
        }
        // 真正 Undo/Redo 只跨最近合法候选；拒绝输入不能偷偷插入额外历史步骤。
        let undo_generation = surface.document().borrow().generation();
        // SAFETY: 两个标准 selector 均由当前真实 responder 实现，不使用物理按键或另造输入绑定。
        unsafe {
            input_client.doCommandBySelector(sel!(undo:));
        }
        // 同一标准命令必须实际推进一次历史并回到先前中文 root；光收到 selector 不能算 Undo 已执行。
        let undo_state = {
            let document = surface.document().borrow();
            (
                document.generation(),
                document
                    .current_revision()
                    .map_err(|error| format!("无法读取原文档revision：{error}"))?,
                document.selection(),
                document.can_redo(),
            )
        };
        if undo_state.0 != undo_generation + 1 || undo_state.1 != chinese_revision || !undo_state.3
        {
            return Err(format!(
                "输入阶段 candidate_undo 未还原原历史：g={}/{} root_restored={} can_redo={} selection={:?} known={:?} marked={:?}",
                undo_state.0,
                undo_generation + 1,
                undo_state.1 == chinese_revision,
                undo_state.3,
                undo_state.2,
                surface.backend.text_view.ivars().folds.known_selection(),
                surface.backend.text_view.ivars().marked_text_range.get(),
            ));
        }
        expect_document_text(surface, &format!("{heading_prefix} A中文{fixture_suffix}"))
            .map_err(|error| format!("输入阶段 candidate_undo_text：{error}"))?;
        // SAFETY: 原 responder、Core 和历史仍由同一 surface 持有；重做只恢复上一合法候选。
        unsafe {
            input_client.doCommandBySelector(sel!(redo:));
        }
        let redo_state = {
            let document = surface.document().borrow();
            (
                document.generation(),
                document
                    .current_revision()
                    .map_err(|error| format!("无法读取原文档revision：{error}"))?,
                document.selection(),
            )
        };
        if redo_state.0 != undo_state.0 + 1 || redo_state.1 != preserved.1 {
            return Err(format!(
                "输入阶段 candidate_redo 未恢复合法候选历史：g={}/{} root_restored={} selection={:?} known={:?}",
                redo_state.0,
                undo_state.0 + 1,
                redo_state.1 == preserved.1,
                redo_state.2,
                surface.backend.text_view.ivars().folds.known_selection(),
            ));
        }
        expect_document_text(
            surface,
            &format!("{heading_prefix} A中😀文{fixture_suffix}"),
        )
        .map_err(|error| format!("输入阶段 candidate_redo_text：{error}"))?;
        // 明确恢复原小样本，后续已有 Unicode/历史验收仍使用原来的字面预期。
        let restore_generation = surface.document().borrow().generation();
        let restore_commits = surface
            .backend
            .text_storage
            .ivars()
            .input_commit_count
            .get();
        let restore_callbacks = surface
            .backend
            .text_storage
            .ivars()
            .input_callback_success_count
            .get();
        // SAFETY: 同源 4-unit 候选范围覆盖完整 Unicode scalar，中文候选的相对 caret 为 2。
        unsafe {
            input_client.setMarkedText_selectedRange_replacementRange(
                &attributed_marked,
                NSRange::new(2, 0),
                NSRange::new(marked_start, 4),
            );
        }
        let restore_state = {
            let document = surface.document().borrow();
            (
                document.generation(),
                document
                    .current_revision()
                    .map_err(|error| format!("无法读取原文档revision：{error}"))?,
                document.selection(),
            )
        };
        if restore_state.0 != restore_generation + 1
            || restore_state.1 != chinese_revision
            || surface
                .backend
                .text_storage
                .ivars()
                .input_commit_count
                .get()
                != restore_commits + 1
            || surface
                .backend
                .text_storage
                .ivars()
                .input_callback_success_count
                .get()
                != restore_callbacks + 1
        {
            return Err(format!(
                "输入阶段 candidate_restore 未提交原中文候选：g={}/{} root_restored={} commits={}/{} callbacks={}/{} selection={:?} known={:?} marked={:?}",
                restore_state.0,
                restore_generation + 1,
                restore_state.1 == chinese_revision,
                surface
                    .backend
                    .text_storage
                    .ivars()
                    .input_commit_count
                    .get(),
                restore_commits + 1,
                surface
                    .backend
                    .text_storage
                    .ivars()
                    .input_callback_success_count
                    .get(),
                restore_callbacks + 1,
                restore_state.2,
                surface.backend.text_view.ivars().folds.known_selection(),
                surface.backend.text_view.ivars().marked_text_range.get(),
            ));
        }
        expect_document_text(surface, &format!("{heading_prefix} A中文{fixture_suffix}"))
            .map_err(|error| format!("输入阶段 candidate_restore_text：{error}"))?;
        input_client.unmarkText();
        if surface.backend.text_view.native_marked_state_for_probe().0 {
            // Public custom getters must not mask a composition still retained by NSTextView itself.
            return Err("NSTextView kept its native marked state after composition commit".into());
        }
        if input_client.hasMarkedText() {
            // Committing composition must remove marked state before subsequent Unicode edits and history commands.
            return Err("TextKit did not commit marked text".to_owned());
        }

        // A supplementary scalar and combining mark expose UTF-16 versus byte/scalar boundary mistakes.
        let unicode_tail = NSString::from_str("😀e\u{301}");
        // SAFETY: this live NSString is a supported dynamic input; the main-thread TextKit
        // responder owns the valid current selection selected by the NSNotFound replacement sentinel.
        unsafe {
            input_client.insertText_replacementRange(&unicode_tail, no_explicit_replacement);
        }
        let edited_text = format!("{heading_prefix} A中文😀e\u{301}{fixture_suffix}");
        expect_document_text(surface, &edited_text)?;
        assert_textkit_matches_piece_tree(surface)?;

        // 程序选择明确保留中文与emoji的完整UTF16方向；不能只设升序Native范围后猜head。
        let unicode_selection_start = heading_prefix.encode_utf16().count() + 2;
        let unicode_selection = NSRange::new(unicode_selection_start, 4);
        let unicode_target = MarkdownTextSelection {
            anchor_utf16: unicode_selection_start,
            head_utf16: unicode_selection_start + 4,
        };
        surface
            .backend
            .text_view
            .set_semantic_native_selection(unicode_target);
        let actual_ordered = surface
            .backend
            .text_view
            .native_ordered_selection_range()
            .map_err(|_| "Unicode测试没有实际单一Native范围")?;
        if input_client.selectedRange() != unicode_selection
            || actual_ordered != unicode_selection
            || surface.document().borrow().selection() != unicode_target
        {
            // 协议、实际Native范围和Core完整方向必须同时相等；UTF16跨surrogate边界不能被兼容降级掩盖。
            return Err("TextKit did not preserve a UTF-16 selection".to_owned());
        }
        let history_insert_offset = format!("{heading_prefix} A中文😀e\u{301}")
            .encode_utf16()
            .count();
        surface
            .backend
            .text_view()
            .setSelectedRange(NSRange::new(history_insert_offset, 0));

        // Structural undo/redo must replay canonical edits without enabling AppKit's full-string history.
        let history_probe = NSString::from_str("!");
        // SAFETY: the live NSString and main-thread input client satisfy the synchronous dynamic
        // input contract; the current scalar-aligned UTF-16 selection is the replacement target.
        unsafe {
            input_client.insertText_replacementRange(&history_probe, no_explicit_replacement);
        }
        let history_text = format!("{heading_prefix} A中文😀e\u{301}!{fixture_suffix}");
        expect_document_text(surface, &history_text)?;
        // SAFETY: undo: is a standard responder selector implemented by this main-thread text view;
        // the call takes no borrowed argument or escaping pointer.
        unsafe {
            input_client.doCommandBySelector(sel!(undo:));
        }
        expect_document_text(surface, &edited_text)?;
        assert_textkit_matches_piece_tree(surface)?;
        // SAFETY: redo: is the standard responder selector implemented by this live main-thread view;
        // the canonical structural history and receiver remain owned for the synchronous call.
        unsafe {
            input_client.doCommandBySelector(sel!(redo:));
        }
        expect_document_text(surface, &history_text)?;
        assert_textkit_matches_piece_tree(surface)?;

        // 真实工具栏与故障分支各自 Undo 回到原 root；不把 Native setter 属性检查当作字符提交成功。
        pending_fault::verify(surface)?;
        if native_markdown_pending_selection_trace_enabled() {
            // 显式测量旧 EOF caret 超出删除后长度的真实通知，再通过产品 Undo 恢复同一合成原文。
            pending_selection::verify(surface)?;
        }

        // 后续表格动作由110..113的原请求终点分事件推进；本事件不手动完成Core。
        super::probe_call_diagnostic::allocation_audit_point("exercise_end");
        Ok(())
    }

    /// Ends ownership after the exercise callback returned and produces weak-only release evidence.
    pub fn close(self) -> Result<NativeMarkdownRuntimeProbeCompletion, String> {
        let _owner_lease = ProbeEditingOwnerLease::new(&self.controller)?;
        // Destructuring transfers the unique graph into this finite teardown transaction.
        let Self {
            surface,
            panel,
            container_view,
            controller,
            owned,
            paragraph_observations,
            ..
        } = self;
        let paragraph_observations = paragraph_observations
            .into_inner()
            .unwrap_or_else(ProbeParagraphObservations::not_sampled);
        let mtm = MainThreadMarker::new().expect("native probe teardown runs on the main thread");
        let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
        cohort.track(&panel, mtm);
        cohort.track(&container_view, mtm);
        let panel_weak = Weak::from_retained(&panel);
        let container_view_weak = Weak::from_retained(&container_view);
        // Snapshot weak release evidence only; explicitly drop temporary retained layout handles below.
        let document_weak = Rc::downgrade(surface.document());
        let scroll_view_weak = Weak::from_retained(&surface.backend.scroll_view);
        let gutter_weak = Weak::from_retained(&surface.backend.gutter);
        let text_view_weak = Weak::from_retained(&surface.backend.text_view);
        let text_storage_weak = Weak::from_retained(&surface.backend.text_storage);
        let text_content_storage_weak = Weak::from_retained(&surface.backend.text_content_storage);
        let text_layout_manager = surface
            .backend
            .text_view
            .textLayoutManager()
            .ok_or_else(|| "TextKit 2 layout manager disappeared before teardown".to_owned())?;
        let text_container = text_layout_manager
            .textContainer()
            .ok_or_else(|| "TextKit 2 text container disappeared before teardown".to_owned())?;
        let text_layout_manager_weak = Weak::from_retained(&text_layout_manager);
        let text_container_weak = Weak::from_retained(&text_container);
        let table_cell_editor_weak = surface
            .backend
            .text_view
            .existing_table_cell_editor()
            .map(|cell| Weak::from_retained(&cell));
        drop(text_container);
        drop(text_layout_manager);
        let borrowed = controller.ivars().borrowed.borrow().clone();
        borrowed.capture(&panel)?;
        surface.capture_find_resources()?;
        // The window's responder transition asks AppKit to deactivate the input context itself.
        if !panel.makeFirstResponder(None) {
            // A refused resignation leaves AppKit input/caret ownership active and invalidates teardown.
            return Err("the native responder refused to resign before close".into());
        }
        if panel.firstResponder().is_some_and(|responder| {
            std::ptr::eq::<objc2_app_kit::NSResponder>(
                &*responder,
                surface.backend.text_view().as_super().as_super().as_super(),
            )
        }) {
            // Successful resignation must not leave the original text responder installed.
            return Err(
                "the old native text view remained first responder after resignation".into(),
            );
        }
        // The product seals input after successful commit/save/resignation and before hierarchy teardown.
        surface
            .backend
            .text_view
            .commit_composition_before_release()?;
        let sealed_generation = surface.document().borrow().generation();
        let sealed_length = surface.document().borrow().utf16_len();
        surface.backend.stop_accepting_input();
        let late_input = NSString::from_str("late-native-input");
        let input_client =
            ProtocolObject::<dyn NSTextInputClient>::from_ref(surface.backend.text_view());
        // SAFETY: this supported native string and scalar range stay live through the synchronous public input callback.
        unsafe {
            input_client.insertText_replacementRange(&late_input, NSRange::new(0, 0));
        }
        // A direct delayed TextKit primitive must be sealed as well as the native view input adapter.
        surface
            .backend
            .text_storage
            .replaceCharactersInRange_withString(NSRange::new(0, 0), &late_input);
        if surface.document().borrow().generation() != sealed_generation
            || surface.document().borrow().utf16_len() != sealed_length
        {
            // No source mutation may be accepted after the saved close gate, even before final native deallocation.
            return Err("late native input mutated the sealed Markdown document".into());
        }
        borrowed.capture(&panel)?;
        borrowed.preflight_unmarked()?;
        let cell = surface.backend.text_view.existing_table_cell_editor();
        let controls: Vec<_> = cell.iter().map(|field| &**field).collect();
        // 与产品相同：accepted 后先撤销创建权限，即使观测失败也继续执行唯一清理。
        controller.ivars().field_editor_provider.freeze();
        let field_editor = borrowed.finish_accepted(
            &panel,
            &controls,
            &controller.ivars().undo,
            &controller.ivars().field_editor_provider,
            true,
        );
        *surface.session_field_editor.borrow_mut() = field_editor;
        drop(controls);
        drop(cell);
        if !borrowed.is_detached() {
            // accepted 清理失败不转为成功，也不使用延时、强制 release 或重试。
            return Err(format!(
                "the probe shared field editor did not detach: {}",
                borrowed.failure_diagnostic()
            ));
        }
        panel.setInitialFirstResponder(None);
        panel.orderOut(None);
        panel.close();
        panel.setContentView(None);
        // Use the product release order: detach the editor/TextKit graph before dropping window owners.
        let mut release_barrier = destroy_native_markdown_editor(surface);
        release_barrier.borrowed = Some(borrowed);
        release_barrier.track_surviving_nodes(
            &mut cohort,
            // 原不可变值族角色只在同一诊断组合中存在，单独Raster仍核完整普通释放账本。
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
        panel.setDelegate(None);
        let mut owned = owned.snapshot();
        owned.seal();
        owned.track_remaining(&mut cohort, mtm);
        drop(controller);
        drop(container_view);
        drop(panel);
        cohort.seal();
        Ok(NativeMarkdownRuntimeProbeCompletion {
            cohort,
            owned,
            panel: panel_weak,
            container_view: container_view_weak,
            release_barrier,
            document: document_weak,
            scroll_view: scroll_view_weak,
            gutter: gutter_weak,
            text_view: text_view_weak,
            text_storage: text_storage_weak,
            text_content_storage: text_content_storage_weak,
            text_layout_manager: text_layout_manager_weak,
            text_container: text_container_weak,
            table_cell_editor: table_cell_editor_weak,
            paragraph_observations,
        })
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Consume real window/TextKit geometry and the derived table transaction before inspecting presentation.
fn complete_native_markdown_probe_layout_transaction(
    application: &NSApplication,
    panel: &NativeMarkdownPanel,
    container_view: &NSView,
    surface: &NativeMarkdownTextKitEditorSurface,
) -> Result<(), String> {
    // A mounted visible hierarchy supplies valid TextKit geometry, unlike an unhosted view-only shortcut.
    container_view.layoutSubtreeIfNeeded();
    let text_layout_manager = surface
        .backend
        .text_view()
        .textLayoutManager()
        .ok_or_else(|| "TextKit 2 layout manager disappeared during layout".to_owned())?;
    // Establish TextKit geometry before driving the unchanged derived table transaction body once.
    text_layout_manager.ensureLayoutForBounds(surface.backend.text_view().bounds());
    drop(text_layout_manager);
    complete_native_markdown_probe_table_layout(&surface.backend.text_view)?;
    // Display/update consumes native post-layout actions before this harness requests destruction.
    surface.backend.text_view().displayIfNeeded();
    panel.displayIfNeeded();
    application.updateWindows();
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Execute one scheduled product layout transaction for deterministic geometry/input assertions.
///
/// This checks the real transaction body and resulting cache; actual dispatch latency and callback
/// delivery require the separate application event-loop acceptance path. No nested run-loop wait may
/// substitute for returning from the current serial native callback.
fn complete_native_markdown_probe_table_layout(
    text_view: &PieceBackedMarkdownTextView,
) -> Result<(), String> {
    // Give TextKit current mounted-view geometry before requesting the product's derived transaction.
    let manager = text_view.textLayoutManager().ok_or_else(|| {
        "TextKit 2 manager missing before the deterministic table transaction".to_owned()
    })?;
    manager.ensureLayoutForBounds(text_view.bounds());
    drop(manager);
    text_view.schedule_visible_table_layout_update();
    // Drive exactly the product transaction body once; an already-queued weak callback becomes inert
    // after this body clears its scheduled flag. This helper does not alter production scheduling.
    text_view.run_scheduled_table_layout_update();
    // 只复制已发布的有限key；后面的公开getter可能重入，不能把getter清理后的None误说成从未发布。
    let cache_key_after_executor = {
        let cache = text_view.ivars().visible_table_layout_cache.borrow();
        cache.as_ref().map(|cache| cache.request_key)
    };
    let current_key = text_view.current_table_layout_request_key();
    let published_cache_key = {
        let cache = text_view.ivars().visible_table_layout_cache.borrow();
        cache.as_ref().map(|cache| cache.request_key)
    };
    // 成功条件仍要求getter之后的实际cache精确匹配当前key；诊断不能放宽或重跑事务。
    let cache_is_current = current_key.is_some_and(|request_key| {
        published_cache_key.is_some_and(|published| published == request_key)
    });
    if cache_is_current
        && !text_view.ivars().table_layout_update_scheduled.get()
        && !text_view.ivars().table_layout_rebuilding.get()
    {
        // The one deterministic product transaction completed with the expected cache and idle flags.
        return Ok(());
    }
    // Ref借用已结束；失败只格式化纯key、fold修订与已知完整选择，不读取正文或另取Native状态。
    let fold_revision = text_view.ivars().folds.revision();
    let known_selection = text_view.ivars().folds.known_selection();
    Err(format!(
        "native Markdown deterministic table transaction failed: cache_current={cache_is_current} scheduled={} rebuilding={} current_key={current_key:?} cache_after_executor={cache_key_after_executor:?} published_cache_key={published_cache_key:?} fold_revision={fold_revision} known_selection={known_selection:?}",
        text_view.ivars().table_layout_update_scheduled.get(),
        text_view.ivars().table_layout_rebuilding.get()
    ))
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Translate a known fixture byte boundary into the UTF-16 coordinates consumed by native text APIs.
fn markdown_probe_utf16_offset(source: &str, byte_offset: usize) -> Result<usize, String> {
    // Checked slicing rejects a byte offset inside a UTF-8 scalar before counting native code units.
    source
        .get(..byte_offset)
        .map(|prefix| {
            // The checked prefix is scalar-aligned; native text APIs count its UTF-16 units.
            prefix.encode_utf16().count()
        })
        .ok_or_else(|| "runtime-probe byte offset was not a UTF-8 boundary".to_owned())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Compare the small synthetic fixture with canonical piece-tree text without logging its contents.
fn expect_document_text(
    surface: &NativeMarkdownTextKitEditorSurface,
    expected: &str,
) -> Result<(), String> {
    // Probe fixtures are bounded; production never materializes a complete document for comparison.
    let document = surface.document().borrow();
    let slice = document
        .materialize_viewport(0..document.utf16_len(), 0)
        .map_err(|error| format!("failed to inspect bounded probe text: {error}"))?;
    if slice.text == expected {
        // Exact equality proves the input operation produced only the expected canonical edit.
        return Ok(());
    }

    // Failure diagnostics include lengths and the first scalar offset without printing document contents.
    let mismatch = slice
        .text
        .chars()
        .zip(expected.chars())
        .position(|(actual, expected)| {
            // Stop at the first distinct scalar so failure reports remain bounded and text-free.
            actual != expected
        })
        .unwrap_or_else(|| slice.text.chars().count().min(expected.chars().count()));
    Err(format!(
        "the piece tree contains unexpected text (actual_utf16={}, expected_utf16={}, first_mismatch={mismatch})",
        slice.text.encode_utf16().count(),
        expected.encode_utf16().count(),
    ))
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Check bounded native attribute ranges and explicit underline/font semantics at one valid fixture index.
fn assert_native_markdown_attributes(
    surface: &NativeMarkdownTextKitEditorSurface,
    index: usize,
    expected_underline: bool,
    semantic_label: &str,
) -> Result<(), String> {
    // A valid canonical UTF-16 position is a prerequisite for the native attribute accessor.
    let document_length = surface.document().borrow().utf16_len();
    if index >= document_length {
        // Reject invalid fixture coordinates before invoking an attributed-string accessor that requires bounds.
        return Err(format!(
            "{semantic_label} attribute probe index is outside the document"
        ));
    }

    let mut effective_range = NSRange::new(0, 0);
    // SAFETY: index is inside the still-owned attributed string; effective_range is a live aligned
    // writable NSRange and the synchronous accessor retains no reference to its stack pointer.
    let attributes = unsafe {
        NSAttributedString::attributesAtIndex_effectiveRange(
            surface.backend.text_storage(),
            index,
            &mut effective_range,
        )
    };
    let effective_end = effective_range
        .location
        .checked_add(effective_range.length)
        .ok_or_else(|| format!("{semantic_label} attribute range overflowed"))?;
    if effective_range.length == 0
        || effective_end > document_length
        || effective_range.length > NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS
    {
        // Effective attributes must stay within both canonical document bounds and the presentation-window budget.
        return Err(format!(
            "{semantic_label} attribute range was not a non-empty bounded document range"
        ));
    }

    // SAFETY: these immutable AppKit attribute-name constants have process lifetime.
    let (font_key, underline_key) = unsafe { (NSFontAttributeName, NSUnderlineStyleAttributeName) };
    if attributes.objectForKey(font_key).is_none() {
        // Every accepted native semantic span needs an explicit usable font attribute.
        return Err(format!(
            "{semantic_label} attributes did not contain a font"
        ));
    }
    let has_underline = attributes.objectForKey(underline_key).is_some();
    if has_underline != expected_underline {
        // Underline is authorized only by explicit semantic presentation, not incidental link/rule syntax.
        return Err(format!(
            "{semantic_label} underline attribute presence did not match the presentation contract"
        ));
    }
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Verify required and forbidden Markdown semantics in the current bounded presentation query.
fn assert_native_markdown_styles(
    surface: &NativeMarkdownTextKitEditorSurface,
    index: usize,
    required: &[MarkdownPresentationStyles],
    forbidden: &[MarkdownPresentationStyles],
    semantic_label: &str,
) -> Result<(), String> {
    // Request this position through the same bounded presentation path used by native attribute reads.
    let _ = surface.backend.text_storage.attributes_at_index(index);
    let presentation = surface.backend.text_storage.ivars().presentation.borrow();
    let query = presentation
        .query(index)
        .ok_or_else(|| format!("{semantic_label} had no presentation query"))?;
    if required.iter().any(|style| {
        // Every requested semantic bit must exist in this independently queried presentation span.
        !query.styles.contains(*style)
    }) {
        // A matching display string alone cannot prove the requested semantic style was actually applied.
        return Err(format!(
            "{semantic_label} was missing a required Markdown presentation style"
        ));
    }
    if forbidden.iter().any(|style| {
        // A single forbidden semantic bit is enough to reject presentation overlap/precedence drift.
        query.styles.contains(*style)
    }) {
        // Overlapping parser markers must not leak unrelated heading/rule/underline styles into this span.
        return Err(format!(
            "{semantic_label} contained a forbidden Markdown presentation style"
        ));
    }
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Require delimiter parsing to recover the complete six-column semantic table and aligned source ranges.
fn assert_native_markdown_table_recovered(
    surface: &NativeMarkdownTextKitEditorSurface,
    delimiter_index: usize,
) -> Result<(), String> {
    // A delimiter query must recover its whole semantic table within the bounded presentation contract.
    let _ = surface
        .backend
        .text_storage
        .attributes_at_index(delimiter_index);
    let presentation = surface.backend.text_storage.ivars().presentation.borrow();
    let table = presentation
        .tables()
        .iter()
        .find(|table| table.delimiter_utf16_range.contains(&delimiter_index))
        .ok_or_else(|| "table delimiter query did not recover a complete table".to_owned())?;
    if table.rows.len() != 5 || table.cells.len() != 30 {
        // Delimiter recovery must keep the whole expected semantic grid instead of a truncated viewport fragment.
        return Err("table prototype did not retain the six-column fixture".to_owned());
    }
    if !table.rows[0].is_header || table.rows.iter().skip(1).any(|row| row.is_header) {
        // Exactly the first parsed row is a header; body rows must not inherit header semantics.
        return Err("table prototype did not preserve the header row".to_owned());
    }
    // This fixture deliberately combines left/center/right columns to expose semantic alignment drift.
    let expected_alignments = [
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Right,
        MarkdownTableAlignment::Left,
    ];
    for (cell_index, cell) in table.cells.iter().enumerate() {
        // Row-major semantic coordinates must repeat the declared alignment for each fixture column.
        if cell.column_index != cell_index % expected_alignments.len()
            || cell.alignment != expected_alignments[cell_index % expected_alignments.len()]
        {
            // Every row-major cell must preserve the alignment declared by its column delimiter.
            return Err("table prototype lost a column alignment".to_owned());
        }
    }
    if table.header_utf16_range.start < table.utf16_range.start
        || table.delimiter_utf16_range.end > table.utf16_range.end
    {
        // All component source ranges must remain confined to their containing semantic table.
        return Err("table prototype ranges escaped the recovered table".to_owned());
    }
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Check visible table row/cell geometry, column alignment, plain display text, and inline font traits.
fn assert_native_markdown_table_layout(
    surface: &NativeMarkdownTextKitEditorSurface,
) -> Result<(), String> {
    // 110仅建立原表几何并发起第一格请求，不在本事件推进Core请求。
    let text_view = &surface.backend.text_view;
    surface.backend.scroll_view.layoutSubtreeIfNeeded();
    let clip_view = surface.backend.scroll_view.contentView();
    clip_view.scrollToPoint(NSPoint::new(0.0, 0.0));
    surface
        .backend
        .scroll_view
        .reflectScrolledClipView(&clip_view);
    text_view.invalidate_visible_table_layout_cache();
    complete_native_markdown_probe_table_layout(text_view)?;
    let cache = text_view.ivars().visible_table_layout_cache.borrow();
    let table = cache
        .as_ref()
        .and_then(|cache| {
            // Inspect only the current bounded visible-layout snapshot; no body/native owner escapes.
            cache.tables.iter().find(|table| {
                // Match the independently known fixture identity/shape rather than first-table order.
                table.rows.len() == 5 && table.cells.len() == 30 && table.column_offsets.len() == 7
            })
        })
        .ok_or_else(|| {
            // Missing layout reports bounded viewport/shape metadata without accepting an empty cache.
            let summary = cache.as_ref().map_or_else(
                || "cache=none".to_owned(),
                |cache| {
                    // Copy current native geometry only on failure to locate the missing viewport table.
                    let visible_rect = text_view.visibleRect();
                    let text_origin = text_view.textContainerOrigin();
                    let probe_x = text_origin.x + 1.0;
                    let top_index = text_view.characterIndexForInsertionAtPoint(NSPoint::new(
                        probe_x,
                        visible_rect.origin.y + 1.0,
                    ));
                    let bottom_index = text_view.characterIndexForInsertionAtPoint(NSPoint::new(
                        probe_x,
                        visible_rect.origin.y + (visible_rect.size.height - 1.0).max(0.0),
                    ));
                    let tables = cache
                        .tables
                        .iter()
                        .map(|table| {
                            // Each bounded visible table contributes scalar dimensions only.
                            format!(
                                "rows={},cells={},offsets={}",
                                table.rows.len(),
                                table.cells.len(),
                                table.column_offsets.len()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(";");
                    format!(
                        "visible_rect={:?},bounds={:?},text_origin={:?},viewport={:?},presentation={:?},tables=[{tables}]",
                        visible_rect,
                        text_view.bounds(),
                        text_origin,
                        cache.viewport_utf16_window,
                        cache.utf16_window
                    ) + &format!(",top_index={top_index},bottom_index={bottom_index}")
                },
            );
            format!("the visible layout did not retain the six-column table: {summary}")
        })?;
    // Subpixel rendering may round by less than one point; larger discrepancies violate the shared grid.
    let geometry_matches = |actual: f64, expected: f64| {
        // Compare points in the same native coordinate space with a fixed explicit tolerance.
        (actual - expected).abs() <= 0.75
    };

    if !geometry_matches(table.column_offsets[0], 0.0)
        || !geometry_matches(
            *table.column_offsets.last().unwrap_or(&0.0),
            table.frame.size.width,
        )
        || table
            .column_offsets
            .windows(2)
            .any(|offsets| offsets[1] <= offsets[0])
    {
        // Non-monotonic or incomplete offsets would create overlapping cells or unallocated grid width.
        return Err("the table columns did not form a strictly increasing grid".to_owned());
    }
    if !geometry_matches(table.rows[0].frame.origin.y, table.frame.origin.y)
        || !geometry_matches(
            table
                .rows
                .last()
                .map_or(0.0, |row| row.frame.origin.y + row.frame.size.height),
            table.frame.origin.y + table.frame.size.height,
        )
    {
        // The table frame must cover all row bands exactly without clipping the first or final row.
        return Err("the table frame did not enclose its visible rows".to_owned());
    }

    // This fixture deliberately combines left/center/right columns to expose semantic alignment drift.
    let expected_alignments = [
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Right,
        MarkdownTableAlignment::Left,
    ];
    for (row_index, row) in table.rows.iter().enumerate() {
        // Each visible row must preserve semantic order and occupy a contiguous positive grid band.
        if row.row_index != row_index || row.is_header != (row_index == 0) {
            // Native row order/header identity must agree with the parsed row-major semantic model.
            return Err("the visible table rows lost their semantic identity".to_owned());
        }
        if !geometry_matches(row.frame.origin.x, table.frame.origin.x)
            || !geometry_matches(row.frame.size.width, table.frame.size.width)
            || row.frame.size.height <= 0.0
        {
            // Each row must use the common table origin/width and a positive visible height.
            return Err("a table row escaped the table grid".to_owned());
        }
        if row_index > 0 {
            // Adjacent rows must share an exact boundary; the header has no previous row to compare.
            let previous = &table.rows[row_index - 1];
            if !geometry_matches(
                previous.frame.origin.y + previous.frame.size.height,
                row.frame.origin.y,
            ) {
                // Adjacent rows must share their boundary to keep native hit testing and rendering consistent.
                return Err("the table rows contained a vertical gap or overlap".to_owned());
            }
        }

        let row_cells: Vec<&NativeMarkdownVisibleTableCellLayout> = table
            .cells
            .iter()
            .filter(|cell| {
                // The row's visible population must cover exactly the declared set of columns.
                cell.coordinate.row_index == row_index
            })
            .collect();
        if row_cells.len() != expected_alignments.len() {
            // A complete semantic row needs exactly one visible cell per declared fixture column.
            return Err("a visible table row did not contain six cells".to_owned());
        }
        for (column_index, expected_alignment) in expected_alignments.iter().enumerate() {
            // Every declared column must provide one cell with the exact shared row/column geometry.
            let cell = row_cells
                .iter()
                .copied()
                .find(|cell| {
                    // Resolve by semantic coordinate, independent of vector allocation order.
                    cell.coordinate.column_index == column_index
                })
                .ok_or_else(|| "a visible table cell coordinate was missing".to_owned())?;
            let expected_x = table.frame.origin.x + table.column_offsets[column_index];
            let expected_width =
                table.column_offsets[column_index + 1] - table.column_offsets[column_index];
            if cell.alignment != *expected_alignment
                || cell.is_header != row.is_header
                || !geometry_matches(cell.frame.origin.x, expected_x)
                || !geometry_matches(cell.frame.origin.y, row.frame.origin.y)
                || !geometry_matches(cell.frame.size.width, expected_width)
                || !geometry_matches(cell.frame.size.height, row.frame.size.height)
            {
                // Native cell geometry and semantic coordinates must agree with the shared row/column grid.
                return Err("a table cell did not occupy its exact grid slot".to_owned());
            }

            // Text alignment is geometric as well as semantic: verify the actual native text frame.
            let text_minimum_x = cell.text_frame.origin.x;
            let text_maximum_x = cell.text_frame.origin.x + cell.text_frame.size.width;
            let cell_maximum_x = cell.frame.origin.x + cell.frame.size.width;
            let alignment_is_geometric = match cell.alignment {
                // Left-aligned text starts at the shared horizontal padding.
                MarkdownTableAlignment::Left => geometry_matches(
                    text_minimum_x,
                    cell.frame.origin.x + NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING,
                ),
                // Center-aligned text shares the cell midpoint independent of glyph width.
                MarkdownTableAlignment::Center => geometry_matches(
                    text_minimum_x + cell.text_frame.size.width / 2.0,
                    cell.frame.origin.x + cell.frame.size.width / 2.0,
                ),
                // Right-aligned text ends at the shared trailing padding.
                MarkdownTableAlignment::Right => geometry_matches(
                    text_maximum_x,
                    cell_maximum_x - NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING,
                ),
            };
            if !alignment_is_geometric {
                // Declared alignment must affect actual glyph-frame placement, not only cached semantic metadata.
                return Err("a table cell text frame ignored its column alignment".to_owned());
            }
        }
    }

    // Resolve fixture coordinates without relying on incidental native cell allocation order.
    let table_cell = |row_index: usize, column_index: usize| {
        // A missing coordinate remains an explicit acceptance failure at each required use below.
        table.cells.iter().find(|cell| {
            // Both coordinates must agree before a native fixture cell can be inspected.
            cell.coordinate.row_index == row_index && cell.coordinate.column_index == column_index
        })
    };
    let expected_headers = [
        "任务编号 (ID)",
        "模块名称 (Module)",
        "运行状态 (Status)",
        "优先级 (Priority)",
        "响应时间 (Latency)",
        "详细说明 (Notes)",
    ];
    for (column_index, expected) in expected_headers.iter().enumerate() {
        // Native header display must strip source markers while preserving the independent fixture text.
        let cell = table_cell(0, column_index)
            .ok_or_else(|| "a table header cell was missing".to_owned())?;
        if native_markdown_probe_table_cell_text(cell) != *expected {
            // Preview cell display must remove source syntax while preserving header text.
            return Err("a table header retained Markdown source markers".to_owned());
        }
    }

    // Representative code, strong, emoji, and emphasis cells exercise plain display and font semantics.
    let first_id = table_cell(1, 0).ok_or_else(|| "the first ID cell was missing".to_owned())?;
    let first_module =
        table_cell(1, 1).ok_or_else(|| "the first module cell was missing".to_owned())?;
    let first_status =
        table_cell(1, 2).ok_or_else(|| "the first status cell was missing".to_owned())?;
    let last_note =
        table_cell(4, 5).ok_or_else(|| "the final notes cell was missing".to_owned())?;
    if native_markdown_probe_table_cell_text(first_id) != "#101"
        || native_markdown_probe_table_cell_text(first_module) != "用户鉴权中心 Auth"
        || native_markdown_probe_table_cell_text(first_status) != "🟢 正常"
        || native_markdown_probe_table_cell_text(last_note) != "生成文件大小约 12.5 MB"
    {
        // Representative native cell labels must equal the independently known parsed fixture contents.
        return Err("table display text did not match the parsed Markdown semantics".to_owned());
    }
    if first_status.attributed_text.length() != "🟢 正常".encode_utf16().count() {
        // Emoji display must preserve its surrogate length across the native attributed-string boundary.
        return Err("the table status cell lost its UTF-16 emoji length".to_owned());
    }

    assert_native_markdown_table_cell_font_trait(
        first_id,
        "#101",
        NSFontDescriptorSymbolicTraits::TraitMonoSpace,
        "inline-code ID",
    )?;
    assert_native_markdown_table_cell_font_trait(
        first_module,
        "用户鉴权中心",
        NSFontDescriptorSymbolicTraits::TraitBold,
        "strong module name",
    )?;
    assert_native_markdown_table_cell_font_trait(
        first_module,
        "Auth",
        NSFontDescriptorSymbolicTraits::TraitMonoSpace,
        "inline-code module tag",
    )?;
    assert_native_markdown_table_cell_font_trait(
        last_note,
        "12.5 MB",
        NSFontDescriptorSymbolicTraits::TraitItalic,
        "emphasized note size",
    )?;
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Require a complete native grid for the final four-column table after scrolling to the document tail.
fn assert_native_markdown_eof_table_layout(
    surface: &NativeMarkdownTextKitEditorSurface,
    table_utf16_start: usize,
) -> Result<(), String> {
    // Select the tail table by semantic identity and exact expected shape, never by viewport order alone.
    let text_view = &surface.backend.text_view;
    let cache = text_view.ivars().visible_table_layout_cache.borrow();
    let table = cache
        .as_ref()
        .and_then(|cache| {
            // Inspect only the current bounded visible-layout snapshot; no body/native owner escapes.
            cache.tables.iter().find(|table| {
                // Match the independently known fixture identity/shape rather than first-table order.
                table.table_utf16_start == table_utf16_start
                    && table.rows.len() == 5
                    && table.cells.len() == 20
                    && table.column_offsets.len() == 5
            })
        })
        .ok_or_else(|| {
            // Missing layout reports bounded viewport/shape metadata without accepting an empty cache.
            let summary = cache.as_ref().map_or_else(
                || "cache=none".to_owned(),
                |cache| {
                    // A bounded shape summary identifies tail-table omission without printing cell text.
                    format!(
                        "viewport={:?},presentation={:?},tables={:?}",
                        cache.viewport_utf16_window,
                        cache.utf16_window,
                        cache
                            .tables
                            .iter()
                            .map(|table| (
                                table.table_utf16_start,
                                table.rows.len(),
                                table.cells.len()
                            ))
                            .collect::<Vec<_>>()
                    )
                },
            );
            format!("the EOF table was not materialized as a native grid: {summary}")
        })?;

    if table.rows.iter().any(|row| {
        // Every bounded EOF row needs the full expected four-column visible population.
        table
            .cells
            .iter()
            .filter(|cell| cell.coordinate.row_index == row.row_index)
            .count()
            != 4
    }) {
        // The tail viewport must materialize each complete row rather than omitting off-screen-source cells.
        return Err("an EOF table row did not contain four native cells".to_owned());
    }
    let header = table
        .cells
        .iter()
        .find(|cell| cell.coordinate.row_index == 0 && cell.coordinate.column_index == 0)
        .ok_or_else(|| "the EOF table header cell was missing".to_owned())?;
    if native_markdown_probe_table_cell_text(header) != "阶段" {
        // The final table uses the same source-stripping contract as the initial viewport table.
        return Err("the EOF table header retained Markdown source markers".to_owned());
    }
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Copy the bounded displayed cell string within a local autorelease pool for semantic assertions.
fn native_markdown_probe_table_cell_text(cell: &NativeMarkdownVisibleTableCellLayout) -> String {
    autoreleasepool(|pool| {
        // Cell text is a bounded presentation value; no borrowed Foundation bytes survive this scope.
        let string = cell.attributed_text.string();
        // SAFETY: NSString stays retained and immutable while the pool-scoped UTF-8 borrow
        // is copied into an owned Rust string; no borrowed byte can escape the autorelease pool.
        unsafe { string.to_str(pool).to_owned() }
    })
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Resolve a fixture substring and verify the native font trait used to present that semantic fragment.
fn assert_native_markdown_table_cell_font_trait(
    cell: &NativeMarkdownVisibleTableCellLayout,
    fragment: &str,
    required_trait: NSFontDescriptorSymbolicTraits,
    semantic_label: &str,
) -> Result<(), String> {
    // Locate the independently known semantic fragment before converting its scalar boundary to UTF-16.
    let display_text = native_markdown_probe_table_cell_text(cell);
    let byte_offset = display_text
        .find(fragment)
        .ok_or_else(|| format!("{semantic_label} was absent from the rendered cell"))?;
    let utf16_offset = display_text[..byte_offset].encode_utf16().count();
    let mut effective_range = NSRange::new(0, 0);
    // SAFETY: fragment lookup establishes an in-bounds scalar boundary in this same cell string;
    // effective_range is an aligned live writable NSRange used only by the synchronous accessor.
    let attributes = unsafe {
        NSAttributedString::attributesAtIndex_effectiveRange(
            &cell.attributed_text,
            utf16_offset,
            &mut effective_range,
        )
    };
    // SAFETY: AppKit's immutable font attribute-name constant remains valid for process lifetime.
    let font_key = unsafe { NSFontAttributeName };
    let font = attributes
        .objectForKey(font_key)
        .ok_or_else(|| format!("{semantic_label} had no font attribute"))?
        .downcast::<NSFont>()
        .map_err(|_| format!("{semantic_label} font attribute had the wrong type"))?;
    if !font
        .fontDescriptor()
        .symbolicTraits()
        .contains(required_trait)
    {
        // Parsed inline semantics must survive conversion into concrete AppKit font descriptor traits.
        return Err(format!("{semantic_label} lost its native font trait"));
    }
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Exercise reentrancy/IME scheduling guards and restore original scalar state on every outcome.
fn assert_native_markdown_table_input_guards(
    surface: &NativeMarkdownTextKitEditorSurface,
) -> Result<(), String> {
    // Save scheduling/composition scalars so this adversarial guard exercise cannot alter later tests.
    let text_view = &surface.backend.text_view;
    let ivars = text_view.ivars();
    let original_input_depth = ivars.textkit_input_depth.get();
    let original_marked_range = ivars.marked_text_range.get();
    let original_scheduled = ivars.table_layout_update_scheduled.get();

    let result = (|| {
        // Active TextKit input must refuse derived layout work even with an explicitly missing cache.
        text_view.invalidate_visible_table_layout_cache();
        ivars.table_layout_update_scheduled.set(false);
        ivars
            .textkit_input_depth
            .set(original_input_depth.saturating_add(1));
        text_view.schedule_visible_table_layout_update();
        if ivars.table_layout_update_scheduled.get()
            || ivars.visible_table_layout_cache.borrow().is_some()
        {
            // Reentrant input must not allocate or publish derived table geometry during canonical mutation.
            return Err("table layout work entered an active TextKit input transaction".to_owned());
        }

        // IME marked text independently blocks scheduling after the input transaction itself has ended.
        ivars.textkit_input_depth.set(original_input_depth);
        ivars.marked_text_range.set(Some(NSRange::new(1, 1)));
        text_view.schedule_visible_table_layout_update();
        if ivars.table_layout_update_scheduled.get()
            || ivars.visible_table_layout_cache.borrow().is_some()
        {
            // Marked composition owns its transient range and blocks table rebuilding until commit/cancel.
            return Err("table layout work entered an active IME composition".to_owned());
        }

        // Clearing both guards must allow the unchanged product transaction body to recover.
        ivars.marked_text_range.set(original_marked_range);
        ivars.table_layout_update_scheduled.set(false);
        complete_native_markdown_probe_table_layout(text_view)?;
        if !ivars
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| !cache.tables.is_empty())
        {
            // Clearing input guards must resume the real scheduled layout path rather than leaving it permanently suppressed.
            return Err("table layout did not recover after the input guards cleared".to_owned());
        }
        Ok(())
    })();

    // Restore original scalar guards on both success and any fallible assertion/timeout above.
    ivars.textkit_input_depth.set(original_input_depth);
    ivars.marked_text_range.set(original_marked_range);
    ivars.table_layout_update_scheduled.set(original_scheduled);
    if result.is_err()
        && original_input_depth == 0
        && !original_marked_range.is_some_and(|range| range.length != 0)
    {
        // Failed adversarial work must not leave an apparently valid cache for subsequent assertions.
        text_view.invalidate_visible_table_layout_cache();
    }
    result
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// 沿正式模式捕获入口观察原纯收据，不以调用后的slot存在性推测成功。
fn capture_native_markdown_probe_mode_receipt(
    surface: &NativeMarkdownTextKitEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    mode: MarkdownPresentationMode,
) -> Result<Rc<super::table_layout::NativeMarkdownModeViewportRequest>, String> {
    // 原模式票据工厂会调用Native；使用与正式popup相同的payload屏障覆盖捕获和全部局部claim析构。
    let _payload = PayloadUseGuard::enter(&surface.backend.text_view.ivars().payload_use_depth);
    let transition = surface
        .backend
        .text_view
        .capture_mode_viewport_transition(controller, &surface.backend.text_storage, mode)
        .map_err(|_| "模式回归无法捕获原动作源位置".to_owned())?;
    // 捕获已由原具体入口完成；这里只借原 Capturing 槽的同一收据，不新增诊断访问口。
    let observation = surface
        .backend
        .text_view
        .authorized_mode_action(controller, &surface.backend.text_storage, mode)
        .ok_or("模式参考图捕获后没有原 Capturing 收据")?;
    drop(transition); // 返回值只Rc共享Weak正文/标量；不把额外Native强引用带入实际模式提交或关闭。
    let original = surface.backend.text_view.authorized_mode_action(
        controller,
        &surface.backend.text_storage,
        mode,
    );
    if !original
        .as_ref()
        .is_some_and(|original| Rc::ptr_eq(original, &observation))
    {
        // Native局部析构也可撤销原动作；原票丢失就拒绝，不能让下一入口重新捕获较新权限。
        return Err("模式回归在原动作捕获结束时已失去权限".into());
    }
    Ok(observation)
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// 原真实单格收据贯穿激活、提交、Tab、取消与模式恢复。
#[derive(Clone)]
struct ProbeTableCellExercise {
    table_utf16_start: usize,
    receipt: Rc<NativeMarkdownTableCellReceipt>,
    first_source_range: Option<std::ops::Range<usize>>,
}

fn probe_table_cell_first_ready(_: NativeMarkdownTableCellOutcome) {
    crate::runtime_probe_application::post_runtime_table_cell_terminal(111);
}
fn probe_table_cell_second_ready(_: NativeMarkdownTableCellOutcome) {
    crate::runtime_probe_application::post_runtime_table_cell_terminal(112);
}
fn probe_table_cell_mode_ready(_: NativeMarkdownTableCellOutcome) {
    crate::runtime_probe_application::post_runtime_table_cell_terminal(113);
}

impl NativeMarkdownRuntimeProbeSession {
    /// 每个阶段只消费原真实收据；Core与几何仍由产品既有executor推进，不在probe里循环。
    pub(crate) fn exercise_table_stage(&self, stage: i16) -> Result<(), String> {
        let admitted = if stage == 110 {
            if self.table_owner_lease.borrow().is_some() {
                Err("表格测试owner已借出".to_owned())
            } else {
                // 仅此处建立一次测试图owner；各request的g/input/width票仍独立，不由lease续期。
                ProbeEditingOwnerLease::new(&self.controller).map(|lease| {
                    *self.table_owner_lease.borrow_mut() = Some(lease);
                })
            }
        } else {
            let expected = std::ptr::from_ref(&*self.controller).addr();
            if self
                .table_owner_lease
                .borrow()
                .as_ref()
                .is_some_and(|lease| lease.0 == expected)
                && BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get) == Some(expected)
            {
                Ok(())
            } else {
                Err("表格测试原owner身份已改变".into())
            }
        };
        let result = admitted.and_then(|()| self.exercise_table_stage_inner(stage));
        if result.is_err() || stage == 113 {
            if result.is_err() {
                self.surface
                    .backend
                    .text_view
                    .ivars()
                    .pending_table_cell_activation
                    .set(None);
            }
            // 内层所有Native claims已结束；正常终点或错误都归还同一借出，不留到102 close。
            let previous = self.table_owner_lease.borrow_mut().take();
            drop(previous);
        }
        result
    }

    fn exercise_table_stage_inner(&self, stage: i16) -> Result<(), String> {
        super::probe_call_diagnostic::allocation_audit_point("table_stage_begin");
        let surface = &self.surface;
        let controller = &self.controller;
        let text_view = &surface.backend.text_view;
        if stage == 110 {
            // 110仅建立原表几何并发起第一格请求，不在本事件推进Core请求。
            let text_view = &surface.backend.text_view;
            surface.backend.scroll_view.layoutSubtreeIfNeeded();
            super::probe_call_diagnostic::allocation_audit_point("table_110_layout_return");
            let clip_view = surface.backend.scroll_view.contentView();
            clip_view.scrollToPoint(NSPoint::new(0.0, 0.0));
            surface
                .backend
                .scroll_view
                .reflectScrolledClipView(&clip_view);
            super::probe_call_diagnostic::allocation_audit_point("table_110_scroll_return");
            text_view.invalidate_visible_table_layout_cache();
            complete_native_markdown_probe_table_layout(text_view)?;
            super::probe_call_diagnostic::allocation_audit_point("table_110_geometry_return");
            // 槽内只取Rc；有限cell属性复制及cache析构都在Ref借用外。
            let (table_utf16_start, first_cell) = {
                let cache = text_view
                    .ivars()
                    .visible_table_layout_cache
                    .borrow()
                    .as_ref()
                    .map(Rc::clone);
                let table = cache
            .as_ref()
            .and_then(|cache| cache.tables.iter().find(|table| table.cells.len() == 30))
            .ok_or_else(|| {
                let summary = cache.as_ref().map_or_else(
                    || "cache=none".to_owned(),
                    |cache| {
                        let visible_rect = text_view.visibleRect();
                        let text_origin = text_view.textContainerOrigin();
                        format!(
                            "visible_rect={visible_rect:?},text_origin={text_origin:?},viewport={:?},presentation={:?},tables={:?}",
                            cache.viewport_utf16_window,
                            cache.utf16_window,
                            cache
                                .tables
                                .iter()
                                .map(|table| (table.rows.len(), table.cells.len()))
                                .collect::<Vec<_>>()
                        )
                    },
                );
                format!("the editable six-column table was not visible: {summary}")
            })?;
                let first_cell = table
                    .cells
                    .iter()
                    .find(|cell| {
                        cell.coordinate.row_index == 1 && cell.coordinate.column_index == 0
                    })
                    .cloned()
                    .ok_or_else(|| "the first editable body cell was missing".to_owned())?;
                (table.table_utf16_start, first_cell)
            };
            if text_view.existing_table_cell_editor().is_some() {
                // 首次显式激活之前不能分配可复用表格字段。
                return Err(
                    "the table cell editor was allocated before first activation".to_owned(),
                );
            }

            let receipt = text_view.probe_prepare_table_cell_receipt(
                surface.document(),
                first_cell.coordinate,
                probe_table_cell_first_ready,
            )?;
            super::probe_call_diagnostic::allocation_audit_point("table_110_receipt_return");
            if !text_view.probe_begin_table_cell_editing(first_cell, Rc::clone(&receipt)) {
                return Err("首格原请求未被接受".into());
            }
            super::probe_call_diagnostic::allocation_audit_point("table_110_request_return");
            #[cfg(feature = "native-raster-surface-probe")]
            // 原同步请求已返回且文档借用已结束；只观察已有窗口/图层，不推进布局或激活。
            super::probe_call_diagnostic::layer_audit_point("table_110_request_return", text_view);
            self.table_cell_exercise
                .replace(Some(ProbeTableCellExercise {
                    table_utf16_start,
                    receipt,
                    first_source_range: None,
                }));
            return Ok(());
        }
        let state = self
            .table_cell_exercise
            .borrow()
            .clone()
            .ok_or("表格回归原请求缺失")?;
        if state.receipt.outcome() != Some(NativeMarkdownTableCellOutcome::Activated)
            || !state.receipt.current_for(text_view)
        {
            return Err(format!(
                "表格激活原收据未成功或已撤权：{:?}",
                state.receipt.outcome()
            ));
        }
        let table_utf16_start = state.table_utf16_start;
        let editor =
            native_markdown_table_intent_step(&|| state.receipt.current_for(text_view), || {
                text_view.existing_table_cell_editor()
            })
            .map_err(|_| "原表格字段权限已改变")?
            .ok_or("表格真实字段缺失")?;
        match stage {
            111 => {
                // 111消费真实首格Activated后，才允许检查复用字段与草稿拒绝。
                // 使用已声明合成超限草稿检查失败保留；不复制实际文档。
                // 预算拒绝必须保留字段、活动身份与原canonical代次。
                let generation_before_rejection = surface.document().borrow().generation();
                super::probe_call_diagnostic::allocation_audit_point("table_before_oversized");
                let oversized = NSString::from_str(&"x".repeat(2 * 1_024 * 1_024));
                super::probe_call_diagnostic::allocation_audit_point("table_oversized_created");
                editor.setStringValue(&oversized);
                super::probe_call_diagnostic::allocation_audit_point("table_oversized_set_return");
                if text_view.commit_active_table_cell().is_ok()
                    || editor.isHidden()
                    || editor.stringValue().length() != oversized.length()
                    || text_view.ivars().active_table_cell.borrow().is_none()
                    || surface.document().borrow().generation() != generation_before_rejection
                {
                    // 事务失败不得清草稿、改变历史或释放当前单格所有权。
                    return Err(
                        "a rejected table edit lost its visible draft or changed canonical text"
                            .into(),
                    );
                }
                super::probe_call_diagnostic::allocation_audit_point("table_commit_rejected");
                // 同一真实超限草稿也必须拒绝切模式，不只验证独立cell提交函数。
                // 此处继续沿实有controller和完整产品入口，不能用弱化的probe专用实现。
                if surface
                    .backend
                    .set_presentation_mode(MarkdownPresentationMode::Source, controller)
                    .is_ok()
                    || surface.backend.text_storage.presentation_mode()
                        != MarkdownPresentationMode::InlinePreview
                    || editor.isHidden()
                    || text_view.ivars().active_table_cell.borrow().is_none()
                    || surface.document().borrow().generation() != generation_before_rejection
                {
                    return Err("超限表格草稿在模式切换中被丢失或改变了展示模式".into());
                }
                super::probe_call_diagnostic::allocation_audit_point("table_mode_rejected");
                drop(oversized);
                // 恢复有限草稿后发同一生产Next，后继事件只等其原请求终点。
                editor.setStringValue(&NSString::from_str("#101-updated"));
                super::probe_call_diagnostic::allocation_audit_point("table_draft_restored");
                let coordinate = NativeMarkdownTableCellCoordinate {
                    table_utf16_start: state.table_utf16_start,
                    row_index: 1,
                    column_index: 1,
                };
                let receipt = text_view.probe_prepare_table_cell_receipt(
                    surface.document(),
                    coordinate,
                    probe_table_cell_second_ready,
                )?;
                text_view.probe_navigate_active_table_cell(
                    NativeMarkdownTableNavigation::Next,
                    Rc::clone(&receipt),
                );
                self.table_cell_exercise
                    .replace(Some(ProbeTableCellExercise { receipt, ..state }));

                Ok(())
            }
            112 => {
                let target_activated = state.receipt.outcome()
                    == Some(NativeMarkdownTableCellOutcome::Activated)
                    && !text_view.ivars().table_layout_update_scheduled.get()
                    && text_view
                        .ivars()
                        .pending_table_cell_activation
                        .get()
                        .is_none()
                    && text_view
                        .ivars()
                        .active_table_cell
                        .borrow()
                        .as_ref()
                        .is_some_and(|active| {
                            // 原第一行首格的后继必须为同一绝对行第1列。
                            active.coordinate.row_index == 1 && active.coordinate.column_index == 1
                        });
                if !target_activated {
                    // 只有原提交及确切后继字段都已完成，才能继续取消和模式回归。
                    return Err(
                        "Tab navigation did not activate the next table cell after its transaction"
                            .to_owned(),
                    );
                }
                let active = text_view
                    .ivars()
                    .active_table_cell
                    .borrow()
                    .clone()
                    .ok_or_else(|| "the next table cell had no active edit state".to_owned())?;
                if active.coordinate.row_index != 1 || active.coordinate.column_index != 1 {
                    // 不能用任意可见字段冒充原导航的精确行列。
                    return Err("Tab navigation did not follow spreadsheet row order".to_owned());
                }

                // 只消费激活本格的同一Ready；该有限行已包含两列，不能重新复制整表作oracle。
                let text_storage = &surface.backend.text_storage;
                let (first_source_range, second_source_range) = {
                    let active = text_view.ivars().active_table_cell.borrow();
                    let authority = active
                        .as_ref()
                        .and_then(|active| active.authority.as_ref())
                        .ok_or("后继格缺少原Ready权限")?;
                    let document = surface.document().borrow();
                    let window = authority
                        .window
                        .presentation_for(&document)
                        .map_err(|_| "后继格Ready已失效")?;
                    use omni_markdown_core::markdown_viewport_presentation::table_window::MarkdownTableWindowCellSource;
                    let source =
                        |column| match &window.cell(1, column).ok_or("后继格缺少逻辑列")?.source
                        {
                            MarkdownTableWindowCellSource::Present(range) => {
                                Ok::<_, String>(range.clone())
                            }
                            MarkdownTableWindowCellSource::ImplicitEmpty {
                                insertion_anchor_utf16,
                            } => Ok(*insertion_anchor_utf16..*insertion_anchor_utf16),
                        };
                    (source(0)?, source(1)?)
                };
                if text_storage
                    .table_cell_source(&first_source_range)
                    .as_deref()
                    != Some("#101-updated")
                {
                    // 字段文字变化不能代替canonical真实编辑。
                    return Err("the committed table cell did not update the piece tree".to_owned());
                }
                // Escape须丢弃第二格草稿并清掉全部当前与待激活状态。
                let original_second_source =
                    text_storage
                        .table_cell_source(&second_source_range)
                        .ok_or_else(|| "the second table cell source was unavailable".to_owned())?;
                editor.setStringValue(&NSString::from_str("must-not-be-committed"));
                text_view.cancel_active_table_cell();
                if text_storage
                    .table_cell_source(&second_source_range)
                    .as_deref()
                    != Some(original_second_source.as_str())
                {
                    // 取消只影响草稿；该格canonical源必须逐值保持。
                    return Err("Escape committed a cancelled table cell edit".to_owned());
                }
                if text_view.ivars().active_table_cell.borrow().is_some()
                    || text_view
                        .ivars()
                        .pending_table_cell_activation
                        .get()
                        .is_some()
                    || !editor.isHidden()
                    || editor.stringValue().length() != 0
                {
                    // 隐藏后的复用字段不得保留文字或当前/待激活坐标。
                    return Err("the cancelled table cell editor retained active state".to_owned());
                }

                // 已有cell继续复用同一个字段；真实模式动作必须提交一次草稿并维持源位置。
                complete_native_markdown_probe_table_layout(text_view)?;
                let mode_cell = {
                    let cache = text_view
                        .ivars()
                        .visible_table_layout_cache
                        .borrow()
                        .as_ref()
                        .map(Rc::clone);
                    cache
                        .as_ref()
                        .and_then(|cache| {
                            cache
                                .tables
                                .iter()
                                .find(|table| table.table_utf16_start == table_utf16_start)
                        })
                        .and_then(|table| {
                            table.cells.iter().find(|cell| {
                                cell.coordinate.row_index == 1 && cell.coordinate.column_index == 0
                            })
                        })
                        .cloned()
                        .ok_or("模式切换回归缺少当前第一格几何")?
                };

                let receipt = text_view.probe_prepare_table_cell_receipt(
                    surface.document(),
                    mode_cell.coordinate,
                    probe_table_cell_mode_ready,
                )?;
                if !text_view.probe_begin_table_cell_editing(mode_cell, Rc::clone(&receipt)) {
                    return Err("模式格原激活请求未被接受".into());
                }
                self.table_cell_exercise
                    .replace(Some(ProbeTableCellExercise {
                        receipt,
                        first_source_range: Some(first_source_range),
                        ..state
                    }));
                Ok(())
            }
            113 => {
                let text_storage = &surface.backend.text_storage;
                let first_source_range = state
                    .first_source_range
                    .clone()
                    .ok_or("模式格缺少原提交前源范围")?;
                editor.setStringValue(&NSString::from_str("#101-mode-draft"));
                let before_mode_generation = surface.document().borrow().generation();
                // 正式popup也在首Native前捕获同一Capturing票；先取得纯观察收据，不能事后猜slot为空的原因。
                let mode_request = capture_native_markdown_probe_mode_receipt(
                    surface,
                    controller,
                    MarkdownPresentationMode::Source,
                )?;
                surface
                    .backend
                    .set_presentation_mode(MarkdownPresentationMode::Source, controller)?;
                complete_native_markdown_probe_table_layout(text_view)?;
                if mode_request.completed() != Some(true)
                    || surface.document().borrow().generation() != before_mode_generation + 1
                    || surface.backend.text_storage.presentation_mode()
                        != MarkdownPresentationMode::Source
                    || !surface.backend.scroll_view.rulersVisible()
                    || !editor.isHidden()
                    || text_view.ivars().active_table_cell.borrow().is_some()
                {
                    return Err(format!(
                        "活cell切换Source未完成一次提交或真实源位置认证：completion={:?} phase={}",
                        mode_request.completed(),
                        mode_request.completion_phase()
                    ));
                }
                drop(mode_request); // 只保Weak/标量的测试票据也在关闭前结束。
                let inserted_units = "#101-mode-draft".encode_utf16().count();
                if text_storage
                    .table_cell_source(
                        &(first_source_range.start..first_source_range.start + inserted_units),
                    )
                    .as_deref()
                    != Some("#101-mode-draft")
                {
                    return Err("模式切换没有提交当前cell的真实草稿".into());
                }
                let input =
                    ProtocolObject::<dyn NSTextInputClient>::from_ref(surface.backend.text_view());
                // SAFETY: 活主线程NSTextView实现原产品undo:，无借出的Doc或Native临时参数跨调用。
                unsafe {
                    input.doCommandBySelector(sel!(undo:));
                }
                if text_storage
                    .table_cell_source(&first_source_range)
                    .as_deref()
                    != Some("#101-updated")
                {
                    return Err("模式切换的cell提交未由一次真实Undo恢复".into());
                }
                complete_native_markdown_probe_table_layout(text_view)?;
                let mode_request = capture_native_markdown_probe_mode_receipt(
                    surface,
                    controller,
                    MarkdownPresentationMode::InlinePreview,
                )?;
                surface
                    .backend
                    .set_presentation_mode(MarkdownPresentationMode::InlinePreview, controller)?;
                complete_native_markdown_probe_table_layout(text_view)?;
                if mode_request.completed() != Some(true)
                    || surface.backend.scroll_view.rulersVisible()
                {
                    return Err(format!(
                        "Source返回Preview未完成真实源位置认证：completion={:?} phase={}",
                        mode_request.completed(),
                        mode_request.completion_phase()
                    ));
                }

                // 先保留110的首次字段分配门槛；此处才验插表实现对既有字段的真实复用与一次 Undo。
                toolbar::verify(surface)?;
                complete_native_markdown_probe_layout_transaction(
                    &self.application,
                    &self.panel,
                    &self.container_view,
                    surface,
                )?;
                let observations = capture_probe_paragraph_observations(surface);
                let previous = self.paragraph_observations.replace(Some(observations));
                drop(previous);
                let previous = self.table_cell_exercise.borrow_mut().take();
                drop(previous);
                Ok(())
            }
            _ => Err("未知表格原请求续段".into()),
        }
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Require the native string primitive to agree with every UTF-16 code unit of the small probe document.
fn assert_textkit_matches_piece_tree(
    surface: &NativeMarkdownTextKitEditorSurface,
) -> Result<(), String> {
    // Borrow the canonical-backed NSString primitive without asking NSTextView for a legacy layout graph.
    let textkit_string = surface.backend.text_storage().string();
    let document = surface.document().borrow();
    if textkit_string.len_utf16() != document.utf16_len() {
        // A native primitive length mismatch would invalidate every downstream selection/range calculation.
        return Err("TextKit and the piece tree disagree on UTF-16 length".to_owned());
    }
    for index in 0..document.utf16_len() {
        // Exhaustive code-unit comparison is bounded by the small synthetic fixture, including surrogates.
        if textkit_string.characterAtIndex(index)
            != document
                .utf16_code_unit_at(index)
                .map_err(|error| format!("failed to inspect probe code unit: {error}"))?
        {
            // The attributed-string primitive must expose the exact canonical code units, including surrogate halves.
            return Err("TextKit and the piece tree disagree on a UTF-16 code unit".to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
mod memory_find_expectation_tests {
    use super::memory_expected_next_match;

    #[test]
    fn next_is_fixed_from_original_selection_within_finite_prefix() {
        let matches = [10..12, 20..22, 30..32];
        assert_eq!(
            memory_expected_next_match(&matches, false, &(0..0)),
            Some(10..12)
        );
        assert_eq!(
            memory_expected_next_match(&matches, false, &(10..12)),
            Some(20..22)
        );
        assert_eq!(
            memory_expected_next_match(&matches, false, &(20..22)),
            Some(30..32)
        );
        assert_eq!(memory_expected_next_match(&matches, false, &(30..32)), None);
        assert_eq!(memory_expected_next_match(&matches, false, &(40..40)), None);
        assert_eq!(
            memory_expected_next_match(std::slice::from_ref(&matches[0]), true, &(10..12)),
            Some(10..12)
        );
    }
}
