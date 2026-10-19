//! 工作区结果的有限恢复信号；这里只保存纯弱邮箱，绝不保存正文、原生对象或第二worker。

use crate::cancelable_mailbox::CancelableMailbox;
use omni_markdown_core::{
    MarkdownSearchPage, MarkdownWorkspaceError, MarkdownWorkspacePage, MarkdownWorkspacePager,
};
use std::sync::Weak;

pub(crate) type DirectoryResult =
    Result<(MarkdownWorkspacePager, MarkdownWorkspacePage), MarkdownWorkspaceError>;
pub(crate) type SearchResult = Result<MarkdownSearchPage, MarkdownWorkspaceError>;

/// 交付原权限固定在worker创建时；主线程恢复不能领取后来查询的新代次。
pub(crate) struct WorkspaceDelivery<T> {
    pub(crate) owner: usize,
    pub(crate) generation: u64,
    pub(crate) intent: u64,
    pub(crate) mailbox: Weak<CancelableMailbox<T>>,
}

impl<T> Clone for WorkspaceDelivery<T> {
    fn clone(&self) -> Self {
        // std Weak只拷贝Rust控制块元数据，不retain/load Objective-C对象或业务值。
        Self {
            owner: self.owner,
            generation: self.generation,
            intent: self.intent,
            mailbox: Weak::clone(&self.mailbox),
        }
    }
}

/// 目录和搜索复用现有单worker，不能在两个独立槽里各自排一个当前结果。
#[derive(Clone)]
pub(crate) enum WorkspaceReadySignal {
    Directory(WorkspaceDelivery<DirectoryResult>),
    Search(WorkspaceDelivery<SearchResult>),
}

impl WorkspaceReadySignal {
    pub(crate) fn owner(&self) -> usize {
        match self {
            Self::Directory(value) => value.owner,
            Self::Search(value) => value.owner,
        }
    }

    /// 仅表示仍有未取消payload；实际授权仍须精确Arc、owner、模式和两种代次联合核验。
    pub(crate) fn has_pending(&self) -> bool {
        match self {
            Self::Directory(value) => value
                .mailbox
                .upgrade()
                .is_some_and(|mailbox| mailbox.has_pending()),
            Self::Search(value) => value
                .mailbox
                .upgrade()
                .is_some_and(|mailbox| mailbox.has_pending()),
        }
    }

    pub(crate) fn cancel(&self) {
        match self {
            Self::Directory(value) => {
                if let Some(mailbox) = value.mailbox.upgrade() {
                    mailbox.cancel();
                }
            }
            Self::Search(value) => {
                if let Some(mailbox) = value.mailbox.upgrade() {
                    mailbox.cancel();
                }
            }
        }
    }

    fn same(&self, other: &Self) -> bool {
        fn same<T>(a: &WorkspaceDelivery<T>, b: &WorkspaceDelivery<T>) -> bool {
            a.owner == b.owner
                && a.generation == b.generation
                && a.intent == b.intent
                && Weak::ptr_eq(&a.mailbox, &b.mailbox)
        }
        match (self, other) {
            (Self::Directory(a), Self::Directory(b)) => same(a, b),
            (Self::Search(a), Self::Search(b)) => same(a, b),
            _ => false, // 类型不同不能复用同一次交付身份。
        }
    }
}

/// 单槽只移动weak信号；所有邮箱检查/取消和被移出值析构都由外层在RefCell借用之外完成。
#[derive(Default)]
pub(crate) struct WorkspaceReadySlot {
    pending: Option<WorkspaceReadySignal>,
}

impl WorkspaceReadySlot {
    pub(crate) fn snapshot(&self) -> Option<WorkspaceReadySignal> {
        self.pending.clone()
    }

    /// expected是锁外检查前的弱快照；检查期间若重入换槽，则拒绝而非信任旧has_pending。
    pub(crate) fn park(
        &mut self,
        incoming: WorkspaceReadySignal,
        expected: Option<&WorkspaceReadySignal>,
        old_has_pending: bool,
    ) -> Result<Option<WorkspaceReadySignal>, WorkspaceReadySignal> {
        if let Some(old) = self.pending.as_ref() {
            if old.same(&incoming) {
                return Ok(Some(incoming)); // 同一通知coalesce，仍保留唯一原信号。
            }
            if old_has_pending || expected.is_none_or(|snapshot| !old.same(snapshot)) {
                return Err(incoming); // 两个仍pending或检查快照过期，绝不静默覆盖。
            }
        }
        Ok(self.pending.replace(incoming))
    }

    pub(crate) fn take(&mut self) -> Option<WorkspaceReadySignal> {
        self.pending.take()
    }

