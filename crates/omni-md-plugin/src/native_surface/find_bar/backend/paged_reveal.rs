//! A paged Find target retains weak original identities. Only the actual view and
//! an existing Scene can prove that the target became visible.

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputState;

pub(in crate::native_surface) enum NativeMarkdownPagedFindRevealProgress {
    Pending,
    /// Allocate one bounded receipt only for actual visibility; Pending does not
    /// copy the entire Scene key on each scheduling stack.
    Visible(Box<NativeMarkdownPagedFindVisibleProof>),
}

/// The original viewport/composer owner schedules offscreen work. Begin cannot
/// synchronously shape an entire paragraph or substitute selection for visibility.
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

/// Retain no strong document, Source, view, or controller claim. Even the commit
/// carries only original weak Input identity and scalar committed facts.
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
    // Explicit acceptance observes this same terminal state. Its callback retains
    // no document, view, or successor request and creates no second success state.
    #[cfg(feature = "native-markdown-runtime-probe")]
    terminal_observer: crate::native_surface::probe::ProbeTerminalObserver,
}

impl NativeMarkdownPagedFindRevealRequest {
    /// Fix the original selection extent and weak owners, then validate them after
    /// Native weak-reference creation, which may reenter before publication.
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

    /// Root-slot cancellation or a new Find revokes this request only. It neither
    /// rolls back committed text nor reissues authority to a successor action.
    pub(in crate::native_surface) fn cancel(&self) {
        if self.terminal.get() == Terminal::Pending {
            self.terminal.set(Terminal::Cancelled);
            #[cfg(feature = "native-markdown-runtime-probe")]
            self.terminal_observer.notify();
        }
    }

    /// Fail only the original pending request. Callers first preserve the original
    /// Source failure before considering ordinary cancellation. Cancelled, Visible,
    /// and Failed are terminal; later failures cannot revive or overwrite them.
    pub(in crate::native_surface) fn fail(&self, cause: String) {
        if self.terminal.get() == Terminal::Pending {
            *self.failure.borrow_mut() = Some(self.error(cause));
            self.terminal.set(Terminal::Failed);
            // Body failure is still sealing itself. Do not synchronously reenter
            // Native layout and advance that unfinished error stack. Queue only
            // original weak controller/bar/document identities and the error text;
            // this message keeps no document, Source, or Native graph alive.
            let message = self.failure.borrow().as_ref().map(ToString::to_string);
            // Fix product feedback before notifying observers, which may consume
            // the typed failure. Acceptance observation must not remove the
            // production window's Chinese message about already committed effects.
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
                    // Keep all Native setters within the original window's owner
                    // lease. Reentrant close/note changes cannot redirect remaining
                    // feedback to a successor session. Busy/closed drops this UI
                    // projection only; retain the typed cause without replaying replacement.
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

    /// Take an asynchronous failure once. It retains the original commit's weak
    /// identity and fixed facts without executing replacement again.
    pub(in crate::native_surface) fn take_failure(
        &self,
    ) -> Option<NativeMarkdownFindExecutionError> {
        self.failure.borrow_mut().take()
    }

    #[must_use]
    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    pub(in crate::native_surface) fn pending(&self) -> bool {
        self.terminal.get() == Terminal::Pending
    }

    #[must_use]
    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
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
    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    pub(in crate::native_surface) fn completed_visible(&self) -> bool {
        self.terminal.get() == Terminal::Visible
    }

    /// Observe this actual terminal state. Slot existence or status text cannot
    /// establish whether the request is pending, cancelled, failed, or visible.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn probe_completed(&self) -> Option<bool> {
        match self.terminal.get() {
            Terminal::Pending => None,
            Terminal::Visible => Some(true),
            Terminal::Cancelled | Terminal::Failed => Some(false),
        }
    }

    /// Register one original endpoint; an already terminal request returns scheduling
    /// ownership to its caller without executing Find/layout or inventing visibility.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn observe_probe_terminal(
        &self,
        observer: impl FnOnce() + 'static,
    ) -> Result<bool, String> {
        self.terminal_observer
            .observe(self.probe_completed(), observer)
    }

    /// The viewport may legitimately move toward this target. Final completion must
    /// authenticate the producer's original actual key, not require its entry viewport.
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

    /// Consume only a sealed proof from the actual view. Revalidate the same key,
    /// rejecting reentry or a Scene change after proof creation.
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
        // Do not capture a successor key or relayout. Verify actual visibility
        // against the producer's original Source, nonce, and geometry.
        let verified = view
            .verify_find_target_visible(proof.key(), self.range)
            .map_err(|error| error.to_string())?;
        if !verified.key().same(proof.key()) || !self.is_current() {
            return Err("原查找可见证明在复核后已失效".into());
        }
        // Counts certify only the original scan. The same Scene/Input/Source must
        // also pass the actual visibility checks above before publishing success.
        let text = NSString::from_str(&self.feedback.text());
        self.write_feedback(&text)?;
        if !self.is_current() {
            return Err("查找成功反馈已被后继请求撤销".into());
        }
        self.terminal.set(Terminal::Visible);
        #[cfg(feature = "native-markdown-runtime-probe")]
        self.terminal_observer.notify();
        Ok(())
    }

    /// Hidden navigation leaves bar controls and focus untouched. Visible feedback
    /// requires the original authority both before and after the reentrant setter.
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

    /// Deferred failure retains the original committed effect. An asynchronous
    /// producer cannot downgrade it to an ordinary rejection that might replay an edit.
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
