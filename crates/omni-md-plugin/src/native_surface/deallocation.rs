//! Observes actual native deallocation without retaining any tracked graph object.
//!
//! Public Objective-C associations retain timestamp/counter markers, never their source object.
//! A unique-identity cohort coalesces its zero-claim notification onto the main thread after the release stack.
//! The consumer must still reject sticky resource failure and perform complete strict weak graph verification.

#[cfg(any(
    test,
    all(
        feature = "native-block-fragment-diagnostic",
        feature = "native-markdown-runtime-probe"
    )
))]
use super::frozen_value_family::{
    FrozenValueFamily, FrozenValueObserverRole, FrozenValueReleaseObserver,
};
use super::*;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

unsafe extern "C" {
    /// Public objc/runtime.h ABI; the chosen policy retains value under the operation's unique key.
    fn objc_setAssociatedObject(
        object: *const AnyObject,
        key: *const c_void,
        value: *const AnyObject,
        policy: usize,
    );

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// Lets the isolated Native contract inspect its own association, without reading text or inferring source-object types.
    fn objc_getAssociatedObject(object: *const AnyObject, key: *const c_void) -> *const AnyObject;
}

/// The shared ledger admits at most 1024 identities, with a few explicit TextKit roots; the cohort adds a finite observation ceiling.
/// Overflow permanently fails observation rather than certifying a truncated set, without adding business or Native strong ownership.
const MAX_NATIVE_COHORT_IDENTITIES: usize = 2_048;

/// Main-thread completion metadata contains no view, document, mapping or worker owner.
struct NativeDeallocationCallback {
    work: RefCell<Option<Box<dyn FnOnce()>>>,
    consumed: Cell<bool>,
    // Explicit Native acceptance wakes one original scenario without consuming the parent close's sole production handler.
    #[cfg(feature = "native-markdown-runtime-probe")]
    probe_wake: RefCell<Option<Box<dyn FnOnce()>>>,
}

/// Atomic counters can be notified by native destruction without touching main-thread Rust state.
struct NativeDeallocationSignals {
    remaining: AtomicUsize,
    relay_queued: AtomicBool,
    cancelled: AtomicBool,
    // Retains external-resource registration/release failure; a final zero count cannot repair incomplete resource evidence.
    external_failed: AtomicBool,
    callback: MainThreadBound<std::rc::Weak<NativeDeallocationCallback>>,
    started: Instant,
    completed_after_ns: AtomicU64,
}

/// The family retains only this atomic observer, without Native objects, document text, mappings, or main-thread business closures.
#[cfg(any(
    test,
    all(
        feature = "native-block-fragment-diagnostic",
        feature = "native-markdown-runtime-probe"
    )
))]
struct FrozenValueCohortObserver {
    signals: Arc<NativeDeallocationSignals>,
    delivered: AtomicBool,
}

#[cfg(any(
    test,
    all(
        feature = "native-block-fragment-diagnostic",
        feature = "native-markdown-runtime-probe"
    )
))]
impl FrozenValueReleaseObserver for FrozenValueCohortObserver {
    fn released(&self) {
        if self.delivered.swap(true, Ordering::AcqRel) {
            // Consumes the actual family-destruction event once; duplicates cannot decrement claims again and impersonate resource release.
            self.signals.external_failed.store(true, Ordering::Release);
            return;
        }
        // The family emits this event only after sealing and actual Drop of all source/value claims; releases only this cohort's external observation claim.
        // This may run on a Foundation background destruction stack; signals queues only the main-thread continuation, which must still verify the original weak graph.
        self.signals.release_claim();
    }

    fn is_cancelled(&self) -> bool {
        self.signals.cancelled.load(Ordering::Acquire) // Reads only atomics, without the family lock or any Native/document object.
    }
}

