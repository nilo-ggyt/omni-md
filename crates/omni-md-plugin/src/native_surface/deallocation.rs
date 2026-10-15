//! Observes actual native deallocation without retaining any tracked graph object.
//!
//! Public Objective-C associations retain timestamp/counter markers, never their source object.
//! A unique-identity cohort queues one main-thread callback after the last actual release stack.
//! The callback still has to perform the complete strict weak graph verification.

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
}

/// Main-thread completion metadata contains no view, document, mapping or worker owner.
struct NativeDeallocationCallback {
    work: RefCell<Option<Box<dyn FnOnce()>>>,
    consumed: Cell<bool>,
}

/// Atomic counters can be notified by native destruction without touching main-thread Rust state.
struct NativeDeallocationSignals {
    remaining: AtomicUsize,
    relay_queued: AtomicBool,
    cancelled: AtomicBool,
    callback: MainThreadBound<std::rc::Weak<NativeDeallocationCallback>>,
    started: Instant,
    completed_after_ns: AtomicU64,
}

impl NativeDeallocationSignals {
    /// Release one builder/marker claim; only the final actual claim makes completion eligible.
    fn release_claim(self: &Arc<Self>) {
        // The builder contributes one claim and each unique associated marker contributes exactly
        // one more; the runtime removes each marker once, so the previous count is always positive.
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

    /// Coalesce a completed cohort into one callback, including completion before handler installation.
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
            if let Some(work) = work {
                // End state borrowing before the unique actual-completion handler can reenter AppKit or the Host.
                callback.consumed.set(true);
                work();
            } else {
                // Registration may follow synchronous deallocation. A later installation can enqueue
                // one relay, while no strict graph check has happened in this handler-less observation.
                signals.relay_queued.store(false, Ordering::Release);
            }
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
    sealed: bool,
}

impl NativeMarkdownDeallocationCohort {
    /// Begin registration with a builder claim so early native destruction cannot complete a partial set.
    pub(super) fn new(mtm: MainThreadMarker) -> Self {
        let callback = Rc::new(NativeDeallocationCallback {
            work: RefCell::new(None),
            consumed: Cell::new(false),
        });
        let signals = Arc::new(NativeDeallocationSignals {
            remaining: AtomicUsize::new(1),
            relay_queued: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            callback: MainThreadBound::new(Rc::downgrade(&callback), mtm),
            started: Instant::now(),
            completed_after_ns: AtomicU64::new(u64::MAX),
        });
        Self {
            callback,
            signals,
            identities: HashSet::new(),
            sealed: false,
        }
    }

    /// Attach one public association per identity, without retaining the observed native object.
    pub(super) fn track<T: objc2::Message>(&mut self, object: &T, mtm: MainThreadMarker) {
        assert!(!self.sealed, "native cohort registration already ended");
        if !self.identities.insert(std::ptr::from_ref(object).addr()) {
            // Different graph roles can name the same native object; one actual deallocation is one claim.
            return;
        }
        self.signals.remaining.fetch_add(1, Ordering::Relaxed);
        let marker =
            NativeMarkdownDeallocationMarker::alloc(mtm).set_ivars(NativeDeallocationMarkerIvars {
                signals: Arc::clone(&self.signals),
            });
        // SAFETY: NSObject initializes the fully installed atomic metadata without extra preconditions.
        let marker: Retained<NativeMarkdownDeallocationMarker> =
            unsafe { msg_send![super(marker), init] };
        // SAFETY: this is the public runtime ABI from objc/runtime.h. The stable Arc allocation is
        // a unique key for this cohort and every marker keeps it alive. Policy 1 is the documented
        // RETAIN_NONATOMIC association: object retains marker, while marker never retains object.
        unsafe {
            objc_setAssociatedObject(
                std::ptr::from_ref(object).cast(),
                Arc::as_ptr(&self.signals).cast(),
                std::ptr::from_ref(&*marker).cast(),
                1,
            );
        }
    }

    /// Include a surviving weak role without extending its lifetime beyond association installation.
    pub(super) fn track_weak<T: objc2::Message>(&mut self, weak: &Weak<T>, mtm: MainThreadMarker) {
        if let Some(object) = weak.load() {
            // A local upgrade pins only this installation; the marker has no reverse strong source reference.
            self.track(&*object, mtm);
        }
    }

    /// Finish the complete identity set before transferring the operation's weak-only evidence.
    pub(super) fn seal(&mut self) {
        assert!(!self.sealed, "native cohort registration ended twice");
        self.sealed = true;
        // The address set is construction scratch only; no native graph/address catalog remains while waiting.
        self.identities = HashSet::new();
        self.signals.release_claim();
    }

    /// Install one operation-specific handler; only actual completion makes it eligible to execute.
    pub(super) fn when_released(&self, work: impl FnOnce() + 'static) {
        assert!(
            !self.signals.cancelled.load(Ordering::Acquire),
            "a cancelled observation cannot install completion work"
        );
        assert!(
            !self.callback.consumed.get(),
            "native completion already consumed"
        );
        assert!(
            self.callback.work.borrow().is_none(),
            "native completion already installed"
        );
        *self.callback.work.borrow_mut() = Some(Box::new(work));
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
        self.callback.work.borrow_mut().take();
        self.callback.consumed.set(true);
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
            "sealed={} remaining={} relay_queued={} handler_installed={} handler_consumed={} cancelled={} elapsed_us={}",
            self.sealed,
            self.signals.remaining.load(Ordering::Acquire),
            self.signals.relay_queued.load(Ordering::Acquire),
            self.callback.work.borrow().is_some(),
            self.callback.consumed.get(),
            self.signals.cancelled.load(Ordering::Acquire),
            self.elapsed().as_micros(),
        )
    }

    /// Distinguish actual completed ownership from a guessed queue boundary before any native weak loads.
    pub(super) fn has_completed(&self) -> bool {
        self.sealed
            && !self.signals.cancelled.load(Ordering::Acquire)
            && self.signals.remaining.load(Ordering::Acquire) == 0
            && self.signals.completed_after_ns.load(Ordering::Acquire) != u64::MAX
    }
}

impl Drop for NativeMarkdownDeallocationCohort {
    fn drop(&mut self) {
        if !self.sealed {
            // Construction failure releases its builder claim; surviving markers still retain no native source owner.
            self.signals.release_claim();
        }
    }
}

/// Verify actual runtime marker destruction and duplicate-role accounting before constructing TextKit.
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
    cohort.track(&*object, mtm);
    cohort.track(&*object, mtm);
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
    earlier.track(&*object, mtm);
    earlier.seal();
    closing.track(&*object, mtm);
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
    cancelled.track(&*object, mtm);
    preserved.track(&*object, mtm);
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
    println!(
        "native_deallocation_accounting=passed empty_graph duplicate_identity overlapping_cohorts cancelled_key actual_marker_drop"
    );
    Ok(())
}
