//! 正式 Host 原窗口上的真实中文组合、候选框和跨视口历史；不拥有应用事件循环。

use super::*;
use crate::native_surface::paged_body::{
    NativeMarkdownPagedBodyInputObserver, NativeMarkdownPagedBodySourceTarget,
};
use crate::native_surface::paged_input::NativeMarkdownPagedInputCommit;
use crate::native_surface::paged_text_view::NativeMarkdownPagedTextView;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSWindow};
use objc2_foundation::NSTimer;

mod input;
mod validation;
use input::{FocusNotifications, SoftwareInput};

// 两个原生入口共用四 MiB 原文和同一远端位置；只有合成目录允许安装此样本。
const INSERTION: usize = 70_001;
mod native_types {
    pub(super) use super::{
        NativeMarkdownPagedBody, NativeMarkdownPagedBodyPaintObserver,
        NativeMarkdownPagedInputCommit, NativeMarkdownPagedInputState, NativeMarkdownPagedSceneKey,
        NativeMarkdownPagedTextView,
    };
}

/// Host 只安排真实动作后的有限事件；每个终点仍由插件的实际完整帧或系统提交决定。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownHostedImeStep {
    VerifyOpen,
    VerifyTarget,
    BeginInput,
    VerifyCommit,
    VerifyUndo,
    VerifyRedo,
    VerifyRestoreSave,
}

impl NativeMarkdownHostedImeStep {
    pub const ALL: &'static [Self] = &[
        Self::VerifyOpen,
        Self::VerifyTarget,
        Self::BeginInput,
        Self::VerifyCommit,
        Self::VerifyUndo,
        Self::VerifyRedo,
        Self::VerifyRestoreSave,
    ];

    /// 输入阶段分别保原焦点与组合的三十秒期限；其他动作仍只有原三秒完整帧期限。
    pub fn deadline(self) -> Duration {
        if self == Self::VerifyCommit {
            Duration::from_secs(60)
        } else {
            Duration::from_secs(3)
        }
    }
}

/// 分块写出已知 UTF-8 原文；不建立整篇 String、正文数组或第二张文档树。
pub fn write_native_markdown_ime_fixture(path: &std::path::Path) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "无法创建独占的中文输入法合成原文")?;
    file.write_all(b"X").map_err(|_| "无法写入合成原文首字")?;
    let mut block = [0u8; 4095];
    for scalar in block.chunks_exact_mut(3) {
        scalar.copy_from_slice("中".as_bytes());
    }
    let mut remaining = validation::ORIGINAL_UTF16 - 1;
    while remaining != 0 {
        let count = remaining.min(block.len() / 3);
        file.write_all(&block[..count * 3])
            .map_err(|_| "无法写入合成原文分块")?;
        remaining -= count;
    }
    file.sync_all().map_err(|_| "无法同步合成原文".to_owned())
}

/// 只保当前实际发布的弱场景票；没有像素、Reader、字体或待执行的 Native 动作。
struct Frame {
    key: RefCell<Option<(NativeMarkdownPagedSceneKey, usize)>>,
    signal: ProbeTerminalObserver,
}

impl NativeMarkdownPagedBodyPaintObserver for Frame {
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        *self.key.borrow_mut() = Some((key.clone(), paints));
        self.signal.notify(); // Body 已结束自己的借用；Host 回调只排原后继事件。
    }
}

/// 当前生产会话的弱观察；窗口、Doc、SDK、字体和映射仍完全由正式组合根拥有。
pub struct NativeMarkdownHostedImeProbe {
    path: std::path::PathBuf,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    document: WeakMarkdownDocument,
    controller: Weak<NativeMarkdownController>,
    view: Weak<NativeMarkdownPagedTextView>,
    panel: Weak<NativeMarkdownPanel>,
    next: Cell<usize>,
    frame: RefCell<Option<Rc<Frame>>>,
    target: RefCell<Option<NativeMarkdownPagedBodySourceTarget>>,
    composition: RefCell<Option<Rc<Composition>>>,
    committed: RefCell<Option<Vec<u16>>>,
    completed: Cell<bool>,
}