impl NativeDeallocationSignals {
    /// Releases one registered claim; reaching zero permits a relay, while final release validation still checks failure and the original weak graph.
    fn release_claim(self: &Arc<Self>) {
        // The builder, each unique associated marker, and each admitted external obligation own
        // one claim. Their consuming release paths balance exactly once, so the prior count is positive.
        if self.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Publish timing before making a relay eligible, including a concurrent handler installation.
            self.completed_after_ns.store(
                self.started
                    .elapsed()
                    .as_nanos()
                    .min(u128::from(u64::MAX - 1)) as u64,
                Ordering::Release,
            );
            // End the final native deallocation stack before any weak graph loads or host callback.
            self.queue_if_complete();
        }
    }

    /// Coalesces a terminal claim count into one relay, including retirement before handler installation.
    /// The consumer separately checks actual completion and the original graph before accepting release.
    fn queue_if_complete(self: &Arc<Self>) {
        if self.cancelled.load(Ordering::Acquire)
            || self.remaining.load(Ordering::Acquire) != 0
            || self.completed_after_ns.load(Ordering::Acquire) == u64::MAX
            || self.relay_queued.swap(true, Ordering::AcqRel)
        {
            // Unreleased objects and an already queued relay cannot initiate another strict check.
            return;
        }
        let signals = Arc::clone(self);
        enqueue_native_markdown_main_task(move || {
            #[cfg(feature = "native-close-stage-diagnostic")]
            super::lifecycle::diagnostic_close_stage(
                "relay_enter",
                0,
                format_args!(
                    "remaining={} queued={} cancelled={}",
                    signals.remaining.load(Ordering::Acquire),
                    signals.relay_queued.load(Ordering::Acquire),
                    signals.cancelled.load(Ordering::Acquire)
                ),
            );
            if signals.cancelled.load(Ordering::Acquire) {
                // Scope promotion revoked this observer before its queued relay could claim completion.
                return;
            }
            let mtm =
                MainThreadMarker::new().expect("native deallocation relays use the main queue");
            let Some(callback) = signals.callback.get(mtm).upgrade() else {
                // Dropped operation metadata has no authority to revive a completed/replaced session.
                return;
            };
            let work = callback.work.borrow_mut().take();
            #[cfg(feature = "native-markdown-runtime-probe")]
            let probe_wake = {
                let wake = callback.probe_wake.borrow_mut().take();
                // Both slots are taken outside their borrows. After this probe consumes the terminal observation,
                // a later parent close may still install its unconsumed production handler and relay the same zero-claim fact.
                signals.relay_queued.store(false, Ordering::Release);
                wake
            };
            if let Some(work) = work {
                // End state borrowing before the unique actual-completion handler can reenter AppKit or the Host.
                callback.consumed.set(true);
                work();
            } else {
                // Registration may follow synchronous deallocation. A later installation can enqueue
                // one relay, while no strict graph check has happened in this handler-less observation.
                signals.relay_queued.store(false, Ordering::Release);
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            if let Some(wake) = probe_wake {
                // Relay entry observed the original terminal claim count and removed this wake from its slot.
                // A production handler may drop the cohort and cancel future observations without erasing this terminal observation.
                // Delivery requires work to return normally; panic does not deliver through a finally path. The listener must
                // still check its original weak owner and terminal resource facts, without reviving closed work or inferring installation success.
                wake();
            } // Cancellation before relay entry is rejected above; all closures are released outside slot borrows.
        });
    }
}

/// The associated value holds only a counter claim; it has no reverse reference to its source object.
struct NativeDeallocationMarkerIvars {
    signals: Arc<NativeDeallocationSignals>,
}

impl Drop for NativeDeallocationMarkerIvars {
    fn drop(&mut self) {
        // The runtime removes this unique association at source deallocation or explicit observer cancellation.
        // Cancellation disables completion first; only an uncancelled final claim supplies actual release authority.
        self.signals.release_claim();
    }
}

define_class!(
    // SAFETY: NSObject has no extra subclass requirements. The marker's installed ivar owns only
    // Send/Sync atomic signal metadata, and its main-thread initialization retains no source object.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NativeDeallocationMarkerIvars]
    struct NativeMarkdownDeallocationMarker;
    // SAFETY: the initialized NSObject subclass preserves native identity and main-thread construction.
    unsafe impl NSObjectProtocol for NativeMarkdownDeallocationMarker {}
);

