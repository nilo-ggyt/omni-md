//! 主线程缓存的唯一 owner 借出；状态借用不跨 native factory、retain 或 drop。

use std::cell::{Cell, RefCell};

/// 空 payload 不分配容量或取得原生资源；仅供失效后恢复可用的空槽。
pub(super) trait CachePayload: Sized {
    fn empty() -> Self;

    /// Ready 状态在局部 owner 内失效；具体 scratch 可清 key 而保留活动会话的有限容量。
    fn invalidate(&mut self) {
        // 默认丢弃资源；调用者已经结束 slot 借用，原生析构允许重入。
        *self = Self::empty();
    }
}

/// Ready 拥有唯一 payload，Loaned 由同步栈独占，Closed 永久拒绝恢复。
pub(super) struct CacheOwner<T: CachePayload> {
    payload: RefCell<Option<T>>,
    loaned: Cell<bool>,
    closed: Cell<bool>,
    epoch: Cell<u64>,
}

/// Busy 只改变本次计算方式；不能毒化外层 owner 或发布第二个缓存。
pub(super) enum CacheAdmission<'a, T: CachePayload> {
    Ready(CacheLease<'a, T>),
    Loaned,
    Closed,
}

impl<T: CachePayload> CacheOwner<T> {
    /// 接收已有唯一 owner；不复制其原生 claim 或业务容量。
    pub(super) fn new(payload: T) -> Self {
        Self {
            payload: RefCell::new(Some(payload)),
            loaned: Cell::new(false),
            closed: Cell::new(false),
            epoch: Cell::new(0),
        }
    }

    /// 移出 payload 后才允许调用方执行任何可能重入的原生操作。
    pub(super) fn begin(&self) -> CacheAdmission<'_, T> {
        if self.closed.get() {
            // 已 accepted release 的空壳不能重建缓存或获得新 loan。
            return CacheAdmission::Closed;
        }
        if self.loaned.get() {
            // 外层尚在 native 栈，内层只能使用无缓存的局部计算。
            return CacheAdmission::Loaned;
        }
        let payload =
            { self.payload.borrow_mut().take() }.expect("ready cache owns exactly one payload");
        self.loaned.set(true);
        CacheAdmission::Ready(CacheLease {
            owner: self,
            payload: Some(payload),
            epoch: self.epoch.get(),
            restore: true,
        })
    }

    /// 即使 owner 正借出也推进 epoch，外层旧 payload 归还时必须被丢弃。
    pub(super) fn invalidate(&self) {
        self.invalidate_loans();
        if let CacheAdmission::Ready(mut lease) = self.begin() {
            // Ready 值可在本地清 key 并复用容量；native 析构重入仍看到 Loaned。
            lease.payload_mut().invalidate();
        }
    }

    /// 内容仍可复用但语义请求已变；Ready payload 保留，Loaned 的旧 owner 不可复活。
    pub(super) fn invalidate_loans(&self) {
        self.advance_epoch();
    }

    /// 请求只捕获标量 epoch，不取得 payload；归还仍由原 lease 独占完成。
    pub(super) fn epoch(&self) -> u64 {
        self.epoch.get()
    }

    /// 封闭在先；若仍有同步 loan，由其归还栈完成实际销毁，不能提前宣称为空。
    pub(super) fn close(&self) {
        self.closed.set(true);
        self.advance_epoch();
        if !self.loaned.get() {
            // 空槽先移出最后 owner；没有 RefMut 跨越原生字段析构。
            self.loaned.set(true);
            let previous = { self.payload.borrow_mut().take() };
            drop(previous);
            self.loaned.set(false);
        }
    }

    /// Closed 且所有 loan 都已实际 drop，才代表 payload、key 与容量全部不存在。
    pub(super) fn is_released(&self) -> bool {
        self.closed.get() && !self.loaned.get() && self.payload.borrow().is_none()
    }

    fn advance_epoch(&self) {
        // epoch 只由本槽递增；耗尽意味着内部生命周期不变量已经无法继续表达。
        self.epoch.set(
            self.epoch
                .get()
                .checked_add(1)
                .expect("cache epoch overflow"),
        );
    }
}

/// 仅同步持有具体 payload；析构结束前保持 Loaned，避免 native drop 产生假释放。
pub(super) struct CacheLease<'a, T: CachePayload> {
    owner: &'a CacheOwner<T>,
    payload: Option<T>,
    epoch: u64,
    restore: bool,
}

impl<T: CachePayload> CacheLease<'_, T> {
    /// payload 位于当前栈而非 RefCell；重入者只能见到 Loaned，不能取得同一可变引用。
    pub(super) fn payload_mut(&mut self) -> &mut T {
        self.payload.as_mut().expect("live cache lease payload")
    }

    /// 调用方的文档身份/代次/封闭检查失败时丢弃，不能将旧请求写回活槽。
    pub(super) fn discard(&mut self) {
        self.restore = false;
    }
}