impl NativeMarkdownHostedImeProbe {
    /// 只绑定当前真实 Host 打开的独占样本；不会创建窗口或从目录重新选择另一个文稿。
    pub fn observe_active_document(path: &std::path::Path) -> Result<Self, String> {
        let path = path
            .canonicalize()
            .map_err(|_| "无法解析中文输入法合成路径")?;
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("原窗口没有正式分页正文")?;
            {
                let document = editor.document().borrow();
                if document.path().canonicalize().ok().as_ref() != Some(&path)
                    || document.generation() != 0
                    || document.can_undo()
                    || document.can_redo()
                    || !document.original_file_identity_is_unchanged()
                {
                    return Err("中文验收没有取得原合成文稿的初始身份".into());
                }
            }
            let probe = Self {
                path,
                body: Rc::downgrade(&editor.body),
                document: Rc::downgrade(editor.document()),
                controller: Weak::from_retained(&surface.controller),
                view: Weak::from(&**editor.text_view),
                panel: Weak::from_retained(&surface.panel),
                next: Cell::new(0),
                frame: RefCell::new(None),
                target: RefCell::new(None),
                composition: RefCell::new(None),
                committed: RefCell::new(None),
                completed: Cell::new(false),
            };
            validation::verify(&probe.document, &[])?;
            Ok(probe)
        })
    }

    /// 原弱文稿与正文必须仍是当前会话；相同文件名或后来的同静态插件不能接管输入。
    fn current(
        &self,
    ) -> Result<
        (
            Rc<NativeMarkdownPagedBody>,
            Retained<NativeMarkdownPagedTextView>,
        ),
        String,
    > {
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("中文验收原正文已经关闭")?;
            let body = self.body.upgrade().ok_or("中文验收原 Body 已释放")?;
            let document = self.document.upgrade().ok_or("中文验收原 Doc 已释放")?;
            let controller = self.controller.load().ok_or("中文验收原控制器已释放")?;
            let view = self.view.load().ok_or("中文验收原输入客户已释放")?;
            if !Rc::ptr_eq(&body, &editor.body)
                || !Rc::ptr_eq(&document, editor.document())
                || !std::ptr::eq(&*controller, &*surface.controller)
                || !std::ptr::eq(&*view, &**editor.text_view)
            {
                return Err("中文验收原会话身份已被替换".into());
            }
            Ok((body, view))
        })
    }

    /// 动作前订阅下一次真实 CPU；不标脏、不强制绘制，也不使用旧像素满足后继动作。
    fn arm_frame(&self, body: &NativeMarkdownPagedBody) -> Result<(), String> {
        let frame = Rc::new(Frame {
            key: RefCell::new(None),
            signal: ProbeTerminalObserver::default(),
        });
        let observer: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = frame.clone();
        body.observe_next_cpu_frame(Rc::downgrade(&observer))
            .map_err(|cause| cause.to_string())?;
        *self.frame.borrow_mut() = Some(frame);
        Ok(())
    }

    /// 原发布通知、当前 Ready、完整 Input 和 CPU 计数必须一致，不能借新帧给旧动作补票。
    fn verify_frame(&self, view: &NativeMarkdownPagedTextView) -> Result<(), String> {
        let key = view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        if view.cpu_paint_pending() != Some(false) {
            return Err("中文验收原 CPU 尚未完成".into());
        }
        if let Some(frame) = self.frame.borrow_mut().take() {
            let published = frame.key.borrow();
            let (original, paints) = published.as_ref().ok_or("中文验收未收到原完整帧通知")?;
            if !original.same(&key) || view.cpu_paints() != Some(*paints) {
                return Err("中文验收完整帧被后继输入或几何替换".into());
            }
        }
        Ok(())
    }

    /// 四 MiB 单段只允许推进至本次远端视口附近；原读取和整形计量不能已经完成整篇。
    fn verify_lazy_progress(&self, body: &NativeMarkdownPagedBody) -> Result<(), String> {
        let progress = body.progress().map_err(|cause| cause.to_string())?;
        let beyond_visible = (INSERTION + 9000) * 3 - 2;
        if progress.complete
            || progress.source_total_bytes < validation::ORIGINAL_BYTES
            || progress.frontier_source_bytes >= beyond_visible
            || progress.shaped_input_bytes >= beyond_visible
        {
            return Err("实际中文远端视口越过了有界读取和整形范围".into());
        }
        Ok(())
    }

    /// 历史必须恢复同一次实际汉字、原完整前后缀和光标；仅净长度相同不能满足撤销重做。
    fn verify_history(
        &self,
        view: &NativeMarkdownPagedTextView,
        inserted: &[u16],
    ) -> Result<(), String> {
        validation::verify(&self.document, inserted)?;
        let input = view.input_state().map_err(|cause| cause.to_string())?;
        if input.marked.is_some()
            || input.selection != MarkdownTextSelection::caret(INSERTION + inserted.len())
        {
            return Err("实际中文历史没有恢复原组合前后的光标或仍有未完成组合".into());
        }
        let document = self.document.upgrade().ok_or("原历史文稿已释放")?;
        let document = document.borrow();
        if document.can_undo() != !inserted.is_empty() || document.can_redo() != inserted.is_empty()
        {
            return Err("实际中文组合不是唯一一笔可撤销输入".into());
        }
        Ok(())
    }

    /// 只有实际完成的帧或系统组合才交付下一步；同步已完成只返回一次排队权限。
    pub fn observe_step_terminal(
        &self,
        step: NativeMarkdownHostedImeStep,
        observer: impl FnOnce() + 'static,
    ) -> Result<bool, String> {
        if NativeMarkdownHostedImeStep::ALL.get(self.next.get()) != Some(&step) {
            return Err("中文验收终点观察失序".into());
        }
        if step == NativeMarkdownHostedImeStep::VerifyCommit {
            let composition = self
                .composition
                .borrow()
                .clone()
                .ok_or("原系统组合尚未开始")?;
            return composition.signal.observe(
                composition.result.borrow().as_ref().map(Result::is_ok),
                observer,
            );
        }
        if step == NativeMarkdownHostedImeStep::VerifyOpen && self.frame.borrow().is_none() {
            let (body, view) = self.current()?;
            if let Ok(key) = view.capture_ready_scene_key()
                && view.cpu_paint_pending() == Some(false)
                && let Some(paints) = view.cpu_paints()
            {
                // 初次已经实际发布时仍保存这张原票，排队期间的新输入不能换一张 Ready 补签。
                *self.frame.borrow_mut() = Some(Rc::new(Frame {
                    key: RefCell::new(Some((key, paints))),
                    signal: ProbeTerminalObserver::default(),
                }));
                return Ok(true);
            }
            self.arm_frame(&body)?;
        }
        let frame = self
            .frame
            .borrow()
            .clone()
            .ok_or("原中文动作没有完整帧观察")?;
        frame
            .signal
            .observe(frame.key.borrow().as_ref().map(|_| true), observer)
    }

    /// 每个事件只做当前用例的一步；真实历史命令与保存均调用正式正文和窗口实现。
    pub fn step(&self, step: NativeMarkdownHostedImeStep) -> Result<(), String> {
        if NativeMarkdownHostedImeStep::ALL.get(self.next.get()) != Some(&step) {
            return Err("中文验收动作失序或重复".into());
        }
        let (body, view) = self.current()?;
        if step != NativeMarkdownHostedImeStep::VerifyCommit {
            self.verify_frame(&view)?;
        }
        self.verify_lazy_progress(&body)?;
        match step {
            NativeMarkdownHostedImeStep::VerifyOpen => {
                validation::verify(&self.document, &[])?;
                self.arm_frame(&body)?;
                *self.target.borrow_mut() = Some(
                    body.request_source_target(NSRange::new(INSERTION, 0))
                        .map_err(|cause| cause.to_string())?,
                );
            }
            NativeMarkdownHostedImeStep::VerifyTarget => {
                let target = self.target.borrow_mut().take().ok_or("原远端目标不存在")?;
                if target.is_pending()
                    || body
                        .take_source_target_visible(&target)
                        .map_err(|cause| cause.to_string())?
                        .is_none()
                {
                    return Err("原七万字远端目标没有实际可见证明".into());
                }
                let caret = view
                    .screen_rect(NSRange::new(INSERTION, 0))
                    .map_err(|cause| cause.to_string())?
                    .rect;
                let panel = self.panel.load().ok_or("原输入面板已释放")?;
                let point = panel.convertPointFromScreen(NSPoint::new(
                    caret.origin.x,
                    caret.origin.y + caret.size.height * 0.5,
                ));
                self.arm_frame(&body)?;
                for kind in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
                    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                        kind, point, NSEventModifierFlags::empty(), 1.0, panel.windowNumber(), None, 1, 1, 1.0,
                    ).ok_or("无法创建原远端光标鼠标事件")?;
                    // SAFETY: 原公开鼠标签名和同一面板坐标；不写 Core 选区或伪造输入法插入。
                    unsafe {
                        if kind == NSEventType::LeftMouseDown {
                            let _: () = msg_send![&*view, mouseDown: &*event];
                        } else {
                            let _: () = msg_send![&*view, mouseUp: &*event];
                        }
                    }
                }
            }
            NativeMarkdownHostedImeStep::BeginInput => {
                if view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    .selection
                    != MarkdownTextSelection::caret(INSERTION)
                {
                    return Err("原实际命中没有定位至七万字后的光标".into());
                }
                let panel = self.panel.load().ok_or("原系统输入面板已释放")?;
                if !panel.makeFirstResponder(Some(&*view)) {
                    return Err("原正文未取得真实输入焦点".into());
                }
                let composition = Composition::install(
                    &body,
                    &view,
                    panel.as_super().as_super(),
                    self.document.clone(),
                )?;
                *self.composition.borrow_mut() = Some(composition);
            }
            NativeMarkdownHostedImeStep::VerifyCommit => {
                let composition = self
                    .composition
                    .borrow_mut()
                    .take()
                    .ok_or("原系统组合不存在")?;
                let actual = composition.finish()?;
                self.verify_history(&view, &actual)?;
                *self.committed.borrow_mut() = Some(actual);
                self.arm_frame(&body)?;
                view.command(sel!(undo:))
                    .map_err(|cause| cause.to_string())?;
            }
            NativeMarkdownHostedImeStep::VerifyUndo => {
                self.verify_history(&view, &[])?;
                self.arm_frame(&body)?;
                view.command(sel!(redo:))
                    .map_err(|cause| cause.to_string())?;
            }
            NativeMarkdownHostedImeStep::VerifyRedo => {
                let committed = self.committed.borrow();
                self.verify_history(&view, committed.as_deref().ok_or("原实际汉字收据丢失")?)?;
                drop(committed);
                self.arm_frame(&body)?;
                view.command(sel!(undo:))
                    .map_err(|cause| cause.to_string())?;
            }
            NativeMarkdownHostedImeStep::VerifyRestoreSave => {
                self.verify_history(&view, &[])?;
                if view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    .selection
                    != MarkdownTextSelection::caret(INSERTION)
                {
                    return Err("真实中文撤销没有恢复原光标".into());
                }
                with_active_native_markdown_surface(|surface| surface.save_current_document())?;
                self.verify_disk()?;
                self.completed.set(true);
                println!(
                    "system_ime stage=hosted_result passed=true actual_han=true visible_candidate_near_caret=true exact_four_mebibytes=true undo_redo_restore=true saved=true viewport_lazy=true input_source_selected=false physical_keyboard=false"
                );
            }
        }
        self.next.set(self.next.get() + 1);
        Ok(())
    }

    /// 打开的是合成文件；保存后重新映射并逐块核对，不读取用户库或构造整篇字符串。
    fn verify_disk(&self) -> Result<(), String> {
        let saved = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(&self.path, 4 * 1024 * 1024)
                .map_err(|_| "无法重新映射保存后的合成原文")?,
        ));
        validation::verify(&Rc::downgrade(&saved), &[])
    }

    /// 原成功末端或失败收尾均只调用产品既有返回动作，不能关闭后来的会话。
    pub fn request_return(&self) -> Result<(), String> {
        self.current()?;
        let composition = self.composition.borrow_mut().take();
        drop(composition); // 先撤输入、面板等待和通知，再由真实窗口执行严格关闭。
        request_native_markdown_window_exit()
    }

    /// 正式 Host 的释放回执之后再核原弱 Doc/Body/控制器和完整磁盘内容。
    pub fn verify_native_closed(&self) -> Result<(), String> {
        if self.body.upgrade().is_some()
            || self.document.upgrade().is_some()
            || self.view.load().is_some()
            || self.panel.load().is_some()
            || self.controller.load().is_some()
        {
            return Err("中文验收原 Native 或业务资源尚未释放".into());
        }
        if !self.completed.get() {
            return Err("中文验收尚未完成候选、历史和保存".into());
        }
        self.verify_disk()
    }
}

