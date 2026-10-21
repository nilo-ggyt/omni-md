//! Carry one Find target through the existing bounded layout transaction. Publish
//! success only when the actual target's first glyph enters the visible clip.

use super::*;

/// Explicit diagnostics record only the public monotonic clock and fixed phases,
/// without acquiring text, Native objects, or new action authority.
#[cfg(feature = "native-markdown-runtime-probe")]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn trace_find_native_phase(phase: &'static str) {
    native_markdown_trace! {
        // SAFETY: The signature matches mach_time.h's argument-free uint64 clock;
        // this call neither accesses AppKit nor invokes callbacks.
        unsafe extern "C" {
            fn mach_absolute_time() -> u64;
        }
        // SAFETY: The public clock has no preconditions. The independent sampler's
        // timebase interprets raw ticks; this diagnostic does not assume milliseconds.
        let ticks = unsafe { mach_absolute_time() };
        eprintln!("[probe] find native phase={phase} awake_ticks={ticks}");
        // Sample this PID through its existing ledger to distinguish selection
        // installation from viewport relocation; this is no product-memory acceptance receipt.
        crate::native_surface::probe_call_diagnostic::memory_point(phase);
    }
}

/// Bounded diagnostics use scalars already read by this action, never extra layout
/// or Native queries solely for observation.
#[cfg(feature = "native-markdown-runtime-probe")]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn trace_find_geometry(
    phase: &'static str,
    probe: &std::ops::Range<usize>,
    clip: NSRect,
    frame: Option<NSRect>,
    target: NSPoint,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if COUNT.fetch_add(1, Ordering::Relaxed) >= 64 {
        return; // Diagnostic volume cannot grow with document length or later Find actions.
    }
    eprintln!(
        "native_find_geometry phase={phase} probe={probe:?} clip={clip:?} frame={frame:?} target={target:?}"
    );
}
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use objc2_app_kit::{NSTextLayoutManagerSegmentOptions, NSTextLayoutManagerSegmentType};

/// Feedback describes this committed action only; delayed tickets contain no
/// document contents or search string.
#[derive(Clone, Copy)]
pub(in crate::native_surface) enum NativeMarkdownFindRevealFeedback {
    Found,
    Numbered(NativeMarkdownFindPosition),
    ReplacedNext,
    ReplacedLast,
}

/// Position proved by the original complete navigation scan. Both nonzero scalars
/// end with the original reveal request and retain neither query nor document text.
#[derive(Clone, Copy)]
pub(in crate::native_surface) struct NativeMarkdownFindPosition {
    pub(super) ordinal: std::num::NonZeroUsize,
    pub(super) total: std::num::NonZeroUsize,
}

impl NativeMarkdownFindRevealFeedback {
    /// Only the original visible endpoint may consume this message. Replacement
    /// must not reuse match totals from the pre-edit document.
    pub(super) fn text(self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Found => "找到匹配".into(),
            Self::Numbered(position) => {
                format!("第 {} / {} 处匹配", position.ordinal, position.total).into()
            }
            Self::ReplacedNext => "已替换，并选中下一个匹配".into(),
            Self::ReplacedLast => "已替换，没有更多匹配".into(),
        }
    }
}

/// Hidden navigation is a limited entry-time capability, never authority to replace
/// text or select all matches while the bar is hidden.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native_surface) enum NativeMarkdownFindVisibility {
    Visible,
    HiddenNavigation,
}

impl NativeMarkdownFindVisibility {
    /// Only Next/Previous may use the query preserved on hide. A closed bar always rejects entry.
    pub(in crate::native_surface) fn capture(
        action: MarkdownFindAction,
        visible: bool,
        closed: bool,
    ) -> Option<Self> {
        if closed {
            return None;
        }
        if visible {
            return Some(Self::Visible);
        }
        matches!(
            action,
            MarkdownFindAction::Next | MarkdownFindAction::Previous
        )
        .then_some(Self::HiddenNavigation)
    }

    /// Visibility transitions revoke the original stack. Hiding cannot convert a
    /// visible request into an authorized hidden-navigation request.
    pub(in crate::native_surface) fn permits(
        self,
        visible: bool,
        closed: bool,
        expected_revision: u64,
        current_revision: u64,
    ) -> bool {
        // Hide/show can restore the same boolean; the monotonic revision must still
        // exactly match the original entry ticket.
        !closed && visible == (self == Self::Visible) && expected_revision == current_revision
    }
}

/// Scalar authority for one reveal. Equal generations do not establish document
/// identity; weak allocation identity is checked separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
struct FindRevealStamp {
    owner: usize,
    generation: u64,
    length: usize,
    width: i64,
    mode: MarkdownPresentationMode,
    input_epoch: u64,
    table_epoch: u64,
}

/// Slot membership, live foreground ownership, and allocation identity must all
/// hold. A surviving Rc or one matching revision cannot substitute for authority.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn find_reveal_permissions(
    expected: FindRevealStamp,
    actual: FindRevealStamp,
    slot: bool,
    foreground: bool,
    document: bool,
    bar_authorized: bool,
) -> bool {
    expected == actual && slot && foreground && document && bar_authorized
}

