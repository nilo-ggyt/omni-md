//! 一次Paged查找目标只持原身份弱票；真正可见证明只能来自现成Scene和实际View。

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputState;

pub(in crate::native_surface) enum NativeMarkdownPagedFindRevealProgress {
    Pending,
    /// 只有真实成功才分配一次有限收据；Pending不在每次调度栈复制整份SceneKey。
    Visible(Box<NativeMarkdownPagedFindVisibleProof>),
}

/// 原viewport/composer owner实现离屏调度；begin不能同步整段shape或用选区代替可见性。
pub(in crate::native_surface) trait NativeMarkdownPagedFindRevealPort {
    fn cancel_pending(&self);
    fn begin(
        &self,
        request: Rc<NativeMarkdownPagedFindRevealRequest>,
    ) -> Result<NativeMarkdownPagedFindRevealProgress, String>;
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Terminal {
    Pending,
    Visible,
    Cancelled,
    Failed,
}

/// 不持正文、Source、View或Controller强引用；Commit本身也只有原Input弱身份和标量。
pub(in crate::native_surface) struct NativeMarkdownPagedFindRevealRequest {
    view: Weak<NativeMarkdownPagedTextView>,
    controller: Weak<NativeMarkdownController>,
    input: std::rc::Weak<NativeMarkdownPagedInput>,
    document: WeakMarkdownDocument,
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    state: NativeMarkdownPagedInputState,
    epoch: u64,
    intent: u64,
    visibility: NativeMarkdownFindVisibility,
    bar_revision: u64,
    range: NSRange,
    feedback: NativeMarkdownFindRevealFeedback,
    committed: Option<Rc<NativeMarkdownPagedInputCommit>>,
    terminal: Cell<Terminal>,
    failure: RefCell<Option<NativeMarkdownFindExecutionError>>,
    // 只在显式验收构建观察原终态；回调不保正文、View 或后继请求，没有第二份成功状态。
    #[cfg(feature = "native-markdown-runtime-probe")]
    terminal_observer: crate::native_surface::probe::ProbeTerminalObserver,
}

impl NativeMarkdownPagedFindRevealRequest {
    pub(super) fn new(
        backend: &NativeMarkdownPagedFindBackend<'_>,
        bar: &Rc<NativeMarkdownFindBar>,
        visibility: NativeMarkdownFindVisibility,
        bar_revision: u64,
        state: NativeMarkdownPagedInputState,
        target: PagedFindTarget,
        committed: Option<Rc<NativeMarkdownPagedInputCommit>>,
    ) -> Result<Rc<Self>, String> {
        let start = target
            .selection
            .anchor_utf16
            .min(target.selection.head_utf16);
        let end = target
            .selection
            .anchor_utf16
            .max(target.selection.head_utf16);
        if end > state.utf16_len {
            return Err("原查找定位范围超过已提交正文".into());
        }
        let request = Rc::new(Self {
            view: Weak::new(backend.view),
            controller: Weak::new(backend.controller),
            input: Rc::downgrade(backend.input),
            document: backend.input.document_identity(),
            bar: Rc::downgrade(bar),
            state,
            epoch: backend.epoch,
            intent: backend.intent,
            visibility,
            bar_revision,
            range: NSRange::new(start, end - start),
            feedback: target.feedback,
            committed,
            terminal: Cell::new(Terminal::Pending),
            failure: RefCell::new(None),
            #[cfg(feature = "native-markdown-runtime-probe")]
            terminal_observer: crate::native_surface::probe::ProbeTerminalObserver::default(),
        });
        if !request.is_current() {
            return Err("原查找定位请求已经失效".into());
        }
        Ok(request)
    }

    /// Root取消槽或接受新Find时只撤此请求；不回滚已提交文字或给后继动作重签权限。
    pub(in crate::native_surface) fn cancel(&self) {
        if self.terminal.get() == Terminal::Pending {
            self.terminal.set(Terminal::Cancelled);
            #[cfg(feature = "native-markdown-runtime-probe")]
            self.terminal_observer.notify();
        }
    }