/// Owns operation metadata while markers observe each unique native identity's actual lifetime.
pub(super) struct NativeMarkdownDeallocationCohort {
    callback: Rc<NativeDeallocationCallback>,
    signals: Arc<NativeDeallocationSignals>,
    identities: HashSet<usize>,
    // These Weak references permit removing this cohort's own markers at retirement without extending source-object lifetime.
    observed: Vec<Weak<AnyObject>>,
    sealed: bool,
}

/// Owns one actual external resource's return obligation, retaining only cohort atomics rather than the resource or main-thread business graph.
#[cfg(any(
    feature = "native-raster-surface-probe",
    feature = "native-paged-production"
))]
pub(in crate::native_surface) struct NativeMarkdownDeallocationClaim {
    signals: Option<Arc<NativeDeallocationSignals>>,
}

#[cfg(any(
    feature = "native-raster-surface-probe",
    feature = "native-paged-production"
))]
impl NativeMarkdownDeallocationClaim {
    /// The caller already returned the actual resource; consumes its claim so later Drop cannot signal completion again.
    pub(in crate::native_surface) fn release(mut self) {
        self.signals
            .take()
            .expect("资源claim只能消费一次")
            .release_claim();
    }
}

#[cfg(any(
    feature = "native-raster-surface-probe",
    feature = "native-paged-production"
))]
impl Drop for NativeMarkdownDeallocationClaim {
    fn drop(&mut self) {
        if let Some(signals) = self.signals.take() {
            // Dropping an unreturned obligation records failure; metadata Drop cannot stand in for successful resource release.
            signals.external_failed.store(true, Ordering::Release);
            signals.release_claim();
        }
    }
}

