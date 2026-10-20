//! 同一Find界面选择具体正文后端；查询、正则与匿名替换记录继续复用原实现。

use super::*;

#[cfg(feature = "native-paged-production")]
use crate::native_surface::{
    paged_input::{
        NativeMarkdownPagedInput, NativeMarkdownPagedInputCommit, NativeMarkdownPagedInputOutcome,
    },
    paged_text_view::{NativeMarkdownPagedFindVisibleProof, NativeMarkdownPagedTextView},
};

#[cfg(feature = "native-paged-production")]
#[path = "backend/paged_reveal.rs"]
mod paged_reveal;
#[cfg(feature = "native-paged-production")]
pub(in crate::native_surface) use paged_reveal::*;
#[cfg(all(
    feature = "native-paged-find-probe",
    feature = "native-markdown-runtime-probe"
))]
#[path = "backend/probe.rs"]
mod probe;
#[cfg(all(
    feature = "native-paged-find-probe",
    feature = "native-markdown-runtime-probe"
))]
pub use probe::run_native_paged_find_backend_control;
#[cfg(all(
    feature = "native-paged-body-probe",
    feature = "native-markdown-runtime-probe"
))]
pub use probe::run_native_paged_find_body_transaction_control;

/// composition root提供当前实际视图，不以隐藏NSTextView为Paged路径保留第二套正文图。
pub(in crate::native_surface) enum NativeMarkdownFindBackend<'a> {
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    TextKit {
        view: &'a PieceBackedMarkdownTextView,
        controller: &'a NativeMarkdownController,
        intent: u64,
    },
    #[cfg(feature = "native-paged-production")]
    Paged(NativeMarkdownPagedFindBackend<'a>),
}

pub(in crate::native_surface) enum NativeMarkdownFindExecution {
    Complete,
    #[cfg(feature = "native-paged-production")]
    Pending(Rc<NativeMarkdownPagedFindRevealRequest>),
}

/// 已发布的Core结果必须单独保留；任何后续显示错误都不能把调用方带回可重放的Rejected分支。
pub(in crate::native_surface) enum NativeMarkdownFindExecutionError {
    Rejected(String),
    #[cfg(feature = "native-paged-production")]
    CommittedEffect {
        commit: Rc<NativeMarkdownPagedInputCommit>,
        cause: String,
    },
}

impl std::fmt::Debug for NativeMarkdownFindExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(cause) => formatter.debug_tuple("Rejected").field(cause).finish(),
            #[cfg(feature = "native-paged-production")]
            Self::CommittedEffect { cause, .. } => formatter
                .debug_tuple("CommittedEffect")
                .field(cause)
                .finish(),
        }
    }
}
impl std::fmt::Display for NativeMarkdownFindExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(cause) => formatter.write_str(cause),
            #[cfg(feature = "native-paged-production")]
            Self::CommittedEffect { commit, cause } => {
                if commit.invalidation.characters_changed {
                    write!(
                        formatter,
                        "替换已生效，请勿重复替换；定位显示未完成：{cause}"
                    )
                } else {
                    write!(formatter, "文档选择已更新；定位显示未完成：{cause}")
                }
            }
        }
    }
}
impl std::error::Error for NativeMarkdownFindExecutionError {}

pub(in crate::native_surface) fn execute_find_backend(
    bar: &Rc<NativeMarkdownFindBar>,
    backend: NativeMarkdownFindBackend<'_>,
    action: MarkdownFindAction,
) -> Result<NativeMarkdownFindExecution, NativeMarkdownFindExecutionError> {
    match backend {
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        NativeMarkdownFindBackend::TextKit {
            view,
            controller,
            intent,
        } => {
            // 旧具体适配器保留其真实通知、原生选择安装和延后Reveal路径。
            execute_textkit_find_action(bar, view, controller, intent, action)
                .map(|()| NativeMarkdownFindExecution::Complete)
                .map_err(NativeMarkdownFindExecutionError::Rejected)
        }
        #[cfg(feature = "native-paged-production")]
        NativeMarkdownFindBackend::Paged(backend) => backend.execute(bar, action),
    }
}

