//! 真实Rust载荷与线程交错验证；不以可见行、对象抽样或等待时间推断完整释放。

use super::*;
use std::sync::Barrier;
use std::sync::atomic::AtomicUsize;

struct SourcePayload {
    bytes: Arc<Vec<u8>>,
    claim: FrozenValueClaim, // 字段顺序使正文引用先归还，claim最后才报告实际payload析构。
}

struct StringValue {
    source: Arc<SourcePayload>,
    claim: FrozenValueClaim, // 真实源先drop，值析构不能提前签发完成。
}

struct AnnotationPayload {
    source: Arc<StringValue>,
    runs: Arc<Vec<u8>>,
    claim: FrozenValueClaim, // 有限run载荷与源引用均先归还。
}

struct AnnotationValue {
    data: Arc<AnnotationPayload>,
    _claim: FrozenValueClaim, // 最后一个派生值归还共享Data后才结束此值claim。
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
        self.clone() // 只交付纯原子/Weak观察元数据，不增加真实载荷强引用。
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::Acquire)
    }
}

impl FrozenValueReleaseObserver for Observation {
    fn released(&self) {
        // 这两项实际Weak观察证明通知没有抢在载荷drop之前，也没有在family锁内回调。
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
        self.cancelled.load(Ordering::Acquire) // 只有取消标量，没有业务或重入调用。
    }
}

fn source(family: &Arc<FrozenValueFamily>) -> Arc<SourcePayload> {
    Arc::new(SourcePayload {
        bytes: Arc::new(vec![1, 2, 3, 4]), // 有限真实堆载荷；不读取用户文件或构造正文镜像。
        claim: family.claim_source().unwrap(),
    })
}

fn string(source: &Arc<SourcePayload>) -> Arc<StringValue> {
    Arc::new(StringValue {
        source: Arc::clone(source),
        claim: source.claim.claim_string_value().unwrap(),
    })
}

#[test]
fn all_four_real_destructors_and_payload_fields_precede_the_completion_callback() {
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
    // 字段被真实使用且仍持原source；不能用只有claim的替身冒充完整payload。
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
    drop(value); // 最后一份真实字段引用自然结束整条所有权链。
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
    family.seal_sources(); // 重复封闭没有第二个完成事实或第二次通知。
    assert_eq!(observation.hits(), 1);
}

#[test]
fn external_source_handle_keeps_real_bytes_alive_after_all_values_drop() {
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
    drop(external_source); // 外部源句柄被计入同一个payload，而不是提前按窗口根drop结算。
    assert!(weak_bytes.upgrade().is_none());
    assert_eq!(observation.hits(), 1);
}

#[test]
fn sealed_sources_still_allow_true_substrings_on_a_foundation_style_worker_thread() {
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
        let child = string(&worker_parent.source); // 先登记子值，父值始终保持真实源存活。
        drop(worker_parent);
        worker_created.wait();
        worker_release.wait();
        drop(child); // 最后一次析构在此实际工作线程发生，通知不要求主线程或定时器。
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
    // 成功安装字段之后的普通失败，和异常展开，都必须由真实字段Drop补偿。
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
    assert_eq!(family.snapshot().active[0], 1); // 取消只改变观察角色，绝不释放任何源claim。
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
    // 已消费的旧角色仍活着且明确取消时，可向新cohort交付同一事实，不重演析构。
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
    let family = FrozenValueFamily::new();
    family.seal_sources();
    let first = Observation::new(&family, Weak::new());
    family
        .observe(FrozenValueObserverRole::Editor, first.erased())
        .unwrap();
    assert_eq!(first.hits(), 1);
    drop(first); // Weak失效没有提供取消事件，不允许覆盖原角色。
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
    underflowing.state.lock().unwrap().active[0] = 0; // 只在测试内部注入不可能的重复归还前态。
    underflowing.seal_sources();
    // 先注入矛盾再归还的Drop不panic；最终零不再有完整析构权限。
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
    let family = FrozenValueFamily::new();
    let source = family.claim_source().unwrap();
    family.state.lock().unwrap().active[0] = 0;
    assert!(matches!(
        source.claim_string_value(),
        Err(FrozenValueFamilyError::MissingLiveParent)
    )); // 测试故意损坏内部计数；公开API不能构造一个没有父claim的子值。
    drop(source);
    family.seal_sources();
    assert_eq!(family.snapshot().constructed[2], 0);
    assert!(family.snapshot().sticky_failure);
    assert!(!family.snapshot().payload_value_destructors_complete);
}

#[test]
fn poisoned_state_never_authorizes_release_but_real_payload_drop_still_runs() {
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
        panic!("synthetic family poison"); // 真实Mutex毒化，不能只手动设置失败标志代替。
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
        panic!("synthetic observer violation"); // 验证第三方违约不会让Drop再次向外panic。
    }

    fn is_cancelled(&self) -> bool {
        false
    }
}

#[test]
fn observer_panic_is_sticky_and_cannot_escape_the_last_resource_destructor() {
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