impl NativeMarkdownDeallocationCohort {
    /// Admits an external resource obligation before sealing; its final return participates in the same coalesced main-thread relay.
    #[cfg(any(
        feature = "native-raster-surface-probe",
        feature = "native-paged-production"
    ))]
    pub(in crate::native_surface) fn resource_claim(&mut self) -> NativeMarkdownDeallocationClaim {
        assert!(!self.sealed, "资源登记不能晚于cohort封闭");
        self.signals.remaining.fetch_add(1, Ordering::AcqRel);
        NativeMarkdownDeallocationClaim {
            signals: Some(Arc::clone(&self.signals)),
        }
    }
    /// Reads diagnostic atomics only, without counter mutation, callback installation, or release authority; does not replace has_completed.
    #[cfg(feature = "native-close-stage-diagnostic")]
    pub(super) fn diagnostic_state(&self) -> (usize, bool, bool) {
        (
            self.signals.remaining.load(Ordering::Acquire),
            self.signals.relay_queued.load(Ordering::Acquire),
            self.callback.consumed.get(),
        )
    }

    /// Begin registration with a builder claim so early native destruction cannot complete a partial set.
    pub(super) fn new(mtm: MainThreadMarker) -> Self {
        let callback = Rc::new(NativeDeallocationCallback {
            work: RefCell::new(None),
            consumed: Cell::new(false),
            #[cfg(feature = "native-markdown-runtime-probe")]
            probe_wake: RefCell::new(None),
        });
        let signals = Arc::new(NativeDeallocationSignals {
            remaining: AtomicUsize::new(1),
            relay_queued: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            external_failed: AtomicBool::new(false),
            callback: MainThreadBound::new(Rc::downgrade(&callback), mtm),
            started: Instant::now(),
            completed_after_ns: AtomicU64::new(u64::MAX),
        });
        Self {
            callback,
            signals,
            identities: HashSet::new(),
            observed: Vec::new(),
            sealed: false,
        }
    }

    /// Registers one complete family observation in the original role's cohort; actual family claims account for dynamically derived values.
    #[cfg(any(
        test,
        all(
            feature = "native-block-fragment-diagnostic",
            feature = "native-markdown-runtime-probe"
        )
    ))]
    pub(super) fn track_frozen_values(
        &mut self,
        family: &Arc<FrozenValueFamily>,
        role: FrozenValueObserverRole,
    ) {
        assert!(!self.sealed, "不可变值登记晚于原资源cohort封闭");
        if self
            .signals
            .remaining
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .is_err()
        {
            // An unrepresentable count cannot certify closure; creates no observer without its corresponding claim.
            self.signals.external_failed.store(true, Ordering::Release);
            return;
        }
        let observer: Arc<dyn FrozenValueReleaseObserver> = Arc::new(FrozenValueCohortObserver {
            signals: Arc::clone(&self.signals),
            delivered: AtomicBool::new(false),
        });
        if family.observe(role, observer).is_err() {
            // Failed registration returns only its temporary admission claim; the sticky failure permanently prevents successful completion certification.
            self.signals.external_failed.store(true, Ordering::Release);
            self.signals.release_claim();
        }
    }

    /// Installs this cohort's own association on each actual Native identity; Weak records support symmetric removal without retaining the source.
    pub(super) fn track(&mut self, object: &AnyObject, mtm: MainThreadMarker) {
        assert!(!self.sealed, "native cohort registration already ended");
        let address = std::ptr::from_ref(object).addr();
        if self.identities.contains(&address) {
            // Addresses may be reused even during construction; only the actual original live Weak proves the same Native lifetime.
            let duplicate = self.observed.iter().any(|weak| {
                weak.load()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq(actual, object))
            });
            if duplicate {
                return; // Multiple roles for the same object share one marker and actual-deallocation claim.
            }
        }
        if self.observed.len() >= MAX_NATIVE_COHORT_IDENTITIES {
            self.signals.external_failed.store(true, Ordering::Release);
            return; // Failure remains sticky; installs no association that cannot be removed precisely at retirement.
        }
        if self
            .signals
            .remaining
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .is_err()
        {
            self.signals.external_failed.store(true, Ordering::Release);
            return; // An unrepresentable count creates no marker without a matching claim.
        }
        // Records identity before installing the association; Native construction/release may reenter, and final Drop must still find this original identity.
        self.identities.insert(address);
        self.observed.push(Weak::new(object));
        let marker =
            NativeMarkdownDeallocationMarker::alloc(mtm).set_ivars(NativeDeallocationMarkerIvars {
                signals: Arc::clone(&self.signals),
            });
        // SAFETY: All ivars precede standard NSObject initialization; the marker holds only Send/Sync atomic metadata.
        let marker: Retained<NativeMarkdownDeallocationMarker> =
            unsafe { msg_send![super(marker), init] };
        // SAFETY: The public runtime ABI uses the unique live Arc address solely as this cohort's key. The source retains its marker,
        // which has no reverse source edge; Drop removes only this key, preserving other cohorts and source contents.
        unsafe {
            objc_setAssociatedObject(
                std::ptr::from_ref(object),
                Arc::as_ptr(&self.signals).cast(),
                std::ptr::from_ref(&*marker).cast(),
                1,
            );
        }
    }

    /// Finish the complete identity set before transferring the operation's weak-only evidence.
    pub(super) fn seal(&mut self) {
        assert!(!self.sealed, "native cohort registration ended twice");
        self.sealed = true;
        // The address index serves construction deduplication only; bounded Weak records survive until retirement to remove markers from still-live values.
        self.identities = HashSet::new();
        self.signals.release_claim();
    }

    /// Install one operation-specific handler; only actual completion makes it eligible to execute.
    pub(super) fn when_released(&self, work: impl FnOnce() + 'static) {
        assert!(
            !self.signals.cancelled.load(Ordering::Acquire),
            "已取消的资源观察不能安装后续回调"
        );
        assert!(!self.callback.consumed.get(), "原生资源完成回调已被消费");
        assert!(
            self.callback.work.borrow().is_none(),
            "原生资源完成回调已安装"
        );
        *self.callback.work.borrow_mut() = Some(Box::new(work));
        self.signals.queue_if_complete();
    }

    /// Installs one bounded wake for the existing Source scenario; the same relay reports zero claims while the production slot remains owned by parent close.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn probe_when_released(&self, wake: impl FnOnce() + 'static) {
        assert!(
            !self.signals.cancelled.load(Ordering::Acquire),
            "已取消的资源观察不能安装验收唤醒"
        );
        assert!(
            self.callback.probe_wake.borrow().is_none(),
            "原生资源验收唤醒已安装"
        );
        *self.callback.probe_wake.borrow_mut() = Some(Box::new(wake));
        self.signals.queue_if_complete();
    }

    /// Revoke callback authority before removing only this cohort's association from a terminal shell.
    ///
    /// This operation is observation cancellation, never an actual-deallocation signal. Terminal
    /// callers must already have proved the complete payload graph released and the panel detached.
    pub(super) fn cancel_observation<T: objc2::Message>(&mut self, panel: Option<&T>) {
        // A queued relay checks cancellation before touching its weak callback. Clearing the work
        // also removes every project callback capture before the framework-held shell is accepted.
        self.signals.cancelled.store(true, Ordering::Release);
        let work = self.callback.work.borrow_mut().take();
        self.callback.consumed.set(true);
        #[cfg(feature = "native-markdown-runtime-probe")]
        let probe_wake = self.callback.probe_wake.borrow_mut().take();
        #[cfg(feature = "native-markdown-runtime-probe")]
        drop(probe_wake);
        drop(work); // Callback-capture destruction may borrow slots or call Native; all RefCell borrows have ended first.
        if let Some(panel) = panel {
            // SAFETY: the caller transiently owns this live main-thread panel. The stable Arc key
            // identifies only this observer's association; nil removes that marker, not another
            // owner's association or a panel retain. Cancellation preceded marker destruction.
            unsafe {
                objc_setAssociatedObject(
                    std::ptr::from_ref(panel).cast(),
                    Arc::as_ptr(&self.signals).cast(),
                    std::ptr::null(),
                    1,
                );
            }
        }
    }

    /// Report measured registration-to-actual-completion latency without introducing a timer or retry.
    pub(super) fn elapsed(&self) -> Duration {
        let completed = self.signals.completed_after_ns.load(Ordering::Acquire);
        if completed == u64::MAX {
            // An unfinished cohort may be reported diagnostically, but cannot authorize weak verification yet.
            self.signals.started.elapsed()
        } else {
            // Timing stops at the final actual release signal rather than including later event dispatch latency.
            Duration::from_nanos(completed)
        }
    }

    /// Expose bounded counter evidence for one terminal probe failure, without authorizing completion.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn diagnostic(&self) -> String {
        format!(
            "sealed={} remaining={} relay_queued={} handler_installed={} handler_consumed={} cancelled={} external_failed={} elapsed_us={}",
            self.sealed,
            self.signals.remaining.load(Ordering::Acquire),
            self.signals.relay_queued.load(Ordering::Acquire),
            self.callback.work.borrow().is_some(),
            self.callback.consumed.get(),
            self.signals.cancelled.load(Ordering::Acquire),
            self.signals.external_failed.load(Ordering::Acquire),
            self.elapsed().as_micros(),
        )
    }

    /// Distinguish actual completed ownership from a guessed queue boundary before any native weak loads.
    pub(super) fn has_completed(&self) -> bool {
        self.sealed
            && !self.signals.cancelled.load(Ordering::Acquire)
            && !self.signals.external_failed.load(Ordering::Acquire)
            && self.signals.remaining.load(Ordering::Acquire) == 0
            && self.signals.completed_after_ns.load(Ordering::Acquire) != u64::MAX
    }
}

