//! Observes actual native deallocation without retaining any tracked graph object.
//!
//! Public Objective-C associations retain timestamp/counter markers, never their source object.
//! A unique-identity cohort queues one main-thread callback after the last actual release stack.
//! The callback still has to perform the complete strict weak graph verification.

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
    /// 仅原生合同检查自身关联是否仍存在；不读取正文或推断原对象类型。
    fn objc_getAssociatedObject(object: *const AnyObject, key: *const c_void) -> *const AnyObject;
}

/// 共同 ledger 至多登记1024身份，另有少量显式TextKit根；cohort额外设有限防御上限。
/// 超限保持观察失败，绝不能截断清单后认证关闭；这里不增加第二份业务或Native强owner。
const MAX_NATIVE_COHORT_IDENTITIES: usize = 2_048;

/// Main-thread completion metadata contains no view, document, mapping or worker owner.
struct NativeDeallocationCallback {
    work: RefCell<Option<Box<dyn FnOnce()>>>,
    consumed: Cell<bool>,
    // 显式原生验收只唤醒一个原场景，不消费父close唯一的生产handler。
    #[cfg(feature = "native-markdown-runtime-probe")]
    probe_wake: RefCell<Option<Box<dyn FnOnce()>>>,
}

/// Atomic counters can be notified by native destruction without touching main-thread Rust state.
struct NativeDeallocationSignals {
    remaining: AtomicUsize,
    relay_queued: AtomicBool,
    cancelled: AtomicBool,
    // 外部不可变值登记失败仍保留事实；最终零计数不能修复不完整的资源观察。
    external_failed: AtomicBool,
    callback: MainThreadBound<std::rc::Weak<NativeDeallocationCallback>>,
    started: Instant,
    completed_after_ns: AtomicU64,
}

/// 家族只持这个原子观察者，既不保Native对象，也不捕获正文、映射或主线程业务闭包。
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
            // 同一原子观察槽只能消费一次真实家族析构事件；重复不能再减计数冒充资源释放。
            self.signals.external_failed.store(true, Ordering::Release);
            return;
        }
        // seal且所有具体源/值claim真实Drop后，家族才发此事件。它只归还本cohort的外部观察claim。
        // 此处可能在Foundation后台析构栈中；原signals仅排主线程续体，仍须核完整既有弱图。
        self.signals.release_claim();
    }

    fn is_cancelled(&self) -> bool {
        self.signals.cancelled.load(Ordering::Acquire) // 不获取家族锁或访问任何Native/正文对象。
    }
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
                // 两个原槽都已取出且没有借用；验收先消费真实完成后，未来父close仍可
                // 给尚未消费的生产槽安装handler，并从同一remaining==0事实重新排原relay。
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
                // relay入口已核真实释放，且本次观察已从原槽取出。生产handler正常消费
                // 原cohort会执行Drop并撤销后续观察，却不能抹掉本次已经发生的释放事实。
                // 只有work正常返回才到这里；panic不经finally交付。观察者仍须核自己原弱
                // owner与在途终点，不能借此通知复活已关闭的业务或把资源完成当作安装成功。
                wake();
            } // 进入relay以前的取消仍在上方拒绝；所有闭包始终在槽借用之外归还。
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
    // 这些Weak仅用于cohort结束时卸载自己的marker；原对象寿命不受本集合影响。
    observed: Vec<Weak<AnyObject>>,
    sealed: bool,
}

/// 一份真实外部资源的归还义务；只保cohort原子事实，不保资源本身或主线程业务图。
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
    /// 调用方已经完成真实资源归还；取走原claim后，重复析构不能再发送完成。
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
            // 未获真实归还就丢弃观察是失败，不把元数据Drop冒充资源成功结束。
            signals.external_failed.store(true, Ordering::Release);
            signals.release_claim();
        }
    }
}

impl NativeMarkdownDeallocationCohort {
    /// 在seal前接纳一个外部资源义务；最后真实归还仍走同一主线程一次relay。
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
    /// cfg只读signals；没有fetch/sub/swap、回调安装或释放授权，不替代has_completed。
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

