//! Advance body mouse selection through the original pointer source and each actual action ticket;
//! return system drag-and-drop before changing the original selection.

use super::*;
use objc2::runtime::Bool;
use objc2_app_kit::{
    NSEventMask, NSEventTrackingRunLoopMode, NSLinkAttributeName, NSTextSelectionGranularity,
};

/// Return a synchronous handoff to parent mouseDown. NativeOwned cannot recapture current
/// authority before calling super. Fixed tickets, Weak identities and scalars stay inline
/// on this stack, avoiding a separate Box allocation for every native mouse handoff.
#[allow(
    clippy::large_enum_variant,
    reason = "synchronous mouse forwarding keeps fixed tickets inline instead of adding a hot-path heap allocation"
)]
pub(super) enum NativeMarkdownMouseDisposition {
    Handled,
    NativeOwned(NativeMarkdownMouseNativeForward),
}

#[derive(Clone)]
/// Identify the original logical source and storage geometry without retaining its document root.
struct MouseSource {
    document: WeakMarkdownDocument,
    generation: u64,
    length: usize,
    width: i64,
    mode: MarkdownPresentationMode,
}

impl MouseSource {
    /// Require both original generation and UTF-16 extent before a source coordinate remains usable.
    fn current(&self) -> bool {
        self.document.upgrade().is_some_and(|document| {
            let document = document.borrow();
            document.generation() == self.generation && document.utf16_len() == self.length
        })
    }

    /// Combine original document authority with this storage's identity, mode, width and edit permission.
    fn storage_is_current(&self, storage: &PieceBackedMarkdownTextStorage) -> bool {
        self.same_storage_context(storage) && self.current()
    }

    /// Check the unchanged storage context separately while a known cell commit advances the source generation.
    fn same_storage_context(&self, storage: &PieceBackedMarkdownTextStorage) -> bool {
        storage.ivars().accepts_edits.get()
            && storage.presentation_mode() == self.mode
            && storage.ivars().table_projection_width_key.get() == self.width
            && std::rc::Weak::ptr_eq(&self.document, &storage.ivars().document.borrow())
    }

    /// Prove both UTF-16 endpoints belong to the original root before navigation;
    /// a later commit may map only this real source, never repair guessed coordinates.
    fn contains_selection(&self, selection: MarkdownTextSelection) -> bool {
        self.document.upgrade().is_some_and(|document| {
            let document = document.borrow();
            document.generation() == self.generation
                && document.utf16_len() == self.length
                && document
                    .byte_offset_for_utf16(selection.anchor_utf16)
                    .is_ok()
                && document.byte_offset_for_utf16(selection.head_utf16).is_ok()
        })
    }
}

/// Keep only pure capture/Weak/scalar authority. Temporarily retain original storage across
/// the synchronous super call so destructor reentry cannot separate validation from forwarding.
pub(super) struct NativeMarkdownMouseNativeForward {
    captured: NativeMarkdownCapturedSelection,
    source: MouseSource,
    input_epoch: u64,
}

impl NativeMarkdownMouseNativeForward {
    /// Execute super once only while the original captured action and retained storage still match.
    pub(super) fn if_current<T>(
        &self,
        view: &PieceBackedMarkdownTextView,
        work: impl FnOnce() -> T,
    ) -> Option<T> {
        let initial = || {
            self.captured.current()
                && !view.ivars().textkit_graph_teardown.get()
                && view.ivars().table_input_intent_epoch.get() == self.input_epoch
                && self.source.current()
        };
        let storage = step(&initial, || view.ivars().text_storage.load()).ok()??;
        if !initial() || !self.source.storage_is_current(&storage) {
            return None;
        }
        // The storage claim covers the complete synchronous super stack. No old action
        // writes selection or scrolling state after that stack returns.
        Some(work())
    }
}

/// Bind one concrete mouse action to its original selection capture and input epoch.
struct MouseAction {
    captured: NativeMarkdownCapturedSelection,
    source: MouseSource,
    input_epoch: u64,
}