/// 同一实际输入客户的一次组合；弱订阅和 timer 都由此 owner 唯一释放。
struct Composition {
    weak: std::rc::Weak<Self>,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    document: WeakMarkdownDocument,
    view: Weak<NativeMarkdownPagedTextView>,
    window: Weak<NSWindow>,
    software: RefCell<Option<Rc<SoftwareInput>>>,
    focus: RefCell<Option<FocusNotifications>>,
    timer: RefCell<Option<Retained<NSTimer>>>,
    marked: Cell<bool>,
    result: RefCell<Option<Result<Vec<u16>, String>>>,
    signal: ProbeTerminalObserver,
}

impl Composition {
    /// 绑定已显示的生产窗口；只观察与激活原客户，不创建另一个应用或改变当前输入源。
    fn install(
        body: &Rc<NativeMarkdownPagedBody>,
        view: &NativeMarkdownPagedTextView,
        window: &NSWindow,
        document: WeakMarkdownDocument,
    ) -> Result<Rc<Self>, String> {
        let owner = Rc::new_cyclic(|weak| Self {
            weak: weak.clone(),
            body: Rc::downgrade(body),
            document,
            view: Weak::from(view),
            window: Weak::from(window),
            software: RefCell::new(None),
            focus: RefCell::new(None),
            timer: RefCell::new(None),
            marked: Cell::new(false),
            result: RefCell::new(None),
            signal: ProbeTerminalObserver::default(),
        });
        let erased: Rc<dyn NativeMarkdownPagedBodyInputObserver> = owner.clone();
        body.observe_input(Rc::downgrade(&erased))
            .map_err(|cause| cause.to_string())?;
        owner.arm_timeout("原窗口三十秒内未取得真实中文输入焦点");
        let app = NSApplication::sharedApplication(view.mtm());
        let ready = owner.weak.clone();
        let updated = owner.weak.clone();
        let (focus, first) = FocusNotifications::new(
            &app,
            window,
            view,
            move || {
                if let Some(owner) = ready.upgrade() {
                    owner.start();
                }
            },
            move || {
                if let Some(owner) = updated.upgrade() {
                    let driver = owner.software.borrow().clone();
                    if let Some(driver) = driver {
                        autoreleasepool(|_| driver.host_updated());
                    }
                }
            },
        );
        *owner.focus.borrow_mut() = Some(focus);
        first(); // 正式 Host 已经显示此窗口；首次检查不发全局输入或切换输入源。
        Ok(owner)
    }

