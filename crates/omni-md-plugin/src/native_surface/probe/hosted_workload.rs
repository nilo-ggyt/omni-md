//! 真实Host会话上的长文工作负载桥；复用原动作和实际输入，不创建第二套窗口或生命周期。

use super::*;

mod editing;
use editing::HostedEditingProbe;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 原只读工作负载及Unicode编辑的具名阶段；动作与检查分属真实后继事件，不能同步假定异步完成。
pub enum NativeMarkdownHostedStep {
    VerifyOpen,
    SelectSource,
    VerifySource,
    ShowFind,
    VerifyFindShown,
    EnterTableQuery,
    VerifyTableQuery,
    FindNext,
    VerifyTableFind,
    SelectPreview,
    VerifyPreview,
    EnterCodeQuery,
    VerifyCodeQuery,
    VerifyCodeFind,
    VerifyCodeFindPreview,
    BeginEditing,
    VerifyInserted,
    UndoEditing,
    VerifyUndo,
    RedoEditing,
    VerifyRedo,
    RestoreEditing,
    VerifyRestored,
    SaveRestored,
    VerifySaved,
}

/// 只持原弱正文、标量收据及原动作的有限请求；实际session与Native强图继续完全归Host。
pub struct NativeMarkdownHostedWorkload {
    workload: NativeMarkdownMemoryWorkload,
    editing: RefCell<Option<HostedEditingProbe>>,
    editing_started: Cell<bool>,
    editing_completed: Cell<bool>,
    editing_failed: Cell<bool>,
}

