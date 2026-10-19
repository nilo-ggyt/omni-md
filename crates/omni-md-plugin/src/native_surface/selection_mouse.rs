//! 正文鼠标选择沿原指针源与每次真实动作票推进；系统拖放只在未改变选择之前交还。

use super::*;
use objc2::runtime::Bool;
use objc2_app_kit::{
    NSEventMask, NSEventTrackingRunLoopMode, NSLinkAttributeName, NSTextSelectionGranularity,
};

/// 返回给父mouseDown的只是同步handoff；NativeOwned不得在调用super前另领当前权限。
/// 转交载荷只含固定票据、Weak 与标量，留在同步栈上，避免每次原生鼠标转交都分配 Box。
#[allow(
    clippy::large_enum_variant,
    reason = "同步鼠标转交保留固定内联票据，避免为热路径增加一次独立堆分配"
)]
pub(super) enum NativeMarkdownMouseDisposition {
    Handled,
    NativeOwned(NativeMarkdownMouseNativeForward),
}

#[derive(Clone)]
struct MouseSource {
    document: WeakMarkdownDocument,
    generation: u64,
    length: usize,
    width: i64,
    mode: MarkdownPresentationMode,
}

impl MouseSource {
    fn current(&self) -> bool {
        self.document.upgrade().is_some_and(|document| {
            let document = document.borrow();
            document.generation() == self.generation && document.utf16_len() == self.length
        })
    }

    fn storage_is_current(&self, storage: &PieceBackedMarkdownTextStorage) -> bool {
        self.same_storage_context(storage) && self.current()
    }

    fn same_storage_context(&self, storage: &PieceBackedMarkdownTextStorage) -> bool {
        storage.ivars().accepts_edits.get()
            && storage.presentation_mode() == self.mode
            && storage.ivars().table_projection_width_key.get() == self.width
            && std::rc::Weak::ptr_eq(&self.document, &storage.ivars().document.borrow())
    }

    /// 原点导航须先证明UTF16两端属于旧root；提交以后只允许映射这个真实源，不能修猜坐标。
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

/// 票只含纯选择capture/Weak及标量；同步进入super期间才临时持有原storage，封住析构重入间隙。
pub(super) struct NativeMarkdownMouseNativeForward {
    captured: NativeMarkdownCapturedSelection,
    source: MouseSource,
    input_epoch: u64,
}

impl NativeMarkdownMouseNativeForward {
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
        // 原storage claim覆盖super整个同步栈；返回之后不再用旧动作写选择或滚动位置。
        Some(work())
    }
}

struct MouseAction {
    captured: NativeMarkdownCapturedSelection,
    source: MouseSource,
    input_epoch: u64,
}

impl MouseAction {
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

    fn forward(self) -> NativeMarkdownMouseDisposition {
        NativeMarkdownMouseDisposition::NativeOwned(NativeMarkdownMouseNativeForward {
            captured: self.captured,
            source: self.source,
            input_epoch: self.input_epoch,
        })
    }
}

/// 手势只保纯源事实、初始范围/粒度与最后一张已接受收据；没有Native对象或正文强root。
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
    fn current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        self.last.current()
            && self.source.current()
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().table_input_intent_epoch.get() == self.input_epoch
    }
}

fn step<T>(current: &impl Fn() -> bool, work: impl FnOnce() -> T) -> Result<T, String> {
    native_markdown_table_intent_step(current, work)
        .map_err(|_| "鼠标对应的笔记、窗口或输入已经改变".into())
}

fn retain_event(event: &NSEvent, current: &impl Fn() -> bool) -> Result<Retained<NSEvent>, String> {
    step(current, || {
        // SAFETY: 原事件是本次mouseDown/NSWindow同步handler的有效借用；retain不逸出tracking栈。
        unsafe { Retained::retain(std::ptr::from_ref(event).cast_mut()) }
    })?
    .ok_or_else(|| "鼠标事件已释放".into())
}

fn ordered(selection: MarkdownTextSelection) -> NSRange {
    let start = selection.anchor_utf16.min(selection.head_utf16);
    NSRange::new(
        start,
        selection.anchor_utf16.max(selection.head_utf16) - start,
    )
}

fn click_granularity(count: isize) -> NSTextSelectionGranularity {
    match count {
        2 => NSTextSelectionGranularity::Word,
        3.. => NSTextSelectionGranularity::Paragraph,
        _ => NSTextSelectionGranularity::Character,
    }
}

/// 被replacement覆盖的旧源没有唯一后果；端点只按UTF16位移，之后还须在新root换算byte。
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

