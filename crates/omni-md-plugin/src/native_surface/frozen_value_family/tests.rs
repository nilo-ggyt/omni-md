//! Exercise the family's Rust claim/Drop contracts with finite fixture payloads and real threads.
//! These carriers and observers are test implementations, not actual Frozen NSString/annotation
//! objects; their counters and Weak checks do not prove Native graph or whole-product release.

use super::*;
use std::sync::Barrier;
use std::sync::atomic::AtomicUsize;

struct SourcePayload {
    bytes: Arc<Vec<u8>>,
    claim: FrozenValueClaim, // Field order returns fixture byte ownership before its claim reports payload destruction.
}

struct StringValue {
    source: Arc<SourcePayload>,
    claim: FrozenValueClaim, // The fixture Source reference drops first, so this value claim cannot report completion before it.
}

struct AnnotationPayload {
    source: Arc<StringValue>,
    runs: Arc<Vec<u8>>,
    claim: FrozenValueClaim, // The finite fixture run payload and Source reference both drop before this claim.
}

struct AnnotationValue {
    data: Arc<AnnotationPayload>,
    _claim: FrozenValueClaim, // The last derived fixture value returns shared Data before ending this value claim.
}

struct Observation {
    hits: AtomicUsize,
    cancelled: AtomicBool,
    payload_alive_at_release: AtomicBool,
    complete_seen_inside_callback: AtomicBool,
    payload: Weak<Vec<u8>>,
    family: Weak<FrozenValueFamily>,
}

impl Observation {
    fn new(family: &Arc<FrozenValueFamily>, payload: Weak<Vec<u8>>) -> Arc<Self> {
        // This existing test observer retains only atomics and weak fixture/family metadata;
        // it cannot keep the byte payload alive merely by being registered.
        Arc::new(Self {
            hits: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            payload_alive_at_release: AtomicBool::new(false),
            complete_seen_inside_callback: AtomicBool::new(false),
            payload,
            family: Arc::downgrade(family),
        })
    }

    fn erased(self: &Arc<Self>) -> Arc<dyn FrozenValueReleaseObserver> {
        self.clone() // Transfer atomic/Weak observation metadata only, without another strong payload reference.
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::Acquire)
    }
}

impl FrozenValueReleaseObserver for Observation {
    fn released(&self) {
        // In this fixture, Weak byte observation checks Drop ordering, and snapshot reentry
        // checks that notification occurs outside the family mutex. Neither observes Native dealloc.
        self.payload_alive_at_release
            .store(self.payload.upgrade().is_some(), Ordering::Release);
        if let Some(family) = self.family.upgrade() {
            self.complete_seen_inside_callback.store(
                family.snapshot().payload_value_destructors_complete,
                Ordering::Release,
            );
        }
        self.hits.fetch_add(1, Ordering::AcqRel);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) // Cancellation scalar only, without business or reentrant calls.
    }
}

fn source(family: &Arc<FrozenValueFamily>) -> Arc<SourcePayload> {
    // Construct the existing finite Rust fixture carrier and install its real family claim.
    // This does not capture a Core snapshot or instantiate the native frozen Source adapter.
    Arc::new(SourcePayload {
        bytes: Arc::new(vec![1, 2, 3, 4]), // Finite heap fixture, without user files or a document-text mirror.
        claim: family.claim_source().unwrap(),
    })
}

fn string(source: &Arc<SourcePayload>) -> Arc<StringValue> {
    // The fixture retains the parent payload while deriving the corresponding actual family claim.
    Arc::new(StringValue {
        source: Arc::clone(source),
        claim: source.claim.claim_string_value().unwrap(),
    })
}

