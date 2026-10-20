//! 仅原生回归观察一个真实行请求的终点；收据不拥有正文、Native图或Core scratch。

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native_surface) enum NativeMarkdownTableCellOutcome {
    Activated,
    Cancelled { committed: bool },
    Failed { committed: bool },
}

fn record_terminal(
    slot: &Cell<Option<NativeMarkdownTableCellOutcome>>,
    outcome: NativeMarkdownTableCellOutcome,
) -> bool {
    if slot.get().is_some() {
        return false;
    }
    slot.set(Some(outcome));
    true
}

pub(in crate::native_surface) struct NativeMarkdownTableCellReceipt {
    document: WeakMarkdownDocument,
    view_identity: usize,
    key: NativeMarkdownTableLayoutRequestKey,
    accepted_key: Cell<NativeMarkdownTableLayoutRequestKey>,
    committed: RefCell<Option<Rc<NativeMarkdownCommittedSelection>>>,
    coordinate: NativeMarkdownTableCellCoordinate,
    accepted_input_epoch: u64,
    fold_revision: u64,
    outcome: Cell<Option<NativeMarkdownTableCellOutcome>>,
    callback: Cell<Option<fn(NativeMarkdownTableCellOutcome)>>,
}

impl NativeMarkdownTableCellReceipt {
    pub(in crate::native_surface) fn outcome(&self) -> Option<NativeMarkdownTableCellOutcome> {
        self.outcome.get()
    }
    /// 激活终点之后、下一实际输入之前仍守同一原票；只读Rust值，不升级Native weak。
    pub(in crate::native_surface) fn current_for(
        &self,
        view: &PieceBackedMarkdownTextView,
    ) -> bool {
        let key = self.accepted_key.get();
        let committed = self.committed.borrow().as_ref().map(Rc::clone);
        self.view_identity == std::ptr::from_ref(view).addr()
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().table_input_intent_epoch.get() == self.accepted_input_epoch
            && committed.as_ref().map_or_else(
                || view.ivars().folds.read_is_current(self.fold_revision),
                |receipt| receipt.current(),
            )
            && self.document.upgrade().is_some_and(|document| {
                let document = document.borrow();
                document.generation() == key.document_generation
                    && document.utf16_len() == key.document_length
            })
            && view
                .ivars()
                .visible_table_layout_cache
                .borrow()
                .as_ref()
                .is_some_and(|cache| {
                    std::rc::Weak::ptr_eq(&cache.document_identity, &self.document)
                        && cache.request_key.text_view_width == self.key.text_view_width
                        && cache.request_key.presentation_mode == self.key.presentation_mode
                })
    }
    pub(super) fn activated(
        &self,
        key: NativeMarkdownTableLayoutRequestKey,
        committed: Option<Rc<NativeMarkdownCommittedSelection>>,
    ) {
        if self.outcome.get().is_some() {
            return;
        } // 既有终点的权限数据同样不可被迟到激活改写。
        // 只接受原request经唯一pre-notify票推进后的key；不重采当前generation/fold授权。
        self.accepted_key.set(key);
        *self.committed.borrow_mut() = committed;
        self.finish(NativeMarkdownTableCellOutcome::Activated);
    }
    pub(super) fn finish(&self, outcome: NativeMarkdownTableCellOutcome) {
        if !record_terminal(&self.outcome, outcome) {
            return; // 终点只签一次；槽清理不能覆盖成功或重复投递。
        }
        if let Some(callback) = self.callback.take() {
            // 队列只携带函数指针和Copy结果；driver必须在owner放回后才消费独立事件。
            enqueue_native_markdown_main_task(move || callback(outcome));
        }
    }
    pub(super) fn matches(
        &self,
        view: &PieceBackedMarkdownTextView,
        identity: &WeakMarkdownDocument,
        key: NativeMarkdownTableLayoutRequestKey,
        target: NativeMarkdownTableCellCoordinate,
        intent: &NativeMarkdownTableCellIntent,
    ) -> bool {
        self.outcome.get().is_none()
            && self.view_identity == std::ptr::from_ref(view).addr()
            && std::rc::Weak::ptr_eq(&self.document, identity)
            && self.key.document_generation == key.document_generation
            && self.key.document_length == key.document_length
            && self.key.presentation_mode == key.presentation_mode
            && self.key.text_view_width == key.text_view_width
            && self.coordinate == target
            && self.accepted_input_epoch == intent.input_epoch
            && self.fold_revision == intent.fold_revision
    }
}

