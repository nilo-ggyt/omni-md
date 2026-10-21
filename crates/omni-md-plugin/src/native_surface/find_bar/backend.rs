//! Adapt the same Find interface to its actual document backend while sharing
//! existing query, regex, and anonymous replacement-spool implementations.

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
    feature = "native-paged-body-probe",
    feature = "native-markdown-runtime-probe"
))]
#[path = "backend/probe.rs"]
mod probe;
#[cfg(all(
    feature = "native-paged-body-probe",
    feature = "native-markdown-runtime-probe"
))]
pub use probe::run_native_paged_find_body_control;
#[cfg(all(
    feature = "native-paged-body-probe",
    feature = "native-markdown-runtime-probe"
))]
pub use probe::run_native_paged_find_body_transaction_control;

/// The composition root supplies the actual current view. The Paged path must not
/// keep a second document graph alive through a hidden NSTextView.
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

/// Completion and an original pending reveal are distinct outcomes; Pending does
/// not certify that the selected target has reached the visible viewport.
pub(in crate::native_surface) enum NativeMarkdownFindExecution {
    Complete,
    #[cfg(feature = "native-paged-production")]
    Pending(Rc<NativeMarkdownPagedFindRevealRequest>),
}

/// Preserve an already published Core result separately. A later presentation
/// failure cannot return callers to a rejected outcome that might replay an edit.
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

/// Dispatch to the actual mounted backend without constructing another editor or
/// changing which backend owns selection and edit transactions.
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
            // The independent TextKit adapter keeps its actual notifications,
            // Native selection installation, and deferred reveal path.
            execute_textkit_find_action(bar, view, controller, intent, action)
                .map(|()| NativeMarkdownFindExecution::Complete)
                .map_err(NativeMarkdownFindExecutionError::Rejected)
        }
        #[cfg(feature = "native-paged-production")]
        NativeMarkdownFindBackend::Paged(backend) => backend.execute(bar, action),
    }
}

#[cfg(feature = "native-paged-production")]
/// Synchronous actions borrow actual owners; the original viewport/composer
/// coordinator provides the reveal port.
pub(in crate::native_surface) struct NativeMarkdownPagedFindBackend<'a> {
    pub(in crate::native_surface) view: &'a NativeMarkdownPagedTextView,
    pub(in crate::native_surface) input: &'a Rc<NativeMarkdownPagedInput>,
    pub(in crate::native_surface) epoch: u64,
    pub(in crate::native_surface) controller: &'a NativeMarkdownController,
    pub(in crate::native_surface) intent: u64,
    pub(in crate::native_surface) reveal: &'a dyn NativeMarkdownPagedFindRevealPort,
}

#[cfg(feature = "native-paged-production")]
/// Carry this explicit action's logical target with its feedback. Independent
/// original tickets still authenticate authority and committed effects.
struct PagedFindTarget {
    selection: MarkdownTextSelection,
    feedback: NativeMarkdownFindRevealFeedback,
}