#[test]
fn all_four_real_destructors_and_payload_fields_precede_the_completion_callback() {
    // Exercise each family's claim kind with actual Rust field Drop. This validates metadata
    // settlement order for these fixture carriers, not Foundation construction or superclass teardown.
    let family = FrozenValueFamily::new();
    let source = source(&family);
    let source_weak = Arc::downgrade(&source.bytes);
    let string = string(&source);
    let data = Arc::new(AnnotationPayload {
        source: Arc::clone(&string),
        runs: Arc::new(vec![7, 8, 9]),
        claim: string.claim.claim_annotation_payload().unwrap(),
    });
    let runs_weak = Arc::downgrade(&data.runs);
    let value = AnnotationValue {
        data: Arc::clone(&data),
        _claim: data.claim.claim_annotation_value().unwrap(),
    };
    // Verify that the fixture actually retains its original Source chain rather than carrying
    // claims alone. The native adapters' own payload layout is a separate verification boundary.
    assert!(Arc::ptr_eq(&value.data.source, &string));
    let observation = Observation::new(&family, source_weak.clone());
    family
        .observe(FrozenValueObserverRole::Editor, observation.erased())
        .unwrap();
    family.seal_sources();
    assert_eq!(family.snapshot().active, [1, 1, 1, 1]);
    drop(source);
    drop(string);
    drop(data);
    assert_eq!(observation.hits(), 0);
    drop(value); // The last fixture field reference naturally ends this entire Rust ownership chain.
    assert!(source_weak.upgrade().is_none());
    assert!(runs_weak.upgrade().is_none());
    let snapshot = family.snapshot();
    assert!(snapshot.payload_value_destructors_complete);
    assert_eq!(snapshot.active, [0; 4]);
    assert_eq!(snapshot.constructed, [1; 4]);
    assert_eq!(snapshot.destructed, [1; 4]);
    assert_eq!(observation.hits(), 1);
    assert!(!observation.payload_alive_at_release.load(Ordering::Acquire));
    assert!(
        observation
            .complete_seen_inside_callback
            .load(Ordering::Acquire)
    );
    family.seal_sources(); // Repeated sealing creates neither another completion fact nor a second notification.
    assert_eq!(observation.hits(), 1);
}

#[test]
fn external_source_handle_keeps_real_bytes_alive_after_all_values_drop() {
    // An independent fixture Source handle keeps its heap payload and Source claim alive after
    // derived values end. Closing only the value/root cannot count as complete payload release.
    let family = FrozenValueFamily::new();
    let source = source(&family);
    let external_source = Arc::clone(&source);
    let weak_bytes = Arc::downgrade(&source.bytes);
    let value = string(&source);
    let observation = Observation::new(&family, weak_bytes.clone());
    family
        .observe(FrozenValueObserverRole::Editor, observation.erased())
        .unwrap();
    family.seal_sources();
    drop(value);
    drop(source);
    assert_eq!(family.snapshot().active, [1, 0, 0, 0]);
    assert!(weak_bytes.upgrade().is_some());
    assert_eq!(observation.hits(), 0);
    drop(external_source); // The external fixture handle belongs to this same payload; a window-root analogy cannot settle it early.
    assert!(weak_bytes.upgrade().is_none());
    assert_eq!(observation.hits(), 1);
}

#[test]
fn sealed_sources_still_allow_true_substrings_on_a_foundation_style_worker_thread() {
    // This is a real Rust worker and barrier sequence, not a Foundation substring invocation.
    // Existing Source lineage may derive another fixture value after new-source admission seals.
    let family = FrozenValueFamily::new();
    let source = source(&family);
    let parent = string(&source);
    let weak_bytes = Arc::downgrade(&source.bytes);
    let observation = Observation::new(&family, weak_bytes.clone());
    family
        .observe(FrozenValueObserverRole::Resources, observation.erased())
        .unwrap();
    let child_created = Arc::new(Barrier::new(2));
    let release_child = Arc::new(Barrier::new(2));
    let worker_parent = Arc::clone(&parent);
    let worker_created = Arc::clone(&child_created);
    let worker_release = Arc::clone(&release_child);
    family.seal_sources();
    assert!(matches!(
        family.claim_source(),
        Err(FrozenValueFamilyError::SourcesSealed)
    ));
    let worker = std::thread::spawn(move || {
        let child = string(&worker_parent.source); // Register the child while its parent still retains the original fixture Source.
        drop(worker_parent);
        worker_created.wait();
        worker_release.wait();
        drop(child); // Final fixture destruction occurs on this actual worker, without a main-thread requirement or timer.
    });
    child_created.wait();
    drop(parent);
    drop(source);
    assert_eq!(family.snapshot().active, [1, 0, 1, 0]);
    assert_eq!(observation.hits(), 0);
    release_child.wait();
    worker.join().unwrap();
    assert!(weak_bytes.upgrade().is_none());
    assert!(family.snapshot().payload_value_destructors_complete);
    assert_eq!(family.snapshot().constructed, [1, 0, 2, 0]);
    assert_eq!(family.snapshot().destructed, [1, 0, 2, 0]);
    assert_eq!(observation.hits(), 1);
}

