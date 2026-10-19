//! 菜单插表只观察自身字符后果创建的原行请求；不重发输入、不授予产品布局权限。

use super::*;

pub(in crate::native_surface) struct NativeMarkdownTableInsertionObservation {
    original: NativeMarkdownSelectionCapture,
    document: WeakMarkdownDocument,
    view_identity: usize,
    before_key: NativeMarkdownTableLayoutRequestKey,
    input_epoch: u64,
    expected_generation: u64,
    expected_length: usize,
    target: NativeMarkdownTableCellCoordinate,
    after: MarkdownTextSelection,
    receipt: RefCell<Option<Rc<NativeMarkdownTableCellReceipt>>>,
    outcome: Cell<Option<NativeMarkdownTableCellOutcome>>,
    callback: Cell<Option<fn(NativeMarkdownTableCellOutcome)>>,
}

impl NativeMarkdownTableInsertionObservation {
    pub(in crate::native_surface) fn outcome(&self) -> Option<NativeMarkdownTableCellOutcome> {
        self.receipt
            .borrow()
            .as_ref()
            .map_or_else(|| self.outcome.get(), |receipt| receipt.outcome())
    }

    pub(in crate::native_surface) fn current_for(
        &self,
        view: &PieceBackedMarkdownTextView,
    ) -> bool {
        self.receipt
            .borrow()
            .as_ref()
            .is_some_and(|receipt| receipt.current_for(view))
    }

    fn committed(&self) -> bool {
        self.document.upgrade().is_some_and(|document| {
            let document = document.borrow();
            document.generation() == self.expected_generation
                && document.utf16_len() == self.expected_length
        })
    }

    /// 未绑定的失败/关闭也投递一次终点；绑定以后只由原 request 的既有 observer 负责。
    pub(in crate::native_surface) fn finish_unbound(
        &self,
        outcome: NativeMarkdownTableCellOutcome,
    ) {
        if self.receipt.borrow().is_some() || !record_terminal(&self.outcome, outcome) {
            return; // 后继 scope 或关闭不能覆盖真实请求已经负责的终点。
        }
        if let Some(callback) = self.callback.take() {
            enqueue_native_markdown_main_task(move || callback(outcome));
        }
    }

    pub(in crate::native_surface) fn cancel_unbound(&self) {
        self.finish_unbound(NativeMarkdownTableCellOutcome::Cancelled {
            committed: self.committed(),
        });
    }

    fn bind(
        &self,
        view: &PieceBackedMarkdownTextView,
        document: &SharedMarkdownDocument,
        key: NativeMarkdownTableLayoutRequestKey,
        target: NativeMarkdownTableCellCoordinate,
        intent: &NativeMarkdownTableCellIntent,
    ) -> Option<Rc<NativeMarkdownTableCellReceipt>> {
        let committed = intent.committed.as_ref()?;
        if self.outcome.get().is_some()
            || self.receipt.borrow().is_some()
            || self.view_identity != std::ptr::from_ref(view).addr()
            || !std::rc::Weak::ptr_eq(&self.document, &Rc::downgrade(document))
            || !committed.is_text_successor_of(&self.original)
            || committed.selection != self.after
            || key.document_generation != self.expected_generation
            || key.document_length != self.expected_length
            || key.presentation_mode != self.before_key.presentation_mode
            || key.text_view_width != self.before_key.text_view_width
            || target != self.target
            || intent.input_epoch != self.input_epoch
        {
            return None; // 同范围或同代次数值不能替换首菜单动作的原字符转换与原输入/宽度/模式。
        }
        // 实际 fold 和完整选择只来自该次原同 Rc 及其 product intent；不预猜 fold 修订增量。
        let receipt = Rc::new(NativeMarkdownTableCellReceipt {
            document: self.document.clone(),
            view_identity: self.view_identity,
            key,
            accepted_key: Cell::new(key),
            committed: RefCell::new(Some(Rc::clone(committed))),
            coordinate: target,
            accepted_input_epoch: intent.input_epoch,
            fold_revision: intent.fold_revision,
            outcome: Cell::new(None),
            callback: Cell::new(self.callback.take()),
        });
        self.receipt.replace(Some(Rc::clone(&receipt)));
        Some(receipt)
    }
}

/// 栈退出只清本次尚未绑定的 Rc；不能把后继观察槽或其回调 take 掉。
pub(in crate::native_surface) struct NativeMarkdownTableInsertionObservationScope<'a> {
    view: &'a PieceBackedMarkdownTextView,
    observation: Option<Rc<NativeMarkdownTableInsertionObservation>>,
}
impl Drop for NativeMarkdownTableInsertionObservationScope<'_> {
    fn drop(&mut self) {
        let Some(original) = self.observation.as_ref() else {
            return;
        };
        let removed = {
            let mut slot = self.view.ivars().table_insertion_observation.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, original))
            {
                slot.take()
            } else {
                None
            } // 已绑定或后继取代时，不撤掉真实 request 或新观察者。
        };
        if let Some(observation) = removed {
            observation.finish_unbound(NativeMarkdownTableCellOutcome::Failed {
                committed: observation.committed(),
            });
        }
    }
}