    /// 同一真实焦点交接后才投递唯一字母；原候选可见并贴近光标后，公共 driver 才发送确认键。
    fn start(&self) {
        if self.result.borrow().is_some() || self.software.borrow().is_some() {
            return;
        }
        let (Some(view), Some(window)) = (self.view.load(), self.window.load()) else {
            self.complete(Err("原中文输入客户已释放".into()));
            return;
        };
        let app = NSApplication::sharedApplication(view.mtm());
        let stopped = self.weak.clone();
        let result = SoftwareInput::new(
            &app,
            &window,
            &view,
            self.body.clone(),
            self.document.clone(),
            Instant::now() + Duration::from_secs(30),
            Rc::new(move || {
                if let Some(owner) = stopped.upgrade() {
                    let cause = owner
                        .software
                        .borrow()
                        .as_ref()
                        .and_then(|driver| driver.failed.get())
                        .unwrap_or("实际系统中文输入序列没有完成");
                    owner.complete(Err(cause.into()));
                }
            }),
        );
        let driver = match result {
            Ok(driver) => driver,
            Err(cause) => {
                self.complete(Err(cause.into()));
                return;
            }
        };
        *self.software.borrow_mut() = Some(driver.clone());
        self.arm_timeout("原三十秒内未观察到实际候选与中文确认");
        driver.prepare_next();
    }