#[test]
fn annotation_payload_and_values_can_be_derived_after_source_sealing() {
    // Direct claim derivation checks allowed lineage after sealing. It creates no native
    // dictionaries/annotations and therefore establishes no attributed-string behavior.
    let family = FrozenValueFamily::new();
    let source = source(&family);
    let string = string(&source);
    family.seal_sources();
    let data = string.claim.claim_annotation_payload().unwrap();
    let first = data.claim_annotation_value().unwrap();
    let second = data.claim_annotation_value().unwrap();
    assert_eq!(family.snapshot().active, [1, 1, 1, 2]);
    drop(first);
    drop(second);
    drop(data);
    drop(string);
    drop(source);
    assert_eq!(family.snapshot().constructed, [1, 1, 1, 2]);
    assert_eq!(family.snapshot().destructed, [1, 1, 1, 2]);
    assert!(family.snapshot().payload_value_destructors_complete);
}

#[test]
fn partial_initialization_and_unwinding_balance_real_installed_claims() {
    let family = FrozenValueFamily::new();
    let source = source(&family);
    // Ordinary rejection and Rust panic unwind return claims installed in these fixture fields.
    // This is not an Objective-C initialization failure or a Foundation exception catch.
    let rejected: Result<(), &'static str> = {
        let _partial = string(&source);
        Err("synthetic init rejected")
    };
    assert!(rejected.is_err());
    let panic = catch_unwind(AssertUnwindSafe(|| {
        let _partial = string(&source);
        panic!("synthetic init unwind");
    }));
    assert!(panic.is_err());
    assert_eq!(family.snapshot().active, [1, 0, 0, 0]);
    assert_eq!(family.snapshot().constructed, [1, 0, 2, 0]);
    assert_eq!(family.snapshot().destructed, [0, 0, 2, 0]);
    family.seal_sources();
    drop(source);
    assert!(family.snapshot().payload_value_destructors_complete);
}

#[test]
fn three_observers_cancel_replace_and_late_delivery_share_one_real_completion() {
    // The existing atomic test observers exercise role admission, explicit cancellation, and
    // late delivery of one family metadata fact without replaying payload destruction.
    let family = FrozenValueFamily::new();
    let source = source(&family);
    let old_editor = Observation::new(&family, Weak::new());
    let editor = Observation::new(&family, Weak::new());
    let full = Observation::new(&family, Weak::new());
    let resources = Observation::new(&family, Weak::new());
    family
        .observe(FrozenValueObserverRole::Editor, old_editor.erased())
        .unwrap();
    assert_eq!(
        family.observe(FrozenValueObserverRole::Editor, editor.erased()),
        Err(FrozenValueFamilyError::ObserverAlreadyRegistered)
    );
    old_editor.cancelled.store(true, Ordering::Release);
    family
        .observe(FrozenValueObserverRole::Editor, editor.erased())
        .unwrap();
    family
        .observe(FrozenValueObserverRole::FullGraph, full.erased())
        .unwrap();
    family.seal_sources();
    assert_eq!(family.snapshot().active[0], 1); // Cancellation changes observation authority only; no Source claim is released.
    drop(source);
    family
        .observe(FrozenValueObserverRole::Resources, resources.erased())
        .unwrap();
    assert_eq!(old_editor.hits(), 0);
    assert_eq!(editor.hits(), 1);
    assert_eq!(full.hits(), 1);
    assert_eq!(resources.hits(), 1);
    assert_eq!(
        family.observe(FrozenValueObserverRole::Resources, resources.erased()),
        Err(FrozenValueFamilyError::ObserverAlreadyRegistered)
    );
    // An explicitly cancelled, still-live consumed role may transfer the same completed fact
    // to its replacement; this does not repeat any payload destructor.
    resources.cancelled.store(true, Ordering::Release);
    let replacement = Observation::new(&family, Weak::new());
    family
        .observe(FrozenValueObserverRole::Resources, replacement.erased())
        .unwrap();
    assert_eq!(replacement.hits(), 1);
    assert_eq!(family.snapshot().constructed, [1, 0, 0, 0]);
}