impl PieceBackedMarkdownTextView {
    /// 首Native前从原矩阵、Weak正文及原fold/输入值签票；下一动作只准推进一次输入epoch。
    pub(in crate::native_surface) fn probe_prepare_table_cell_receipt(
        &self,
        document: &SharedMarkdownDocument,
        coordinate: NativeMarkdownTableCellCoordinate,
        callback: fn(NativeMarkdownTableCellOutcome),
    ) -> Result<Rc<NativeMarkdownTableCellReceipt>, String> {
        let identity = Rc::downgrade(document);
        let key = self
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .filter(|cache| std::rc::Weak::ptr_eq(&cache.document_identity, &identity))
            .map(|cache| cache.request_key)
            .ok_or("表格回归缺少原同源矩阵")?;
        let fold_revision = self.ivars().folds.revision();
        let accepted_input_epoch = self
            .ivars()
            .table_input_intent_epoch
            .get()
            .checked_add(1)
            .ok_or("表格输入代次溢出")?;
        let document_ref = document.borrow();
        if self.ivars().textkit_graph_teardown.get()
            || !self.ivars().folds.read_is_current(fold_revision)
            || document_ref.generation() != key.document_generation
            || document_ref.utf16_len() != key.document_length
            || key.presentation_mode != MarkdownPresentationMode::InlinePreview
        {
            return Err("表格回归原owner或源权限已改变".into());
        }
        Ok(Rc::new(NativeMarkdownTableCellReceipt {
            document: identity,
            view_identity: std::ptr::from_ref(self).addr(),
            key,
            accepted_key: Cell::new(key),
            committed: RefCell::new(None),
            coordinate,
            accepted_input_epoch,
            fold_revision,
            outcome: Cell::new(None),
            callback: Cell::new(Some(callback)),
        }))
    }

    /// 同普通产品激活body；外部只附上预声明收据，不手动推进Core或创建字段。
    pub(in crate::native_surface) fn probe_begin_table_cell_editing(
        &self,
        cell: NativeMarkdownVisibleTableCellLayout,
        receipt: Rc<NativeMarkdownTableCellReceipt>,
    ) -> bool {
        let accepted =
            self.request_table_cell_activation_observed(cell.coordinate, Some(Rc::clone(&receipt)));
        if !accepted {
            receipt.finish(if receipt.current_for(self) {
                NativeMarkdownTableCellOutcome::Failed { committed: false }
            } else {
                NativeMarkdownTableCellOutcome::Cancelled { committed: false }
            });
        }
        accepted
    }

    /// Next仍进入原导航body；原目标及预声明代次不由后继当前状态重新采样。
    pub(in crate::native_surface) fn probe_navigate_active_table_cell(
        &self,
        direction: NativeMarkdownTableNavigation,
        receipt: Rc<NativeMarkdownTableCellReceipt>,
    ) {
        if !self.navigate_active_table_cell_observed(direction, Some(Rc::clone(&receipt))) {
            receipt.finish(if receipt.current_for(self) {
                NativeMarkdownTableCellOutcome::Failed { committed: false }
            } else {
                NativeMarkdownTableCellOutcome::Cancelled { committed: false }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 三种真实终点只接受首次值；失败或取消不能被后续清槽误升级为激活。
    fn first_terminal_is_immutable() {
        for first in [
            NativeMarkdownTableCellOutcome::Activated,
            NativeMarkdownTableCellOutcome::Cancelled { committed: false },
            NativeMarkdownTableCellOutcome::Failed { committed: true },
        ] {
            let slot = Cell::new(None);
            assert!(record_terminal(&slot, first));
            assert!(!record_terminal(
                &slot,
                NativeMarkdownTableCellOutcome::Activated
            ));
            assert!(!record_terminal(
                &slot,
                NativeMarkdownTableCellOutcome::Cancelled { committed: true }
            ));
            assert_eq!(slot.get(), Some(first));
        }
    }
}