impl Drop for NativeMarkdownDeallocationCohort {
    fn drop(&mut self) {
        // Revokes authority before dropping callback captures or markers; explicit removal cleans observation but cannot certify source-object destruction.
        self.signals.cancelled.store(true, Ordering::Release);
        let work = self.callback.work.borrow_mut().take();
        self.callback.consumed.set(true);
        #[cfg(feature = "native-markdown-runtime-probe")]
        let probe_wake = self.callback.probe_wake.borrow_mut().take();
        #[cfg(feature = "native-markdown-runtime-probe")]
        drop(probe_wake);
        drop(work); // Releases outside borrows because captured Drop may reenter the same Native/resource boundary.
        let observed = std::mem::take(&mut self.observed);
        for weak in observed {
            if let Some(object) = weak.load() {
                // SAFETY: This temporary original Weak upgrade keeps the receiver live; removes only the stable signals-address key.
                // Actual marker destruction balances its claim, while cancellation permanently revokes completion and relay authority.
                unsafe {
                    objc_setAssociatedObject(
                        std::ptr::from_ref(&*object),
                        Arc::as_ptr(&self.signals).cast(),
                        std::ptr::null(),
                        1,
                    );
                }
            } // An actually nil object needs no removal; a key already removed by cancel_observation cannot destroy its marker again.
        }
        if !self.sealed {
            // Failed construction still balances its builder claim; cancellation preceded all Native cleanup and prevents a successful completion signal.
            self.signals.release_claim();
        }
    }
}