impl MouseAction {
    /// A current selection capture alone cannot override changed input, closed graph or replaced storage.
    fn current(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> bool {
        self.captured.current()
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().table_input_intent_epoch.get() == self.input_epoch
            && self.source.storage_is_current(storage)
    }

    /// Transfer the same action authority without opening a new source or retaining a native event.
    fn forward(self) -> NativeMarkdownMouseDisposition {
        NativeMarkdownMouseDisposition::NativeOwned(NativeMarkdownMouseNativeForward {
            captured: self.captured,
            source: self.source,
            input_epoch: self.input_epoch,
        })
    }
}

/// Retain pure source facts, original range/granularity and the last accepted receipt,
/// without a native object or strong document root in the gesture.
struct MouseGesture {
    source: MouseSource,
    anchor: MarkdownTextSelection,
    granularity: NSTextSelectionGranularity,
    extending: bool,
    dragged: bool,
    last: Rc<NativeMarkdownCommittedSelection>,
    input_epoch: u64,
}

impl MouseGesture {
    /// Continue only from the last actually committed selection under the same original source and input epoch.
    fn current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        self.last.current()
            && self.source.current()
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().table_input_intent_epoch.get() == self.input_epoch
    }
}

/// Revalidate the original mouse authority around each potentially reentrant native operation.
fn step<T>(current: &impl Fn() -> bool, work: impl FnOnce() -> T) -> Result<T, String> {
    native_markdown_table_intent_step(current, work)
        .map_err(|_| "鼠标对应的笔记、窗口或输入已经改变".into())
}

/// Retain only the actual borrowed tracking event, under the same ticket, until its synchronous owner releases it.
fn retain_event(event: &NSEvent, current: &impl Fn() -> bool) -> Result<Retained<NSEvent>, String> {
    step(current, || {
        // SAFETY: mouseDown/the synchronous NSWindow handler lent this live event;
        // the retained claim never escapes the tracking stack.
        unsafe { Retained::retain(std::ptr::from_ref(event).cast_mut()) }
    })?
    .ok_or_else(|| "鼠标事件已释放".into())
}

/// Derive a finite ordered range without losing the separately retained logical anchor/head direction.
fn ordered(selection: MarkdownTextSelection) -> NSRange {
    let start = selection.anchor_utf16.min(selection.head_utf16);
    NSRange::new(
        start,
        selection.anchor_utf16.max(selection.head_utf16) - start,
    )
}

/// Preserve standard click granularity: one character, double-click word, triple-or-more paragraph.
fn click_granularity(count: isize) -> NSTextSelectionGranularity {
    match count {
        2 => NSTextSelectionGranularity::Word,
        3.. => NSTextSelectionGranularity::Paragraph,
        _ => NSTextSelectionGranularity::Character,
    }
}

/// A source endpoint covered by replacement has no unique consequence. Apply UTF-16
/// displacement only; the successor root must still prove its byte/scalar boundary.
fn map_position(at: usize, edit: &MarkdownHistoryTextEdit) -> Option<usize> {
    let range = &edit.replaced_utf16_range;
    if range.start > range.end {
        return None;
    }
    if at < range.start {
        Some(at)
    } else if at >= range.end {
        at.checked_add_signed(edit.change_in_utf16_length)
    } else {
        None
    }
}

/// Map only an intact source interval wholly outside the replaced range; overlapping text has no unique successor.
fn map_range(range: NSRange, edit: &MarkdownHistoryTextEdit) -> Option<NSRange> {
    let end = range.location.checked_add(range.length)?;
    if edit.replaced_utf16_range.start > edit.replaced_utf16_range.end {
        return None;
    }
    if range.length == 0 {
        return map_position(range.location, edit).map(|at| NSRange::new(at, 0));
    }
    if end <= edit.replaced_utf16_range.start {
        Some(range)
    } else if range.location >= edit.replaced_utf16_range.end {
        Some(NSRange::new(
            range
                .location
                .checked_add_signed(edit.change_in_utf16_length)?,
            range.length,
        ))
    } else {
        None
    }
}