#[cfg(feature = "native-paged-production")]
/// 同步动作只借用真实owner；reveal port由原viewport/composer协调器提供。
pub(in crate::native_surface) struct NativeMarkdownPagedFindBackend<'a> {
    pub(in crate::native_surface) view: &'a NativeMarkdownPagedTextView,
    pub(in crate::native_surface) input: &'a Rc<NativeMarkdownPagedInput>,
    pub(in crate::native_surface) epoch: u64,
    pub(in crate::native_surface) controller: &'a NativeMarkdownController,
    pub(in crate::native_surface) intent: u64,
    pub(in crate::native_surface) reveal: &'a dyn NativeMarkdownPagedFindRevealPort,
}

#[cfg(feature = "native-paged-production")]
/// 同一明确动作的逻辑目标与反馈相随，权限和已提交状态仍由独立原票认证。
struct PagedFindTarget {
    selection: MarkdownTextSelection,
    feedback: NativeMarkdownFindRevealFeedback,
}

#[cfg(feature = "native-paged-production")]
impl NativeMarkdownPagedFindBackend<'_> {
    fn execute(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        action: MarkdownFindAction,
    ) -> Result<NativeMarkdownFindExecution, NativeMarkdownFindExecutionError> {
        let mut committed: Option<Rc<NativeMarkdownPagedInputCommit>> = None;
        let result = self.execute_inner(bar, action, &mut committed);
        result.map_err(|cause| match committed {
            Some(commit) => NativeMarkdownFindExecutionError::CommittedEffect { commit, cause },
            None => NativeMarkdownFindExecutionError::Rejected(cause),
        })
    }

    fn execute_inner(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        action: MarkdownFindAction,
        committed: &mut Option<Rc<NativeMarkdownPagedInputCommit>>,
    ) -> Result<NativeMarkdownFindExecution, String> {
        self.reveal.cancel_pending(); // 新显式动作先撤旧请求；不能让旧请求借相同查询或视口续权。
        let bar_revision = bar.layout_revision.get();
        let visibility =
            NativeMarkdownFindVisibility::capture(action, bar.visible.get(), bar.closed.get())
                .ok_or("查找请求已失效")?;
        let before = self.input.state().map_err(|error| error.to_string())?;
        let geometry_epoch = self
            .view
            .find_geometry_epoch()
            .ok_or("查找正文几何已经失效")?;
        let session_current = |epoch| {
            epoch == self.epoch
                && self
                    .view
                    .find_input_is_current(self.input, epoch, &before, Some(geometry_epoch))
                && native_markdown_controller_has_foreground_authority(self.controller)
                && self.controller.find_input_intent() == self.intent
                && visibility.permits(
                    bar.visible.get(),
                    bar.closed.get(),
                    bar_revision,
                    bar.layout_revision.get(),
                )
        };
        if !self.view.session_is_current() || !session_current(self.epoch) {
            return Err("查找原正文会话已经失效".into());
        }
        let permit = self
            .input
            .capture(self.epoch, &session_current)
            .map_err(|error| error.to_string())?;
        let document_identity = self.input.document_identity();
        let current = || session_current(self.epoch);
        if !current() {
            return Err("查找原输入状态已失效".into());
        }
        let request = self
            .view
            .capture_scene_request()
            .map_err(|error| error.to_string())?;
        if !std::rc::Weak::ptr_eq(&request.document, &document_identity)
            || request.input != before
            || !current()
        {
            return Err("查找视图与原输入文档身份不同".into());
        }
        let query = native_markdown_table_intent_step(&current, || Query::read(bar, action))
            .map_err(|_| "读取查找输入时原权限已失效")??;
        let document = document_identity.upgrade().ok_or("查找正文已经释放")?;
        // 独立虚拟NSString只有WeakDoc；Regex不得为了获取字符串而创建NSTextStorage或隐藏View。
        let string = if query.regex {
            Some(
                native_markdown_table_intent_step(&current, || {
                    PieceBackedMarkdownString::new(&document, self.view.mtm())
                })
                .map_err(|_| "建立查找字符串适配时原权限已失效")?,
            )
        } else {
            None
        }; // 字面扫描不需要Foundation正文适配器，只复用Core的有限源窗口。
        document
            .borrow()
            .byte_offset_for_utf16(0)
            .map_err(|error| error.to_string())?;
        let selection = before.directed_selection().map_or_else(
            || {
                before
                    .selection
                    .anchor_utf16
                    .min(before.selection.head_utf16)
                    ..before
                        .selection
                        .anchor_utf16
                        .max(before.selection.head_utf16)
            },
            |selection| {
                selection.anchor_utf16.min(selection.head_utf16)
                    ..selection.anchor_utf16.max(selection.head_utf16)
            },
        ); // 多选扫描沿旧Core范围，不能从第一真实范围伪造主head。
        let capture_count = if query.regex {
            query.compiled_regex()?.numberOfCaptureGroups() + 1
        } else {
            0
        };
        let template = ReplacementTemplate::parse(&query.replacement, query.regex, capture_count);
        let mut spool = if action == MarkdownFindAction::ReplaceAll {
            Some(ReplacementSpool::new(&template)?)
        } else {
            None
        };
        let captures = if !template.captures.is_empty()
            && matches!(
                action,
                MarkdownFindAction::ReplaceNext | MarkdownFindAction::ReplaceAll
            ) {
            CaptureRanges::Include
        } else {
            CaptureRanges::Omit
        };
        let mut navigation = NavigationMatches::default();
        let mut matches = Vec::new();
        query.visit(
            &document,
            string
                .as_deref()
                .map_or_else(|| ns_string!(""), |string| string.as_ref()),
            matches!(
                action,
                MarkdownFindAction::Next
                    | MarkdownFindAction::Previous
                    | MarkdownFindAction::ReplaceNext
            ),
            captures,
            |found| {
                if !current() {
                    return Err("查找期间原选择或输入权限已失效".into());
                }
                if action == MarkdownFindAction::SelectAll {
                    if matches.len() == 1000 {
                        return Err("全部选中最多支持 1,000 处匹配".into());
                    }
                    matches.push(found.range.clone());
                }
                if let Some(spool) = &mut spool
                    && found.precise
                {
                    spool.push(&found)?;
                }
                if matches!(
                    action,
                    MarkdownFindAction::Next
                        | MarkdownFindAction::Previous
                        | MarkdownFindAction::ReplaceNext
                ) {
                    navigation.observe(found, &selection);
                }
                Ok(true)
            },
        )?;
        document
            .borrow()
            .byte_offset_for_utf16(0)
            .map_err(|error| error.to_string())?;
        if !current() {
            return Err("查找期间文档或输入已变更".into());
        }
        drop(string); // 匹配结束后归还全部Foundation查询对象，再进入canonical可变事务。
        drop(document);
        if !current() {
            return Err("查找临时对象释放后原权限已失效".into());
        }
        let target = match action {
            MarkdownFindAction::Next => navigation.navigation_target(false),
            MarkdownFindAction::Previous => navigation.navigation_target(true),
            MarkdownFindAction::ReplaceNext => {
                let target = navigation.next.or(navigation.first);
                if let Some(found) = navigation.current.filter(|found| found.precise) {
                    let mut replacement = ReplacementSpool::new(&template)?;
                    replacement.push(&found)?;
                    let after = if let Some(target) = &target {
                        replacement.map_selection(MarkdownTextSelection {
                            anchor_utf16: target.range.start,
                            head_utf16: target.range.end,
                        })?
                    } else {
                        MarkdownTextSelection::caret(
                            found
                                .range
                                .start
                                .checked_add(replacement.inserted_utf16())
                                .ok_or("替换后选择溢出")?,
                        )
                    };
                    let outcome = self
                        .input
                        .apply_replacements_observed(
                            &permit,
                            replacement.into_replacements()?,
                            after,
                            &session_current,
                        )
                        .map_err(|error| error.to_string())?;
                    self.publish_outcome(outcome, committed)?;
                    return self.reveal_target(
                        bar,
                        visibility,
                        bar_revision,
                        before,
                        PagedFindTarget {
                            selection: after,
                            feedback: if target.is_some() {
                                NativeMarkdownFindRevealFeedback::ReplacedNext
                            } else {
                                NativeMarkdownFindRevealFeedback::ReplacedLast
                            },
                        },
                        committed,
                    );
                }
                target // 当前并非精确匹配时只导航，不误删NFKD字符内的部分命中。
            }
            MarkdownFindAction::ReplaceAll => {
                let mut spool = spool.ok_or("全部替换缺少原匿名记录")?;
                let count = spool.count();
                let after = spool.map_selection(before.selection)?;
                let outcome = self
                    .input
                    .apply_replacements_observed(
                        &permit,
                        spool.into_replacements()?,
                        after,
                        &session_current,
                    )
                    .map_err(|error| error.to_string())?;
                self.publish_outcome(outcome, committed)?;
                self.feedback(
                    bar,
                    visibility,
                    bar_revision,
                    before,
                    &format!("已替换 {count} 处匹配"),
                    committed,
                )?;
                return Ok(NativeMarkdownFindExecution::Complete);
            }
            MarkdownFindAction::SelectAll => {
                let outcome = self
                    .input
                    .set_selected_ranges(&permit, &matches, &session_current)
                    .map_err(|error| error.to_string())?;
                self.publish_outcome(outcome, committed)?;
                self.feedback(
                    bar,
                    visibility,
                    bar_revision,
                    before,
                    &format!("已选中 {} 处匹配", matches.len()),
                    committed,
                )?;
                return Ok(NativeMarkdownFindExecution::Complete);
            }
            MarkdownFindAction::Hide => return Err("隐藏查找栏应由原布局入口执行".into()),
        };
        if let Some(target) = target {
            let after = MarkdownTextSelection {
                anchor_utf16: target.range.start,
                head_utf16: target.range.end,
            };
            let outcome = self
                .input
                .select_find_result(&permit, after, &session_current)
                .map_err(|error| error.to_string())?;
            self.publish_outcome(outcome, committed)?;
            self.reveal_target(
                bar,
                visibility,
                bar_revision,
                before,
                PagedFindTarget {
                    selection: after,
                    feedback: NativeMarkdownFindRevealFeedback::Found,
                },
                committed,
            )
        } else {
            self.feedback(
                bar,
                visibility,
                bar_revision,
                before,
                "没有匹配结果",
                committed,
            )?;
            Ok(NativeMarkdownFindExecution::Complete)
        }
    }

    fn publish_outcome(
        &self,
        outcome: NativeMarkdownPagedInputOutcome,
        committed: &mut Option<Rc<NativeMarkdownPagedInputCommit>>,
    ) -> Result<(), String> {
        match outcome {
            NativeMarkdownPagedInputOutcome::Committed(commit) => {
                let commit = Rc::new(commit);
                *committed = Some(Rc::clone(&commit)); // 先保留真实提交事实，再执行任何可重入显示后果。
                self.view
                    .apply_find_commit(self.input, &commit)
                    .map_err(|error| error.to_string())
            }
            NativeMarkdownPagedInputOutcome::Unchanged(_) => Ok(()), // 真空匹配不清redo、不采用after，也不虚构新提交。
        }
    }

    fn feedback(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        visibility: NativeMarkdownFindVisibility,
        revision: u64,
        before: crate::native_surface::paged_input::NativeMarkdownPagedInputState,
        text: &str,
        committed: &Option<Rc<NativeMarkdownPagedInputCommit>>,
    ) -> Result<(), String> {
        let current = || self.feedback_current(bar, visibility, revision, before, committed);
        let status = native_markdown_table_intent_step(&current, || NSString::from_str(text))
            .map_err(|_| "原查找反馈权限已经失效")?;
        native_markdown_table_intent_step(&current, || bar.status.setStringValue(&status))
            .map_err(|_| "原查找反馈已被后继操作撤销")?;
        drop(status);
        current()
            .then_some(())
            .ok_or_else(|| "原查找反馈结束后权限已经失效".into())
    }

    fn feedback_current(
        &self,
        bar: &NativeMarkdownFindBar,
        visibility: NativeMarkdownFindVisibility,
        revision: u64,
        before: crate::native_surface::paged_input::NativeMarkdownPagedInputState,
        committed: &Option<Rc<NativeMarkdownPagedInputCommit>>,
    ) -> bool {
        let current = |epoch| {
            epoch == self.epoch
                && self.view.session_is_current()
                && native_markdown_controller_has_foreground_authority(self.controller)
                && self.controller.find_input_intent() == self.intent
                && visibility.permits(
                    bar.visible.get(),
                    bar.closed.get(),
                    revision,
                    bar.layout_revision.get(),
                )
        };
        let expected = committed.as_ref().map_or(before, |commit| commit.state);
        current(self.epoch)
            && self
                .view
                .find_input_is_current(self.input, self.epoch, &expected, None)
            && committed
                .as_ref()
                .is_none_or(|commit| commit.check_current(&current).is_ok())
    }

    fn reveal_target(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        visibility: NativeMarkdownFindVisibility,
        revision: u64,
        before: crate::native_surface::paged_input::NativeMarkdownPagedInputState,
        target: PagedFindTarget,
        committed: &Option<Rc<NativeMarkdownPagedInputCommit>>,
    ) -> Result<NativeMarkdownFindExecution, String> {
        if !self.feedback_current(bar, visibility, revision, before, committed) {
            return Err("原查找定位权限已经失效".into());
        }
        let expected = committed.as_ref().map_or(before, |commit| commit.state);
        let request = NativeMarkdownPagedFindRevealRequest::new(
            self,
            bar,
            visibility,
            revision,
            expected,
            target,
            committed.clone(),
        )?;
        request.show_pending()?;
        // 已可见的现成Scene可以立即完成；Pending才请原Composer定位，不同步整段排版。
        let ready = match self.view.capture_ready_scene_key() {
            Ok(key) => match self.view.verify_find_target_visible(&key, request.range()) {
                Ok(proof) => Some(NativeMarkdownPagedFindRevealProgress::Visible(Box::new(proof))),
                Err(crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending) => None,
                Err(cause) => { request.cancel(); return Err(cause.to_string()); }
            },
            Err(crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending) => None,
            Err(cause) => { request.cancel(); return Err(cause.to_string()); }
        };
        let progress = match ready.map_or_else(|| self.reveal.begin(Rc::clone(&request)), Ok) {
            Ok(progress) => progress,
            Err(cause) => {
                request.cancel(); // begin可能已经持原请求；失败后不得让迟到producer再覆盖错误反馈。
                return Err(cause);
            }
        };
        if !request.is_current() {
            request.cancel();
            return Err("原查找请求在开始定位时已经失效".into());
        }
        match progress {
            NativeMarkdownPagedFindRevealProgress::Pending => {
                Ok(NativeMarkdownFindExecution::Pending(request))
            }
            NativeMarkdownPagedFindRevealProgress::Visible(proof) => {
                if let Err(cause) = request.finish_visible(&proof) {
                    request.cancel();
                    return Err(cause);
                }
                Ok(NativeMarkdownFindExecution::Complete)
            }
        }
    }
}