#[test]
fn dead_consumed_observer_is_not_mistaken_for_explicit_cancellation() {
    // Expiration alone supplies no cancellation fact, even when family completion occurred
    // before registration. The consumed role must remain protected from replacement.
    let family = FrozenValueFamily::new();
    family.seal_sources();
    let first = Observation::new(&family, Weak::new());
    family
        .observe(FrozenValueObserverRole::Editor, first.erased())
        .unwrap();
    assert_eq!(first.hits(), 1);
    drop(first); // Weak expiration supplies no cancellation event and permits no overwrite of the original role.
    let second = Observation::new(&family, Weak::new());
    assert_eq!(
        family.observe(FrozenValueObserverRole::Editor, second.erased()),
        Err(FrozenValueFamilyError::ObserverAlreadyConsumed)
    );
    assert_eq!(second.hits(), 0);
    assert!(family.snapshot().payload_value_destructors_complete);
}

#[test]
fn already_cancelled_registration_does_not_consume_the_original_role() {
    // Reject an unauthorized observer before changing its slot, then allow a fresh authorized
    // observer to receive the actual later metadata completion.
    let family = FrozenValueFamily::new();
    let cancelled = Observation::new(&family, Weak::new());
    cancelled.cancelled.store(true, Ordering::Release);
    assert_eq!(
        family.observe(FrozenValueObserverRole::Editor, cancelled.erased()),
        Err(FrozenValueFamilyError::ObserverCancelled)
    );
    let valid = Observation::new(&family, Weak::new());
    family
        .observe(FrozenValueObserverRole::Editor, valid.erased())
        .unwrap();
    family.seal_sources();
    assert_eq!(valid.hits(), 1);
    assert_eq!(cancelled.hits(), 0);
}

#[test]
fn invalid_parent_kind_permanently_blocks_completion_after_real_zero() {
    // This direct invalid claim operation exercises the existing factory contract. Actual
    // subsequent zero ownership cannot restore evidence after that ownership-kind violation.
    let family = FrozenValueFamily::new();
    let source = family.claim_source().unwrap();
    assert!(matches!(
        source.claim_annotation_payload(),
        Err(FrozenValueFamilyError::InvalidParentKind)
    ));
    family.seal_sources();
    drop(source);
    assert_eq!(family.snapshot().active, [0; 4]);
    assert!(family.snapshot().sticky_failure);
    assert!(!family.snapshot().payload_value_destructors_complete);
}

#[test]
fn checked_counter_overflow_and_underflow_cannot_turn_into_completion() {
    // Existing private-state injections expose impossible accounting states. They validate
    // sticky rejection, not real resource exhaustion or externally delivered destruction events.
    let overflowing = FrozenValueFamily::new();
    overflowing.state.lock().unwrap().constructed[0] = usize::MAX;
    assert!(matches!(
        overflowing.claim_source(),
        Err(FrozenValueFamilyError::CounterOverflow)
    ));
    overflowing.seal_sources();
    assert_eq!(overflowing.snapshot().active, [0; 4]);
    assert!(overflowing.snapshot().sticky_failure);
    assert!(!overflowing.snapshot().payload_value_destructors_complete);

    let underflowing = FrozenValueFamily::new();
    let source = underflowing.claim_source().unwrap();
    underflowing.state.lock().unwrap().active[0] = 0; // Test-only impossible pre-state for a repeated settlement.
    underflowing.seal_sources();
    // Drop after the injected contradiction must not panic; eventual zero cannot recover
    // permission to report complete payload/value destruction.
    drop(source);
    assert!(underflowing.snapshot().sticky_failure);
    assert!(!underflowing.snapshot().payload_value_destructors_complete);

    let destructor_overflow = FrozenValueFamily::new();
    let source = destructor_overflow.claim_source().unwrap();
    destructor_overflow.state.lock().unwrap().destructed[0] = usize::MAX;
    destructor_overflow.seal_sources();
    drop(source);
    assert_eq!(destructor_overflow.snapshot().active, [0; 4]);
    assert!(destructor_overflow.snapshot().sticky_failure);
    assert!(
        !destructor_overflow
            .snapshot()
            .payload_value_destructors_complete
    );
}

