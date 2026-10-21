//! Observe immutable payload/value destruction for explicit frozen-value diagnostics and Rust
//! contract tests. The family retains no text, native object, or active session.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
/// Each claim belongs to an actual resource field or concrete value. Expiration of a parent
/// object cannot stand in for destruction of all derived values.
pub(super) enum FrozenValueClaimKind {
    SourcePayload,
    AnnotationPayload,
    StringValue,
    AnnotationValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
/// Each of the three existing close roles has one observation slot, without duplicating resource
/// inventories or creating additional close authority.
pub(super) enum FrozenValueObserverRole {
    Editor,
    FullGraph,
    Resources,
}

/// Implementations may retain only atomic release signals or weak metadata, without text,
/// strong native objects, or business closures.
pub(super) trait FrozenValueReleaseObserver: Send + Sync {
    /// Deliver the same actual destruction fact outside the family mutex; the implementation
    /// must still honor cancellation of its own cohort.
    fn released(&self);

    /// Read cancellation scalars only: no family reentry, other locks, Native, or business work.
    fn is_cancelled(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Errors create no claim. Lost accounting precision or an invalid parent kind permanently
/// prevents a successful completion receipt.
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
            Self::SourcesSealed => "不可变值家族已停止创建新源", // Valid derivation from existing values remains permitted.
            Self::InvalidParentKind => "不可变值派生的父claim类型无效", // Arbitrary active counts cannot authorize a new value.
            Self::MissingLiveParent => "不可变值派生缺少仍存活的父claim", // Contradictory accounting is not completed release.
            Self::CounterOverflow => "不可变值生命周期计数溢出", // Lost precision leaves permanent failure.
            Self::ObserverAlreadyRegistered => "该释放角色已有未取消的观察者", // Preserve the original close role's authority.
            Self::ObserverAlreadyConsumed => "该释放角色的观察事实已被消费", // An expired Weak is not explicit cancellation.
            Self::ObserverCancelled => "不能登记已经取消的释放观察者", // No new slot is granted to an unauthorized observer.
            Self::ObserverPanicked => "不可变值释放观察者未遵守无panic合同", // Actual resource Drop must still complete.
            Self::Failed => "不可变值家族已失去完整释放证明", // Later zero counts cannot restore successful evidence.
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for FrozenValueFamilyError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Fixed scalars only. Completion describes payload/value-field destruction, not return from a
/// background Objective-C superclass dealloc or release of a complete native graph.
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

/// One editor's family metadata may be shared across Foundation callback threads; that does not
/// grant Rust Send/Sync to any native value type.
pub(super) struct FrozenValueFamily {
    state: Mutex<FamilyState>,
    failed: AtomicBool,
}

#[must_use = "claim必须放在真实资源字段之后，并在资源实际析构后由Drop归还"]
/// Not Clone and no manual successful release. Forgetting the guard keeps completion pending,
/// rather than manufacturing evidence that its actual resource was destroyed.
pub(super) struct FrozenValueClaim {
    family: Arc<FrozenValueFamily>,
    kind: FrozenValueClaimKind,
}

type Notifications = [Option<Arc<dyn FrozenValueReleaseObserver>>; 3];

impl FrozenValueFamily {
    pub(super) fn new() -> Arc<Self> {
        // Allocate metadata only. Actual source admission and observer ownership remain separate
        // operations; an empty unsealed family has no completed-release authority.
        Arc::new(Self {
            state: Mutex::new(FamilyState::default()),
            failed: AtomicBool::new(false),
        })
    }

    /// Register a new Source before the active editor stops publication. Callers must not leak
    /// bare snapshot clones outside their corresponding actual payload claim.
    pub(super) fn claim_source(
        self: &Arc<Self>,
    ) -> Result<FrozenValueClaim, FrozenValueFamilyError> {
        let mut state = self.lock_state();
        self.require_healthy()?;
        if state.sealed_sources {
            // Sealing preserves existing-value derivation while refusing a new entry with no
            // still-live original Source lineage.
            return Err(FrozenValueFamilyError::SourcesSealed);
        }
        self.register(&mut state, FrozenValueClaimKind::SourcePayload)?;
        Ok(FrozenValueClaim {
            family: Arc::clone(self),
            kind: FrozenValueClaimKind::SourcePayload,
        })
    }

    /// Close new-source admission only. Existing Sources, strings, and annotation payloads may
    /// still derive actual values through their original live claims.
    pub(super) fn seal_sources(&self) {
        let notifications = {
            let mut state = self.lock_state();
            state.sealed_sources = true;
            self.take_completion(&mut state)
        };
        self.notify(notifications); // End the mutex lease first; observers receive only an actual completion fact.
    }

    /// Each role has one authorized slot. Only explicit cancellation permits replacement;
    /// an expired Weak cannot be guessed to mean cancellation.
    pub(super) fn observe(
        &self,
        role: FrozenValueObserverRole,
        observer: Arc<dyn FrozenValueReleaseObserver>,
    ) -> Result<(), FrozenValueFamilyError> {
        let notification = {
            let mut state = self.lock_state();
            self.require_healthy()?;
            if self.cancelled(&observer)? {
                // An already-cancelled observer has no admission authority and leaves the
                // original role slot unchanged.
                return Err(FrozenValueFamilyError::ObserverCancelled);
            }
            let index = role as usize;
            let previous = match &state.observers[index] {
                ObserverSlot::Vacant => None, // First registration for this role need not inspect another role.
                ObserverSlot::Pending(previous) => Some(Arc::clone(previous)),
                ObserverSlot::Dispatched(previous) => {
                    // A consumed slot remains nonreplaceable after Weak expiration; callers
                    // cannot manufacture an explicit cancellation event from that absence.
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
                // This trait method permits atomic scalar reads only. An original observer
                // that remains authorized must not be overwritten.
                return Err(FrozenValueFamilyError::ObserverAlreadyRegistered);
            }
            self.require_healthy()?;
            if state.completed {
                // Late registration consumes the existing actual fact, without resetting counts,
                // scheduling a wait, or retaining another strong observer in the family.
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
        // Copy metadata under the same mutex. Sticky failure suppresses successful completion
        // even if later actual drops bring every counter back to zero.
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
                // Preserve actual counters so later Drop can balance ownership, but mutex
                // poison permanently prevents successful completion evidence.
                self.failed.store(true, Ordering::Release);
                poisoned.into_inner()
            }
        }
    }

    fn require_healthy(&self) -> Result<(), FrozenValueFamilyError> {
        if self.failed.load(Ordering::Acquire) {
            return Err(FrozenValueFamilyError::Failed); // Once precision is lost, refuse all further claim admission.
        }
        Ok(())
    }

    fn register(
        &self,
        state: &mut FamilyState,
        kind: FrozenValueClaimKind,
    ) -> Result<(), FrozenValueFamilyError> {
        // Compute both increments before publication; no fallible operation may leave an active
        // count without the corresponding constructed count and returned guard.
        let index = kind as usize;
        let next = state.active[index]
            .checked_add(1)
            .zip(state.constructed[index].checked_add(1));
        let Some((active, constructed)) = next else {
            // Commit both checked values together. Failure leaves no guardless active count.
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
        // The actual parent guard selects the permitted lineage. New-source sealing does not
        // block this derivation, but invalid kind or missing live-parent accounting is permanent failure.
        let mut state = self.lock_state();
        self.require_healthy()?;
        if parent != required {
            // Wrong parent kind violates the factory's ownership contract; later zero counts
            // cannot conceal that earlier inconsistency.
            self.failed.store(true, Ordering::Release);
            return Err(FrozenValueFamilyError::InvalidParentKind);
        }
        if state.active[parent as usize] == 0 {
            // The borrowed parent claim must keep this count positive. Contradiction is an
            // internal failure and permanently prohibits successful completion.
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
        // Actual resource-field destruction reaches this sole settlement path. Continue balancing
        // metadata even after failure, then move any actual completion notifications outside the lock.
        let notifications = {
            let mut state = self.lock_state();
            let index = kind as usize;
            if let Some(active) = state.active[index].checked_sub(1) {
                state.active[index] = active;
            } else {
                // Drop must neither panic nor wrap. Underflow leaves sticky failure; a resulting
                // zero grants no successful release authority.
                self.failed.store(true, Ordering::Release);
            }
            if let Some(destructed) = state.destructed[index].checked_add(1) {
                state.destructed[index] = destructed;
            } else {
                // Actual destruction continues, but unrepresentable cumulative counts prevent
                // a complete successful receipt.
                self.failed.store(true, Ordering::Release);
            }
            self.take_completion(&mut state)
        };
        self.notify(notifications); // Notify after actual payload fields dropped and after releasing the family mutex.
    }

    fn take_completion(&self, state: &mut FamilyState) -> Notifications {
        let mut notifications = [None, None, None];
        if state.completed
            || !state.sealed_sources
            || self.failed.load(Ordering::Acquire)
            || state.active != [0; 4]
        {
            // Unsealed admission, any live claim, sticky failure, or prior completion prevents
            // creation of another completion fact.
            return notifications;
        }
        if state.constructed != state.destructed {
            // Zero active ownership must match all actual construction/destruction counts;
            // a corrupted zero cannot authorize premature completion.
            self.failed.store(true, Ordering::Release);
            return notifications;
        }
        state.completed = true;
        for (slot, notification) in state.observers.iter_mut().zip(&mut notifications) {
            if let ObserverSlot::Pending(observer) = slot {
                // Keep only weak identity for duplicate prevention. Move the strong observer
                // into this out-of-lock notification, without retaining business resources here.
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
        // Observer cancellation is a scalar-only contract. A violating panic cannot escape a
        // resource destructor or later turn lost observer evidence into a successful release.
        match catch_unwind(AssertUnwindSafe(|| observer.is_cancelled())) {
            Ok(cancelled) => Ok(cancelled),
            Err(_) => {
                // Even observer violation must not trigger another panic from resource Drop;
                // subsequent completion checks retain permanent failure.
                self.failed.store(true, Ordering::Release);
                Err(FrozenValueFamilyError::ObserverPanicked)
            }
        }
    }

    fn notify(&self, notifications: Notifications) {
        // Every callback runs without the family mutex and respects its own cohort's actual
        // cancellation. Receiving one family event does not complete that cohort's native graph.
        for observer in notifications.into_iter().flatten() {
            match self.cancelled(&observer) {
                Ok(false) => {
                    if catch_unwind(AssertUnwindSafe(|| observer.released())).is_err() {
                        // Callbacks bridge atomic claims only; a contract-violating panic must
                        // not escape the destruction stack.
                        self.failed.store(true, Ordering::Release);
                    }
                }
                Ok(true) => {} // Cancellation revokes this observer only, without changing family counts or another role.
                Err(_) => {} // cancelled already recorded sticky failure; do not manufacture successful notification.
            }
        }
    }
}

impl FrozenValueClaim {
    pub(super) fn claim_string_value(&self) -> Result<Self, FrozenValueFamilyError> {
        // Only a still-live Source payload claim can create an actual string-value claim.
        self.family.derive(
            self.kind,
            FrozenValueClaimKind::SourcePayload,
            FrozenValueClaimKind::StringValue,
        )
    }

    pub(super) fn claim_annotation_payload(&self) -> Result<Self, FrozenValueFamilyError> {
        // Annotation payload ownership derives from a concrete live string value in this family.
        self.family.derive(
            self.kind,
            FrozenValueClaimKind::StringValue,
            FrozenValueClaimKind::AnnotationPayload,
        )
    }

    pub(super) fn claim_annotation_value(&self) -> Result<Self, FrozenValueFamilyError> {
        // Each concrete annotation object independently derives from its shared actual payload.
        self.family.derive(
            self.kind,
            FrozenValueClaimKind::AnnotationPayload,
            FrozenValueClaimKind::AnnotationValue,
        )
    }
}

impl Drop for FrozenValueClaim {
    fn drop(&mut self) {
        self.family.drop_claim(self.kind); // Actual guard Drop is the sole settlement path; no manual successful release exists.
    }
}

#[cfg(test)]
#[path = "frozen_value_family/tests.rs"]
mod tests;