impl NativeMarkdownHostedWorkload {
    /// 观察已由真实Host激活的指定合成笔记；路径不符立即失败，不切换笔记或打开新会话。
    pub fn observe_active_document(path: &Path) -> Result<Self, String> {
        require_main_thread()?;
        let path = path
            .canonicalize()
            .map_err(|_| "无法解析Hosted合成笔记路径")?;
        let workload = NativeMarkdownMemoryWorkload {
            owner: MemoryWorkloadOwner::Hosted,
            path,
            facts: RefCell::new(None),
            pending_mode: RefCell::new(None),
            pending_find: RefCell::new(None),
            pending_query: Cell::new(None),
            verified_query: Cell::new(None),
            find_highlights: RefCell::new(None),
            #[cfg(all(
                feature = "native-raster-surface-probe",
                feature = "native-close-stage-diagnostic"
            ))]
            raster_close_observation: RefCell::new(None),
        };
        // 先核实际document.path和磁盘基线，再固定原Weak；之后VerifyOpen不得重签另一窗口。
        let facts = workload.capture_open_facts()?;
        *workload.facts.borrow_mut() = Some(facts);
        workload.verify_unchanged()?;
        Ok(Self {
            workload,
            editing: RefCell::new(None),
            editing_started: Cell::new(false),
            editing_completed: Cell::new(false),
            editing_failed: Cell::new(false),
        })
    }

    /// 复用原工作负载生产动作与检查；调用者用真实事件边界安排后继验证，不执行嵌套run loop。
    pub fn step(&self, step: NativeMarkdownHostedStep) -> Result<(), String> {
        require_main_thread()?;
        match step {
            NativeMarkdownHostedStep::VerifyOpen => self.workload.verify_open(), // 原401。
            NativeMarkdownHostedStep::SelectSource => self.workload.change_mode(true), // 原402/412。
            NativeMarkdownHostedStep::VerifySource => self.workload.verify_mode(true), // 原403/413。
            NativeMarkdownHostedStep::ShowFind => self.workload.show_find(), // 原404，插件内部入口。
            NativeMarkdownHostedStep::VerifyFindShown => self.workload.verify_find_shown(), // 原405。
            NativeMarkdownHostedStep::EnterTableQuery => self.workload.enter_query(false), // 原406。
            NativeMarkdownHostedStep::VerifyTableQuery => self.workload.verify_query(false), // 原407。
            NativeMarkdownHostedStep::FindNext => self.workload.find_next(), // 原408/416/420。
            NativeMarkdownHostedStep::VerifyTableFind => self.workload.verify_find(false, false), // 原409。
            NativeMarkdownHostedStep::SelectPreview => self.workload.change_mode(false), // 原410/418。
            NativeMarkdownHostedStep::VerifyPreview => self.workload.verify_mode(false), // 原411/419。
            NativeMarkdownHostedStep::EnterCodeQuery => self.workload.enter_query(true), // 原414。
            NativeMarkdownHostedStep::VerifyCodeQuery => self.workload.verify_query(true), // 原415。
            NativeMarkdownHostedStep::VerifyCodeFind => self.workload.verify_find(true, false), // 原417。
            NativeMarkdownHostedStep::VerifyCodeFindPreview => {
                self.workload.verify_find(true, true)
            } // 原421。
            step @ (NativeMarkdownHostedStep::BeginEditing
            | NativeMarkdownHostedStep::VerifyInserted
            | NativeMarkdownHostedStep::UndoEditing
            | NativeMarkdownHostedStep::VerifyUndo
            | NativeMarkdownHostedStep::RedoEditing
            | NativeMarkdownHostedStep::VerifyRedo
            | NativeMarkdownHostedStep::RestoreEditing
            | NativeMarkdownHostedStep::VerifyRestored
            | NativeMarkdownHostedStep::SaveRestored
            | NativeMarkdownHostedStep::VerifySaved) => self.editing_step(step),
        }
    }

    /// 下一验证只观察动作保存的原票；返回 false 后必须等同一请求终点，不能自行重排检查。
    pub fn observe_step_terminal(
        &self,
        step: NativeMarkdownHostedStep,
        observer: impl FnOnce() + 'static,
    ) -> Result<bool, String> {
        require_main_thread()?;
        match step {
            NativeMarkdownHostedStep::VerifyOpen => {
                self.observe_open_paint(observer) // 首屏 layer 可能晚于窗口 show 事件，等待原实际发布点。
            }
            NativeMarkdownHostedStep::VerifySource | NativeMarkdownHostedStep::VerifyPreview => {
                let request = self
                    .workload
                    .pending_mode
                    .borrow()
                    .as_ref()
                    .map(|receipt| Rc::clone(&receipt.request))
                    .ok_or("模式终点观察缺少原动作票据")?;
                request.observe_probe_terminal(observer) // 先结束 pending_mode 借用，再安装一次观察。
            }
            NativeMarkdownHostedStep::VerifyCodeFindPreview => {
                #[cfg(feature = "native-block-fragment-diagnostic")]
                {
                    self.workload.observe_preview_find_paint(observer)?;
                    Ok(false) // 已有终点也由一次原回调排事件，不在当前栈内执行功能验证。
                }
                #[cfg(not(feature = "native-block-fragment-diagnostic"))]
                {
                    self.observe_find_terminal(observer)
                }
            }
            NativeMarkdownHostedStep::VerifyTableFind
            | NativeMarkdownHostedStep::VerifyCodeFind => self.observe_find_terminal(observer),
            NativeMarkdownHostedStep::SelectSource
            | NativeMarkdownHostedStep::ShowFind
            | NativeMarkdownHostedStep::VerifyFindShown
            | NativeMarkdownHostedStep::EnterTableQuery
            | NativeMarkdownHostedStep::VerifyTableQuery
            | NativeMarkdownHostedStep::FindNext
            | NativeMarkdownHostedStep::SelectPreview
            | NativeMarkdownHostedStep::EnterCodeQuery
            | NativeMarkdownHostedStep::VerifyCodeQuery
            | NativeMarkdownHostedStep::BeginEditing
            | NativeMarkdownHostedStep::VerifyInserted
            | NativeMarkdownHostedStep::UndoEditing
            | NativeMarkdownHostedStep::VerifyUndo
            | NativeMarkdownHostedStep::RedoEditing
            | NativeMarkdownHostedStep::VerifyRedo
            | NativeMarkdownHostedStep::RestoreEditing
            | NativeMarkdownHostedStep::VerifyRestored
            | NativeMarkdownHostedStep::SaveRestored
            | NativeMarkdownHostedStep::VerifySaved => {
                // 其他步骤的生产入口同步提交已有输入/焦点/历史/保存事实；仍在下一独立事件核验。
                // 此处不提供延时、轮询或新收据；未来新增异步请求必须明确加入上方原终点分支。
                Ok(true)
            }
        }
    }

    /// 原 Hosted 窗口与输入票贯穿首屏等待；不从另一个已画好的 editor 重签身份。
    fn observe_open_paint(&self, observer: impl FnOnce() + 'static) -> Result<bool, String> {
        #[cfg(feature = "native-raster-surface-probe")]
        if super::super::raster_surface::NativeMarkdownRasterSurface::requested() {
            let facts = self
                .workload
                .facts
                .borrow()
                .clone()
                .ok_or("首屏观察缺少原窗口身份")?;
            return with_active_native_markdown_surface(|surface| {
                if !self.workload.current(surface, &facts) {
                    return Err("首屏观察前原笔记或窗口已失效".into());
                }
                let editor = surface.editor.as_ref().ok_or("首屏观察缺少原 editor")?;
                let raster = editor
                    .backend
                    .text_view
                    .ivars()
                    .raster_surface
                    .borrow()
                    .clone()
                    .ok_or("首屏观察缺少原光栅实例")?;
                raster.observe_first_paint(observer) // 只有纯 Rust 原终点槽操作，不触发绘图或 Native getter。
            });
        }
        drop(observer); // 普通路径保持同步阶段约定，不安装一个没有对应生产者的等待槽。
        Ok(true)
    }

    /// 非绘图验证继续只观察原Find终点；不为无关步骤注册背景观察。
    fn observe_find_terminal(&self, observer: impl FnOnce() + 'static) -> Result<bool, String> {
        let request = self
            .workload
            .pending_find
            .borrow()
            .as_ref()
            .map(|receipt| Rc::clone(&receipt.request))
            .ok_or("Find终点观察缺少原动作票据")?;
        request.observe_probe_terminal(observer)
    }

    /// 原编辑票先移出RefCell再执行Native；失败永久记住，不能重跑乱序步骤伪造成功。
    fn editing_step(&self, step: NativeMarkdownHostedStep) -> Result<(), String> {
        if self.editing_failed.get() {
            return Err("长文编辑验收已有失败，不能恢复旧步骤权限".into());
        }
        let result = if step == NativeMarkdownHostedStep::BeginEditing {
            if self.editing_started.replace(true) {
                Err("长文编辑验收不能重复开始".into())
            } else {
                HostedEditingProbe::begin(&self.workload).map(|probe| {
                    *self.editing.borrow_mut() = Some(probe);
                })
            }
        } else {
            let probe = self.editing.borrow_mut().take();
            match probe {
                Some(mut probe) => {
                    let result = probe.step(&self.workload, step);
                    if result.is_ok() && probe.completed() {
                        self.editing_completed.set(true); // 只在VerifySaved完整通过后记完成。
                    }
                    *self.editing.borrow_mut() = Some(probe);
                    result
                }
                None => Err("长文编辑步骤缺少原BeginEditing票据".into()),
            }
        };
        if result.is_err() {
            self.editing_failed.set(true); // 结束所有Native/RefCell借用后固定首个失败。
        }
        result
    }

    fn require_editing_completion(&self) -> Result<(), String> {
        if self.editing_failed.get()
            || (self.editing_started.get() && !self.editing_completed.get())
        {
            return Err("长文编辑尚未恢复保存并完成验证，不能报告正常返回成功".into());
        }
        Ok(())
    }

    /// 只给仍属原会话的真实surface发送正式ExitToLauncher；不终止应用，不伪造Host回执。
    pub fn request_return(&self) -> Result<(), String> {
        require_main_thread()?;
        self.require_editing_completion()?;
        self.workload.verify_unchanged()?; // 原surface已切换时不得关闭后来打开的其他插件窗口。
        let editing = self.editing.borrow_mut().take();
        drop(editing); // 严格关闭之前释放有限片段和所有动作票；没有正文或Native强owner转交completion。
        self.workload.request_return()
    }

    /// 核原Native资源与只读磁盘事实；Host另核matching close、session Drop和LauncherReady。
    pub fn verify_native_closed(&self) -> Result<(), String> {
        require_main_thread()?;
        self.require_editing_completion()?;
        self.workload.verify_native_closed()
    }
}

/// TLS空槽只有在产品主线程才有意义；后台线程不能靠自己的空TLS伪报整个Native图已关闭。
fn require_main_thread() -> Result<(), String> {
    MainThreadMarker::new()
        .map(|_| ())
        .ok_or_else(|| "Hosted工作负载只能在真实AppKit主线程调用".to_owned())
}