/// 只携原观察的 Weak 穿过真实菜单队列；命令未执行或在 view 入口前失败时也不得悬挂终点。
pub(in crate::native_surface) struct NativeMarkdownTableInsertionInvocation {
    observation: std::rc::Weak<NativeMarkdownTableInsertionObservation>,
    started: Cell<bool>,
}
impl NativeMarkdownTableInsertionInvocation {
    pub(in crate::native_surface) fn begin(&self) {
        self.started.set(true); // 仅区别原队列已执行；不改变产品权限或原 source capture。
    }
}
impl Drop for NativeMarkdownTableInsertionInvocation {
    fn drop(&mut self) {
        if let Some(observation) = self.observation.upgrade() {
            let committed = observation.committed();
            let outcome = if self.started.get() {
                NativeMarkdownTableCellOutcome::Failed { committed }
            } else {
                NativeMarkdownTableCellOutcome::Cancelled { committed }
            };
            observation.finish_unbound(outcome); // 已绑定的原 request 自行完成；旧 Weak 不接管后继槽。
        }
    }
}

impl NativeMarkdownController {
    /// 原菜单 dispatch 已核真实 item；这里纯借 ACTIVE 元数据，只带原预签观察的 Weak 入队。
    pub(in crate::native_surface) fn probe_table_insertion_invocation(
        &self,
    ) -> Option<MainThreadBound<NativeMarkdownTableInsertionInvocation>> {
        let observation = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            let surface = active.as_ref()?;
            if !std::ptr::eq(&*surface.controller, self) {
                return None; // 相同 tag 不能把外来 controller 的动作绑定到当前观察。
            }
            let view = &surface.editor.as_ref()?.backend.text_view;
            view.ivars()
                .table_insertion_observation
                .borrow()
                .as_ref()
                .map(Rc::downgrade)
        })?;
        Some(MainThreadBound::new(
            NativeMarkdownTableInsertionInvocation {
                observation,
                started: Cell::new(false),
            },
            self.mtm(),
        ))
    }
}

impl PieceBackedMarkdownTextView {
    /// 菜单执行前固定原 capture 和有限后态；此槽仅用于回归，不改变实际 action 的准入条件。
    pub(in crate::native_surface) fn probe_prepare_table_insertion_observation(
        &self,
        document: &SharedMarkdownDocument,
        added_units: usize,
        target: NativeMarkdownTableCellCoordinate,
        after: MarkdownTextSelection,
        callback: fn(NativeMarkdownTableCellOutcome),
    ) -> Result<Rc<NativeMarkdownTableInsertionObservation>, String> {
        if self.ivars().table_insertion_observation.borrow().is_some() {
            return Err("菜单插表已有未完成的原观察动作".into());
        }
        let original = self
            .ivars()
            .folds
            .capture_selection()
            .map_err(|error| error.to_string())?;
        let input_epoch = self
            .ivars()
            .table_input_intent_epoch
            .get()
            .checked_add(1)
            .ok_or("菜单插表输入代次溢出")?;
        let before_key =
            selection_native_step(&original, || self.current_table_layout_request_key())
                .map_err(|error| error.to_string())?
                .ok_or("菜单插表缺少原布局身份")?;
        let expected_generation = before_key
            .document_generation
            .checked_add(1)
            .ok_or("菜单插表文档代次溢出")?;
        let expected_length = before_key
            .document_length
            .checked_add(added_units)
            .ok_or("菜单插表长度溢出")?;
        let valid = {
            let document = document.borrow();
            document.generation() == before_key.document_generation
                && document.utf16_len() == before_key.document_length
                && before_key.presentation_mode == MarkdownPresentationMode::InlinePreview
        };
        if !valid
            || !original.current()
            || self.ivars().table_insertion_observation.borrow().is_some()
        {
            return Err("菜单插表观察的原文档或输入已改变".into());
        }
        let observation = Rc::new(NativeMarkdownTableInsertionObservation {
            original,
            document: Rc::downgrade(document),
            view_identity: std::ptr::from_ref(self).addr(),
            before_key,
            input_epoch,
            expected_generation,
            expected_length,
            target,
            after,
            receipt: RefCell::new(None),
            outcome: Cell::new(None),
            callback: Cell::new(Some(callback)),
        });
        self.ivars()
            .table_insertion_observation
            .replace(Some(Rc::clone(&observation)));
        Ok(observation)
    }

    pub(in crate::native_surface) fn probe_table_insertion_scope(
        &self,
    ) -> NativeMarkdownTableInsertionObservationScope<'_> {
        NativeMarkdownTableInsertionObservationScope {
            view: self,
            observation: self
                .ivars()
                .table_insertion_observation
                .borrow()
                .as_ref()
                .map(Rc::clone),
        }
    }

    /// 实际 product gate 已固定 key/intent/同 Rc 后，观察者才可附到这个原请求；不匹配只使回归失败。
    pub(in crate::native_surface) fn probe_bind_table_insertion_observation(
        &self,
        document: &SharedMarkdownDocument,
        key: NativeMarkdownTableLayoutRequestKey,
        target: NativeMarkdownTableCellCoordinate,
        intent: &NativeMarkdownTableCellIntent,
    ) -> Option<Rc<NativeMarkdownTableCellReceipt>> {
        let observation = self
            .ivars()
            .table_insertion_observation
            .borrow_mut()
            .take()?;
        let receipt = observation.bind(self, document, key, target, intent);
        if receipt.is_none() {
            observation.finish_unbound(NativeMarkdownTableCellOutcome::Failed {
                committed: observation.committed(),
            });
        }
        receipt
    }
}