    /// 在原角色cohort中登记一个完整家族事实；动态派生值由家族的真实claim逐一结算。
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
            // 计数不再精确时不能签发关闭完成；不制造没有对应计数的新观察者。
            self.signals.external_failed.store(true, Ordering::Release);
            return;
        }
        let observer: Arc<dyn FrozenValueReleaseObserver> = Arc::new(FrozenValueCohortObserver {
            signals: Arc::clone(&self.signals),
            delivered: AtomicBool::new(false),
        });
        if family.observe(role, observer).is_err() {
            // 登记没有成立，归还的是该次临时登记claim。失败标量永久禁止把此cohort认证为完成。
            self.signals.external_failed.store(true, Ordering::Release);
            self.signals.release_claim();
        }
    }

    /// 每个实际原生身份安装本cohort自己的关联；保留Weak只为对称卸载，绝不保活原对象。
    pub(super) fn track(&mut self, object: &AnyObject, mtm: MainThreadMarker) {
        assert!(!self.sealed, "native cohort registration already ended");
        let address = std::ptr::from_ref(object).addr();
        if self.identities.contains(&address) {
            // 构造阶段地址也可能复用；只有同地址的旧Weak仍真活着才是同一原生生命周期。
            let duplicate = self.observed.iter().any(|weak| {
                weak.load()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq(actual, object))
            });
            if duplicate {
                return; // 同一对象的多个角色只产生一个marker/真实析构claim。
            }
        }
        if self.observed.len() >= MAX_NATIVE_COHORT_IDENTITIES {
            self.signals.external_failed.store(true, Ordering::Release);
            return; // 失败是永久事实，不安装无法在结束时精确卸载的新关联。
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
            return; // 计数无法表达时不制造没有配对claim的marker。
        }
        // 入集合在安装关联之前完成；Native构造/释放可重入，最终Drop仍能找到本次原身份。
        self.identities.insert(address);
        self.observed.push(Weak::new(object));
        let marker =
            NativeMarkdownDeallocationMarker::alloc(mtm).set_ivars(NativeDeallocationMarkerIvars {
                signals: Arc::clone(&self.signals),
            });
        // SAFETY: 标准NSObject初始化前全部ivars已安装；marker仅含Send/Sync原子元数据。
        let marker: Retained<NativeMarkdownDeallocationMarker> =
            unsafe { msg_send![super(marker), init] };
        // SAFETY: 公开runtime ABI；唯一Arc地址只标识本cohort的key。原对象retain marker，
        // marker没有原对象回链；Drop卸载也只移除此key，不能触碰其他cohort或原对象内容。
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
        // 地址索引只服务构造去重；剩余有限Weak保留到cohort结束，以便卸载未析构原值上的本marker。
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

    /// 只给既有Source场景安装一次有限唤醒；实际零claim沿同一relay交付，生产完成槽仍归父close。
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
        drop(work); // 回调捕获的析构允许重新借槽或调用Native；此时全部RefCell借用已经结束。
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
        // 先撤权，再析构任何回调捕获或关联marker；主动卸载只完成观察清理，不是原对象析构收据。
        self.signals.cancelled.store(true, Ordering::Release);
        let work = self.callback.work.borrow_mut().take();
        self.callback.consumed.set(true);
        #[cfg(feature = "native-markdown-runtime-probe")]
        let probe_wake = self.callback.probe_wake.borrow_mut().take();
        #[cfg(feature = "native-markdown-runtime-probe")]
        drop(probe_wake);
        drop(work); // 必须在借用外释放，捕获的Drop可能重入同一原生/资源边界。
        let observed = std::mem::take(&mut self.observed);
        for weak in observed {
            if let Some(object) = weak.load() {
                // SAFETY: 原Weak本次临时升级保证接收者活着；只清稳定原signals地址的本key。
                // 本marker真实析构会配平自己的计数，但cancelled已经永久禁止完成与relay授权。
                unsafe {
                    objc_setAssociatedObject(
                        std::ptr::from_ref(&*object),
                        Arc::as_ptr(&self.signals).cast(),
                        std::ptr::null(),
                        1,
                    );
                }
            } // 已真实nil的对象无需清理；cancel_observation已移除的key也不会再次析构marker。
        }
        if !self.sealed {
            // 构造失败仍要配平builder；取消已在所有Native清理之前生效，不能产生成功完成信号。
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
        // 普通probe没有冻结值构造者；只在真实家族路径或同源单测验证这两个释放反例。
        // 三个关闭角色必须等待同一真实家族，不能只等原始SourceClaim或任意一个父对象结束。
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
        // 同一角色的旧观察只有明确取消才可替换；不会把旧取消收据重新解释为释放。
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
            "" // 未编译的家族检查不进入成功标签，原Native marker检查仍完整执行。
        }
    );
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// 回调取消的实际析构检查；只持Rust弱槽与有限标量，不持被观察Native或业务正文。
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
        self.borrow_clear.set(clear); // 真正在捕获析构中借槽，不能靠源码顺序或标量假设通过。
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// 只取得原对象上本key实际marker的Weak；调用期间原对象和关联本身提供临时生命期。
fn associated_marker_weak(
    object: &AnyObject,
    signals: &Arc<NativeDeallocationSignals>,
) -> Result<Weak<AnyObject>, String> {
    // SAFETY: 接收者为本合同持有的活NSObject，key为仍活Arc的原地址；仅读取本观察关联。
    let marker = unsafe {
        objc_getAssociatedObject(std::ptr::from_ref(object), Arc::as_ptr(signals).cast())
    };
    // SAFETY: 非空结果仍由活原对象的同key关联拥有，本同步调用内不移除该关联。
    let marker = unsafe { marker.as_ref() }.ok_or("原生合同没有取得实际关联marker")?;
    Ok(Weak::new(marker))
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// 在仍活原对象上Drop一份cohort，核其marker与signals真正归还且另一cohort仍独立等待。
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
            // 此闭包绝不该执行；捕获probe的真实Drop验证取消不会在RefCell借用内析构业务捕获。
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
            // 原对象仍活时取消观察，验收槽同样只归还捕获，不能投递一次伪完成。
            drop(wake_probe);
            called.set(true);
        });
        if explicitly_cancelled {
            discarded.cancel_observation(Some(&*object));
            if discarded.has_completed() || signals.remaining.load(Ordering::Acquire) != 0 {
                return Err("显式取消被错误解释为原对象实际析构".into());
            }
        } // 另一分支直接Drop仍在等待的cohort，不能依赖调用者先显式取消。
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