    /// timer 只负责真实失败，不推进操作或补签 Ready；任何完成先取消本 owner 的 timer。
    fn arm_timeout(&self, cause: &'static str) {
        if let Some(old) = self.timer.borrow_mut().take() {
            old.invalidate();
        }
        let weak = std::sync::Arc::new(dispatch2::MainThreadBound::new(
            self.weak.clone(),
            MainThreadMarker::new().unwrap(),
        ));
        let block = block2::RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
            if let Some(owner) = weak.get(MainThreadMarker::new().unwrap()).upgrade() {
                owner.complete(Err(cause.into()));
            }
        });
        // SAFETY: 原主线程标准 run loop；block 只持主线程绑定的 Weak，不保活正文或窗口。
        *self.timer.borrow_mut() = Some(unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(30.0, false, &block)
        });
    }

    /// 首个实际成功或失败保留，清理产生的组合回调不能改写这次结局。
    fn complete(&self, result: Result<Vec<u16>, String>) {
        if self.result.borrow().is_some() {
            return;
        }
        *self.result.borrow_mut() = Some(result);
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.invalidate();
        }
        self.signal.notify();
    }

    /// 先固定原候选终点，再撤通知、按键和 context；清理成功不能覆盖实际组合失败。
    fn finish(&self) -> Result<Vec<u16>, String> {
        let result = self
            .result
            .borrow_mut()
            .take()
            .ok_or("实际中文输入尚无终点")?;
        let focus = self.focus.borrow_mut().take();
        drop(focus);
        let driver = self
            .software
            .borrow_mut()
            .take()
            .ok_or("原中文输入 driver 不存在")?;
        let candidate = driver.candidate_observed();
        let failed = driver.failed.get();
        let cleaned = driver.cleanup();
        let result = result?;
        if !candidate || failed.is_some() || !cleaned {
            return Err("真实中文候选或输入资源收尾不完整".into());
        }
        Ok(result)
    }
}