#[cfg(feature = "native-paged-production")]
impl NativeMarkdownPagedFindBackend<'_> {
    /// Retain the first real commit even if later rendering fails, so callers can
    /// report the committed effect without repeating replacement or selection.
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
        // Revoke the previous explicit request first; equal query text or viewport
        // geometry cannot renew an older request's authority.
        self.reveal.cancel_pending();
        let bar_revision = bar.layout_revision.get();
        let visibility =
            NativeMarkdownFindVisibility::capture(action, bar.visible.get(), bar.closed.get())
                .ok_or("查找请求已失效")?;
        let before = self.input.state().map_err(|error| error.to_string())?;
        let geometry_epoch = self
            .view
            .find_geometry_epoch()
            .ok_or("查找正文几何已经失效")?;
        // Freeze entry-time Input and geometry authority before Native query reads.
        // A reentrant getter cannot bless this action with a newer selection or Scene.
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
        // The virtual NSString retains only WeakDoc. Regex string access must not
        // create NSTextStorage or a hidden view as a second document graph.
        let string = if query.regex {
            Some(
                native_markdown_table_intent_step(&current, || {
                    PieceBackedMarkdownString::new(&document, self.view.mtm())
                })
                .map_err(|_| "建立查找字符串适配时原权限已失效")?,
            )
        } else {
            None
        }; // Literal scanning needs only Core's bounded source windows, no Foundation adapter.
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
        ); // Multi-selection uses its original Core extent; the first actual range is no invented primary head.
        let capture_count = if query.regex {
            query.compiled_regex()?.numberOfCaptureGroups() + 1
        } else {
            0
        };
        let template = ReplacementTemplate::parse(&query.replacement, query.regex, capture_count);
        // Replace All streams anonymous records rather than accumulating all match
        // contents in memory. Other actions allocate no replacement spool yet.
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
                // Selection is an explicit bounded collection, unlike streaming
                // navigation/replacement. Reject before publishing an incomplete set.
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
                    navigation.observe(found, &selection)?;
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
        // Return Foundation query claims before entering a mutable canonical transaction;
        // their destruction may reenter, so validate original authority again afterward.
        drop(string);
        drop(document);
        if !current() {
            return Err("查找临时对象释放后原权限已失效".into());
        }
        // A completed scan establishes totals, but deferred reveal must separately
        // prove actual visibility through the original Source/Input authority.
        let total = navigation.total;
        let target = match action {
            MarkdownFindAction::Next => navigation.navigation_target(false),
            MarkdownFindAction::Previous => navigation.navigation_target(true),
            MarkdownFindAction::ReplaceNext => {
                let target = navigation.next.or(navigation.first);
                if let Some(found) = navigation.current.filter(|found| found.found.precise) {
                    // Editing may change matching semantics; replacement feedback
                    // cannot retain the pre-edit ordinal or total.
                    let found = found.found;
                    let mut replacement = ReplacementSpool::new(&template)?;
                    replacement.push(&found)?;
                    // Map the next original match through this exact replacement.
                    // With no successor, leave the caret after the inserted UTF-16 text.
                    let after = if let Some(target) = &target {
                        replacement.map_selection(MarkdownTextSelection {
                            anchor_utf16: target.found.range.start,
                            head_utf16: target.found.range.end,
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
                // An imprecise/current mismatch only navigates; do not delete a
                // partial normalized match inside an original NFKD character.
                target
            }
            MarkdownFindAction::ReplaceAll => {
                // Consume the original spool in one Core edit transaction, mapping
                // the original selection without publishing intermediate replacements.
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
                // Only the completed bounded scan may publish all actual ranges.
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
            let feedback = if matches!(
                action,
                MarkdownFindAction::Next | MarkdownFindAction::Previous
            ) {
                NativeMarkdownFindRevealFeedback::Numbered(NativeMarkdownFindPosition {
                    ordinal: target.ordinal,
                    total: std::num::NonZeroUsize::new(total)
                        .expect("存在真实导航目标就必有完整正数总量"),
                })
            } else {
                NativeMarkdownFindRevealFeedback::Found
            }; // ReplaceNext navigation still makes no broader count promise than replacement feedback.
            let after = MarkdownTextSelection {
                anchor_utf16: target.found.range.start,
                head_utf16: target.found.range.end,
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
                    feedback,
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

    /// Record committed Core state before any reentrant visual consequence. An
    /// unchanged operation neither creates a receipt nor disturbs undo/redo.
    fn publish_outcome(
        &self,
        outcome: NativeMarkdownPagedInputOutcome,
        committed: &mut Option<Rc<NativeMarkdownPagedInputCommit>>,
    ) -> Result<(), String> {
        match outcome {
            NativeMarkdownPagedInputOutcome::Committed(commit) => {
                let commit = Rc::new(commit);
                // Preserve the actual commit before Native presentation can reenter or fail.
                *committed = Some(Rc::clone(&commit));
                self.view
                    .apply_find_commit(self.input, &commit)
                    .map_err(|error| error.to_string())
            }
            // No matches preserve redo and the original selection; invent no new commit.
            NativeMarkdownPagedInputOutcome::Unchanged(_) => Ok(()),
        }
    }

    /// Build and publish status under the original action/commit authority, then
    /// recheck after releasing the temporary NSString, whose destruction can reenter.
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

    /// After an edit, use its actual committed Input state; before a commit, use
    /// the entry state. Neither path reacquires a newer current document selection.
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

    /// Hand the original target to the existing composer only when the current
    /// ready Scene cannot already prove visibility. Every failure cancels this request.
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
        // An existing ready Scene can finish immediately. Only Pending requests
        // ask the original composer to locate the target, without synchronous paragraph layout.
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
                // Begin may already retain this request; cancellation prevents a
                // late producer from overwriting the failure feedback.
                request.cancel();
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