/// Verifies project marker lifetime rules using isolated actual NSObject associations.
/// This scoped runtime contract does not certify document UI, whole-product release, or peak RSS.
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) fn verify_native_markdown_deallocation_accounting(
    mtm: MainThreadMarker,
) -> Result<(), String> {
    let mut empty = NativeMarkdownDeallocationCohort::new(mtm);
    empty.seal();
    if !empty.has_completed() {
        // The builder claim alone must complete an empty graph; no queue turn supplies missing authority.
        return Err("an empty native deallocation cohort did not complete".into());
    }
    let object = NSObject::new();
    let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
    cohort.track(&object, mtm);
    cohort.track(&object, mtm);
    cohort.seal();
    if cohort.signals.remaining.load(Ordering::Acquire) != 1 || cohort.has_completed() {
        // Two graph roles alias one object, so exactly one outstanding actual-deallocation claim must remain.
        return Err(format!(
            "native deallocation identity accounting failed: {}",
            cohort.diagnostic()
        ));
    }
    drop(object);
    if !cohort.has_completed() {
        // Public association removal must destroy the marker without any event loop or reverse source retain.
        return Err(format!(
            "native deallocation marker retained its source: {}",
            cohort.diagnostic()
        ));
    }
    let object = NSObject::new();
    let mut earlier = NativeMarkdownDeallocationCohort::new(mtm);
    let mut closing = NativeMarkdownDeallocationCohort::new(mtm);
    earlier.track(&object, mtm);
    earlier.seal();
    closing.track(&object, mtm);
    closing.seal();
    if earlier.has_completed() || closing.has_completed() {
        // Installing final-close observation must not remove the still-live note-switch marker.
        return Err("overlapping native cohorts overwrote an earlier association".into());
    }
    drop(object);
    if !earlier.has_completed() || !closing.has_completed() {
        // One actual object destruction completes both independent operation-specific claims.
        return Err("overlapping native cohorts did not both observe actual release".into());
    }
    let object = NSObject::new();
    let mut cancelled = NativeMarkdownDeallocationCohort::new(mtm);
    let mut preserved = NativeMarkdownDeallocationCohort::new(mtm);
    cancelled.track(&object, mtm);
    preserved.track(&object, mtm);
    cancelled.seal();
    preserved.seal();
    cancelled.when_released(|| panic!("cancelled observer emitted completion"));
    cancelled.cancel_observation(Some(&*object));
    if cancelled.has_completed()
        || cancelled.signals.remaining.load(Ordering::Acquire) != 0
        || cancelled.callback.work.borrow().is_some()
        || preserved.signals.remaining.load(Ordering::Acquire) != 1
    {
        // Removing this observer releases its marker but cannot certify object destruction or remove another key.
        return Err(
            "cancelled observation retained callback authority or changed another cohort".into(),
        );
    }
    drop(object);
    if cancelled.has_completed() || !preserved.has_completed() {
        // Later actual deallocation completes only the independent live observer, never the cancelled receipt.
        return Err("cancelled and actual release authority were conflated".into());
    }
    verify_dropped_cohort_unloads_own_marker(mtm)?;
    #[cfg(any(
        test,
        all(
            feature = "native-block-fragment-diagnostic",
            feature = "native-markdown-runtime-probe"
        )
    ))]
    {
        // Ordinary probes have no frozen-value constructor; exercise these cases only where the actual family path is compiled.
        // All three close roles must wait for the same actual family, not merely the original SourceClaim or an arbitrary parent object.
        let family = FrozenValueFamily::new();
        let source = family.claim_source().map_err(|error| error.to_string())?;
        let value = source
            .claim_string_value()
            .map_err(|error| error.to_string())?;
        let mut editor = NativeMarkdownDeallocationCohort::new(mtm);
        let mut full = NativeMarkdownDeallocationCohort::new(mtm);
        let mut resources = NativeMarkdownDeallocationCohort::new(mtm);
        editor.track_frozen_values(&family, FrozenValueObserverRole::Editor);
        full.track_frozen_values(&family, FrozenValueObserverRole::FullGraph);
        resources.track_frozen_values(&family, FrozenValueObserverRole::Resources);
        editor.seal();
        full.seal();
        resources.seal();
        family.seal_sources();
        drop(source);
        if editor.has_completed() || full.has_completed() || resources.has_completed() {
            return Err("冻结子值仍存活时关闭角色提前完成".into());
        }
        drop(value);
        if !editor.has_completed() || !full.has_completed() || !resources.has_completed() {
            return Err("真实冻结值析构没有完成三个原资源角色".into());
        }
        // Replaces an old observation for the same role only after explicit cancellation; never relabels its cancelled receipt as release.
        let family = FrozenValueFamily::new();
        let source = family.claim_source().map_err(|error| error.to_string())?;
        let mut earlier = NativeMarkdownDeallocationCohort::new(mtm);
        earlier.track_frozen_values(&family, FrozenValueObserverRole::FullGraph);
        earlier.seal();
        earlier.cancel_observation::<NSObject>(None);
        let mut later = NativeMarkdownDeallocationCohort::new(mtm);
        later.track_frozen_values(&family, FrozenValueObserverRole::FullGraph);
        later.seal();
        family.seal_sources();
        drop(source);
        if earlier.has_completed() || !later.has_completed() {
            return Err("冻结值观察取消与实际释放权限混淆".into());
        }
    }
    println!(
        "native_deallocation_accounting=passed empty_graph duplicate_identity overlapping_cohorts cancelled_key actual_marker_drop dropped_cohort_own_marker callback_drop_outside_borrow preserved_other_cohort{}",
        if cfg!(any(
            test,
            all(
                feature = "native-block-fragment-diagnostic",
                feature = "native-markdown-runtime-probe"
            )
        )) {
            " frozen_value_three_roles frozen_value_cancelled_role"
        } else {
            "" // Uncompiled family checks contribute no success label; the actual Native marker checks still run in full.
        }
    );
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Observes actual callback-capture destruction during cancellation, retaining only Rust weak slots and finite scalars, not Native sources or document text.
struct CancelledCallbackDropProbe {
    callback: std::rc::Weak<NativeDeallocationCallback>,
    dropped: Rc<Cell<bool>>,
    borrow_clear: Rc<Cell<bool>>,
}