impl NativeMarkdownPagedBodyInputObserver for Composition {
    fn committed(&self, commit: &NativeMarkdownPagedInputCommit) {
        if self.result.borrow().is_some() {
            return;
        }
        let Some(driver) = self.software.borrow().clone() else {
            return;
        };
        if driver.stopped.get() {
            return;
        }
        if commit.state.marked.is_some() {
            self.marked.set(true);
            driver.marked(commit);
            return;
        }
        if !self.marked.get()
            || driver.next.get() != 2
            || !driver.candidate_observed()
            || driver.waiting.get().is_none_or(|(index, _)| index != 1)
        {
            driver.unexpected_commit(commit);
            return;
        }
        let result = if Instant::now() >= driver.deadline {
            Err("实际中文提交超过原期限".into())
        } else {
            validation::capture_han(&self.document, &self.body, commit).map_err(str::to_owned)
        };
        self.complete(result);
    }

    fn local_input_changed(&self) {
        // 局部格草稿不属于本合成正文的系统组合，不能满足中文提交终点。
    }
}

impl Drop for Composition {
    fn drop(&mut self) {
        if let Some(timer) = self.timer.get_mut().take() {
            timer.invalidate();
        }
        self.focus.get_mut().take();
        self.software.get_mut().take(); // driver 自有 Drop 撤候选 timer 和尚未交付按键。
    }
}