impl<T: CachePayload> Drop for CacheLease<'_, T> {
    fn drop(&mut self) {
        let payload = self.payload.take();
        if self.restore && !self.owner.closed.get() && self.epoch == self.owner.epoch.get() {
            // 只移动唯一 owner，不调用 native retain；原槽在本 loan 内始终为空。
            let previous = self.owner.payload.replace(payload);
            drop(previous);
        } else {
            // 先析构旧值；它可能再次 invalidate/close，故之后必须重新检查 Closed。
            drop(payload);
            if !self.owner.closed.get() {
                // 空值不分配也不调用 native；失效后仍可由后续真实 query 按需填充。
                let empty = T::empty();
                let previous = self.owner.payload.replace(Some(empty));
                drop(previous);
            }
        }
        self.owner.loaned.set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    struct Payload {
        bytes: Vec<u8>,
        on_drop: Option<Box<dyn FnOnce()>>,
    }

    impl CachePayload for Payload {
        fn empty() -> Self {
            Self {
                bytes: Vec::new(),
                on_drop: None,
            }
        }
    }

    impl Drop for Payload {
        fn drop(&mut self) {
            if let Some(callback) = self.on_drop.take() {
                // 模拟原生字段最后 release 的同步回调，而非只检查 Rust 析构次数。
                callback();
            }
        }
    }

    #[test]
    fn factory_and_retain_reentry_observe_loan_without_any_state_borrow() {
        let owner = CacheOwner::new(Payload::empty());
        let CacheAdmission::Ready(mut lease) = owner.begin() else {
            panic!("ready")
        };
        for _stage in ["factory", "retain"] {
            // 真正模拟 callback 在 native 边界重新查询同一槽，不能借出第二 owner。
            assert!(owner.payload.try_borrow_mut().is_ok());
            assert!(matches!(owner.begin(), CacheAdmission::Loaned));
        }
        lease.payload_mut().bytes.extend_from_slice(b"history");
        drop(lease);
        let CacheAdmission::Ready(mut next) = owner.begin() else {
            panic!("restored")
        };
        assert_eq!(next.payload_mut().bytes, b"history");
    }

    #[test]
    fn invalidation_during_loan_advances_epoch_and_cannot_restore_old_capacity() {
        let owner = CacheOwner::new(Payload {
            bytes: vec![1; 8192],
            on_drop: None,
        });
        let CacheAdmission::Ready(lease) = owner.begin() else {
            panic!("ready")
        };
        owner.invalidate();
        assert!(matches!(owner.begin(), CacheAdmission::Loaned));
        drop(lease);
        let CacheAdmission::Ready(mut next) = owner.begin() else {
            panic!("empty ready")
        };
        assert_eq!(next.payload_mut().bytes.capacity(), 0);
    }

    #[test]
    fn accepted_close_waits_for_real_drop_and_drop_reentry_cannot_reopen() {
        let owner = Rc::new(CacheOwner::new(Payload::empty()));
        let weak = Rc::downgrade(&owner);
        let CacheAdmission::Ready(mut lease) = owner.begin() else {
            panic!("ready")
        };
        lease.payload_mut().bytes = vec![7; 8192];
        lease.payload_mut().on_drop = Some(Box::new(move || {
            // 旧 native payload 正在析构时，Closed 仍不能被误报为已释放。
            let owner = weak.upgrade().expect("owner");
            assert!(owner.payload.try_borrow_mut().is_ok());
            assert!(!owner.is_released());
            assert!(matches!(owner.begin(), CacheAdmission::Closed));
            owner.invalidate();
        }));
        owner.close();
        assert!(!owner.is_released());
        drop(lease);
        assert!(owner.is_released());
    }

    #[test]
    fn ready_invalidation_drop_can_close_without_republishing_empty_owner() {
        let owner = Rc::new(CacheOwner::new(Payload::empty()));
        let weak = Rc::downgrade(&owner);
        let CacheAdmission::Ready(mut lease) = owner.begin() else {
            panic!("ready")
        };
        lease.payload_mut().on_drop = Some(Box::new(move || {
            // Ready 失效会真实销毁 native 值；该 drop 重入的 close 必须胜过之后归还。
            let owner = weak.upgrade().expect("owner");
            assert!(owner.payload.try_borrow_mut().is_ok());
            assert!(matches!(owner.begin(), CacheAdmission::Loaned));
            owner.close();
            assert!(!owner.is_released());
        }));
        drop(lease);
        owner.invalidate();
        assert!(owner.is_released());
        assert!(matches!(owner.begin(), CacheAdmission::Closed));
    }
}
