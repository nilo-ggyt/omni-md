//! 点击从原矩阵取得一次权限；结束前格草稿后只消费该次提交后果，不重新签发点击。

use super::*;
use objc2_app_kit::{NSSelectionAffinity, NSSelectionGranularity};

/// 原真实点击只有一份事件载荷；Latest 被替换而非累积，Up/取消后立即归还 Native 事件。
pub(in crate::native_surface) struct NativeMarkdownTableCellPointer {
    events: RefCell<Option<PointerEvents>>,
    input_epoch: u64,
    identity: WeakMarkdownDocument,
    key: NativeMarkdownTableLayoutRequestKey,
    window_number: isize,
    down_timestamp: f64,
    pub(super) ended: Cell<bool>,
    pub(super) extending: bool,
    click_count: isize,
    pub(super) anchor: MarkdownTextSelection,
    down_hit: usize,
    head: Cell<usize>,
    initial_field_range: Cell<Option<NSRange>>,
    captured: RefCell<Option<NativeMarkdownCapturedSelection>>,
    pub(super) body_selection: Cell<bool>,
    // 同格 Shift 的真实 raw 字段使用局部坐标；不把未提交草稿偏移误作旧 canonical 坐标。
    raw_field: Option<NativeMarkdownTableRawFieldPointer>,
}

struct NativeMarkdownTableRawFieldPointer {
    field: Weak<NSTextField>,
    editor: Weak<NSTextView>,
    coordinate: NativeMarkdownTableCellCoordinate,
    source: std::ops::Range<usize>,
    anchor: usize,
    down_hit: usize,
    down_range: NSRange,
    head: Cell<usize>,
}

struct PointerEvents {
    _down: Rc<Retained<NSEvent>>,
    latest: Rc<Retained<NSEvent>>,
}

/// 仅跨当前同步 mouseDown 栈保存真实字段命中；Native 返回后不再保留 editor 或事件。
pub(in crate::native_surface) struct NativeMarkdownTableFieldMouseSelection {
    editor: Weak<NSTextView>,
    epoch: u64,
    coordinate: NativeMarkdownTableCellCoordinate,
    source: std::ops::Range<usize>,
    document: WeakMarkdownDocument,
    before: NSRange,
    down: NSRange,
    extending: bool,
}

impl NativeMarkdownTableCellPointer {
    #[cfg(all(
        feature = "native-markdown-runtime-probe",
        feature = "native-table-row-append"
    ))]
    /// 只借原 Down 的对象身份，供预武装合成票绑定同一实际手势；不返回事件或窗口强引用。
    pub(in crate::native_surface) fn probe_down_identity(&self) -> Option<usize> {
        self.events
            .borrow()
            .as_ref()
            .map(|events| std::ptr::from_ref(&***events._down).addr())
    }
    /// 权限随原 input/root/width 存活；只含 Weak 文档，不持有第二正文或窗口 owner。
    fn current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        !view.ivars().textkit_graph_teardown.get()
            && view.ivars().folds.owner_is_current()
            && view.ivars().table_input_intent_epoch.get() == self.input_epoch
            && self.identity.upgrade().is_some_and(|document| {
                let document = document.borrow();
                document.generation() == self.key.document_generation
                    && document.utf16_len() == self.key.document_length
            })
    }

    pub(in crate::native_surface) fn cancel(&self) {
        self.ended.set(true);
        let previous = self.events.borrow_mut().take();
        drop(previous); // NSEvent 最后 release 不跨 RefCell 借用，关闭/后继输入不会继承旧事件。
        let previous = self.captured.borrow_mut().take();
        drop(previous); // 原选择票也在终点归还，不让活动字段保留无用的旧 fold 资源。
    }

    fn granularity(&self) -> NSSelectionGranularity {
        match self.click_count {
            count if count >= 3 => NSSelectionGranularity::SelectByParagraph,
            2 => NSSelectionGranularity::SelectByWord,
            _ => NSSelectionGranularity::SelectByCharacter,
        }
    }

    /// 借用 Latest 的同 Rc 事实；Native getter 重入交付更新事件时旧调用立即失效。
    pub(super) fn latest_event(&self) -> Option<Rc<Retained<NSEvent>>> {
        self.events
            .borrow()
            .as_ref()
            .map(|events| Rc::clone(&events.latest))
    }

    pub(super) fn owns_event(&self, event: &Rc<Retained<NSEvent>>) -> bool {
        self.events
            .borrow()
            .as_ref()
            .is_some_and(|events| Rc::ptr_eq(&events.latest, event))
    }
}

/// 位置在真实编辑之后才位移；被替换范围内部没有唯一存活锚点，不能猜测新表头。
fn map_click_position(position: usize, edit: &MarkdownHistoryTextEdit) -> Option<usize> {
    let range = &edit.replaced_utf16_range;
    if range.start > range.end {
        return None;
    }
    if position < range.start {
        Some(position)
    } else if position >= range.end {
        // 零长插入的同位点取原字符之后；真实源字符随插入整体向后移动。
        position.checked_add_signed(edit.change_in_utf16_length)
    } else {
        None // 非空replacement覆盖的位置没有存活证明，拒绝旧点击而不是重取当前H。
    }
}

/// 点击格的完整源范围必须存活；半开区间相接可映射，任何内容重叠都不继承权限。
fn map_click_source_range(
    source: &std::ops::Range<usize>,
    edit: &MarkdownHistoryTextEdit,
) -> Option<std::ops::Range<usize>> {
    let replaced = &edit.replaced_utf16_range;
    if source.start > source.end || replaced.start > replaced.end {
        return None; // 畸形范围不能因端点恰好能映射而变成合法源权限。
    }
    if source.is_empty() {
        // ImplicitEmpty与显式空格均保留原锚点；后继Ready还须证明确为同一真实cell范围。
        let at = map_click_position(source.start, edit)?;
        Some(at..at)
    } else if source.end <= replaced.start {
        Some(source.clone()) // 前格的尾端相接不属于后格replacement，不能错误向后移动。
    } else if source.start >= replaced.end {
        Some(
            source
                .start
                .checked_add_signed(edit.change_in_utf16_length)?
                ..source.end.checked_add_signed(edit.change_in_utf16_length)?,
        )
    } else {
        None // 包括插入发生在格内的情况；不能再用当前row/column重新认领被改过的源。
    }
}

