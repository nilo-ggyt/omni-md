//! 单个前台操作拥有的可取消结果槽；排队通知只需保存该邮箱的弱引用。

use std::sync::{Mutex, MutexGuard};

/// 关闭状态与结果在同一锁内更新，禁止取消后重新发布业务载荷。
struct MailboxState<T> {
    closed: bool,
    pending: Option<T>,
}

/// 每个操作代次独享一个邮箱；取消同步释放结果，不依赖主队列通知执行。
pub(crate) struct CancelableMailbox<T> {
    state: Mutex<MailboxState<T>>,
}

impl<T> CancelableMailbox<T> {
    /// 初始没有结果且允许发布；外层所有者决定 Arc/Weak 的生命周期。
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(MailboxState {
                closed: false,
                pending: None,
            }),
        }
    }

    /// 发布当前代次的最新结果；被替换或拒绝的值在解锁后销毁。
    pub(crate) fn publish(&self, value: T) -> bool {
        // 保留尚未转移的入参，使失败分支也能在锁外运行任意析构逻辑。
        let mut incoming = Some(value);
        let (accepted, discarded) = {
            let (mut state, poisoned) = self.lock_state();
            if poisoned {
                // Unwind 期间的状态不再作为有效结果使用；永久关闭并取走旧载荷。
                state.closed = true;
                (false, state.pending.take())
            } else if state.closed {
                // 已取消的代次不能因为迟到的 worker 而再次拥有结果。
                (false, None)
            } else {
                // 同代次新结果替换旧结果，锁内仅移动所有权而不执行 T::drop。
                (true, std::mem::replace(&mut state.pending, incoming.take()))
            }
        };
        // T 的析构可能重入 cancel/publish；此处已无 MutexGuard。
        drop(discarded);
        drop(incoming);
        accepted
    }

    /// 取出一次已发布结果；关闭或 poison 状态不能向 UI 返回旧载荷。
    pub(crate) fn take(&self) -> Option<T> {
        let (result, discarded) = {
            let (mut state, poisoned) = self.lock_state();
            if poisoned {
                // 不把异常中断的共享状态恢复为可用结果；同步排空旧载荷。
                state.closed = true;
                (None, state.pending.take())
            } else if state.closed {
                // cancel 已撤销消费权限；防御性取走任何残余值但不交给调用者。
                (None, state.pending.take())
            } else {
                // 只转移现有值，不创建结果、查询或后续调度。
                (state.pending.take(), None)
            }
        };
        // 拒绝的结果必须在 guard 退出后析构，允许资源清理安全重入。
        drop(discarded);
        result
    }

    /// 只观察待交付事实，不消费值；取消/poison后迟到通知不能占用恢复槽。
    pub(crate) fn has_pending(&self) -> bool {
        let (pending, discarded) = {
            let (mut state, poisoned) = self.lock_state();
            if poisoned {
                // 不恢复异常前的业务权限，仍按既有失败边界在解锁后释放载荷。
                state.closed = true;
                (false, state.pending.take())
            } else {
                (!state.closed && state.pending.is_some(), None)
            }
        };
        drop(discarded);
        pending
    }

    /// 永久撤销发布与消费权限，并在返回前释放邮箱拥有的结果。
    pub(crate) fn cancel(&self) {
        let discarded = {
            let (mut state, _) = self.lock_state();
            // 即使此前发生 poison，也必须完成幂等关闭与资源所有权移出。
            state.closed = true;
            state.pending.take()
        };
        // 关闭线性化点已提交；载荷析构可以再次进入本邮箱而不会死锁。
        drop(discarded);
    }

    /// 保留 poison 事实供调用者拒绝结果；仅取回 guard 以完成资源清理。
    fn lock_state(&self) -> (MutexGuard<'_, MailboxState<T>>, bool) {
        match self.state.lock() {
            // 正常路径允许当前操作依照 closed 状态继续。
            Ok(guard) => (guard, false),
            // 不 clear_poison；所有未来访问继续拒绝业务使用并执行清理。
            Err(error) => (error.into_inner(), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CancelableMailbox;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    /// 析构计数和一次性回调分别证明实际释放以及锁外重入。
    struct DropWitness {
        drops: Arc<AtomicUsize>,
        on_drop: Option<Box<dyn FnOnce() + Send>>,
    }

    impl DropWitness {
        /// 每个值贡献一个独立析构计数，不共享业务载荷。
        fn new(drops: &Arc<AtomicUsize>) -> Self {
            Self {
                drops: Arc::clone(drops),
                on_drop: None,
            }
        }
    }

    impl Drop for DropWitness {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
            if let Some(callback) = self.on_drop.take() {
                // 仅执行一次测试回调，模拟载荷释放时进入其他取消路径。
                callback();
            }
        }
    }

    #[test]
    fn cancel_releases_queued_payload_before_weak_notification_runs() {
        // 通知仍排队时关闭会话，不能由通知延长结果或邮箱生命周期。
        let drops = Arc::new(AtomicUsize::new(0));
        let mailbox = Arc::new(CancelableMailbox::new());
        let queued_notification = Arc::downgrade(&mailbox);
        assert!(mailbox.publish(DropWitness::new(&drops)));
        mailbox.cancel();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(mailbox.take().is_none());
        drop(mailbox);
        assert!(queued_notification.upgrade().is_none());
    }

    #[test]
    fn replaced_payload_can_reenter_cancel_without_holding_mutex() {
        // 旧值析构立即取消新值；若 publish 在锁内 drop 会发生死锁。
        let drops = Arc::new(AtomicUsize::new(0));
        let mailbox = Arc::new(CancelableMailbox::new());
        let weak = Arc::downgrade(&mailbox);
        let mut first = DropWitness::new(&drops);
        first.on_drop = Some(Box::new(move || {
            if let Some(owner) = weak.upgrade() {
                // 模拟仍存活的同代次所有者在清理回调中撤销操作。
                owner.cancel();
            }
        }));
        assert!(mailbox.publish(first));
        assert!(mailbox.publish(DropWitness::new(&drops)));
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert!(mailbox.take().is_none());
    }

    #[test]
    fn cancelled_worker_publish_releases_value_and_cannot_revive_slot() {
        // 屏障明确建立 cancel 先于 worker 发布的竞争顺序。
        let drops = Arc::new(AtomicUsize::new(0));
        let mailbox = Arc::new(CancelableMailbox::new());
        let barrier = Arc::new(Barrier::new(2));
        let worker_mailbox = Arc::clone(&mailbox);
        let worker_barrier = Arc::clone(&barrier);
        let worker_drops = Arc::clone(&drops);
        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            worker_mailbox.publish(DropWitness::new(&worker_drops))
        });
        mailbox.cancel();
        barrier.wait();
        assert!(!worker.join().expect("worker must finish"));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(mailbox.take().is_none());
    }

    #[test]
    fn consumed_payload_belongs_to_consumer_and_cancel_is_idempotent() {
        // take 转移所有权，后续取消不能重复销毁调用者持有的值。
        let drops = Arc::new(AtomicUsize::new(0));
        let mailbox = CancelableMailbox::new();
        assert!(mailbox.publish(DropWitness::new(&drops)));
        let consumed = mailbox.take().expect("published value must be available");
        mailbox.cancel();
        mailbox.cancel();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(consumed);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(!mailbox.publish(DropWitness::new(&drops)));
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn poisoned_state_rejects_publication_and_drains_both_values() {
        // 异常只污染锁内状态，不能使待处理业务值永久滞留。
        let drops = Arc::new(AtomicUsize::new(0));
        let mailbox = CancelableMailbox::new();
        assert!(mailbox.publish(DropWitness::new(&drops)));
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mailbox.state.lock().expect("initial mutex is healthy");
            panic!("controlled mailbox poison");
        }));
        assert!(unwind.is_err());
        assert!(!mailbox.publish(DropWitness::new(&drops)));
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert!(mailbox.take().is_none());
        mailbox.cancel();
    }

    #[test]
    fn poisoned_take_drops_payload_instead_of_delivering_stale_result() {
        // UI 消费路径同样不能因 poisoned guard 被恢复而使用异常前结果。
        let drops = Arc::new(AtomicUsize::new(0));
        let mailbox = CancelableMailbox::new();
        assert!(mailbox.publish(DropWitness::new(&drops)));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mailbox.state.lock().expect("initial mutex is healthy");
            panic!("controlled mailbox poison");
        }));
        assert!(mailbox.take().is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn pending_observation_never_consumes_and_rejects_cancel_or_poison() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mailbox = Arc::new(CancelableMailbox::new());
        let mut value = DropWitness::new(&drops);
        let weak = Arc::downgrade(&mailbox);
        value.on_drop = Some(Box::new(move || {
            // has_pending处理poison也必须先解锁，否则析构的cancel会死锁。
            if let Some(mailbox) = weak.upgrade() {
                mailbox.cancel();
            }
        }));
        assert!(!mailbox.has_pending());
        assert!(mailbox.publish(value));
        assert!(mailbox.has_pending());
        assert!(mailbox.has_pending());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mailbox.state.lock().unwrap();
            panic!("controlled ready observation poison");
        }));
        assert!(!mailbox.has_pending());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(mailbox.take().is_none());
        assert!(!mailbox.has_pending());
    }
}