/// Shift的anchor与指针head分别具有源身份；唯一编辑位于两端之间不等于任一端被覆盖。
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
    /// 父入口已经排除表格hit；此同步handoff首次Native前取得正文自己的原动作票。
    pub(super) fn coordinate_body_mouse_down(
        &self,
        event: &NSEvent,
    ) -> NativeMarkdownMouseDisposition {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let result = self.begin_body_mouse(event);
        match result {
            Ok(disposition) => disposition,
            Err(_) => NativeMarkdownMouseDisposition::Handled, // 失权或无法认证时不替旧点击补票，也不再进super。
        }
    }

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
        drop(document); // 后续所有Native只保原Weak，不能把原root延长到整个tracking。
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
        // IME与系统多选不伪造成单一head；未有表格草稿时完整保留系统行为。
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
        })()?; // Graph/selection等局部全部析构后才继续原源发布。
        if !current() {
            return Err("原鼠标导航已撤权".into());
        }
        if !action.source.contains_selection(target.selection) {
            return Err("原点击不在笔记的合法源边界".into());
        }
        // Shift整个gesture保留点击前anchor，不能把第一次扩选后的新head倒灌成下一帧起点。
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
            // 进入自有tracking或collapse之前就归还：系统拖放必须仍看到原实际selected range。
            return Ok(action.forward());
        }
        if !active_cell
            && !extending
            && count == 1
            && target.selection.head_utf16 < action.source.length
        {
            let has_link = (|| {
                // SAFETY: 静态AppKit键在主线程已加载；原票与上面的scalar验证保证index仍在原storage内。
                // 只读取这一个实际属性，不复制URL、不从语义LINK样式猜链接，更不自行打开外部地址。
                let link = step(&current, || unsafe {
                    NSAttributedString::attribute_atIndex_effectiveRange(
                        &storage,
                        NSLinkAttributeName,
                        target.selection.head_utf16,
                        std::ptr::null_mut(),
                    )
                })?;
                Ok::<_, String>(link.is_some())
            })()?; // 链接对象claim先结束，之后再核原票；Forward不能携带Native链接对象。
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
            // g0只守原capture。g0+1仅允许commit内部与其必须存在的同pre-notifyRc联合通过；
            // 无该Rc的外部编辑仍被内部g0 authority拒绝，不能由此分支独自授权鼠标继续。
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
                // 只接受这次纯发布直接交付的同 Rc；原 root/g/len/owner/input/width 仍由 origin 联合核验。
                *owned_selection.borrow_mut() = Some(Rc::clone(receipt));
            })?;
            if consequence.changed {
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
        // 初次点击使用提交前真实point的唯一源后果；不在移动后的像素上重新hit并换一个目标。
        let current = || action.current(self, &storage);
        (|| {
            let window = step(&current, || self.window())?.ok_or("正文窗口已关闭")?;
            if !step(&current, || window.makeFirstResponder(Some(self)))? {
                return Err("正文未取得键盘输入焦点".to_owned());
            }
            Ok(())
        })()?; // 窗口临时claim先析构；focus不是选择setter，拒绝后不能继续写入原鼠标目标。
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
        // 同步tracking沿用当前栈的storage临时claim；Gesture仍无Native owner。
        // 避免先release再进入tracking产生无法纯核width/mode的间隙，返回后不追加旧动作。
        self.track_body_mouse(event, gesture, &storage)?;
        Ok(NativeMarkdownMouseDisposition::Handled)
    }

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
        // 只这一个栈内槽持有真实事件；纯Gesture及view均不保存Native事件缓存。
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
                        }; // take的RefMut已结束，之后Native getter/retain/autoscroll都不借该槽。
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
                            // SAFETY: NSWindow只在同步handler内提供该借用；空指针是公开timeout语义。
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
                                    return Ok(true); // Escape只取消本gesture；其余按键归还系统原队列一次。
                                }
                                let event = Rc::new(retain_event(event, &current)?);
                                let previous = last_event.replace(event);
                                drop(previous); // Native事件最终release不在Ref借用中，随后核原票。
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
                            let event = last_event.borrow().clone(); // Rc clone结束借用后才进入Native。
                            self.drag_body_mouse(&event, &mut gesture, raw.is_null(), &|| {
                                !cancelled.get() && source.storage_is_current(storage)
                            })?;
                            drop(event);
                            Ok(finished
                                || !gesture.current(self)
                                || !source.storage_is_current(storage)
                                || cancelled.get())
                        })(); // 事件及其局部claims已析构；状态仅在Native区间之外放回。
                        let result = result.map(|done| {
                            // done的计算早于临时事件release；其同步重入若撤权，当前回调就结束tracking。
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
                            cancelled.set(true);
                            if failure.borrow().is_none() {
                                *failure.borrow_mut() = Some(error);
                            }
                            true
                        }
                    };
                    // SAFETY: NSWindow在同步handler期间提供可写BOOL；不保存stop指针到回调之外。
                    unsafe {
                        *stop.as_mut() = Bool::from(done);
                    }
                });
            // SAFETY: AppKit已在主线程加载；只读取公开不可变runloop mode常量供本同步tracking使用。
            let mode = unsafe { NSEventTrackingRunLoopMode };
            // 正timeout的nil仅触发边缘检查，不当作成功/取消；无常驻timer或事件监控器。
            window.trackEventsMatchingMask_timeout_mode_handler(
                NSEventMask::LeftMouseDragged
                    | NSEventMask::LeftMouseUp
                    | NSEventMask::MouseCancelled
                    | NSEventMask::KeyDown,
                0.05,
                mode,
                &handler,
            );
        } // 同步handler及其对tracking局部的借用先结束，再释放window和最后一个事件。
        drop(window);
        drop(last_event); // 最后一个真实事件Native claim在状态借用外释放。
        let error = failure.into_inner();
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(())
        }
    }

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
            })()?; // 临时storage已drop；没有边缘滚动就不创建新serial或属性发布。
            if !current() {
                return Err("鼠标边缘检查结束时已撤权".into());
            }
            if !outside {
                return Ok(());
            }
        }
        // 只有上个已接受选择仍属同gesture，才从这一真实drag/tick建立新的动作票。
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
        // autoscroll改变view原点；重新转换同一真实window point，不能复用滚前point卡住head。
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
    fn edit(range: std::ops::Range<usize>, delta: isize) -> MarkdownHistoryTextEdit {
        MarkdownHistoryTextEdit {
            replaced_utf16_range: range,
            change_in_utf16_length: delta,
            selection_after: MarkdownTextSelection::caret(0),
        }
    }

    #[test]
    /// 只按唯一编辑映射未覆盖源，反向anchor/head不能在映射时丢方向。
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
    /// 中文和emoji的byte长度不是UTF16位移；原末端只有按code unit映射才能仍指向同一源字。
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