#[test]
fn inconsistent_live_parent_is_rejected_without_registering_an_orphan_value() {
    // This retained test deliberately corrupts a private parent count. A borrowed real claim
    // cannot reach this contradictory state through the public derivation API.
    let family = FrozenValueFamily::new();
    let source = family.claim_source().unwrap();
    family.state.lock().unwrap().active[0] = 0;
    assert!(matches!(
        source.claim_string_value(),
        Err(FrozenValueFamilyError::MissingLiveParent)
    )); // The injected count corruption must not register a child without its required live parent.
    drop(source);
    family.seal_sources();
    assert_eq!(family.snapshot().constructed[2], 0);
    assert!(family.snapshot().sticky_failure);
    assert!(!family.snapshot().payload_value_destructors_complete);
}

#[test]
fn poisoned_state_never_authorizes_release_but_real_payload_drop_still_runs() {
    // A real Rust thread panic while holding the family mutex poisons actual synchronization.
    // Fixture bytes still drop, but the lost accounting proof must suppress successful notification.
    let family = FrozenValueFamily::new();
    let source = source(&family);
    let weak_bytes = Arc::downgrade(&source.bytes);
    let observation = Observation::new(&family, weak_bytes.clone());
    family
        .observe(FrozenValueObserverRole::Editor, observation.erased())
        .unwrap();
    let worker_family = Arc::clone(&family);
    let worker = std::thread::spawn(move || {
        let _state = worker_family.state.lock().unwrap();
        panic!("synthetic family poison"); // Actual mutex poison, rather than manually setting only the family's failure flag.
    });
    assert!(worker.join().is_err());
    family.seal_sources();
    drop(source);
    assert!(weak_bytes.upgrade().is_none());
    assert_eq!(family.snapshot().active, [0; 4]);
    assert!(family.snapshot().sticky_failure);
    assert!(!family.snapshot().payload_value_destructors_complete);
    assert_eq!(observation.hits(), 0);
}

struct PanickingObserver;

impl FrozenValueReleaseObserver for PanickingObserver {
    fn released(&self) {
        panic!("synthetic observer violation"); // Existing violating test observer checks containment of panic on the Drop stack.
    }

    fn is_cancelled(&self) -> bool {
        // Keep this fixture admitted so its deliberate release-callback violation is exercised.
        false
    }
}

#[test]
fn observer_panic_is_sticky_and_cannot_escape_the_last_resource_destructor() {
    // The deliberately panicking test observer must not interrupt final fixture payload Drop.
    // Its violation leaves a permanently failed metadata receipt despite actual zero ownership.
    let family = FrozenValueFamily::new();
    let source = source(&family);
    let weak_bytes = Arc::downgrade(&source.bytes);
    family
        .observe(FrozenValueObserverRole::Editor, Arc::new(PanickingObserver))
        .unwrap();
    family.seal_sources();
    assert!(catch_unwind(AssertUnwindSafe(|| drop(source))).is_ok());
    assert!(weak_bytes.upgrade().is_none());
    assert_eq!(family.snapshot().active, [0; 4]);
    assert!(family.snapshot().sticky_failure);
    assert!(!family.snapshot().payload_value_destructors_complete);
}