/// Original identity and coordinates from match scanning or a replacement receipt;
/// arming must not acquire the current document generation anew.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) enum NativeMarkdownFindRevealSelection {
    Captured {
        capture: NativeMarkdownSelectionCapture,
        native_before: Option<NSRange>,
    },
    Committed(Rc<NativeMarkdownCommittedSelection>),
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) struct NativeMarkdownFindRevealTarget {
    pub(in crate::native_surface) document: WeakMarkdownDocument,
    pub(in crate::native_surface) generation: u64,
    pub(in crate::native_surface) range: std::ops::Range<usize>,
    pub(in crate::native_surface) feedback: NativeMarkdownFindRevealFeedback,
    pub(in crate::native_surface) visibility: NativeMarkdownFindVisibility,
    /// Comes from action entry; arming never rereads newer show/hide authority.
    pub(in crate::native_surface) bar_revision: u64,
    pub(in crate::native_surface) table_epoch: u64,
    pub(in crate::native_surface) selection: NativeMarkdownFindRevealSelection,
}

/// Each view has one bounded target. Rust weak identities extend neither document
/// nor bar lifetime and retain no Native object or source fragment.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) struct NativeMarkdownFindRevealRequest {
    document: WeakMarkdownDocument,
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    stamp: FindRevealStamp,
    probe: std::ops::Range<usize>,
    feedback: NativeMarkdownFindRevealFeedback,
    visibility: NativeMarkdownFindVisibility,
    bar_revision: u64,
    /// New Find publishes once through its entry capture. Replacement reuses its
    /// committed receipt rather than advancing selection serial a second time.
    selection_capture: Option<NativeMarkdownSelectionCapture>,
    committed: RefCell<Option<Rc<NativeMarkdownCommittedSelection>>>,
    /// Selection callbacks may reenter the executor; coarse relocation must finish
    /// before that executor can consume the target.
    ready: Cell<bool>,
    /// Success requires the complete bounded batch; a literal fallback is no completion receipt.
    prepared: Cell<bool>,
    /// Independent workloads observe only the original request endpoint; default
    /// product builds contain no extra receipt state.
    #[cfg(feature = "native-markdown-runtime-probe")]
    completed: Cell<Option<bool>>,
}

#[cfg(feature = "native-markdown-runtime-probe")]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl NativeMarkdownFindRevealRequest {
    /// Read the original actual endpoint. A removed slot or a successor bar's status
    /// cannot replace this receipt.
    pub(in crate::native_surface) fn probe_completed(&self) -> Option<bool> {
        self.completed.get()
    }

    /// Cancellation records failure for unfinished requests; normal removal must
    /// not overwrite success already issued at the actual endpoint.
    fn record_probe_removal(&self) {
        if self.completed.get().is_none() {
            self.completed.set(Some(false));
        }
    }
}

/// Strong controller/storage claims exist only on the synchronous executor stack;
/// the outer PayloadUseGuard prevents payload teardown within that stack.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) struct NativeMarkdownFindRevealContext {
    request: Rc<NativeMarkdownFindRevealRequest>,
    controller: Retained<NativeMarkdownController>,
    storage: Retained<PieceBackedMarkdownTextStorage>,
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl NativeMarkdownFindRevealContext {
    /// Read Rust metadata only; another getter after a Native call cannot reacquire authority.
    pub(in crate::native_surface) fn is_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        if !self.permissions_are_current(view) {
            return false;
        }
        self.request.selection_is_current()
    }

    /// The publication callback accepts its own transition through existing owner,
    /// source, bar, and input authority, never newly read authority.
    fn permissions_are_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        let request = &self.request;
        let slot_current = view
            .ivars()
            .find_reveal
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, request));
        let identity = self.storage.ivars().document.borrow().clone();
        let Some(document) = identity.upgrade() else {
            return false;
        };
        let actual = {
            let document = document.borrow();
            FindRevealStamp {
                owner: std::ptr::from_ref(&*self.controller).addr(),
                generation: document.generation(),
                length: document.utf16_len(),
                width: self.storage.ivars().table_projection_width_key.get(),
                mode: self.storage.presentation_mode(),
                input_epoch: self.controller.find_input_intent(),
                table_epoch: view.ivars().table_input_intent_epoch.get(),
            }
        };
        find_reveal_permissions(
            request.stamp,
            actual,
            slot_current,
            !view.ivars().textkit_graph_teardown.get()
                && self.storage.ivars().accepts_edits.get()
                && native_markdown_controller_has_foreground_authority(&self.controller),
            std::rc::Weak::ptr_eq(&identity, &request.document),
            request.bar.upgrade().is_some_and(|bar| {
                request.visibility.permits(
                    bar.visible.get(),
                    bar.closed.get(),
                    request.bar_revision,
                    bar.layout_revision.get(),
                )
            }),
        )
    }

    /// Mark only the current ticket after final cache publication. An old local
    /// value cannot certify a successor target's preparation.
    pub(in crate::native_surface) fn mark_prepared(&self, view: &PieceBackedMarkdownTextView) {
        if self.is_current(view) {
            self.request.prepared.set(true);
        }
    }

    /// Lay out only the first complete Unicode scalar. Even a long multiline match
    /// requires neither resident layout of its full selection nor full-document layout.
    pub(in crate::native_surface) fn probe(&self) -> std::ops::Range<usize> {
        self.request.probe.clone()
    }
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl NativeMarkdownFindRevealRequest {
    fn selection_is_current(&self) -> bool {
        let committed = self.committed.borrow();
        if let Some(committed) = committed.as_ref() {
            // Once publication attaches the committed receipt to this same Rc,
            // capture revocation is the expected transition; consume that receipt only.
            committed.current()
        } else {
            self.selection_capture
                .as_ref()
                .is_some_and(NativeMarkdownSelectionCapture::current)
        }
    }
}