/// Map the intact selected source while preserving forward or reversed logical direction.
fn map_selection(
    selection: MarkdownTextSelection,
    edit: &MarkdownHistoryTextEdit,
) -> Option<MarkdownTextSelection> {
    let range = map_range(ordered(selection), edit)?;
    let end = range.location.checked_add(range.length)?;
    Some(if selection.head_utf16 < selection.anchor_utf16 {
        MarkdownTextSelection {
            anchor_utf16: end,
            head_utf16: range.location,
        }
    } else {
        MarkdownTextSelection {
            anchor_utf16: range.location,
            head_utf16: end,
        }
    })
}

/// Shift's anchor and pointer head have separate source identities; an edit between them
/// does not mean either endpoint itself was overwritten.
fn map_endpoints(
    selection: MarkdownTextSelection,
    edit: &MarkdownHistoryTextEdit,
) -> Option<MarkdownTextSelection> {
    Some(MarkdownTextSelection {
        anchor_utf16: map_position(selection.anchor_utf16, edit)?,
        head_utf16: map_position(selection.head_utf16, edit)?,
    })
}

impl PieceBackedMarkdownTextView {
    /// Parent dispatch already excluded table hits; capture the body's original action before the first native call.
    pub(super) fn coordinate_body_mouse_down(
        &self,
        event: &NSEvent,
    ) -> NativeMarkdownMouseDisposition {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let result = self.begin_body_mouse(event);
        match result {
            Ok(disposition) => disposition,
            // Revoked or unprovable input neither renews the old click nor forwards it to super.
            Err(_) => NativeMarkdownMouseDisposition::Handled,
        }
    }