#[cfg(feature = "native-markdown-runtime-probe")]
impl Drop for CancelledCallbackDropProbe {
    fn drop(&mut self) {
        self.dropped.set(true);
        let clear = self.callback.upgrade().is_some_and(|callback| {
            callback.work.try_borrow_mut().is_ok() && callback.probe_wake.try_borrow_mut().is_ok()
        });
        self.borrow_clear.set(clear); // Actually borrows the slots during capture destruction rather than inferring safety from source order or an assumed flag.
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Obtains Weak only to this key's actual marker; the live source and association provide its lifetime during this synchronous call.
fn associated_marker_weak(
    object: &AnyObject,
    signals: &Arc<NativeDeallocationSignals>,
) -> Result<Weak<AnyObject>, String> {
    // SAFETY: The contract retains this live NSObject; the key is the original live Arc address, and only this observation's association is read.
    let marker = unsafe {
        objc_getAssociatedObject(std::ptr::from_ref(object), Arc::as_ptr(signals).cast())
    };
    // SAFETY: The live source's same-key association still owns the nonnull result and is not removed during this synchronous call.
    let marker = unsafe { marker.as_ref() }.ok_or("原生合同没有取得实际关联marker")?;
    Ok(Weak::new(marker))
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Drops one cohort while the actual source lives, checking real marker/signal release while another cohort independently waits.
fn verify_dropped_cohort_unloads_own_marker(mtm: MainThreadMarker) -> Result<(), String> {
    for explicitly_cancelled in [false, true] {
        let object = NSObject::new();
        let object_weak = Weak::from_retained(&object);
        let mut discarded = NativeMarkdownDeallocationCohort::new(mtm);
        let mut preserved = NativeMarkdownDeallocationCohort::new(mtm);
        discarded.track(&object, mtm);
        preserved.track(&object, mtm);
        discarded.seal();
        preserved.seal();
        let discarded_marker = associated_marker_weak(&object, &discarded.signals)?;
        let preserved_marker = associated_marker_weak(&object, &preserved.signals)?;
        let signals = Arc::clone(&discarded.signals);
        let signals_weak = Arc::downgrade(&signals);
        let callback_called = Rc::new(Cell::new(false));
        let callback_dropped = Rc::new(Cell::new(false));
        let borrow_clear = Rc::new(Cell::new(false));
        let probe = CancelledCallbackDropProbe {
            callback: Rc::downgrade(&discarded.callback),
            dropped: Rc::clone(&callback_dropped),
            borrow_clear: Rc::clone(&borrow_clear),
        };
        let called = Rc::clone(&callback_called);
        discarded.when_released(move || {
            // This cancelled callback must never execute; the captured probe's actual Drop checks that cancellation releases captures outside RefCell borrows.
            drop(probe);
            called.set(true);
        });
        let wake_called = Rc::new(Cell::new(false));
        let wake_dropped = Rc::new(Cell::new(false));
        let wake_borrow_clear = Rc::new(Cell::new(false));
        let wake_probe = CancelledCallbackDropProbe {
            callback: Rc::downgrade(&discarded.callback),
            dropped: Rc::clone(&wake_dropped),
            borrow_clear: Rc::clone(&wake_borrow_clear),
        };
        let called = Rc::clone(&wake_called);
        discarded.probe_when_released(move || {
            // Cancelling observation of a live source also releases probe captures without delivering a fabricated completion.
            drop(wake_probe);
            called.set(true);
        });
        if explicitly_cancelled {
            discarded.cancel_observation(Some(&*object));
            if discarded.has_completed() || signals.remaining.load(Ordering::Acquire) != 0 {
                return Err("显式取消被错误解释为原对象实际析构".into());
            }
        } // The other branch directly drops the waiting cohort, without assuming callers explicitly cancel first.
        drop(discarded);
        if discarded_marker.load().is_some()
            || !signals.cancelled.load(Ordering::Acquire)
            || signals.remaining.load(Ordering::Acquire) != 0
            || signals.relay_queued.load(Ordering::Acquire)
            || callback_called.get()
            || !callback_dropped.get()
            || !borrow_clear.get()
            || wake_called.get()
            || !wake_dropped.get()
            || !wake_borrow_clear.get()
        {
            return Err("cohort Drop没有真实卸载自己的marker或取消回调发生借用重入".into());
        }
        if object_weak.load().is_none()
            || preserved_marker.load().is_none()
            || preserved.has_completed()
            || preserved.signals.remaining.load(Ordering::Acquire) != 1
        {
            return Err("卸载一份cohort改变了原对象或另一份原观察权限".into());
        }
        drop(signals);
        if signals_weak.upgrade().is_some() {
            return Err("cohort Drop后关联marker仍保留该cohort的原子signals".into());
        }
        drop(object);
        if object_weak.load().is_some()
            || preserved_marker.load().is_some()
            || !preserved.has_completed()
        {
            return Err("原对象真实析构后另一cohort没有独立完成".into());
        }
    }
    Ok(())
}