/// Preserve the caret for an empty EOF match. A supplementary-plane first scalar
/// must retain both UTF-16 surrogates.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn first_match_scalar(
    start: usize,
    length: usize,
    first: Option<u16>,
) -> Option<std::ops::Range<usize>> {
    if start > length {
        // Do not clamp stale/out-of-range coordinates into a different valid position.
        return None;
    }
    if start == length {
        return Some(start..start);
    }
    let units = if (0xd800..=0xdbff).contains(&first?) {
        2
    } else {
        1
    };
    let end = start.checked_add(units)?;
    (end <= length).then_some(start..end)
}

/// Require valid segment/clip geometry. For an oversized glyph, visibility follows
/// its origin; this does not claim the entire match is simultaneously visible.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn find_segment_is_visible(frame: NSRect, clip: NSRect) -> bool {
    let Some(origin) = native_markdown_minimal_cell_scroll_origin(frame, clip) else {
        return false; // NaN, infinite, or empty clip geometry cannot prove visibility.
    };
    (origin.x - clip.origin.x).abs() <= 0.5 && (origin.y - clip.origin.y).abs() <= 0.5
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl PieceBackedMarkdownTextView {
    /// New commands, resize, physical input, and accepted release revoke authority
    /// synchronously. Drop claims after ending the slot borrow.
    pub(in crate::native_surface) fn cancel_pending_find_reveal(&self) {
        // A previous mode target cannot later restore this same view over a new Find action.
        self.cancel_pending_mode_viewport();
        let old = self.ivars().find_reveal.borrow_mut().take();
        #[cfg(feature = "native-markdown-runtime-probe")]
        if let Some(old) = old.as_ref() {
            old.record_probe_removal(); // Only the original Rust ticket changes; no borrow or Native call remains.
        }
        drop(old);
    }

    /// TextKit updating frame height for actual content is not a user width change;
    /// revoke only targets whose column width differs.
    pub(in crate::native_surface) fn cancel_find_reveal_for_width(&self, width: f64) {
        let metric = native_markdown_layout_metric(width);
        let changed = self
            .ivars()
            .find_reveal
            .borrow()
            .as_ref()
            .is_some_and(|request| request.stamp.width != metric);
        if changed {
            self.cancel_pending_find_reveal();
        }
    }

    /// Clone only the Rust Rc while borrowing. Native weak loads/retains happen
    /// outside that borrow and must preserve original authority across reentry.
    pub(in crate::native_surface) fn pending_find_reveal_context(
        &self,
    ) -> Option<NativeMarkdownFindRevealContext> {
        let request = self.ivars().find_reveal.borrow().as_ref().map(Rc::clone)?;
        if !request.ready.get() || self.ivars().textkit_graph_teardown.get() {
            // Do not consume during Native coarse relocation; accepted release owns closed-slot cleanup.
            return None;
        }
        let controller = self.ivars().controller.get()?.load()?;
        if std::ptr::from_ref(&*controller).addr() != request.stamp.owner
            || controller.find_input_intent() != request.stamp.input_epoch
            || !native_markdown_controller_has_foreground_authority(&controller)
        {
            self.remove_find_reveal_if_same(&request);
            return None;
        }
        let storage = self.ivars().text_storage.load()?;
        let context = NativeMarkdownFindRevealContext {
            request,
            controller,
            storage,
        };
        if context.is_current(self) {
            Some(context)
        } else {
            // Input during a Native weak upgrade revokes only this old target, never its successor.
            self.remove_find_reveal_if_same(&context.request);
            None
        }
    }

    fn remove_find_reveal_if_same(&self, request: &Rc<NativeMarkdownFindRevealRequest>) {
        let old = {
            let mut slot = self.ivars().find_reveal.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, request))
            {
                slot.take()
            } else {
                None // A successor already occupies the slot; this old executor cannot take it.
            }
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        if let Some(old) = old.as_ref() {
            old.record_probe_removal();
        }
        drop(old);
    }

    /// Observe the original ticket after synchronous Find returns. Missing tickets
    /// mean no receipt; newer UI cannot establish the previous action's success.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn observe_find_reveal_for_probe(
        &self,
    ) -> Option<Rc<NativeMarkdownFindRevealRequest>> {
        self.ivars().find_reveal.borrow().as_ref().map(Rc::clone)
    }

    /// Arm one target and relocate the existing Native viewport once for coarse
    /// positioning. Later bounded preparation is not another command or timed retry.
    pub(in crate::native_surface) fn begin_find_reveal(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        controller: &NativeMarkdownController,
        intent: u64,
        target: NativeMarkdownFindRevealTarget,
    ) -> Result<(), String> {
        let current = || {
            native_markdown_controller_has_foreground_authority(controller)
                && controller.find_input_intent() == intent
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == target.table_epoch
                && target.visibility.permits(
                    bar.visible.get(),
                    bar.closed.get(),
                    target.bar_revision,
                    bar.layout_revision.get(),
                )
        };
        if !current() {
            return Err("查找定位已被新的输入或关闭撤销".into());
        }
        let selection = MarkdownTextSelection {
            anchor_utf16: target.range.start,
            head_utf16: target.range.end,
        };
        let (original_capture, captured, committed) = match target.selection {
            NativeMarkdownFindRevealSelection::Captured {
                capture,
                native_before,
            } => {
                // The explicit Find target supplies direction. Its capture predates
                // query getters and must not be reacquired from the current selection.
                let captured = capture
                    .clone()
                    .finish(selection, true)
                    .map_err(|error| error.to_string())?;
                let captured = if let Some(native_before) = native_before {
                    captured
                        .with_observed_native_before(native_before)
                        .map_err(|error| error.to_string())?
                } else {
                    // Actual multi-selection has no single prior range/head; keep
                    // the original capture without inventing a prior single selection.
                    captured
                }
                .with_head_side(Some(false))
                .with_vertical_goal(None);
                (Some(capture), Some(captured), None)
            }
            NativeMarkdownFindRevealSelection::Committed(committed) => {
                if !committed.current() || committed.selection != selection {
                    // Replacement's committed receipt must match this exact target;
                    // republishing the selection to obtain a new serial is invalid.
                    return Err("替换后的查找选择收据已失效".into());
                }
                (None, None, Some(committed))
            }
        };
        let storage = self.ivars().text_storage.load().ok_or("笔记编辑器已关闭")?;
        if !current()
            || original_capture
                .as_ref()
                .is_some_and(|capture| !capture.current())
            || committed
                .as_ref()
                .is_some_and(|committed| !committed.current())
        {
            return Err("查找定位已失效".into());
        }
        let identity = storage.ivars().document.borrow().clone();
        if !std::rc::Weak::ptr_eq(&identity, &target.document) {
            return Err("查找所属笔记已变化".into());
        }
        let document = identity.upgrade().ok_or("笔记文档已关闭")?;
        // Generation equality is insufficient: either the original capture or the
        // replacement receipt must identify this exact document allocation.
        let belongs = captured
            .as_ref()
            .map(|captured| captured.belongs_to(&document))
            .or_else(|| {
                committed.as_ref().map(|committed| {
                    committed
                        .captured_selection()
                        .is_ok_and(|captured| captured.belongs_to(&document))
                })
            })
            .unwrap_or(false);
        if !belongs {
            return Err("查找选择不属于原笔记收据".into());
        }
        // Read only the first code unit needed to bound the target scalar. The
        // complete match remains a selection, not a materialized layout range.
        let (generation, length, probe) = {
            let document = document.borrow();
            if document.generation() != target.generation {
                return Err("查找定位期间正文已变更".into());
            }
            let length = document.utf16_len();
            let first = (target.range.start < length)
                .then(|| document.utf16_code_unit_at(target.range.start).ok())
                .flatten();
            (
                document.generation(),
                length,
                first_match_scalar(target.range.start, length, first).ok_or("查找定位范围无效")?,
            )
        };
        if target.range.end > length || target.range.end < target.range.start || !current() {
            return Err("查找定位范围已失效".into());
        }
        let request = Rc::new(NativeMarkdownFindRevealRequest {
            document: identity,
            bar: Rc::downgrade(bar),
            stamp: FindRevealStamp {
                owner: std::ptr::from_ref(controller).addr(),
                generation,
                length,
                width: storage.ivars().table_projection_width_key.get(),
                mode: storage.presentation_mode(),
                input_epoch: intent,
                table_epoch: target.table_epoch,
            },
            probe,
            feedback: target.feedback,
            visibility: target.visibility,
            bar_revision: target.bar_revision,
            selection_capture: original_capture,
            committed: RefCell::new(committed),
            ready: Cell::new(false),
            prepared: Cell::new(false),
            #[cfg(feature = "native-markdown-runtime-probe")]
            completed: Cell::new(None),
        });
        // Acquire local Native ownership before slot publication. Failure must not
        // strand an unready ticket that prevents subsequent cache consumption.
        if !current() || !request.selection_is_current() {
            return Err("查找输入已变更".into());
        }
        let actual_controller = self
            .ivars()
            .controller
            .get()
            .and_then(Weak::load)
            .ok_or("查找所属窗口已释放")?;
        if !current() || !request.selection_is_current() {
            return Err("查找输入已变更".into());
        }
        if std::ptr::from_ref(&*actual_controller).addr() != std::ptr::from_ref(controller).addr() {
            return Err("查找栏已由其他操作接管".into());
        }
        let previous = self.ivars().find_reveal.replace(Some(Rc::clone(&request)));
        #[cfg(feature = "native-markdown-runtime-probe")]
        if let Some(previous) = previous.as_ref() {
            // Replacing the slot also gives the old request an explicit unsuccessful endpoint.
            previous.record_probe_removal();
        }
        drop(previous);
        // This context never crosses events; it authenticates the same target
        // before and after each potentially reentrant Native step.
        let context = NativeMarkdownFindRevealContext {
            request,
            controller: actual_controller,
            storage,
        };
        let is_current = || context.is_current(self);
        let result = (|| {
            native_markdown_table_intent_step(&is_current, || {
                if bar.is_visible() {
                    // Hidden Cmd-G only navigates; it neither mounts the bar,
                    // changes input focus, nor lays out hidden feedback controls.
                    bar.status.setStringValue(ns_string!("正在定位匹配…"));
                }
            })?;
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_native_phase("selection_begin");
            let committed = if let Some(captured) = captured {
                if !is_current() {
                    return Err(NativeMarkdownStableTableFailure::InputIntentExpired);
                }
                context
                    .storage
                    .update_active_selection_observed(captured, |committed| {
                        // No Native call or document borrow here: attach the
                        // original transition receipt to this same Rc before attrs can reenter.
                        if context.permissions_are_current(self)
                            && context.request.committed.borrow().is_none()
                        {
                            *context.request.committed.borrow_mut() = Some(Rc::clone(committed));
                        }
                    })
                    .ok_or(NativeMarkdownStableTableFailure::InputIntentExpired)?
            } else {
                context
                    .request
                    .committed
                    .borrow()
                    .as_ref()
                    .map(Rc::clone)
                    .ok_or(NativeMarkdownStableTableFailure::InputIntentExpired)?
            };
            let same_receipt = context
                .request
                .committed
                .borrow()
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, &committed));
            if !same_receipt || !self.apply_committed_selection_if(&committed, is_current) {
                return Err(NativeMarkdownStableTableFailure::InputIntentExpired);
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_native_phase("selection_return");
            // Preserve the full selection while relocating the Native viewport
            // to its original first scalar; generic scrolling could eagerly lay out offscreen content.
            self.relocate_find_viewport(&context, &is_current)?;
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_native_phase("coarse_scroll_return");
            Ok::<(), NativeMarkdownStableTableFailure>(())
        })();
        if result.is_err() {
            // Getter/setter reentry removes only this old ticket, preserving any successor slot.
            self.remove_find_reveal_if_same(&context.request);
            return Err("查找定位已失效".into());
        }
        context.request.ready.set(true);
        self.schedule_visible_table_layout_update();
        Ok(())
    }

    /// Return the first actual glyph rectangle. The enumeration callback copies one
    /// scalar rectangle and stops immediately, retaining no Native range.
    fn find_reveal_segment(
        &self,
        context: &NativeMarkdownFindRevealContext,
    ) -> Result<NSRect, NativeMarkdownStableTableFailure> {
        self.source_probe_segment(&context.probe(), &|| context.is_current(self))
    }

    /// Coarse positioning relocates only the original request's viewport. Its
    /// suggested anchor is not success; actual first-glyph/clip verification follows.
    fn relocate_find_viewport(
        &self,
        context: &NativeMarkdownFindRevealContext,
        current: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let manager = native_markdown_table_intent_step(current, || self.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let content = native_markdown_table_intent_step(current, || manager.textContentManager())?
            .ok_or(Failure::MissingGeometry)?;
        let require_original_content_graph = || -> Result<(), Failure> {
            let actual_manager =
                native_markdown_table_intent_step(current, || self.textLayoutManager())?
                    .ok_or(Failure::MissingGeometry)?;
            let actual_content =
                native_markdown_table_intent_step(current, || manager.textContentManager())?
                    .ok_or(Failure::MissingGeometry)?;
            if !std::ptr::eq(&*actual_manager, &*manager)
                || !std::ptr::eq(&*actual_content, &*content)
            {
                // Matching revisions cannot authorize a Native graph detached from this view.
                return Err(Failure::InvalidSourceAssociation);
            }
            Ok(())
        };
        require_original_content_graph()?;
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let document = native_markdown_table_intent_step(current, || provider.documentRange())?;
        let start = native_markdown_table_intent_step(current, || document.location())?;
        let offset = isize::try_from(context.request.probe.start)
            .map_err(|_| Failure::InvalidSourceAssociation)?;
        let location = native_markdown_table_intent_step(current, || {
            content.locationFromLocation_withOffset(&start, offset)
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        if native_markdown_table_intent_step(current, || {
            content.offsetFromLocation_toLocation(&start, &location)
        })? != offset
        {
            // Native positions must round-trip the same canonical offset; an
            // estimated location cannot stand in for the original Find match.
            return Err(Failure::InvalidSourceAssociation);
        }
        let visible = native_markdown_table_intent_step(current, || self.visibleRect())?;
        let visible_source = self.read_visible_utf16_window(visible, context.request.stamp.length);
        if !current() {
            // Visible-line reads can reenter; this old Find cannot relocate a successor viewport.
            return Err(Failure::InputIntentExpired);
        }
        require_original_content_graph()?;
        if !visible_source.is_empty()
            && visible_source.start <= context.request.probe.start
            && context.request.probe.end <= visible_source.end
        {
            // Preserve the viewport for a target already within actual visible
            // lines instead of reestimating their laid-out paragraph positions.
            // Bounded preparation and glyph verification still must pass; source
            // intersection alone cannot establish Find success.
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_geometry(
                "already_in_viewport",
                &context.probe(),
                visible,
                None,
                visible.origin,
            );
            return Ok(());
        }
        let viewport =
            native_markdown_table_intent_step(current, || manager.textViewportLayoutController())?;
        let actual_manager =
            native_markdown_table_intent_step(current, || viewport.textLayoutManager())?
                .ok_or(Failure::MissingGeometry)?;
        if !std::ptr::eq(&*actual_manager, &*manager) {
            // Getter reentry changed the association; do not relocate a successor manager's viewport.
            return Err(Failure::InvalidSourceAssociation);
        }
        let require_original_graph = || -> Result<(), Failure> {
            require_original_content_graph()?;
            let actual_viewport_manager =
                native_markdown_table_intent_step(current, || viewport.textLayoutManager())?
                    .ok_or(Failure::MissingGeometry)?;
            if !std::ptr::eq(&*actual_viewport_manager, &*manager) {
                // Relocation/getters can reenter. An old manager's suggested
                // anchor cannot be applied to a successor Native graph.
                return Err(Failure::InvalidSourceAssociation);
            }
            Ok(())
        };
        // Prove viewport membership before relocation, not after moving a replaced graph.
        require_original_graph()?;
        let anchor = native_markdown_table_intent_step(current, || {
            viewport.relocateViewportToTextLocation(&location)
        })?;
        require_original_graph()?;
        let origin = native_markdown_table_intent_step(current, || self.textContainerOrigin())?;
        let visible = native_markdown_table_intent_step(current, || self.visibleRect())?;
        let target_y = anchor + origin.y;
        if !target_y.is_finite() || !visible.origin.x.is_finite() {
            // Invalid suggested geometry is an explicit failure, not permission to
            // retry another scrolling API or guess the target page.
            return Err(Failure::MissingGeometry);
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_geometry(
            "relocate",
            &context.probe(),
            visible,
            None,
            NSPoint::new(visible.origin.x, target_y),
        );
        let scroll = native_markdown_table_intent_step(current, || self.enclosingScrollView())?
            .ok_or(Failure::MissingGeometry)?;
        let clip = native_markdown_table_intent_step(current, || scroll.contentView())?;
        let document_view = native_markdown_table_intent_step(current, || clip.documentView())?
            .ok_or(Failure::MissingGeometry)?;
        let original_view: &NSView = self;
        if !std::ptr::eq(&*document_view, original_view) {
            // A surviving scroll view need not still host this text view; never move another document view.
            return Err(Failure::InvalidSourceAssociation);
        }
        require_original_graph()?;
        native_markdown_table_intent_step(current, || {
            // Preserve horizontal position. NSClipView constrains document start/EOF;
            // later actual-segment verification checks the resulting position.
            clip.scrollToPoint(NSPoint::new(visible.origin.x, target_y.max(0.0)));
        })?;
        native_markdown_table_intent_step(current, || scroll.reflectScrolledClipView(&clip))?;
        Ok(())
    }

    /// Find and mode continuity share actual first-scalar/EOF geometry, while each
    /// retains its own distinct authority ticket.
    pub(in crate::native_surface) fn source_probe_segment(
        &self,
        probe: &std::ops::Range<usize>,
        current: &impl Fn() -> bool,
    ) -> Result<NSRect, NativeMarkdownStableTableFailure> {
        self.source_probe_segment_with_affinity(probe, false, current)
    }

    /// An empty caret at a wrap boundary can occupy either visual line; selection
    /// navigation preserves the affinity already proved for that caret.
    pub(in crate::native_surface) fn source_probe_segment_with_affinity(
        &self,
        probe: &std::ops::Range<usize>,
        upstream: bool,
        current: &impl Fn() -> bool,
    ) -> Result<NSRect, NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let cache = self.ivars().visible_table_layout_cache.borrow().clone();
        if let Some(cache) = cache {
            let rect = native_markdown_table_scalar_frame(&cache.tables, probe)?;
            if let Some(rect) = rect {
                // A visible CTFrame still needs the original document and complete
                // table-geometry ticket; stale row positions cannot certify new input.
                let actual = native_markdown_table_intent_step(current, || {
                    self.current_table_layout_request_key()
                })?
                .ok_or(Failure::MissingGeometry)?;
                if actual != cache.request_key {
                    return Err(Failure::InputIntentExpired);
                }
                return Ok(rect);
            }
        }
        let manager = native_markdown_table_intent_step(&current, || self.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let content = native_markdown_table_intent_step(&current, || manager.textContentManager())?
            .ok_or(Failure::MissingGeometry)?;
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let native_document =
            native_markdown_table_intent_step(&current, || provider.documentRange())?;
        let start = native_markdown_table_intent_step(&current, || native_document.location())?;
        let from = isize::try_from(probe.start).map_err(|_| Failure::InvalidSourceAssociation)?;
        let to = isize::try_from(probe.end).map_err(|_| Failure::InvalidSourceAssociation)?;
        let location = native_markdown_table_intent_step(&current, || {
            content.locationFromLocation_withOffset(&start, from)
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        let end = native_markdown_table_intent_step(&current, || {
            content.locationFromLocation_withOffset(&start, to)
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        let allocated = native_markdown_table_intent_step(&current, NSTextRange::alloc)?;
        let range = native_markdown_table_intent_step(&current, || {
            NSTextRange::initWithLocation_endLocation(allocated, &location, Some(&end))
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        let origin = native_markdown_table_intent_step(&current, || self.textContainerOrigin())?;
        let frame = Cell::new(None);
        let block = block2::RcBlock::new(
            |_range: *mut NSTextRange,
             rect: NSRect,
             _baseline: f64,
             _container: NonNull<NSTextContainer>| {
                // Copy the first callback rectangle without getters, then stop.
                // Never expand this probe into full-selection or paragraph layout.
                frame.set(Some(NSRect::new(
                    NSPoint::new(rect.origin.x + origin.x, rect.origin.y + origin.y),
                    NSSize::new(rect.size.width.max(1.0), rect.size.height),
                )));
                objc2::runtime::Bool::NO
            },
        );
        let mut options = NSTextLayoutManagerSegmentOptions::RangeNotRequired;
        if probe.is_empty() && upstream {
            // Nonempty probes keep existing glyph geometry; only an explicitly
            // empty position uses upstream wrap affinity.
            options |= NSTextLayoutManagerSegmentOptions::UpstreamAffinity;
        }
        native_markdown_table_intent_step(&current, || {
            manager.enumerateTextSegmentsInRange_type_options_usingBlock(
                &range,
                NSTextLayoutManagerSegmentType::Selection,
                options,
                &block,
            )
        })?;
        drop(block);
        frame
            .get()
            .filter(|rect| rect.size.height.is_finite() && rect.size.height > 0.0)
            .ok_or(Failure::MissingGeometry)
    }

    /// Table-free/Source paths still prepare the same bounded target and actual
    /// viewport; they cannot bypass original-target authentication.
    pub(in crate::native_surface) fn prepare_find_reveal_without_tables(
        &self,
        context: &NativeMarkdownFindRevealContext,
        current: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        self.ensure_stable_table_source_range(&context.probe(), current)?;
        let manager = native_markdown_table_intent_step(current, || self.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let viewport =
            native_markdown_table_intent_step(current, || manager.textViewportLayoutController())?;
        native_markdown_table_intent_step(current, || viewport.layoutViewport())?;
        self.position_find_reveal(context)
    }

    /// Once table height is known, adjust the clip minimally using the actual
    /// segment rather than issuing another general scroll-to-range command.
    pub(in crate::native_surface) fn position_find_reveal(
        &self,
        context: &NativeMarkdownFindRevealContext,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let current = || context.is_current(self);
        let frame = self.find_reveal_segment(context)?;
        let visible = native_markdown_table_intent_step(&current, || self.visibleRect())?;
        let target = native_markdown_minimal_cell_scroll_origin(frame, visible)
            .ok_or(Failure::MissingGeometry)?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_geometry("position", &context.probe(), visible, Some(frame), target);
        if (target.x - visible.origin.x).abs() > 0.01 || (target.y - visible.origin.y).abs() > 0.01
        {
            let scroll =
                native_markdown_table_intent_step(&current, || self.enclosingScrollView())?
                    .ok_or(Failure::MissingGeometry)?;
            let clip = native_markdown_table_intent_step(&current, || scroll.contentView())?;
            native_markdown_table_intent_step(&current, || clip.scrollToPoint(target))?;
            native_markdown_table_intent_step(&current, || scroll.reflectScrolledClipView(&clip))?;
            let manager = native_markdown_table_intent_step(&current, || self.textLayoutManager())?
                .ok_or(Failure::MissingGeometry)?;
            let viewport = native_markdown_table_intent_step(&current, || {
                manager.textViewportLayoutController()
            })?;
            native_markdown_table_intent_step(&current, || viewport.layoutViewport())?;
        }
        // EOF constraints/layout can change the clip. Re-read and verify actual
        // visibility; an accepted scrolling request is insufficient.
        self.verify_find_reveal(context)
    }

    fn verify_find_reveal(
        &self,
        context: &NativeMarkdownFindRevealContext,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        let frame = self.find_reveal_segment(context)?;
        let clip =
            native_markdown_table_intent_step(&|| context.is_current(self), || self.visibleRect())?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_geometry("verify", &context.probe(), clip, Some(frame), clip.origin);
        if find_segment_is_visible(frame, clip) {
            Ok(())
        } else {
            Err(NativeMarkdownStableTableFailure::AnchorUnstable)
        }
    }

    /// If TextKit disallows layout, fail this reveal instead of retrying on later draws.
    pub(in crate::native_surface) fn fail_pending_find_reveal(&self) {
        let Some(context) = self.pending_find_reveal_context() else {
            return;
        };
        if context.is_current(self)
            && let Some(bar) = context.request.bar.upgrade()
            && bar.is_visible()
        {
            bar.status
                .setStringValue(ns_string!("匹配已选中，但当前编辑状态不允许定位"));
        }
        self.remove_find_reveal_if_same(&context.request);
    }

    /// Publish success only after this same bounded executor finishes. Remove
    /// unsuccessful targets rather than retrying through draws or timers.
    pub(in crate::native_surface) fn finish_pending_find_reveal(&self) {
        let Some(context) = self.pending_find_reveal_context() else {
            return;
        };
        let prepared = context.request.prepared.get();
        let verified = prepared && self.verify_find_reveal(&context).is_ok();
        if context.is_current(self)
            && let Some(bar) = context.request.bar.upgrade()
            && bar.is_visible()
        {
            let text = if verified {
                context.request.feedback.text()
            } else if !prepared {
                // Seeing the first character does not prove complete batch
                // preparation; retain the success gate and report the actual failure stage.
                "匹配已选中，但当前内容的布局未完成".into()
            } else {
                // A prepared batch still needs actual viewport verification under
                // the same ticket; selected state cannot substitute for visibility.
                "匹配已选中，但未能完成可见区定位".into()
            };
            let text = NSString::from_str(&text);
            // All earlier Native getters returned. Check pure authority before
            // the setter, then remove only this same slot.
            if context.is_current(self) {
                bar.status.setStringValue(&text);
            }
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        if context.request.completed.get().is_none() {
            // Preserve the actual success conditions. Recheck the same original
            // ticket after the feedback setter and its temporary claims return.
            context
                .request
                .completed
                .set(Some(verified && context.is_current(self)));
        }
        self.remove_find_reveal_if_same(&context.request);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_probe_keeps_unicode_scalar_and_eof_without_full_match_layout() {
        assert_eq!(first_match_scalar(12, 100, Some(0x4e2d)), Some(12..13));
        assert_eq!(first_match_scalar(12, 100, Some(0xd83d)), Some(12..14));
        assert_eq!(first_match_scalar(100, 100, None), Some(100..100));
        assert_eq!(first_match_scalar(100, 99, None), None);
        assert_eq!(first_match_scalar(99, 100, Some(0xd83d)), None);
    }

    #[test]
    fn visible_receipt_rejects_old_clip_and_nonfinite_geometry() {
        let clip = NSRect::new(NSPoint::new(0.0, 100.0), NSSize::new(600.0, 400.0));
        let row = |y| NSRect::new(NSPoint::new(10.0, y), NSSize::new(14.0, 24.0));
        assert!(find_segment_is_visible(row(450.0), clip));
        assert!(!find_segment_is_visible(row(490.0), clip));
        assert!(!find_segment_is_visible(row(700.0), clip));
        assert!(!find_segment_is_visible(row(f64::NAN), clip));
        assert!(!find_segment_is_visible(row(450.0), NSRect::ZERO));
    }
    #[test]
    fn hidden_navigation_preserves_query_without_granting_hidden_mutation() {
        for action in [MarkdownFindAction::Next, MarkdownFindAction::Previous] {
            assert_eq!(
                NativeMarkdownFindVisibility::capture(action, false, false),
                Some(NativeMarkdownFindVisibility::HiddenNavigation)
            );
        }
        for action in [
            MarkdownFindAction::ReplaceNext,
            MarkdownFindAction::ReplaceAll,
            MarkdownFindAction::SelectAll,
            MarkdownFindAction::Hide,
        ] {
            assert_eq!(
                NativeMarkdownFindVisibility::capture(action, false, false),
                None
            );
        }
        for action in [
            MarkdownFindAction::Next,
            MarkdownFindAction::Previous,
            MarkdownFindAction::ReplaceNext,
        ] {
            assert_eq!(
                NativeMarkdownFindVisibility::capture(action, true, true),
                None
            );
        }
        assert!(NativeMarkdownFindVisibility::HiddenNavigation.permits(false, false, 1, 1));
        assert!(!NativeMarkdownFindVisibility::HiddenNavigation.permits(true, false, 1, 1));
        assert!(!NativeMarkdownFindVisibility::Visible.permits(false, false, 1, 1));
        assert!(!NativeMarkdownFindVisibility::HiddenNavigation.permits(false, true, 1, 1));
    }
    #[test]
    fn show_hide_aba_does_not_restore_an_old_navigation_ticket() {
        let original = 40;
        let after_two_transitions = original + 2;
        // Visible/hidden round trips restore the boolean, but both original tickets remain expired.
        assert!(!NativeMarkdownFindVisibility::Visible.permits(
            true,
            false,
            original,
            after_two_transitions
        ));
        assert!(!NativeMarkdownFindVisibility::HiddenNavigation.permits(
            false,
            false,
            original,
            after_two_transitions
        ));
        assert!(NativeMarkdownFindVisibility::Visible.permits(true, false, original, original));
        assert!(
            NativeMarkdownFindVisibility::HiddenNavigation
                .permits(false, false, original, original)
        );
    }
}
