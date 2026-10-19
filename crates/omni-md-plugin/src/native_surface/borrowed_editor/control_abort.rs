//! abortEditing没有正常didEnd通知时，仍只结束abort之前捕获的原客户历史。

use super::*;
use std::rc::Weak as RcWeak;

/// 不保活field、editor或manager；同control的后继客户也不能复用这张原Rc身份票。
pub(in crate::native_surface) struct ControlAbort {
    boundary: RcWeak<ClientHistoryBoundary<FieldHistoryClient>>,
    client: RcWeak<FieldHistoryClient>,
}

impl<T> ClientHistoryBoundary<T> {
    /// Rc弱身份保留控制块而不保留客户；Native观察重入之后仍只签原active。
    fn capture_abort_ticket(&self, same: impl FnOnce(&T) -> bool) -> Option<RcWeak<T>> {
        if self.ending.get() {
            return None;
        }
        let original = self.active.borrow().clone()?;
        let same = same(&original); // 此处已无槽借用；真实客户的Weak getter允许同步重入。
        let still_original = self
            .active
            .borrow()
            .as_ref()
            .is_some_and(|current| Rc::ptr_eq(current, &original));
        (same && still_original && !self.ending.get()).then(|| Rc::downgrade(&original))
    }

    fn abort_ticket_current(&self, ticket: &RcWeak<T>) -> bool {
        self.active
            .borrow()
            .as_ref()
            .is_some_and(|current| std::ptr::eq(Rc::as_ptr(current), ticket.as_ptr()))
    }
}

impl NativeMarkdownBorrowedEditorSnapshot {
    /// 在实际abort之前捕获原客户；没有已观察的自有历史时，不临时调用undoManager造一个。
    pub(in crate::native_surface) fn capture_control_abort(
        &self,
        control: &NSTextField,
    ) -> Option<ControlAbort> {
        let client = self.1.capture_abort_ticket(|client| {
            client
                .field
                .load()
                .is_some_and(|field| std::ptr::eq(&*field, control))
        })?;
        Some(ControlAbort {
            boundary: Rc::downgrade(&self.1),
            client,
        })
    }

    /// 只在实际abort成功后消费；当前control必须已脱离editor，同票客户仍须通过原owned检查。
    pub(in crate::native_surface) fn finished_aborted_control(
        &self,
        ticket: ControlAbort,
        control: &NSTextField,
        undo: &NativeMarkdownSessionUndo,
        ledger: &NativeMarkdownOwnedLedger,
    ) -> Result<(), String> {
        let Some(boundary) = ticket.boundary.upgrade() else {
            return Ok(()); // 原边界已结束，旧票不沿当前窗口寻找另一份历史。
        };
        if !Rc::ptr_eq(&boundary, &self.1) {
            return Err("取消票不属于原输入历史边界".into());
        }
        if !self.1.abort_ticket_current(&ticket.client) {
            return Ok(()); // 同步didEnd已经消费原客户，不能再次清理后来复用的manager。
        }
        let attached = control.currentEditor().is_some();
        if !self.1.abort_ticket_current(&ticket.client) {
            return Ok(()); // getter重入产生后继客户时，不以它仍活动为原abort失败，也不清后继历史。
        }
        if attached {
            let error = "取消输入尚未结束原生编辑".to_owned();
            self.0.borrow_mut().failure.get_or_insert(error.clone());
            return Err(error); // 未成功辞任不能清掉仍活动的Undo，也不能丢弃这次原边界失败。
        }
        self.finish_control_history(
            |client| {
                // finish先取得原结束排他权；迟到旧票对同field新客户也只是不操作，不清其历史。
                std::ptr::eq(std::ptr::from_ref(client), ticket.client.as_ptr())
                    && client
                        .field
                        .load()
                        .is_some_and(|field| std::ptr::eq(&*field, control))
            },
            undo,
            ledger,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_abort_ticket_cannot_end_an_identical_successor_client() {
        let boundary = ClientHistoryBoundary::<usize>::default();
        boundary.observe(|value| *value == 7, || Ok(7)).unwrap();
        let ticket = boundary.capture_abort_ticket(|value| *value == 7).unwrap();
        assert!(boundary.abort_ticket_current(&ticket));
        boundary.finish(|value| *value == 7, |_| Ok(())).unwrap();
        boundary.observe(|value| *value == 7, || Ok(7)).unwrap();
        assert!(!boundary.abort_ticket_current(&ticket));
        assert_eq!(**boundary.active.borrow().as_ref().unwrap(), 7);
    }

    #[test]
    fn native_observation_reentry_cannot_capture_the_replacement_client() {
        let boundary = ClientHistoryBoundary::<usize>::default();
        boundary.observe(|value| *value == 7, || Ok(7)).unwrap();
        let ticket = boundary.capture_abort_ticket(|_| {
            boundary.finish(|value| *value == 7, |_| Ok(())).unwrap();
            boundary.observe(|value| *value == 7, || Ok(7)).unwrap();
            true // 值仍一样也不能让已结束的原Rc重新获得清理权限。
        });
        assert!(ticket.is_none());
        assert_eq!(**boundary.active.borrow().as_ref().unwrap(), 7);
    }
}