impl PieceBackedMarkdownTextView {
    /// 同格 Shift 沿原字段的真实 glyph 开始同一个 Pointer，后续 drag/up 不依赖系统私有 anchor。
    pub(in crate::native_surface) fn begin_table_raw_field_shift(&self, event: &NSEvent) -> bool {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let epoch = self.ivars().table_input_intent_epoch.get();
        let current = || {
            !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == epoch
        };
        let pointer = (|| {
            let flags =
                native_markdown_table_intent_step(&current, || event.modifierFlags()).ok()?;
            if !flags.contains(NSEventModifierFlags::Shift)
                || flags.intersects(
                    NSEventModifierFlags::Command
                        | NSEventModifierFlags::Control
                        | NSEventModifierFlags::Option,
                )
            {
                return None; // 只拥有既有 Shift 扩选，其他修饰组合保持原 Native 行为。
            }
            let (coordinate, source, selection, identity) = self
                .ivars()
                .active_table_cell
                .borrow()
                .as_ref()
                .and_then(|active| {
                    Some((
                        active.coordinate,
                        active.source_utf16_range.clone(),
                        active.field_selection?,
                        active.authority.as_ref()?.document.clone(),
                    ))
                })?;
            let key = native_markdown_table_intent_step(&current, || {
                self.current_table_layout_request_key()
            })
            .ok()??;
            let field =
                native_markdown_table_intent_step(&current, || self.existing_table_cell_editor())
                    .ok()??;
            let actual = native_markdown_table_intent_step(&current, || field.currentEditor())
                .ok()??
                .downcast::<NSTextView>()
                .ok()?;
            if !native_markdown_table_intent_step(&current, || {
                self.table_field_command_is_current(&field, &actual)
            })
            .ok()?
            {
                return None;
            }
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual);
            if native_markdown_table_intent_step(&current, || input.hasMarkedText()).ok()? {
                return None; // 原 IME 得到系统处理机会，不为定位取消或提交 marked text。
            }
            let range =
                native_markdown_table_intent_step(&current, || actual.selectedRange()).ok()?;
            native_markdown_table_field_selection(None, range, Some(selection), false)?;
            let point =
                native_markdown_table_intent_step(&current, || event.locationInWindow()).ok()?;
            let point = native_markdown_table_intent_step(&current, || {
                actual.convertPoint_fromView(point, None)
            })
            .ok()?;
            if !native_markdown_table_intent_step(&current, || {
                NSPointInRect(point, actual.visibleRect())
            })
            .ok()?
            {
                return None; // 点击正文、Find 或另一格时不得截取当前字段的扩选。
            }
            let hit = native_markdown_table_intent_step(&current, || {
                actual.characterIndexForInsertionAtPoint(point)
            })
            .ok()?;
            let count = native_markdown_table_intent_step(&current, || event.clickCount()).ok()?;
            let granularity = match count {
                count if count >= 3 => NSSelectionGranularity::SelectByParagraph,
                2 => NSSelectionGranularity::SelectByWord,
                _ => NSSelectionGranularity::SelectByCharacter,
            };
            let down_range = native_markdown_table_intent_step(&current, || {
                actual.selectionRangeForProposedRange_granularity(NSRange::new(hit, 0), granularity)
            })
            .ok()?;
            let number =
                native_markdown_table_intent_step(&current, || event.windowNumber()).ok()?;
            let timestamp =
                native_markdown_table_intent_step(&current, || event.timestamp()).ok()?;
            let field = native_markdown_table_intent_step(&current, || Weak::from(&*field)).ok()?;
            let editor =
                native_markdown_table_intent_step(&current, || Weak::from(&*actual)).ok()?;
            let down = native_markdown_table_intent_step(&current, || {
                // SAFETY: 原 Down 仍由 panel 同步派发持有；只进入原 gesture 的唯一事件载荷。
                unsafe { Retained::retain(std::ptr::from_ref(event).cast_mut()) }
            })
            .ok()??;
            let down = Rc::new(down);
            Some(Rc::new(NativeMarkdownTableCellPointer {
                events: RefCell::new(Some(PointerEvents {
                    _down: Rc::clone(&down),
                    latest: down,
                })),
                input_epoch: epoch,
                identity,
                key,
                window_number: number,
                down_timestamp: timestamp,
                ended: Cell::new(false),
                extending: true,
                click_count: count,
                anchor: MarkdownTextSelection::caret(source.start),
                down_hit: source.start,
                head: Cell::new(source.start),
                initial_field_range: Cell::new(None),
                captured: RefCell::new(None),
                body_selection: Cell::new(false),
                raw_field: Some(NativeMarkdownTableRawFieldPointer {
                    field,
                    editor,
                    coordinate,
                    source,
                    anchor: selection.anchor_utf16,
                    down_hit: hit,
                    down_range,
                    head: Cell::new(hit),
                }),
            }))
        })();
        let Some(pointer) = pointer else {
            return false;
        };
        if !current() || !pointer.current(self) {
            return false; // Native 借用/Weak 构造析构后已失去原意图，不安装新事件槽。
        }
        let raw = pointer.raw_field.as_ref().unwrap();
        if !self
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .is_some_and(|active| {
                active.coordinate == raw.coordinate
                    && active.source_utf16_range == raw.source
                    && active
                        .field_selection
                        .is_some_and(|selection| selection.anchor_utf16 == raw.anchor)
                    && active.authority.as_ref().is_some_and(|authority| {
                        WeakMarkdownDocument::ptr_eq(&authority.document, &pointer.identity)
                    })
            })
        {
            return false; // 同数值 epoch 不能替代原活动格、源及字段方向的实际关联。
        }
        let previous = self
            .ivars()
            .active_table_cell
            .borrow_mut()
            .as_mut()
            .unwrap()
            .pointer
            .replace(Rc::clone(&pointer));
        if let Some(previous) = previous {
            previous.cancel();
        } // 借用结束后才释放先前事件。
        if self
            .apply_table_raw_field_pointer(&pointer, &current)
            .is_err()
        {
            pointer.cancel();
            self.take_table_pointer_if_same(&pointer);
        }
        true // 原 Down 已由同一实际字段处理；失败也不能把它再派发一次到系统。
    }

    /// 只撤当前原 Pointer 的强边；后继格/新手势不受旧取消和 Up 影响。
    fn take_table_pointer_if_same(&self, pointer: &NativeMarkdownTableCellPointer) {
        let previous = self
            .ivars()
            .active_table_cell
            .borrow_mut()
            .as_mut()
            .and_then(|active| {
                active
                    .pointer
                    .as_ref()
                    .filter(|value| std::ptr::eq(&***value, pointer))?;
                active.pointer.take()
            });
        drop(previous);
    }

    /// raw 字段每一步同时核原活动格、字段及共享 editor 身份；Find 复用同 editor 也不能借权。
    fn raw_table_field_editor(
        &self,
        pointer: &NativeMarkdownTableCellPointer,
        allowed: &impl Fn() -> bool,
    ) -> Result<Retained<NSTextView>, NativeMarkdownStableTableFailure> {
        let raw = pointer
            .raw_field
            .as_ref()
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        if !allowed()
            || !self
                .ivars()
                .active_table_cell
                .borrow()
                .as_ref()
                .is_some_and(|active| {
                    active.coordinate == raw.coordinate
                        && active.source_utf16_range == raw.source
                        && active
                            .pointer
                            .as_ref()
                            .is_some_and(|value| std::ptr::eq(&**value, pointer))
                })
        {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry);
        }
        let field = native_markdown_table_intent_step(allowed, || raw.field.load())?
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let actual = native_markdown_table_intent_step(allowed, || raw.editor.load())?
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        if !native_markdown_table_intent_step(allowed, || {
            self.table_field_command_is_current(&field, &actual)
        })? {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry);
        }
        Ok(actual)
    }

    /// 每个原生阶段都核同一 active/field/editor；共享 editor 仍活不代表它还属于原格。
    fn raw_table_field_is_current(
        &self,
        pointer: &NativeMarkdownTableCellPointer,
        field: &NSTextField,
        actual: &NSTextView,
    ) -> bool {
        let raw = pointer.raw_field.as_ref().unwrap();
        let active = || {
            self.ivars()
                .active_table_cell
                .borrow()
                .as_ref()
                .is_some_and(|active| {
                    active.coordinate == raw.coordinate
                        && active.source_utf16_range == raw.source
                        && active
                            .pointer
                            .as_ref()
                            .is_some_and(|value| std::ptr::eq(&**value, pointer))
                        && active.authority.as_ref().is_some_and(|authority| {
                            WeakMarkdownDocument::ptr_eq(&authority.document, &pointer.identity)
                        })
                })
        };
        // active 的 Ref 已在布尔值计算后结束；currentEditor 的真实 Native getter 不跨该借用。
        active() && self.table_field_command_is_current(field, actual) && active()
    }

    /// 同一字段的真实 raw caret 与 Native 粒度决定 head，未提交草稿不使用旧 canonical 的长度。
    fn apply_table_raw_field_pointer(
        &self,
        pointer: &NativeMarkdownTableCellPointer,
        allowed: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        let event = pointer
            .latest_event()
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let original = || allowed() && pointer.current(self) && pointer.owns_event(&event);
        let actual = self.raw_table_field_editor(pointer, &original)?;
        let field = native_markdown_table_intent_step(&original, || {
            pointer.raw_field.as_ref().unwrap().field.load()
        })?
        .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let current = || original() && self.raw_table_field_is_current(pointer, &field, &actual);
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual);
        if native_markdown_table_intent_step(&current, || input.hasMarkedText())? {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry);
        }
        let raw = pointer.raw_field.as_ref().unwrap();
        let hit = raw.head.get();
        let range = if hit == raw.down_hit {
            raw.down_range // 原 Down 的真实粒度只领取一次；后续回到该点仍是同一词/段落。
        } else {
            native_markdown_table_intent_step(&current, || {
                actual.selectionRangeForProposedRange_granularity(
                    NSRange::new(hit, 0),
                    pointer.granularity(),
                )
            })?
        };
        let head = if hit < raw.anchor {
            range.location
        } else {
            range
                .location
                .checked_add(range.length)
                .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?
        };
        let value = native_markdown_table_intent_step(&current, || actual.string())?;
        let length = native_markdown_table_intent_step(&current, || value.length())?;
        drop(value); // 只取同字段长度，不物化或复制原草稿。
        if raw.anchor > length || head > length || range.location == NSNotFound as usize {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry);
        }
        let start = raw.anchor.min(head);
        let affinity = if head < raw.anchor {
            NSSelectionAffinity::Upstream
        } else {
            NSSelectionAffinity::Downstream
        };
        native_markdown_table_intent_step(&current, || {
            actual.setSelectionGranularity(pointer.granularity())
        })?;
        native_markdown_table_intent_step(&current, || {
            actual.setSelectedRange_affinity_stillSelecting(
                NSRange::new(start, raw.anchor.max(head) - start),
                affinity,
                !pointer.ended.get(),
            )
        })?;
        self.record_table_field_selection(
            &actual,
            Some(MarkdownTextSelection {
                anchor_utf16: raw.anchor,
                head_utf16: head,
            }),
            false,
            &current,
        )
    }

    /// 真实 drag/up 替换原 Latest 后才安装同格 Shift；原字段已复用或输入改变则拒绝并保留草稿。
    fn update_table_raw_field_pointer(
        &self,
        pointer: &NativeMarkdownTableCellPointer,
        event: &NSEvent,
        allowed: &impl Fn() -> bool,
        event_claim: &RefCell<Rc<Retained<NSEvent>>>,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        let current = || allowed() && pointer.owns_event(&event_claim.borrow());
        let kind = native_markdown_table_intent_step(&current, || event.r#type())?;
        if kind == NSEventType::MouseCancelled {
            pointer.cancel();
            self.take_table_pointer_if_same(pointer);
            return Ok(());
        }
        let actual = self.raw_table_field_editor(pointer, &current)?;
        let field = native_markdown_table_intent_step(&current, || {
            pointer.raw_field.as_ref().unwrap().field.load()
        })?
        .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let input_current =
            || current() && self.raw_table_field_is_current(pointer, &field, &actual);
        let point = native_markdown_table_intent_step(&current, || event.locationInWindow())?;
        let point = native_markdown_table_intent_step(&input_current, || {
            actual.convertPoint_fromView(point, None)
        })?;
        let hit = native_markdown_table_intent_step(&input_current, || {
            actual.characterIndexForInsertionAtPoint(point)
        })?;
        drop(field);
        drop(actual); // 临时 editor claim 归还后再替换原事件；不跨状态借用析构 Native 对象。
        let retained = native_markdown_table_intent_step(&current, || {
            // SAFETY: 原 sendEvent 正持有此 drag/up，唯一 Latest 只保留本次真实事件。
            unsafe { Retained::retain(std::ptr::from_ref(event).cast_mut()) }
        })?
        .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let retained = Rc::new(retained);
        let previous_claim = event_claim.replace(Rc::clone(&retained));
        let previous = {
            let mut events = pointer.events.borrow_mut();
            let events = events
                .as_mut()
                .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
            std::mem::replace(&mut events.latest, retained)
        };
        drop(previous);
        drop(previous_claim);
        if !current() {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry);
        }
        pointer.raw_field.as_ref().unwrap().head.set(hit);
        pointer.ended.set(kind == NSEventType::LeftMouseUp);
        self.apply_table_raw_field_pointer(pointer, &current)?;
        if pointer.ended.get() {
            pointer.cancel();
            self.take_table_pointer_if_same(pointer);
        }
        Ok(())
    }

    /// 真实字段选择只有同一活动格及 Native editor 可发布；未知方向清除旧事实但不移动选择。
    fn record_table_field_selection(
        &self,
        actual: &NSTextView,
        explicit: Option<MarkdownTextSelection>,
        extending: bool,
        allowed: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        let epoch = self.ivars().table_input_intent_epoch.get();
        let (coordinate, source, previous, authority) = self
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .and_then(|active| {
                Some((
                    active.coordinate,
                    active.source_utf16_range.clone(),
                    active.field_selection,
                    Rc::clone(active.authority.as_ref()?),
                ))
            })
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let current = || {
            allowed()
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == epoch
                && self
                    .ivars()
                    .active_table_cell
                    .borrow()
                    .as_ref()
                    .is_some_and(|active| {
                        active.coordinate == coordinate
                            && active.source_utf16_range == source
                            && active
                                .authority
                                .as_ref()
                                .is_some_and(|value| Rc::ptr_eq(value, &authority))
                    })
                && authority.document.upgrade().is_some_and(|document| {
                    authority
                        .source_range(&document.borrow(), coordinate)
                        .ok()
                        .as_ref()
                        == Some(&source)
                })
        };
        let field =
            native_markdown_table_intent_step(&current, || self.existing_table_cell_editor())?
                .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        if !native_markdown_table_intent_step(&current, || {
            self.table_field_command_is_current(&field, actual)
        })? {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry);
        }
        let ranges = native_markdown_table_intent_step(&current, || actual.selectedRanges())?;
        let count = native_markdown_table_intent_step(&current, || ranges.count())?;
        let range = native_markdown_table_intent_step(&current, || actual.selectedRange())?;
        drop(ranges);
        drop(field); // Native claims 全部先析构；只在原格仍有权时写入两个标量。
        if !current() {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry);
        }
        let selection = (count == 1)
            .then(|| native_markdown_table_field_selection(previous, range, explicit, extending))
            .flatten();
        self.ivars()
            .active_table_cell
            .borrow_mut()
            .as_mut()
            .unwrap()
            .field_selection = selection;
        if explicit.is_some() && selection.is_none() {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry); // 实际安装不符时不声称已建立方向。
        }
        Ok(())
    }

    /// 现有字段通知及事件尾部只观察实际选择；不替换共享 editor 的私有 delegate。
    pub(in crate::native_surface) fn observe_active_table_field_selection(
        &self,
        event: Option<&NSEvent>,
    ) {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let epoch = self.ivars().table_input_intent_epoch.get();
        let current = || {
            !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == epoch
        };
        let _ = (|| {
            let field =
                native_markdown_table_intent_step(&current, || self.existing_table_cell_editor())?
                    .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
            let actual = native_markdown_table_intent_step(&current, || field.currentEditor())?
                .and_then(|value| value.downcast::<NSTextView>().ok())
                .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
            let extending = if let Some(event) = event {
                native_markdown_table_intent_step(&current, || event.modifierFlags())?
                    .contains(NSEventModifierFlags::Shift)
            } else {
                false // 字段通知只证明真实 caret 或完全未变范围，不能凭通知授予连续扩选。
            };
            self.record_table_field_selection(&actual, None, extending, &current)
        })();
    }

    /// 程序激活维持原全格选择，以两个真实 Native 移动建立 anchor；不猜 selectText 的内部方向。
    pub(super) fn select_entire_table_field(
        &self,
        field: &NSTextField,
        allowed: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        let window = native_markdown_table_intent_step(allowed, || self.window())?
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        if !native_markdown_table_intent_step(allowed, || window.makeFirstResponder(Some(field)))? {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry);
        }
        let actual = native_markdown_table_intent_step(allowed, || field.currentEditor())?
            .and_then(|value| value.downcast::<NSTextView>().ok())
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual);
        if native_markdown_table_intent_step(allowed, || input.hasMarkedText())? {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry); // 初始化不能确认或取消别人的 IME。
        }
        native_markdown_table_intent_step(allowed, || {
            // SAFETY: 公开 NSTextView 标准移动，无 sender 载荷；不注册键盘映射或修改文本。
            unsafe {
                let _: () =
                    msg_send![&*actual, moveToBeginningOfDocument: Option::<&AnyObject>::None];
            }
        })?;
        if native_markdown_table_intent_step(allowed, || actual.selectedRange())?
            != NSRange::new(0, 0)
        {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry);
        }
        native_markdown_table_intent_step(allowed, || {
            // SAFETY: 同一已核 caret 的公开扩选命令；真实动作明确保留起点 0。
            unsafe {
                let _: () = msg_send![&*actual, moveToEndOfDocumentAndModifySelection: Option::<&AnyObject>::None];
            }
        })?;
        let value = native_markdown_table_intent_step(allowed, || actual.string())?;
        let length = native_markdown_table_intent_step(allowed, || value.length())?;
        drop(value); // 借用原字段值只取长度，没有复制源串。
        self.record_table_field_selection(
            &actual,
            Some(MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: length,
            }),
            false,
            allowed,
        )
    }

    /// 实际 field editor 接管鼠标 tracking 前，记录其原 glyph 命中及原粒度，绝不套用预览几何。
    pub(in crate::native_surface) fn capture_table_field_mouse_selection(
        &self,
        event: &NSEvent,
    ) -> Option<NativeMarkdownTableFieldMouseSelection> {
        let epoch = self.ivars().table_input_intent_epoch.get();
        let current = || {
            !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == epoch
        };
        (|| {
            let (coordinate, source, document) = self
                .ivars()
                .active_table_cell
                .borrow()
                .as_ref()
                .and_then(|active| {
                Some((
                    active.coordinate,
                    active.source_utf16_range.clone(),
                    active.authority.as_ref()?.document.clone(),
                ))
            })?;
            let field =
                native_markdown_table_intent_step(&current, || self.existing_table_cell_editor())
                    .ok()??;
            let actual = native_markdown_table_intent_step(&current, || field.currentEditor())
                .ok()??
                .downcast::<NSTextView>()
                .ok()?;
            if !native_markdown_table_intent_step(&current, || {
                self.table_field_command_is_current(&field, &actual)
            })
            .ok()?
            {
                return None;
            }
            let point =
                native_markdown_table_intent_step(&current, || event.locationInWindow()).ok()?;
            let point = native_markdown_table_intent_step(&current, || {
                actual.convertPoint_fromView(point, None)
            })
            .ok()?;
            if !native_markdown_table_intent_step(&current, || {
                NSPointInRect(point, actual.visibleRect())
            })
            .ok()?
            {
                return None; // 此次真实 Down 不在原字段，不能拿正文/工具栏点击改字段方向。
            }
            let hit = native_markdown_table_intent_step(&current, || {
                actual.characterIndexForInsertionAtPoint(point)
            })
            .ok()?;
            let count = native_markdown_table_intent_step(&current, || event.clickCount()).ok()?;
            let granularity = match count {
                count if count >= 3 => NSSelectionGranularity::SelectByParagraph,
                2 => NSSelectionGranularity::SelectByWord,
                _ => NSSelectionGranularity::SelectByCharacter,
            };
            let down = native_markdown_table_intent_step(&current, || {
                actual.selectionRangeForProposedRange_granularity(NSRange::new(hit, 0), granularity)
            })
            .ok()?;
            let before =
                native_markdown_table_intent_step(&current, || actual.selectedRange()).ok()?;
            let extending = native_markdown_table_intent_step(&current, || event.modifierFlags())
                .ok()?
                .contains(NSEventModifierFlags::Shift);
            let editor =
                native_markdown_table_intent_step(&current, || Weak::from(&*actual)).ok()?;
            Some(NativeMarkdownTableFieldMouseSelection {
                editor,
                epoch,
                coordinate,
                source,
                document,
                before,
                down,
                extending,
            })
        })()
        .filter(|_| current())
    }

    /// 同栈原生 tracking 已结束；只有同一字段、原 epoch 与真实最终范围能够建立非 Shift 方向。
    pub(in crate::native_surface) fn finish_table_field_mouse_selection(
        &self,
        mouse: NativeMarkdownTableFieldMouseSelection,
    ) {
        let current = || {
            !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == mouse.epoch
                && self
                    .ivars()
                    .active_table_cell
                    .borrow()
                    .as_ref()
                    .is_some_and(|active| {
                        active.coordinate == mouse.coordinate
                            && active.source_utf16_range == mouse.source
                            && active.authority.as_ref().is_some_and(|authority| {
                                WeakMarkdownDocument::ptr_eq(&authority.document, &mouse.document)
                            })
                    })
        };
        let _ = (|| {
            let actual = native_markdown_table_intent_step(&current, || mouse.editor.load())?
                .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
            let after = native_markdown_table_intent_step(&current, || actual.selectedRange())?;
            let native_gesture = || {
                let down_end = mouse.down.location.checked_add(mouse.down.length)?;
                let end = after.location.checked_add(after.length)?;
                if after.location < mouse.down.location && end == down_end {
                    Some(MarkdownTextSelection {
                        anchor_utf16: down_end,
                        head_utf16: after.location,
                    })
                } else if after.location == mouse.down.location && end >= down_end {
                    Some(MarkdownTextSelection {
                        anchor_utf16: mouse.down.location,
                        head_utf16: end,
                    })
                } else {
                    None // 只有实际 Native tracking 保留原 Down 粒度边缘才能证明最终 head。
                }
            };
            let explicit = if mouse.extending || after == mouse.before && after.length != 0 {
                // 已知方向的 Shift 在原 Pointer 消费；落到系统的未知方向绝不由最终范围补猜。
                None // 原范围未变可能是原生拖放起点；不能把内部点击误作新的逻辑 anchor。
            } else {
                native_gesture()
            };
            self.record_table_field_selection(&actual, explicit, mouse.extending, &current)
        })();
    }

    /// 跨格 Shift 使用原 canonical anchor；选择后继只消费本次发布的唯一收据。
    pub(super) fn install_table_pointer_body_selection(
        &self,
        request: &NativeMarkdownTableCellRequest,
        pointer: &NativeMarkdownTableCellPointer,
    ) -> Result<(), String> {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let storage = self.ivars().text_storage.load().ok_or("原笔记已关闭")?;
        let event = pointer.latest_event().ok_or("原鼠标事件已归还")?;
        let current = || {
            pointer.current(self) && pointer.owns_event(&event) && request.current(self, &storage)
        };
        if !current() {
            return Err("原表格选择已撤权".into());
        }
        let captured = pointer
            .captured
            .borrow()
            .clone()
            .ok_or("原选择票据已归还")?;
        let anchor = pointer.anchor.anchor_utf16;
        let mut head = pointer.head.get();
        if pointer.click_count >= 2 {
            let graph = NativeMarkdownSelectionGraph::load(self, &current)?;
            let granularity = if pointer.click_count >= 3 {
                objc2_app_kit::NSTextSelectionGranularity::Paragraph
            } else {
                objc2_app_kit::NSTextSelectionGranularity::Word
            };
            let expanded = graph.enclosing_selection(
                MarkdownTextSelection::caret(head),
                head < anchor,
                granularity,
                pointer.key.document_length,
                &current,
            )?;
            head = if head < anchor {
                expanded
                    .selection
                    .anchor_utf16
                    .min(expanded.selection.head_utf16)
            } else {
                expanded
                    .selection
                    .anchor_utf16
                    .max(expanded.selection.head_utf16)
            };
        }
        let before =
            native_markdown_table_intent_step(&current, || self.native_ordered_selection_range())
                .map_err(|_| "原鼠标选择已撤权")?
                .map_err(|_| "正文当前选择不是单一范围")?;
        let target = captured
            .with_observed_native_before(before)
            .and_then(|captured| {
                captured.with_selection(
                    MarkdownTextSelection {
                        anchor_utf16: anchor,
                        head_utf16: head,
                    },
                    true,
                )
            })
            .map_err(|_| "原 Shift anchor 或目标已改变")?;
        pointer.body_selection.set(true);
        request.phase.set(NativeMarkdownTableCellPhase::Selecting);
        let receipt = storage
            .update_active_selection_observed(target, |receipt| {
                let previous = request.committed.replace(Some(Rc::clone(receipt)));
                drop(previous); // 纯发布先接住新 serial，后续 Native 通知只能沿这次真实后果继续。
            })
            .ok_or("正文未接受原表格扩选")?;
        let captured = receipt
            .captured_selection()
            .map_err(|_| "表格扩选发布已失效")?;
        let previous = pointer.captured.replace(Some(captured));
        drop(previous);
        let accepted = self.apply_committed_selection_if(&receipt, current);
        if !accepted || !receipt.current() {
            return Err("原表格扩选安装已撤权".into());
        }
        let window = native_markdown_table_intent_step(&current, || self.window())
            .map_err(|_| "原窗口已改变")?
            .ok_or("原窗口已关闭")?;
        if !native_markdown_table_intent_step(&current, || window.makeFirstResponder(Some(self)))
            .map_err(|_| "原鼠标焦点已撤权")?
        {
            return Err("正文未取得输入焦点".into());
        }
        drop(window); // 真实 Native claim 结束后才通知合成后继；通知本身不授予产品权限。
        #[cfg(all(
            feature = "native-markdown-runtime-probe",
            feature = "native-table-row-append"
        ))]
        if current() && receipt.current() {
            NativeMarkdownTableAppendWorkload::observe_pointer_ready(self, pointer, true);
        }
        if pointer.ended.get() {
            pointer.cancel();
            self.ivars()
                .pending_table_cell_activation
                .clear_if_same(request);
        }
        Ok(())
    }

    /// 返回“原 Pointer 已消费、需交还原系统 Up”；两个结果只属于本同步真实事件，不建立后继状态。
    pub(in crate::native_surface) fn route_table_pointer_event(
        &self,
        event: &NSEvent,
    ) -> (bool, bool) {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let kind = event.r#type();
        if !matches!(
            kind,
            NSEventType::LeftMouseDragged | NSEventType::LeftMouseUp | NSEventType::MouseCancelled
        ) {
            return (false, false);
        }
        let request = self.ivars().pending_table_cell_activation.request();
        let pending_pointer = request.as_ref().and_then(|request| request.pointer());
        let active = self
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .and_then(|state| {
                state.pointer.as_ref().map(|pointer| {
                    (
                        state.coordinate,
                        state.source_utf16_range.clone(),
                        Rc::clone(pointer),
                    )
                })
            });
        let pointer =
            pending_pointer.or_else(|| active.as_ref().map(|(_, _, pointer)| Rc::clone(pointer)));
        let Some(pointer) = pointer else {
            return (false, false);
        };
        let Some(storage) = self.ivars().text_storage.load() else {
            return (false, false);
        };
        let Some(original_latest) = pointer.latest_event() else {
            return (false, false);
        };
        let event_claim = RefCell::new(original_latest);
        let current = || {
            pointer.current(self)
                && pointer.owns_event(&event_claim.borrow())
                && storage.ivars().accepts_edits.get()
                && storage.presentation_mode() == pointer.key.presentation_mode
                && storage.ivars().table_projection_width_key.get() == pointer.key.text_view_width
                && WeakMarkdownDocument::ptr_eq(
                    &pointer.identity,
                    &storage.ivars().document.borrow(),
                )
                && request
                    .as_ref()
                    .is_none_or(|request| request.current(self, &storage))
                && active.as_ref().is_none_or(|(coordinate, source, _)| {
                    self.ivars()
                        .active_table_cell
                        .borrow()
                        .as_ref()
                        .is_some_and(|state| {
                            state.coordinate == *coordinate
                                && state.source_utf16_range == *source
                                && state
                                    .pointer
                                    .as_ref()
                                    .is_some_and(|active| Rc::ptr_eq(active, &pointer))
                        })
                })
        };
        let cross_cell_shift = pointer.extending && request.as_ref().is_some_and(|request| {
            matches!(&*request.action.borrow(), NativeMarkdownTableCellAction::ActivateOriginalClick { source_utf16_range, .. }
                if !(source_utf16_range.start..=source_utf16_range.end).contains(&pointer.anchor.anchor_utf16))
        });
        let body_hit = pointer.body_selection.get() || cross_cell_shift;
        let system_mouse_up = Cell::new(false); // 只在原窗口/时间/Pointer 权限通过后记录本次真实终点。
        let result = (|| {
            if pointer.ended.get() || !current() {
                return Err(());
            }
            let window = native_markdown_table_intent_step(&current, || event.windowNumber())
                .map_err(|_| ())?;
            let timestamp = native_markdown_table_intent_step(&current, || event.timestamp())
                .map_err(|_| ())?;
            // 同窗口、同按钮的后续真实事件才属于原 Down；新 Down 从不进入这个入口。
            if window != pointer.window_number || timestamp < pointer.down_timestamp {
                return Err(());
            }
            // preview Down 的唯一构造入口来自 PieceBacked.mouseDown，已经经过 NSWindow.super。
            // raw-field Down 在 Panel 内消费，不能给系统补一个它从未开始的 Up。
            system_mouse_up.set(kind == NSEventType::LeftMouseUp && pointer.raw_field.is_none());
            if pointer.raw_field.is_some() {
                return self
                    .update_table_raw_field_pointer(&pointer, event, &current, &event_claim)
                    .map_err(|_| ()); // 同原 Pointer 的 raw 字段坐标不进入 preview hit/旧源长度判断。
            }
            if kind == NSEventType::MouseCancelled {
                pointer.cancel();
                if let Some(request) = &request {
                    self.ivars()
                        .pending_table_cell_activation
                        .clear_if_same(request);
                }
                return Ok(()); // 系统取消只结束选择手势，不提交或回滚实际草稿。
            }
            let point = native_markdown_table_intent_step(&current, || event.locationInWindow())
                .map_err(|_| ())?;
            let point = native_markdown_table_intent_step(&current, || {
                self.convertPoint_fromView(point, None)
            })
            .map_err(|_| ())?;
            let coordinate = active
                .as_ref()
                .map(|(coordinate, _, _)| *coordinate)
                .or_else(|| request.as_ref().map(|request| request.target))
                .ok_or(())?;
            let cell = if body_hit {
                native_markdown_table_intent_step(&current, || {
                    self.visible_table_cell_at_point(point)
                })
                .map_err(|_| ())?
            } else {
                // 字段内拖选沿原格布局到其边缘；不把邻格的源码当成本格局部偏移。
                let key = native_markdown_table_intent_step(&current, || {
                    self.current_table_layout_request_key()
                })
                .map_err(|_| ())?
                .ok_or(())?;
                self.ivars()
                    .visible_table_layout_cache
                    .borrow()
                    .as_ref()
                    .filter(|cache| {
                        cache.request_key == key
                            && WeakMarkdownDocument::ptr_eq(
                                &cache.document_identity,
                                &pointer.identity,
                            )
                    })
                    .and_then(|cache| {
                        cache
                            .tables
                            .iter()
                            .flat_map(|table| &table.cells)
                            .find(|cell| cell.coordinate == coordinate)
                    })
                    .cloned()
            };
            let hit = if let Some(cell) = cell {
                native_markdown_table_intent_step(&current, || cell.source_index_at_point(point))
                    .map_err(|_| ())?
                    .ok_or(())?
            } else if body_hit {
                let graph = NativeMarkdownSelectionGraph::load(self, &current).map_err(|_| ())?;
                graph
                    .selection_at_point(
                        self,
                        NativeMarkdownSelectionPointQuery {
                            point,
                            anchor: None,
                            extending: false,
                            selecting: true,
                            length: pointer.key.document_length,
                        },
                        &current,
                    )
                    .map_err(|_| ())?
                    .selection
                    .head_utf16
            } else {
                return Err(());
            };
            if !pointer
                .identity
                .upgrade()
                .is_some_and(|document| document.borrow().byte_offset_for_utf16(hit).is_ok())
            {
                return Err(());
            }
            let retained = native_markdown_table_intent_step(&current, || {
                // SAFETY: 本 sendEvent 正在持有该真实事件，单个 Latest claim 替换旧事件。
                unsafe { Retained::retain(std::ptr::from_ref(event).cast_mut()) }
            })
            .map_err(|_| ())?
            .ok_or(())?;
            let retained = Rc::new(retained);
            let previous_claim = event_claim.replace(Rc::clone(&retained));
            let previous = {
                let mut events = pointer.events.borrow_mut();
                let events = events.as_mut().ok_or(())?;
                std::mem::replace(&mut events.latest, retained)
            };
            drop(previous);
            drop(previous_claim);
            if !current() {
                return Err(());
            }
            pointer.head.set(hit);
            pointer.ended.set(kind == NSEventType::LeftMouseUp);
            if let Some((_, source, _)) = &active {
                let field = native_markdown_table_intent_step(&current, || {
                    self.existing_table_cell_editor()
                })
                .map_err(|_| ())?
                .ok_or(())?;
                self.apply_table_field_pointer(&field, source, &pointer, &current)
                    .map_err(|_| ())?;
                if pointer.ended.get() {
                    pointer.cancel();
                    let previous = self
                        .ivars()
                        .active_table_cell
                        .borrow_mut()
                        .as_mut()
                        .and_then(|state| {
                            state
                                .pointer
                                .as_ref()
                                .filter(|candidate| Rc::ptr_eq(candidate, &pointer))?;
                            state.pointer.take()
                        });
                    drop(previous);
                }
            } else if pointer.body_selection.get()
                && let Some(request) = &request
            {
                self.install_table_pointer_body_selection(request, &pointer)
                    .map_err(|_| ())?;
            }
            Ok(())
        })();
        if result.is_err() && pointer.owns_event(&event_claim.borrow()) {
            pointer.cancel();
            let previous = self
                .ivars()
                .active_table_cell
                .borrow_mut()
                .as_mut()
                .and_then(|state| {
                    state
                        .pointer
                        .as_ref()
                        .filter(|candidate| Rc::ptr_eq(candidate, &pointer))?;
                    state.pointer.take()
                });
            drop(previous);
            if let Some(request) = &request {
                self.ivars()
                    .pending_table_cell_activation
                    .clear_if_same(request);
            }
            // 只取消此原 gesture，草稿、正文、历史和先前已接受选择均保留。
        }
        (true, system_mouse_up.get())
    }

    /// 首次点击在 Ready 后恢复原 point 的实际选择；不重播早已结束的 mouseDown tracking。
    pub(super) fn apply_table_field_pointer(
        &self,
        field: &NSTextField,
        source: &std::ops::Range<usize>,
        pointer: &NativeMarkdownTableCellPointer,
        allowed: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        let event = pointer
            .latest_event()
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let current = || allowed() && pointer.current(self) && pointer.owns_event(&event);
        let window = native_markdown_table_intent_step(&current, || self.window())?
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        if !native_markdown_table_intent_step(&current, || window.makeFirstResponder(Some(field)))?
        {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry);
        }
        let actual = native_markdown_table_intent_step(&current, || field.currentEditor())?
            .and_then(|value| value.downcast::<NSTextView>().ok())
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual);
        if native_markdown_table_intent_step(&current, || input.hasMarkedText())? {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry); // 不为鼠标定位提交或取消输入法。
        }
        let length = source
            .end
            .checked_sub(source.start)
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let down = pointer
            .down_hit
            .checked_sub(source.start)
            .filter(|position| *position <= length)
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let head = pointer
            .head
            .get()
            .checked_sub(source.start)
            .filter(|position| *position <= length)
            .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
        let granularity = pointer.granularity();
        let down_range = if let Some(range) = pointer.initial_field_range.get() {
            range // 整个 gesture 固定原单击/词/段落范围，不把上次扩选结果改作 anchor。
        } else {
            let range = native_markdown_table_intent_step(&current, || {
                actual
                    .selectionRangeForProposedRange_granularity(NSRange::new(down, 0), granularity)
            })?;
            pointer.initial_field_range.set(Some(range));
            range
        };
        let head_range = native_markdown_table_intent_step(&current, || {
            actual.selectionRangeForProposedRange_granularity(NSRange::new(head, 0), granularity)
        })?;
        let valid_range = |range: NSRange| {
            range
                .location
                .checked_add(range.length)
                .is_some_and(|end| end <= length)
        };
        if !valid_range(down_range) || !valid_range(head_range) {
            return Err(NativeMarkdownStableTableFailure::MissingGeometry); // 原生粒度范围也须落在真实有限字段内。
        }
        let (anchor, head) = if pointer.extending {
            let anchor = pointer
                .anchor
                .anchor_utf16
                .checked_sub(source.start)
                .filter(|position| *position <= length)
                .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
            (
                anchor,
                if head < anchor {
                    head_range.location
                } else {
                    head_range.location + head_range.length
                },
            )
        } else if head < down_range.location {
            (down_range.location + down_range.length, head_range.location)
        } else {
            (down_range.location, head_range.location + head_range.length)
        };
        let start = anchor.min(head);
        let affinity = if head < anchor {
            NSSelectionAffinity::Upstream
        } else {
            NSSelectionAffinity::Downstream
        };
        native_markdown_table_intent_step(&current, || {
            actual.setSelectionGranularity(granularity)
        })?;
        native_markdown_table_intent_step(&current, || {
            actual.setSelectedRange_affinity_stillSelecting(
                NSRange::new(start, anchor.max(head) - start),
                affinity,
                !pointer.ended.get(),
            )
        })?;
        self.record_table_field_selection(
            &actual,
            Some(MarkdownTextSelection {
                anchor_utf16: anchor,
                head_utf16: head,
            }),
            false,
            &current,
        )
    }

    /// 原格命中、原事件与前格唯一提交后果一起进入有限 RequiredRow，不从当前坐标重领权限。
    pub(super) fn activate_table_cell_from_original_click(
        &self,
        cell: NativeMarkdownVisibleTableCellLayout,
        event: &NSEvent,
    ) {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        self.cancel_table_continuity_for_new_input();
        let epoch = self.ivars().table_input_intent_epoch.get();
        let fold = self.ivars().folds.revision();
        // 原矩阵和点击cell都已存在，首Native前只复制Weak及标量；不保留cache或Native cell引用。
        let captured = {
            let cache = self.ivars().visible_table_layout_cache.borrow();
            cache.as_ref().and_then(|cache| {
                cache
                    .tables
                    .iter()
                    .flat_map(|table| &table.cells)
                    .any(|candidate| {
                        candidate.coordinate == cell.coordinate
                            && candidate.source_utf16_range == cell.source_utf16_range
                            && candidate.frame == cell.frame
                    })
                    .then(|| (cache.document_identity.clone(), cache.request_key))
            })
        };
        let Some((identity, key)) = captured else {
            return;
        };
        let initial = || {
            !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == epoch
                && self.ivars().folds.read_is_current(fold)
                && identity.upgrade().is_some_and(|document| {
                    let document = document.borrow();
                    document.generation() == key.document_generation
                        && document.utf16_len() == key.document_length
                })
        };
        let header_utf16 = identity.upgrade().and_then(|document| {
            let document = document.borrow();
            let byte = document
                .byte_offset_for_utf16(cell.coordinate.table_utf16_start)
                .ok()?;
            let line = document.line_for_byte(byte).ok()?;
            let start = document.byte_offset_for_line(line).ok()?;
            document.utf16_offset_for_byte(start).ok()
        });
        let Some(header_utf16) = header_utf16 else {
            return;
        };
        let Ok(Some(storage)) =
            native_markdown_table_intent_step(&initial, || self.ivars().text_storage.load())
        else {
            return;
        };
        let Ok(Some(actual_key)) =
            native_markdown_table_intent_step(&initial, || self.current_table_layout_request_key())
        else {
            return;
        };
        if actual_key != key || !self.table_input_intent_is_current(&storage, &identity, key, epoch)
        {
            return; // 原点击必须仍命中原矩阵；不能把getter返回的新key贴给旧cell。
        }
        let origin_current = || {
            !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == epoch
                && self.ivars().folds.owner_is_current()
                && storage.ivars().accepts_edits.get()
                && storage.presentation_mode() == key.presentation_mode
                && storage.ivars().table_projection_width_key.get() == key.text_view_width
                && std::rc::Weak::ptr_eq(&identity, &storage.ivars().document.borrow())
        };
        if self
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .is_some_and(|active| active.coordinate == cell.coordinate)
        {
            // 同格已显示原 raw 草稿，真实事件直接交给其 field editor；原生保留 Shift anchor、双击和拖选。
            let current = || origin_current() && initial();
            let mouse = self.capture_table_field_mouse_selection(event);
            if let Ok(Some(editor)) =
                native_markdown_table_intent_step(&current, || self.existing_table_cell_editor())
            {
                let actual = native_markdown_table_intent_step(&current, || editor.currentEditor())
                    .ok()
                    .flatten()
                    .and_then(|value| value.downcast::<NSTextView>().ok());
                let _ = native_markdown_table_intent_step(&current, || {
                    // SAFETY: 原 mouseDown 仍在本同步分发栈；没有重播过去的 Up 或伪造事件。
                    unsafe {
                        if let Some(actual) = actual {
                            let _: () = msg_send![&*actual, mouseDown: event];
                        } else {
                            let _: () = msg_send![&*editor, mouseDown: event];
                        }
                    }
                });
            }
            if let Some(mouse) = mouse {
                self.finish_table_field_mouse_selection(mouse);
            }
            return;
        }
        let Ok(capture) = self.ivars().folds.capture_selection() else {
            return;
        };
        let Some(before_selection) = identity
            .upgrade()
            .map(|document| document.borrow().selection())
        else {
            return;
        };
        let known = self.ivars().folds.known_selection() == Some(before_selection);
        let Ok(captured) = capture.finish(before_selection, known) else {
            return;
        };
        let Ok(flags) = native_markdown_table_intent_step(&initial, || event.modifierFlags())
        else {
            return;
        };
        let extending_field = flags.contains(NSEventModifierFlags::Shift)
            && self.ivars().active_table_cell.borrow().is_some();
        if flags.contains(NSEventModifierFlags::Shift) && !known && !extending_field {
            return; // 没有原方向事实就不能把有序范围的一端猜成跨格 Shift anchor。
        }
        let Ok(window_point) =
            native_markdown_table_intent_step(&initial, || event.locationInWindow())
        else {
            return;
        };
        let Ok(point) = native_markdown_table_intent_step(&initial, || {
            self.convertPoint_fromView(window_point, None)
        }) else {
            return;
        };
        let Ok(Some(original_hit)) =
            native_markdown_table_intent_step(&initial, || cell.source_index_at_point(point))
        else {
            return;
        };
        if !identity.upgrade().is_some_and(|document| {
            document
                .borrow()
                .byte_offset_for_utf16(original_hit)
                .is_ok()
        }) {
            return; // 原 glyph hit 仍须是实际 piece 文档的 scalar caret，不能安装低 surrogate。
        }
        let Ok(count) = native_markdown_table_intent_step(&initial, || event.clickCount()) else {
            return;
        };
        let Ok(window_number) =
            native_markdown_table_intent_step(&initial, || event.windowNumber())
        else {
            return;
        };
        let Ok(timestamp) = native_markdown_table_intent_step(&initial, || event.timestamp())
        else {
            return;
        };
        let Ok(Some(down)) = native_markdown_table_intent_step(&initial, || {
            // SAFETY: 原真实事件由本 mouseDown 回调持有；额外 claim 仅供此未完成 gesture。
            unsafe { Retained::retain(std::ptr::from_ref(event).cast_mut()) }
        }) else {
            return;
        };
        let committed = if extending_field {
            self.commit_table_cell_for_shift_click(&origin_current)
        } else {
            self.commit_table_cell_for_click(&origin_current, &|_| {})
        };
        let outcome = match committed {
            Ok(outcome) => outcome,
            Err(error) => {
                if origin_current() {
                    present_native_markdown_error(&error);
                }
                return; // 失效或失败的旧点击不能再调用公共激活入口领新epoch。
            }
        };
        let mut coordinate = cell.coordinate;
        let mut physical_header = header_utf16;
        let mut source_range = cell.source_utf16_range.clone();
        let mut hit = original_hit;
        let mut anchor = before_selection;
        if extending_field {
            let Some(field_anchor) = outcome.field_anchor else {
                return; // 原字段提交没有交付精确序列化锚点，不能退回旧 canonical anchor。
            };
            anchor = MarkdownTextSelection::caret(field_anchor);
        }
        let after_current = || {
            origin_current()
                && if let Some(receipt) = &outcome.committed {
                    receipt.current()
                } else {
                    initial()
                }
        };
        if !after_current() {
            return;
        }
        if outcome.changed {
            let (Some(edit), Some(_receipt)) = (&outcome.edit, &outcome.committed) else {
                return;
            };
            let Some(mapped_legacy) = map_click_position(coordinate.table_utf16_start, edit) else {
                return;
            };
            let Some(mapped_physical) = map_click_position(physical_header, edit) else {
                return;
            };
            let Some(mapped_source) = map_click_source_range(&source_range, edit) else {
                return;
            };
            coordinate.table_utf16_start = mapped_legacy;
            physical_header = mapped_physical;
            source_range = mapped_source;
            let Some(mapped_hit) = map_click_position(hit, edit) else {
                return;
            };
            hit = mapped_hit;
            if flags.contains(NSEventModifierFlags::Shift) && !extending_field {
                let Some(mapped_anchor) = map_click_position(anchor.anchor_utf16, edit) else {
                    return; // 原 anchor 被真实前格替换覆盖时不猜后继位置。
                };
                anchor.anchor_utf16 = mapped_anchor;
            }
        }
        let expected_generation = key
            .document_generation
            .checked_add(u64::from(outcome.changed));
        let expected_length = outcome
            .edit
            .as_ref()
            .map_or(Some(key.document_length), |edit| {
                key.document_length
                    .checked_add_signed(edit.change_in_utf16_length)
            });
        let header_byte = identity.upgrade().and_then(|document| {
            let document = document.borrow();
            if Some(document.generation()) != expected_generation
                || Some(document.utf16_len()) != expected_length
            {
                return None;
            }
            // UTF16位置先按原edit映射，再在同新root换算byte；UTF8字节数与UTF16delta不能混加。
            let byte = document.byte_offset_for_utf16(physical_header).ok()?;
            let line = document.line_for_byte(byte).ok()?;
            (document.byte_offset_for_line(line).ok()? == byte).then_some(byte)
        });
        let Some(header_byte) = header_byte else {
            return;
        };
        if !after_current() {
            return;
        }
        let Some(expected_generation) = expected_generation else {
            return;
        };
        let Some(expected_length) = expected_length else {
            return;
        };
        let captured = if let Some(receipt) = &outcome.committed {
            let Ok(captured) = receipt.captured_selection() else {
                return;
            };
            captured
        } else {
            captured
        };
        let down = Rc::new(down);
        let pointer = Rc::new(NativeMarkdownTableCellPointer {
            events: RefCell::new(Some(PointerEvents {
                _down: Rc::clone(&down),
                latest: down,
            })),
            input_epoch: epoch,
            identity: identity.clone(),
            key: NativeMarkdownTableLayoutRequestKey {
                document_generation: expected_generation,
                document_length: expected_length,
                ..key
            },
            window_number,
            down_timestamp: timestamp,
            ended: Cell::new(false),
            extending: flags.contains(NSEventModifierFlags::Shift),
            click_count: count,
            anchor,
            down_hit: hit,
            head: Cell::new(hit),
            initial_field_range: Cell::new(None),
            captured: RefCell::new(Some(captured)),
            body_selection: Cell::new(false),
            raw_field: None,
        });
        #[cfg(all(
            feature = "native-markdown-runtime-probe",
            feature = "native-table-row-append"
        ))]
        NativeMarkdownTableAppendWorkload::observe_pointer_begin(
            self,
            &pointer,
            &identity,
            coordinate,
            &source_range,
        );
        let intent = NativeMarkdownTableCellIntent {
            input_epoch: epoch,
            fold_revision: fold,
            committed: outcome.committed.as_ref().map(Rc::clone),
        };
        if let Err(error) = self.start_required_table_cell(
            coordinate,
            NativeMarkdownTableCellAction::ActivateOriginalClick {
                source_utf16_range: source_range,
                pointer: Some(pointer),
            },
            intent,
            Some(header_byte),
            &after_current,
        ) && after_current()
        {
            present_native_markdown_error(&error);
        }
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
    /// 原锚点的前后、相接边界、插入同位点和被替换内容分别处理；溢出与覆盖必须拒绝。
    fn click_anchor_maps_only_the_unique_edit_consequence() {
        let change = edit(10..15, -3);
        assert_eq!(map_click_position(9, &change), Some(9));
        assert_eq!(map_click_position(10, &change), None);
        assert_eq!(map_click_position(14, &change), None);
        assert_eq!(map_click_position(15, &change), Some(12));
        assert_eq!(map_click_position(20, &change), Some(17));
        assert_eq!(map_click_position(10, &edit(10..10, 2)), Some(12));
        assert_eq!(map_click_position(usize::MAX, &edit(0..0, 1)), None);
        assert_eq!(
            map_click_position(1, &edit(std::ops::Range { start: 3, end: 2 }, 0)),
            None
        );
        assert_eq!(map_click_source_range(&(5..10), &change), Some(5..10));
        assert_eq!(map_click_source_range(&(15..20), &change), Some(12..17));
        assert_eq!(map_click_source_range(&(9..11), &change), None);
        assert_eq!(map_click_source_range(&(14..16), &change), None);
        assert_eq!(map_click_source_range(&(9..16), &change), None);
        assert_eq!(map_click_source_range(&(9..11), &edit(10..10, 2)), None);
        assert_eq!(
            map_click_source_range(&(5..10), &edit(10..10, 2)),
            Some(5..10)
        );
        assert_eq!(
            map_click_source_range(&(10..15), &edit(10..10, 2)),
            Some(12..17)
        );
    }

    #[test]
    /// 实际piece编辑的UTF16位移不等于UTF8位移，物理H必须在新root重新换算而非byte加delta。
    fn click_header_maps_utf16_before_resolving_new_source_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("click-unicode.md");
        std::fs::write(&path, "😀\n| H |\n| - |\n| b |\n").unwrap();
        let mut document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        let old_header_utf16 = 3;
        let old_header_byte = document.byte_offset_for_utf16(old_header_utf16).unwrap();
        let before_length = document.utf16_len();
        document
            .replace_utf16_range(0..2, "a", MarkdownTextSelection::caret(1))
            .unwrap();
        let delta = isize::try_from(document.utf16_len()).unwrap()
            - isize::try_from(before_length).unwrap();
        let mapped = map_click_position(old_header_utf16, &edit(0..2, delta)).unwrap();
        let new_header_byte = document.byte_offset_for_utf16(mapped).unwrap();
        assert_eq!(mapped, 2);
        assert_eq!(new_header_byte, 2);
        assert_ne!(
            old_header_byte.checked_add_signed(delta),
            Some(new_header_byte)
        );
        assert_eq!(document.line_for_byte(new_header_byte).unwrap(), 1);
    }
}
