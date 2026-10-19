//! 不可变值家族只观察真实payload和值析构，不持正文、Native对象或活动会话。

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
/// 每个claim属于一个实际资源字段或具体值，不能用父对象消失替代所有派生值析构。
pub(super) enum FrozenValueClaimKind {
    SourcePayload,
    AnnotationPayload,
    StringValue,
    AnnotationValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
/// 三个既有关闭角色各有一个独立观察槽，不复制资源清单或新增关闭权限。
pub(super) enum FrozenValueObserverRole {
    Editor,
    FullGraph,
    Resources,
}

/// 实现只能持原子释放信号或弱元数据，不能捕获正文、Native强对象或业务闭包。
pub(super) trait FrozenValueReleaseObserver: Send + Sync {
    /// 在family锁外移交同一真实析构事实；实现还须自行尊重其cohort的取消状态。
    fn released(&self);

    /// 只读取取消标量，不回调family、不获取其他锁、不执行Native或业务操作。
    fn is_cancelled(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 返回错误不制造claim；已经丢失精确性或父类型错误会永久禁止完成收据。
pub(super) enum FrozenValueFamilyError {
    SourcesSealed,
    InvalidParentKind,
    MissingLiveParent,
    CounterOverflow,
    ObserverAlreadyRegistered,
    ObserverAlreadyConsumed,
    ObserverCancelled,
    ObserverPanicked,
    Failed,
}

impl fmt::Display for FrozenValueFamilyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::SourcesSealed => "不可变值家族已停止创建新源", // 旧值的合法派生仍然有效。
            Self::InvalidParentKind => "不可变值派生的父claim类型无效", // 不允许凭任意活动计数造值。
            Self::MissingLiveParent => "不可变值派生缺少仍存活的父claim", // 计数矛盾不能当作已释放。
            Self::CounterOverflow => "不可变值生命周期计数溢出", // 无法精确表示后永久失败。
            Self::ObserverAlreadyRegistered => "该释放角色已有未取消的观察者", // 不覆盖原关闭权限。
            Self::ObserverAlreadyConsumed => "该释放角色的观察事实已被消费", // 死Weak不等于明确取消。
            Self::ObserverCancelled => "不能登记已经取消的释放观察者", // 不建立没有权限的新槽。
            Self::ObserverPanicked => "不可变值释放观察者未遵守无panic合同", // Drop仍须正常归还资源。
            Self::Failed => "不可变值家族已失去完整释放证明", // 后续零计数也不能恢复成功。
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for FrozenValueFamilyError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 只含固定标量；完成仅表示payload和值析构，不声称后台super dealloc已返回。
pub(super) struct FrozenValueFamilySnapshot {
    pub(super) sealed_sources: bool,
    pub(super) payload_value_destructors_complete: bool,
    pub(super) sticky_failure: bool,
    pub(super) active: [usize; 4],
    pub(super) constructed: [usize; 4],
    pub(super) destructed: [usize; 4],
}

#[derive(Default)]
enum ObserverSlot {
    #[default]
    Vacant,
    Pending(Arc<dyn FrozenValueReleaseObserver>),
    Dispatched(Weak<dyn FrozenValueReleaseObserver>),
}

#[derive(Default)]
struct FamilyState {
    sealed_sources: bool,
    completed: bool,
    active: [usize; 4],
    constructed: [usize; 4],
    destructed: [usize; 4],
    observers: [ObserverSlot; 3],
}

/// 一个editor的家族元数据可跨Foundation调用线程共享，但不因此授予Native类型Send/Sync。
pub(super) struct FrozenValueFamily {
    state: Mutex<FamilyState>,
    failed: AtomicBool,
}

#[must_use = "claim必须放在真实资源字段之后，并在资源实际析构后由Drop归还"]
/// 不可Clone且没有手动成功release；遗忘guard会保持未完成，不能伪造释放。
pub(super) struct FrozenValueClaim {
    family: Arc<FrozenValueFamily>,
    kind: FrozenValueClaimKind,
}

type Notifications = [Option<Arc<dyn FrozenValueReleaseObserver>>; 3];

impl FrozenValueFamily {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FamilyState::default()),
            failed: AtomicBool::new(false),
        })
    }

    /// 新源必须在活动editor停止发布前登记；调用方负责不向外泄漏裸快照克隆。
    pub(super) fn claim_source(
        self: &Arc<Self>,
    ) -> Result<FrozenValueClaim, FrozenValueFamilyError> {
        let mut state = self.lock_state();
        self.require_healthy()?;
        if state.sealed_sources {
            // seal不撤旧值权限，只拒绝没有既存源血缘的新入口。
            return Err(FrozenValueFamilyError::SourcesSealed);
        }
        self.register(&mut state, FrozenValueClaimKind::SourcePayload)?;
        Ok(FrozenValueClaim {
            family: Arc::clone(self),
            kind: FrozenValueClaimKind::SourcePayload,
        })
    }

    /// 只封闭新源入口；现有源、字符串及属性payload仍能正常派生真实值。
    pub(super) fn seal_sources(&self) {
        let notifications = {
            let mut state = self.lock_state();
            state.sealed_sources = true;
            self.take_completion(&mut state)
        };
        self.notify(notifications); // 先结束锁，观察者只能收到真实完成事实。
    }

    /// 每个角色只有一个授权槽；明确取消才可替换，不能把Weak已死猜作取消。
    pub(super) fn observe(
        &self,
        role: FrozenValueObserverRole,
        observer: Arc<dyn FrozenValueReleaseObserver>,
    ) -> Result<(), FrozenValueFamilyError> {
        let notification = {
            let mut state = self.lock_state();
            self.require_healthy()?;
            if self.cancelled(&observer)? {
                // 新观察者从开始就没有权限，不改变原角色槽。
                return Err(FrozenValueFamilyError::ObserverCancelled);
            }
            let index = role as usize;
            let previous = match &state.observers[index] {
                ObserverSlot::Vacant => None, // 首次角色登记不需要探测其他角色。
                ObserverSlot::Pending(previous) => Some(Arc::clone(previous)),
                ObserverSlot::Dispatched(previous) => {
                    // 已消费且Weak已死时仍拒绝覆盖；调用方不能伪造一个明确取消事件。
                    Some(
                        previous
                            .upgrade()
                            .ok_or(FrozenValueFamilyError::ObserverAlreadyConsumed)?,
                    )
                }
            };
            if let Some(previous) = previous
                && !self.cancelled(&previous)?
            {
                // 此trait方法合同仅允许原子标量读取，不能覆盖仍授权的原角色。
                return Err(FrozenValueFamilyError::ObserverAlreadyRegistered);
            }
            self.require_healthy()?;
            if state.completed {
                // 晚登记消费既有真实事实，不重置计数、不排队等待，也不保留强观察者。
                state.observers[index] = ObserverSlot::Dispatched(Arc::downgrade(&observer));
                Some(observer)
            } else {
                state.observers[index] = ObserverSlot::Pending(observer);
                None
            }
        };
        self.notify([notification, None, None]);
        self.require_healthy()
    }

    pub(super) fn snapshot(&self) -> FrozenValueFamilySnapshot {
        let state = self.lock_state();
        let failed = self.failed.load(Ordering::Acquire);
        FrozenValueFamilySnapshot {
            sealed_sources: state.sealed_sources,
            payload_value_destructors_complete: state.completed && !failed,
            sticky_failure: failed,
            active: state.active,
            constructed: state.constructed,
            destructed: state.destructed,
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, FamilyState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                // 保留真实计数以便后续Drop配平，但poison永远不能恢复为完成证明。
                self.failed.store(true, Ordering::Release);
                poisoned.into_inner()
            }
        }
    }

    fn require_healthy(&self) -> Result<(), FrozenValueFamilyError> {
        if self.failed.load(Ordering::Acquire) {
            return Err(FrozenValueFamilyError::Failed); // 已丢精确性时拒绝继续签发新claim。
        }
        Ok(())
    }

    fn register(
        &self,
        state: &mut FamilyState,
        kind: FrozenValueClaimKind,
    ) -> Result<(), FrozenValueFamilyError> {
        let index = kind as usize;
        let next = state.active[index]
            .checked_add(1)
            .zip(state.constructed[index].checked_add(1));
        let Some((active, constructed)) = next else {
            // 两项先共同验证，再一起提交；失败不留下没有guard的活动计数。
            self.failed.store(true, Ordering::Release);
            return Err(FrozenValueFamilyError::CounterOverflow);
        };
        state.active[index] = active;
        state.constructed[index] = constructed;
        Ok(())
    }

    fn derive(
        self: &Arc<Self>,
        parent: FrozenValueClaimKind,
        required: FrozenValueClaimKind,
        child: FrozenValueClaimKind,
    ) -> Result<FrozenValueClaim, FrozenValueFamilyError> {
        let mut state = self.lock_state();
        self.require_healthy()?;
        if parent != required {
            // 类型错误是工厂的所有权违约，不能以后补零掩盖。
            self.failed.store(true, Ordering::Release);
            return Err(FrozenValueFamilyError::InvalidParentKind);
        }
        if state.active[parent as usize] == 0 {
            // &Claim本应使此计数仍为正；矛盾只可能来自内部错误，永久拒绝完成。
            self.failed.store(true, Ordering::Release);
            return Err(FrozenValueFamilyError::MissingLiveParent);
        }
        self.register(&mut state, child)?;
        Ok(FrozenValueClaim {
            family: Arc::clone(self),
            kind: child,
        })
    }

    fn drop_claim(&self, kind: FrozenValueClaimKind) {
        let notifications = {
            let mut state = self.lock_state();
            let index = kind as usize;
            if let Some(active) = state.active[index].checked_sub(1) {
                state.active[index] = active;
            } else {
                // Drop不能panic或回绕；下溢保留永久失败，零也不再有释放权限。
                self.failed.store(true, Ordering::Release);
            }
            if let Some(destructed) = state.destructed[index].checked_add(1) {
                state.destructed[index] = destructed;
            } else {
                // 真实析构仍继续，无法表示累计总数时不再签完整收据。
                self.failed.store(true, Ordering::Release);
            }
            self.take_completion(&mut state)
        };
        self.notify(notifications); // 所有观察者通知均在真实载荷字段及family锁之外。
    }

    fn take_completion(&self, state: &mut FamilyState) -> Notifications {
        let mut notifications = [None, None, None];
        if state.completed
            || !state.sealed_sources
            || self.failed.load(Ordering::Acquire)
            || state.active != [0; 4]
        {
            // 未封闭、仍有任一真实claim、失效或已完成，都不能再次产生完成事实。
            return notifications;
        }
        if state.constructed != state.destructed {
            // 真正零活动量必须与全部实际构造/析构配平；损坏的零不能提前签完成。
            self.failed.store(true, Ordering::Release);
            return notifications;
        }
        state.completed = true;
        for (slot, notification) in state.observers.iter_mut().zip(&mut notifications) {
            if let ObserverSlot::Pending(observer) = slot {
                // 仅留下弱身份用于防重复；强观察者随本次通知移出锁，不保留业务资源。
                let observer = Arc::clone(observer);
                *slot = ObserverSlot::Dispatched(Arc::downgrade(&observer));
                *notification = Some(observer);
            }
        }
        notifications
    }

    fn cancelled(
        &self,
        observer: &Arc<dyn FrozenValueReleaseObserver>,
    ) -> Result<bool, FrozenValueFamilyError> {
        match catch_unwind(AssertUnwindSafe(|| observer.is_cancelled())) {
            Ok(cancelled) => Ok(cancelled),
            Err(_) => {
                // 观察者违约也不能让资源Drop再次panic；后续完成检查永久失败。
                self.failed.store(true, Ordering::Release);
                Err(FrozenValueFamilyError::ObserverPanicked)
            }
        }
    }

    fn notify(&self, notifications: Notifications) {
        for observer in notifications.into_iter().flatten() {
            match self.cancelled(&observer) {
                Ok(false) => {
                    if catch_unwind(AssertUnwindSafe(|| observer.released())).is_err() {
                        // 回调必须只桥原子claim；违约panic不得从析构栈逃逸。
                        self.failed.store(true, Ordering::Release);
                    }
                }
                Ok(true) => {} // 取消只撤此观察者，不改变家族计数或别的角色。
                Err(_) => {}   // cancelled已记录永久失败，不能伪造已通知成功。
            }
        }
    }
}

impl FrozenValueClaim {
    pub(super) fn claim_string_value(&self) -> Result<Self, FrozenValueFamilyError> {
        self.family.derive(
            self.kind,
            FrozenValueClaimKind::SourcePayload,
            FrozenValueClaimKind::StringValue,
        )
    }

    pub(super) fn claim_annotation_payload(&self) -> Result<Self, FrozenValueFamilyError> {
        self.family.derive(
            self.kind,
            FrozenValueClaimKind::StringValue,
            FrozenValueClaimKind::AnnotationPayload,
        )
    }

    pub(super) fn claim_annotation_value(&self) -> Result<Self, FrozenValueFamilyError> {
        self.family.derive(
            self.kind,
            FrozenValueClaimKind::AnnotationPayload,
            FrozenValueClaimKind::AnnotationValue,
        )
    }
}

impl Drop for FrozenValueClaim {
    fn drop(&mut self) {
        self.family.drop_claim(self.kind); // 唯一结算入口是实际guard析构，没有手动成功release。
    }
}

#[cfg(test)]
#[path = "frozen_value_family/tests.rs"]
mod tests;