    /// 只把仍在途的原请求变成失败；调用方必须先核原Source首错，再决定普通取消。
    /// Cancelled/Visible/Failed都是终态，较晚错误不能复活旧请求或覆盖已经交付的首错。
    pub(in crate::native_surface) fn fail(&self, cause: String) {
        if self.terminal.get() == Terminal::Pending {
            *self.failure.borrow_mut() = Some(self.error(cause));
            self.terminal.set(Terminal::Failed);
            // Body.fail还在封闭自身，不能在此同步进入Native布局让未结束的错误栈再次advance。
            // 只排原controller/bar/Doc弱身份和错误文本；消息不保活正文、Source或Native图。
            let message = self.failure.borrow().as_ref().map(ToString::to_string);
            // 先固定产品实际错误反馈，再通知外部观察；观察者可能消费原 typed failure，
            // 不能因验收回调取走错误而让正式窗口丢失本次已提交后果的中文提示。
            #[cfg(feature = "native-markdown-runtime-probe")]
            self.terminal_observer.notify();
            let Some(message) = message else {
                return;
            };
            let mtm = MainThreadMarker::new().expect("Find终态只在AppKit主线程交付");
            let identity = dispatch2::MainThreadBound::new(
                (
                    self.controller.clone(),
                    self.bar.clone(),
                    self.document.clone(),
                ),
                mtm,
            );
            let intent = self.intent;
            crate::native_surface::enqueue_native_markdown_main_task(move || {
                let mtm = MainThreadMarker::new().expect("Find错误反馈只在原主队列执行");
                let (controller, bar, document) = identity.get(mtm);
                let Some(controller) = controller.load() else {
                    return;
                };
                let Some(actual_bar) = bar.upgrade() else {
                    return;
                };
                if native_markdown_controller_has_foreground_authority(&controller)
                    && controller.find_input_intent() == intent
                    && controller.owns_find_input_bar(bar)
                    && !actual_bar.closed.get()
                    && std::rc::Weak::ptr_eq(&actual_bar.document, document)
                {
                    // 多个Native setter必须在同一原窗口owner租约内；重入关闭/换笔记不能让余下反馈落到新会话。
                    // 租约Busy/已关闭只撤本条UI投影，typed源首因仍保留在原请求中，不重试或重放替换。
                    let _ = crate::native_surface::with_active_native_markdown_surface(|surface| {
                        if std::ptr::eq(&*surface.controller, &*controller)
                            && native_markdown_controller_has_foreground_authority(&controller)
                            && controller.find_input_intent() == intent
                            && controller.owns_find_input_bar(bar)
                            && !actual_bar.closed.get()
                            && surface.editor.as_ref().is_some_and(|editor| {
                                std::ptr::eq(document.as_ptr(), Rc::as_ptr(editor.document()))
                            })
                        {
                            controller.present_error(&message);
                        }
                        Ok(())
                    });
                }
            });
        }
    }

    /// 异步失败只可取一次；错误保留原Commit的弱身份和固定提交事实，不重新执行替换。
    pub(in crate::native_surface) fn take_failure(
        &self,
    ) -> Option<NativeMarkdownFindExecutionError> {
        self.failure.borrow_mut().take()
    }

    #[must_use]
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn pending(&self) -> bool {
        self.terminal.get() == Terminal::Pending
    }

    #[must_use]
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn cancelled(&self) -> bool {
        self.terminal.get() == Terminal::Cancelled
    }

    #[must_use]
    pub(in crate::native_surface) fn range(&self) -> NSRange {
        self.range
    }
    #[must_use]
    pub(in crate::native_surface) fn input_state(&self) -> NativeMarkdownPagedInputState {
        self.state
    }
    #[must_use]
    pub(in crate::native_surface) fn epoch(&self) -> u64 {
        self.epoch
    }
    #[must_use]
    pub(in crate::native_surface) fn document(&self) -> WeakMarkdownDocument {
        self.document.clone()
    }
    #[must_use]
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn completed_visible(&self) -> bool {
        self.terminal.get() == Terminal::Visible
    }

    /// 复用原请求的真实终态；Pending 与取消/失败都不能由槽是否存在或栏文案反推。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn probe_completed(&self) -> Option<bool> {
        match self.terminal.get() {
            Terminal::Pending => None,
            Terminal::Visible => Some(true),
            Terminal::Cancelled | Terminal::Failed => Some(false),
        }
    }