    /// Resolve the original pointer against its source, then commit any cell draft and map that one known edit before tracking.
    fn begin_body_mouse(&self, event: &NSEvent) -> Result<NativeMarkdownMouseDisposition, String> {
        self.ivars().folds.invalidate_native_install();
        let capture = self
            .ivars()
            .folds
            .capture_selection()
            .map_err(|_| "正文选择owner已改变")?;
        let input_epoch = self
            .ivars()
            .table_input_intent_epoch
            .get()
            .checked_add(1)
            .ok_or("鼠标输入代次溢出")?;
        self.cancel_table_continuity_for_new_input();
        let initial = || {
            capture.current()
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == input_epoch
        };
        let storage = step(&initial, || self.ivars().text_storage.load())?.ok_or("笔记已经关闭")?;
        let document = storage.document().ok_or("笔记已经关闭")?;
        let (generation, length, before_selection) = {
            let document = document.borrow();
            (
                document.generation(),
                document.utf16_len(),
                document.selection(),
            )
        };
        let source = MouseSource {
            document: Rc::downgrade(&document),
            generation,
            length,
            width: storage.ivars().table_projection_width_key.get(),
            mode: storage.presentation_mode(),
        };
        // Subsequent native work keeps only the original Weak, not a document root for the entire tracking loop.
        drop(document);
        let known = self.ivars().folds.known_selection() == Some(before_selection);
        let captured = capture
            .finish(before_selection, known)
            .map_err(|_| "原选择已改变")?;
        let mut action = MouseAction {
            captured,
            source,
            input_epoch,
        };
        let current = || action.current(self, &storage);
        let flags = step(&current, || event.modifierFlags())?;
        let count = step(&current, || event.clickCount())?;
        let extending = flags.contains(NSEventModifierFlags::Shift);
        let granularity = click_granularity(count);
        let native_before = step(&current, || self.native_ordered_selection_range())?;
        let body_input = ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super());
        let marked = step(&current, || body_input.hasMarkedText())?;
        let active_cell = self.ivars().active_table_cell.borrow().is_some();
        // Neither IME state nor native multi-selection becomes an invented single head.
        // Without a cell draft, preserve their original system handling before any selection edit.
        if !active_cell
            && (marked
                || flags.intersects(NSEventModifierFlags::Option | NSEventModifierFlags::Command)
                || native_before.is_err())
        {
            return Ok(action.forward());
        }
        let native_before = native_before.map_err(|_| "当前系统选择不是单一范围")?;
        action.captured = action
            .captured
            .with_observed_native_before(native_before)
            .map_err(|_| "原Native选择已失效")?;
        let original_head_known = known
            || (before_selection.anchor_utf16 == before_selection.head_utf16
                && native_before == ordered(before_selection));
        if extending && !original_head_known {
            // Shift extension cannot preserve an anchor whose original nonempty direction was never proved.
            return Err("当前非空选择的方向尚未确定".into());
        }
        let current = || action.current(self, &storage);
        let window_point = step(&current, || event.locationInWindow())?;
        let point = step(&current, || self.convertPoint_fromView(window_point, None))?;
        let mut target = (|| {
            let graph = NativeMarkdownSelectionGraph::load(self, &current)?;
            if !extending && granularity != NSTextSelectionGranularity::Character {
                graph.selection_granularity_at_point(
                    self,
                    point,
                    granularity,
                    action.source.length,
                    &current,
                )
            } else {
                graph.selection_at_point(
                    self,
                    NativeMarkdownSelectionPointQuery {
                        point,
                        anchor: extending.then_some((before_selection, granularity)),
                        extending,
                        selecting: false,
                        length: action.source.length,
                    },
                    &current,
                )
            }
        })()?;
        // Graph and temporary selections have ended before original-source publication continues.
        if !current() {
            return Err("原鼠标导航已撤权".into());
        }
        if !action.source.contains_selection(target.selection) {
            return Err("原点击不在笔记的合法源边界".into());
        }
        // Keep the pre-click anchor throughout a Shift gesture; the first expanded head
        // cannot flow backward into the next frame as a replacement origin.
        let mut anchor = if extending {
            before_selection
        } else {
            target.selection
        };
        let native_end = native_before
            .location
            .checked_add(native_before.length)
            .ok_or("原选择范围溢出")?;
        if !active_cell
            && !extending
            && count == 1
            && native_before.length != 0
            && target.selection.head_utf16 >= native_before.location
            && target.selection.head_utf16 <= native_end
        {
            // Hand off before custom tracking or collapse: native drag-and-drop must still
            // observe the original actual selected range.
            return Ok(action.forward());
        }
        if !active_cell
            && !extending
            && count == 1
            && target.selection.head_utf16 < action.source.length
        {
            let has_link = (|| {
                // SAFETY: AppKit's static key is loaded on the main thread. The original
                // ticket and scalar check prove this index remains in the original storage.
                // Inspect one actual attribute, without copying a URL, guessing from LINK
                // styling or independently opening an external destination.
                let link = step(&current, || unsafe {
                    NSAttributedString::attribute_atIndex_effectiveRange(
                        &storage,
                        NSLinkAttributeName,
                        target.selection.head_utf16,
                        std::ptr::null_mut(),
                    )
                })?;
                Ok::<_, String>(link.is_some())
            })()?;
            // End the link-object claim before rechecking authority; forwarding carries no native link object.
            if !current() {
                return Err("链接属性读取结束时原点击已撤权".into());
            }
            if has_link {
                return Ok(action.forward());
            }
        }
        if active_cell {
            let source_before = action.source.clone();
            let owned_selection: RefCell<Option<Rc<NativeMarkdownCommittedSelection>>> =
                RefCell::new(None);
            // g0 guards the original capture. The internal commit's g0+1 transition must
            // also satisfy its same pre-notification Rc authority. An external edit lacking
            // that Rc still fails the commit's original g0 guard; this branch alone cannot authorize continuation.
            let origin = || {
                !self.ivars().textkit_graph_teardown.get()
                    && self.ivars().table_input_intent_epoch.get() == input_epoch
                    && self.ivars().folds.owner_is_current()
                    && source_before.same_storage_context(&storage)
                    && (action.captured.current()
                        || owned_selection
                            .borrow()
                            .as_ref()
                            .is_some_and(|receipt| receipt.current())
                        || source_before.document.upgrade().is_some_and(|document| {
                            source_before.generation.checked_add(1)
                                == Some(document.borrow().generation())
                        }))
            };
            let consequence = self.commit_table_cell_for_click(&origin, &|receipt| {
                // Accept only the Rc directly delivered by this pure publication; origin
                // jointly checks original root/generation/length/owner/input/width authority.
                *owned_selection.borrow_mut() = Some(Rc::clone(receipt));
            })?;
            if consequence.changed {
                // Map the pointer and gesture anchor through exactly the returned committed edit,
                // preserving endpoint identity rather than re-hitting pixels after layout changes.
                let edit = consequence
                    .edit
                    .as_ref()
                    .ok_or("单格提交缺少唯一编辑后果")?;
                let receipt = consequence.committed.ok_or("单格提交缺少原收据")?;
                let generation = generation.checked_add(1).ok_or("提交代次溢出")?;
                let length = length
                    .checked_add_signed(edit.change_in_utf16_length)
                    .ok_or("提交长度溢出")?;
                target.selection = if extending {
                    map_endpoints(target.selection, edit)
                } else {
                    map_selection(target.selection, edit)
                }
                .ok_or("原点击源被提交覆盖")?;
                anchor = if extending {
                    map_endpoints(anchor, edit)
                } else {
                    map_selection(anchor, edit)
                }
                .ok_or("原手势锚点被提交覆盖")?;
                let captured = receipt
                    .captured_selection()
                    .map_err(|_| "单格提交的原收据已撤权")?;
                action.source.generation = generation;
                action.source.length = length;
                if !receipt.current() || !action.source.storage_is_current(&storage) {
                    return Err("单格提交后原点击已撤权".into());
                }
                let current = || {
                    receipt.current()
                        && action.source.storage_is_current(&storage)
                        && self.ivars().table_input_intent_epoch.get() == input_epoch
                };
                let native = step(&current, || self.native_ordered_selection_range())?
                    .map_err(|_| "提交后原Native选择无法认证")?;
                action.captured = captured
                    .with_observed_native_before(native)
                    .map_err(|_| "提交后原选择权限已改变")?;
            }
        }
        // Initial click uses the unique source consequence of its actual pre-commit point,
        // without hit-testing shifted pixels to substitute a different destination.
        let current = || action.current(self, &storage);
        (|| {
            let window = step(&current, || self.window())?.ok_or("正文窗口已关闭")?;
            if !step(&current, || window.makeFirstResponder(Some(self)))? {
                return Err("正文未取得键盘输入焦点".to_owned());
            }
            Ok(())
        })()?;
        // End the window claim first. Focus is not a selection setter; refusal cannot
        // permit a later write to the original pointer target.
        if !current() {
            return Err("正文焦点切换后原点击已撤权".into());
        }
        let source = action.source.clone();
        let receipt = self.install_mouse_target(action, target, &storage, &|| true)?;
        let gesture = MouseGesture {
            source,
            anchor,
            granularity,
            extending,
            dragged: false,
            last: receipt,
            input_epoch,
        };
        if !gesture.current(self) || !gesture.source.storage_is_current(&storage) {
            return Err("鼠标选择安装后已撤权".into());
        }
        // Reuse this stack's temporary storage claim for synchronous tracking; Gesture owns no
        // native object. Avoid a release/reacquire gap in width/mode checks, and append no old action afterward.
        self.track_body_mouse(event, gesture, &storage)?;
        Ok(NativeMarkdownMouseDisposition::Handled)
    }

    /// Publish one captured target to Core, then install only that exact committed receipt under its original action authority.
    fn install_mouse_target(
        &self,
        action: MouseAction,
        target: NativeMarkdownSelectionTarget,
        storage: &PieceBackedMarkdownTextStorage,
        allowed: &impl Fn() -> bool,
    ) -> Result<Rc<NativeMarkdownCommittedSelection>, String> {
        if !allowed() || !action.current(self, storage) {
            return Err("鼠标选择源已改变".into());
        }
        let captured = action
            .captured
            .with_selection(target.selection, true)
            .map_err(|_| "鼠标目标不是合法源边界")?
            .with_head_side(Some(target.head_upstream))
            .with_vertical_goal(None);
        let receipt = storage
            .update_active_selection(captured)
            .ok_or("鼠标选择未被原笔记接受")?;
        let current = || {
            !self.ivars().textkit_graph_teardown.get()
                && allowed()
                && self.ivars().table_input_intent_epoch.get() == action.input_epoch
                && action.source.storage_is_current(storage)
        };
        if !self.apply_committed_selection_if(&receipt, current) || !receipt.current() {
            return Err("鼠标选择安装期间已撤权".into());
        }
        Ok(receipt)
    }

    /// Own the native tracking stack, its one latest retained event and first failure until the synchronous handler ends.
    fn track_body_mouse(
        &self,
        down: &NSEvent,
        gesture: MouseGesture,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> Result<(), String> {
        let source = gesture.source.clone();
        let current = || gesture.current(self) && source.storage_is_current(storage);
        let window = step(&current, || self.window())?.ok_or("鼠标对应窗口已关闭")?;
        let first = retain_event(down, &current)?;
        // Only this stack slot retains the real event; neither the pure Gesture nor view caches native events.
        let last_event = RefCell::new(Rc::new(first));
        let state = RefCell::new(Some(gesture));
        let failure = RefCell::new(None);
        let cancelled = Cell::new(false);
        {
            let handler =
                block2::StackBlock::new(|raw: *mut NSEvent, mut stop: std::ptr::NonNull<Bool>| {
                    let result = (|| -> Result<bool, String> {
                        let Some(mut gesture) = state.borrow_mut().take() else {
                            return Err("鼠标跟踪发生嵌套重入".into());
                        };
                        // Taking the gesture ended RefMut; subsequent getters/retains/autoscroll borrow no state slot.
                        let result = (|| -> Result<bool, String> {
                            let current = || {
                                !cancelled.get()
                                    && gesture.current(self)
                                    && source.storage_is_current(storage)
                            };
                            if !current() {
                                return Ok(true);
                            }
                            if !step(&current, || window.isKeyWindow())? {
                                return Ok(true);
                            }
                            let actual =
                                step(&current, || self.window())?.ok_or("鼠标窗口已断开")?;
                            let same_window = std::ptr::eq(&*actual, &*window);
                            drop(actual);
                            if !same_window || !current() {
                                return Ok(true);
                            }
                            // SAFETY: NSWindow lends this event only within the synchronous handler;
                            // null has the documented timeout meaning and is never dereferenced.
                            let event = unsafe { raw.as_ref() }
                                .map(|event| retain_event(event, &current))
                                .transpose()?;
                            let mut finished = false;
                            if let Some(event) = event.as_ref() {
                                let kind = step(&current, || event.r#type())?;
                                if kind == NSEventType::MouseCancelled {
                                    return Ok(true);
                                }
                                if kind == NSEventType::KeyDown {
                                    if step(&current, || event.keyCode())? != 53 {
                                        step(&current, || window.postEvent_atStart(event, true))?;
                                    }
                                    // Escape cancels this gesture only; other keys return to the original system queue once.
                                    return Ok(true);
                                }
                                let event = Rc::new(retain_event(event, &current)?);
                                let previous = last_event.replace(event);
                                // The final native release runs outside a Ref borrow; recheck the original ticket afterward.
                                drop(previous);
                                if !current() {
                                    return Ok(true);
                                }
                                finished = kind == NSEventType::LeftMouseUp;
                                if kind != NSEventType::LeftMouseDragged && !finished {
                                    return Ok(false);
                                }
                                if !finished {
                                    gesture.dragged = true;
                                }
                            }
                            if !gesture.dragged {
                                return Ok(finished);
                            }
                            // Clone the Rust Rc, then end its slot borrow before any native work.
                            let event = last_event.borrow().clone();
                            self.drag_body_mouse(&event, &mut gesture, raw.is_null(), &|| {
                                !cancelled.get() && source.storage_is_current(storage)
                            })?;
                            drop(event);
                            Ok(finished
                                || !gesture.current(self)
                                || !source.storage_is_current(storage)
                                || cancelled.get())
                        })();
                        // Events and local claims have ended; restore state only outside native operations.
                        let result = result.map(|done| {
                            // done was calculated before event release. Reentrant destruction
                            // that revoked authority must end tracking in this same callback.
                            done || cancelled.get()
                                || !gesture.current(self)
                                || !source.storage_is_current(storage)
                        });
                        *state.borrow_mut() = Some(gesture);
                        result
                    })();
                    let done = match result {
                        Ok(done) => done,
                        Err(error) => {
                            // Preserve the first failure, revoke continuation and stop this actual tracking handler.
                            cancelled.set(true);
                            if failure.borrow().is_none() {
                                *failure.borrow_mut() = Some(error);
                            }
                            true
                        }
                    };
                    // SAFETY: NSWindow lends writable BOOL storage for this synchronous handler;
                    // the stop pointer never escapes this callback.
                    unsafe {
                        *stop.as_mut() = Bool::from(done);
                    }
                });
            // SAFETY: AppKit is loaded on the main thread; borrow its immutable public run-loop mode for synchronous tracking.
            let mode = unsafe { NSEventTrackingRunLoopMode };
            // A positive-timeout null event checks edge scrolling only, proving neither
            // success nor cancellation; no resident timer or event monitor is installed.
            window.trackEventsMatchingMask_timeout_mode_handler(
                NSEventMask::LeftMouseDragged
                    | NSEventMask::LeftMouseUp
                    | NSEventMask::MouseCancelled
                    | NSEventMask::KeyDown,
                0.05,
                mode,
                &handler,
            );
        }
        // End the synchronous handler and its stack borrows before releasing window and the last event.
        drop(window);
        // Release the last actual native event outside all state borrows.
        drop(last_event);
        let error = failure.into_inner();
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(())
        }
    }

    /// Advance one actual drag or edge-scroll tick from the gesture's last accepted selection, without replaying its original click.
    fn drag_body_mouse(
        &self,
        event: &NSEvent,
        gesture: &mut MouseGesture,
        timeout: bool,
        allowed: &impl Fn() -> bool,
    ) -> Result<(), String> {
        if !allowed() || !gesture.current(self) {
            return Err("原鼠标手势已撤销".into());
        }
        if timeout {
            let current = || allowed() && gesture.current(self);
            let outside = (|| {
                let storage =
                    step(&current, || self.ivars().text_storage.load())?.ok_or("鼠标笔记已关闭")?;
                if !gesture.source.storage_is_current(&storage) {
                    return Err("鼠标布局已改变".to_owned());
                }
                let window_point = step(&current, || event.locationInWindow())?;
                let point = step(&current, || self.convertPoint_fromView(window_point, None))?;
                let visible = step(&current, || self.visibleRect())?;
                Ok(!NSPointInRect(point, visible))
            })()?;
            // Temporary storage ended; a tick without edge scrolling creates no new serial or attribute publication.
            if !current() {
                return Err("鼠标边缘检查结束时已撤权".into());
            }
            if !outside {
                return Ok(());
            }
        }
        // Only the same gesture's last accepted selection may authorize a new ticket
        // for this actual drag/tick; arbitrary current state cannot renew the old gesture.
        self.ivars().folds.invalidate_native_install();
        let capture = self
            .ivars()
            .folds
            .capture_selection()
            .map_err(|_| "鼠标手势owner已改变")?;
        let epoch = self
            .ivars()
            .table_input_intent_epoch
            .get()
            .checked_add(1)
            .ok_or("鼠标输入代次溢出")?;
        self.cancel_table_continuity_for_new_input();
        let initial = || {
            allowed()
                && capture.current()
                && self.ivars().table_input_intent_epoch.get() == epoch
                && !self.ivars().textkit_graph_teardown.get()
        };
        let storage =
            step(&initial, || self.ivars().text_storage.load())?.ok_or("鼠标对应笔记已关闭")?;
        if !gesture.source.storage_is_current(&storage) {
            return Err("鼠标手势源布局已改变".into());
        }
        let captured = capture
            .finish(gesture.last.selection, true)
            .map_err(|_| "鼠标手势的上次选择已被替换")?;
        let mut action = MouseAction {
            captured,
            source: gesture.source.clone(),
            input_epoch: epoch,
        };
        let current = || allowed() && action.current(self, &storage);
        let before = step(&current, || self.native_ordered_selection_range())?
            .map_err(|_| "当前鼠标选择不是单一范围")?;
        action.captured = action
            .captured
            .with_observed_native_before(before)
            .map_err(|_| "鼠标原Native范围已失效")?;
        let current = || allowed() && action.current(self, &storage);
        let window_point = step(&current, || event.locationInWindow())?;
        step(&current, || self.autoscroll(event))?;
        // Autoscroll moves the view origin. Reconvert the same actual window point,
        // avoiding a pre-scroll local point that would leave the active endpoint stuck.
        let point = step(&current, || self.convertPoint_fromView(window_point, None))?;
        let target = (|| {
            let graph = NativeMarkdownSelectionGraph::load(self, &current)?;
            graph.selection_at_point(
                self,
                NativeMarkdownSelectionPointQuery {
                    point,
                    anchor: Some((gesture.anchor, gesture.granularity)),
                    extending: gesture.extending,
                    selecting: true,
                    length: gesture.source.length,
                },
                &current,
            )
        })()?;
        if !current() {
            return Err("鼠标导航局部析构后撤权".into());
        }
        gesture.last = self.install_mouse_target(action, target, &storage, allowed)?;
        gesture.input_epoch = epoch;
        drop(storage);
        if !allowed() || !gesture.current(self) {
            return Err("鼠标选择局部析构后已撤权".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Supply one pure logical edit to the public-source coordinate mapping algorithm, without native state injection.
    fn edit(range: std::ops::Range<usize>, delta: isize) -> MarkdownHistoryTextEdit {
        MarkdownHistoryTextEdit {
            replaced_utf16_range: range,
            change_in_utf16_length: delta,
            selection_after: MarkdownTextSelection::caret(0),
        }
    }

    #[test]
    /// Map only source left intact by one unique edit, preserving reversed anchor/head direction.
    fn pointer_source_maps_unique_utf16_consequence() {
        let change = edit(10..15, -3);
        assert_eq!(
            map_range(NSRange::new(5, 5), &change),
            Some(NSRange::new(5, 5))
        );
        assert_eq!(
            map_range(NSRange::new(15, 5), &change),
            Some(NSRange::new(12, 5))
        );
        assert_eq!(map_range(NSRange::new(9, 2), &change), None);
        assert_eq!(map_position(10, &change), None);
        assert_eq!(
            map_selection(
                MarkdownTextSelection {
                    anchor_utf16: 20,
                    head_utf16: 15
                },
                &change
            ),
            Some(MarkdownTextSelection {
                anchor_utf16: 17,
                head_utf16: 12
            })
        );
        assert_eq!(map_range(NSRange::new(5, 10), &edit(10..10, 2)), None);
    }

    #[test]
    /// Chinese/emoji byte lengths are not UTF-16 displacements; code-unit mapping must preserve the same source character.
    fn pointer_consequence_keeps_unicode_source_boundary() {
        let before = "前🙂|a|后";
        let after = "前🙂|猫🐈|后";
        let old_byte = before.find('后').unwrap();
        let new_byte = after.find('后').unwrap();
        let old_units = before[..old_byte].encode_utf16().count();
        let new_units = after[..new_byte].encode_utf16().count();
        let replaced_start = before[..before.find('a').unwrap()].encode_utf16().count();
        let change = edit(replaced_start..replaced_start + 1, 2);
        assert_eq!(map_position(old_units, &change), Some(new_units));
        assert_ne!(old_byte + 2, new_byte);
        assert_eq!(
            map_endpoints(
                MarkdownTextSelection {
                    anchor_utf16: old_units,
                    head_utf16: 1,
                },
                &change,
            ),
            Some(MarkdownTextSelection {
                anchor_utf16: new_units,
                head_utf16: 1,
            }),
        );
    }
}