    pub(crate) fn cancel_owner(&mut self, owner: usize) -> Option<WorkspaceReadySignal> {
        if self
            .pending
            .as_ref()
            .is_some_and(|signal| signal.owner() == owner)
        {
            self.pending.take()
        } else {
            None
        } // 外来取消不能拿走另一窗口的信号。
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::Arc;

    fn signal(
        owner: usize,
        mailbox: &Arc<CancelableMailbox<SearchResult>>,
    ) -> WorkspaceReadySignal {
        WorkspaceReadySignal::Search(WorkspaceDelivery {
            owner,
            generation: 4,
            intent: 8,
            mailbox: Arc::downgrade(mailbox),
        })
    }

    #[test]
    fn real_owner_restore_delivers_once_without_consuming_while_borrowed() {
        let owner = RefCell::new(Some(7));
        let busy = Cell::new(None);
        let ready = RefCell::new(WorkspaceReadySlot::default());
        let dispatched = RefCell::new(Vec::new());
        let mailbox = Arc::new(CancelableMailbox::new());
        mailbox.publish(Err(MarkdownWorkspaceError::SearchSuperseded));
        crate::foreground_owner::with_foreground_owner(
            &owner,
            &busy,
            |value| *value,
            |_| {
                assert_eq!(busy.get(), Some(7));
                assert!(mailbox.has_pending());
                assert!(
                    ready
                        .borrow_mut()
                        .park(signal(7, &mailbox), None, false)
                        .is_ok()
                );
                assert!(dispatched.borrow().is_empty());
                assert!(mailbox.has_pending()); // 存信号不偷走真正结果。
            },
            || {
                assert!(busy.get().is_none());
                assert_eq!(*owner.borrow(), Some(7));
                if let Some(signal) = ready.borrow_mut().take() {
                    dispatched.borrow_mut().push(signal);
                }
            },
        )
        .unwrap();
        assert_eq!(dispatched.borrow().len(), 1);
        assert!(ready.borrow_mut().take().is_none());
        assert!(mailbox.take().is_some());
        assert!(!mailbox.has_pending());
    }

    #[test]
    fn live_other_signal_is_rejected_and_duplicate_coalesces() {
        let a = Arc::new(CancelableMailbox::new());
        let b = Arc::new(CancelableMailbox::new());
        a.publish(Err(MarkdownWorkspaceError::SearchSuperseded));
        b.publish(Err(MarkdownWorkspaceError::SearchSuperseded));
        let mut slot = WorkspaceReadySlot::default();
        assert!(slot.park(signal(1, &a), None, false).is_ok());
        let original = slot.snapshot().unwrap();
        assert!(slot.park(signal(1, &a), Some(&original), true).is_ok());
        assert!(slot.park(signal(1, &b), Some(&original), true).is_err());
        assert!(slot.snapshot().unwrap().same(&original));
        assert!(a.has_pending() && b.has_pending());
    }

    #[test]
    fn canceled_or_consumed_signal_cannot_block_a_new_mailbox() {
        let a = Arc::new(CancelableMailbox::new());
        let b = Arc::new(CancelableMailbox::new());
        a.publish(Err(MarkdownWorkspaceError::SearchSuperseded));
        let mut slot = WorkspaceReadySlot::default();
        slot.park(signal(2, &a), None, false).ok();
        let old = slot.snapshot();
        a.cancel();
        assert!(!old.as_ref().unwrap().has_pending());
        b.publish(Err(MarkdownWorkspaceError::SearchSuperseded));
        assert!(slot.park(signal(2, &b), old.as_ref(), false).is_ok());
        assert!(slot.cancel_owner(3).is_none());
        assert!(slot.cancel_owner(2).is_some());
        assert!(slot.take().is_none());
        assert!(!signal(2, &a).has_pending());
    }

    #[test]
    fn stale_snapshot_never_authorizes_overwriting_a_new_signal() {
        let a = Arc::new(CancelableMailbox::new());
        let b = Arc::new(CancelableMailbox::new());
        let c = Arc::new(CancelableMailbox::new());
        let mut slot = WorkspaceReadySlot::default();
        slot.park(signal(3, &a), None, false).ok();
        let stale = slot.snapshot();
        slot.cancel_owner(3);
        slot.park(signal(3, &b), None, false).ok();
        assert!(slot.park(signal(3, &c), stale.as_ref(), false).is_err());
        assert!(slot.snapshot().unwrap().same(&signal(3, &b)));
    }
    #[test]
    fn cancel_before_restore_and_dropped_owner_leave_no_replayable_payload() {
        let owner = RefCell::new(Some(9));
        let busy = Cell::new(None);
        let ready = RefCell::new(WorkspaceReadySlot::default());
        let dispatched = Cell::new(0);
        let mailbox = Arc::new(CancelableMailbox::new());
        mailbox.publish(Err(MarkdownWorkspaceError::SearchSuperseded));
        crate::foreground_owner::with_foreground_owner(
            &owner,
            &busy,
            |value| *value,
            |_| {
                assert!(
                    ready
                        .borrow_mut()
                        .park(signal(9, &mailbox), None, false)
                        .is_ok()
                );
                mailbox.cancel();
            },
            || {
                // 恢复事件只唤醒仍pending的信号，取消后不得排出一个空结果任务。
                let signal = ready.borrow_mut().take();
                if signal.is_some_and(|signal| signal.has_pending()) {
                    dispatched.set(dispatched.get() + 1);
                }
            },
        )
        .unwrap();
        assert_eq!(dispatched.get(), 0);
        assert!(ready.borrow_mut().take().is_none());
        let weak_only = signal(9, &mailbox);
        drop(mailbox);
        assert!(!weak_only.has_pending()); // 纯信号没有延长邮箱或正文/FD载荷生命。
    }
}