    /// 仅登记一次原终点，已结束则把排队权限交还调用者；不会执行 Find、重排或补造可见证明。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn observe_probe_terminal(
        &self,
        observer: impl FnOnce() + 'static,
    ) -> Result<bool, String> {
        self.terminal_observer
            .observe(self.probe_completed(), observer)
    }

    /// 不要求旧viewport仍相同：原请求可以合法滚到目标；最后必须认证producer给出的真实原key。
    pub(in crate::native_surface) fn is_current(&self) -> bool {
        if self.terminal.get() != Terminal::Pending {
            return false;
        }
        let Some(controller) = self.controller.load() else {
            return false;
        };
        if !native_markdown_controller_has_foreground_authority(&controller)
            || controller.find_input_intent() != self.intent
        {
            return false;
        }
        let Some(bar) = self.bar.upgrade() else {
            return false;
        };
        if !self.visibility.permits(
            bar.visible.get(),
            bar.closed.get(),
            self.bar_revision,
            bar.layout_revision.get(),
        ) {
            return false;
        }
        let Some(input) = self.input.upgrade() else {
            return false;
        };
        if !std::rc::Weak::ptr_eq(&input.document_identity(), &self.document)
            || !input.state().is_ok_and(|actual| actual == self.state)
        {
            return false;
        }
        let Some(view) = self.view.load() else {
            return false;
        };
        view.find_input_is_current(&input, self.epoch, &self.state, None)
    }

    pub(super) fn show_pending(&self) -> Result<(), String> {
        self.write_feedback(ns_string!("正在定位匹配"))
    }

    /// 只消费真实View签发的sealed证明；再次认证同一个key，拒绝proof返回后发生的重入或换scene。
    pub(in crate::native_surface) fn finish_visible(
        &self,
        proof: &NativeMarkdownPagedFindVisibleProof,
    ) -> Result<(), String> {
        if !self.is_current()
            || proof.range() != self.range
            || proof.key().request.input != self.state
            || !std::rc::Weak::ptr_eq(&proof.key().request.document, &self.document)
        {
            return Err("查找可见证明不属于原请求".into());
        }
        let view = self.view.load().ok_or("查找正文视图已经释放")?;
        // 不重捕后继key或重新布局；只对producer已提供的原Source/nonce/几何执行真实可见性复核。
        let verified = view
            .verify_find_target_visible(proof.key(), self.range)
            .map_err(|error| error.to_string())?;
        if !verified.key().same(proof.key()) || !self.is_current() {
            return Err("原查找可见证明在复核后已失效".into());
        }
        let text = match self.feedback {
            NativeMarkdownFindRevealFeedback::Found => ns_string!("找到匹配"),
            NativeMarkdownFindRevealFeedback::ReplacedNext => {
                ns_string!("已替换，并选中下一个匹配")
            }
            NativeMarkdownFindRevealFeedback::ReplacedLast => ns_string!("已替换，没有更多匹配"),
        };
        self.write_feedback(text)?;
        if !self.is_current() {
            return Err("查找成功反馈已被后继请求撤销".into());
        }
        self.terminal.set(Terminal::Visible);
        #[cfg(feature = "native-markdown-runtime-probe")]
        self.terminal_observer.notify();
        Ok(())
    }

    fn write_feedback(&self, text: &NSString) -> Result<(), String> {
        if !self.is_current() {
            return Err("原查找反馈权限已经失效".into());
        }
        if self.visibility == NativeMarkdownFindVisibility::HiddenNavigation {
            return Ok(());
        }
        let bar = self.bar.upgrade().ok_or("原查找栏已经释放")?;
        if !self.is_current() {
            return Err("原查找反馈在写入前已经失效".into());
        }
        bar.status.setStringValue(text);
        if !self.is_current() {
            return Err("原查找反馈在写入后已经失效".into());
        }
        Ok(())
    }

    /// 延后失败仍返回原提交事实；异步producer不得把它降格成可重放的普通Rejected。
    pub(in crate::native_surface) fn error(
        &self,
        cause: String,
    ) -> NativeMarkdownFindExecutionError {
        match &self.committed {
            Some(commit) => NativeMarkdownFindExecutionError::CommittedEffect {
                commit: Rc::clone(commit),
                cause,
            },
            None => NativeMarkdownFindExecutionError::Rejected(cause),
        }
    }
}
