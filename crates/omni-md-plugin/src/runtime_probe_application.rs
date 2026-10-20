//! Shared standalone probe driver with real native event/pool boundaries and strict graph release.

use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2::{ClassType, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSEvent, NSEventModifierFlags, NSEventType,
};
use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Instant;

/// 全驱动最多192条；最后一条专用于明确溢出，不能将不完整诊断当完整阶段证据。
const MAX_PROBE_STAGE_RECORDS: u16 = 208;

/// 只管理日志准入，不影响原驱动是否执行、成功、失败或等待资源。
#[derive(Default)]
struct ProbeStageBudget {
    emitted: u16,
}

impl ProbeStageBudget {
    /// 有限普通点之后至多一个溢出点，后续永久静默，不通过新case重置预算。
    fn admit(&mut self) -> Option<(u16, bool)> {
        if self.emitted >= MAX_PROBE_STAGE_RECORDS {
            return None; // 预算耗尽只停止观测，原动作和门禁保持原样。
        }
        self.emitted += 1;
        Some((self.emitted, self.emitted == MAX_PROBE_STAGE_RECORDS))
    }
}

/// 时间原点在唯一application构造之前建立，只含Rust时钟和标量。
struct ProbeStageTrace {
    origin: Instant,
    budget: ProbeStageBudget,
}

thread_local! {
    // 模块本身仅native-markdown-runtime-probe构建存在，default产品无该TLS/日志成本。
    static PROBE_STAGE_TRACE: RefCell<Option<ProbeStageTrace>> = const { RefCell::new(None) };
}

/// 固定阶段名称不会接收正文、路径、Native身份地址或任意错误文本。
#[derive(Clone, Copy)]
enum ProbeStageBoundary {
    PostReturned,
    DeliveryEnter,
    StageReturned,
    DispatchEntered,
}

impl ProbeStageBoundary {
    /// router交付位置紧邻run_stage，因此delivery和enter合成一条而非重复占预算。
    fn label(self) -> &'static str {
        match self {
            Self::PostReturned => "post_return", // 原postEvent同步调用已经返回。
            Self::DeliveryEnter => "delivery_enter", // 原router持有的driver即将进入原stage。
            Self::StageReturned => "stage_return", // 原stage完整处理及后继排队均已返回。
            Self::DispatchEntered => "dispatch_enter", // 既有main-dispatch回调已实际开始执行。
        }
    }
}

/// 日志只消耗当前callback已拥有的case/stage/result标量，不升级弱owner或调用Native getter。
fn trace_probe_stage(
    case: Option<usize>,
    stage: i16,
    boundary: ProbeStageBoundary,
    success: Option<bool>,
) {
    let record = PROBE_STAGE_TRACE.with_borrow_mut(|slot| {
        let trace = slot.as_mut()?;
        let (sequence, exhausted) = trace.budget.admit()?;
        Some((sequence, trace.origin.elapsed().as_nanos(), exhausted))
    });
    let Some((sequence, elapsed_ns, exhausted)) = record else {
        return; // 未初始化或已耗尽不产生输出，绝不改变驱动动作。
    };
    // TLS借用已经结束；输出本身不会持有driver、文档、窗口或字段。
    let event = if exhausted {
        "budget_exhausted"
    } else {
        boundary.label()
    };
    eprintln!(
        "native_driver_stage seq={sequence} elapsed_ns={elapsed_ns} case={case:?} stage={stage} event={event} ok={success:?}"
    );
}

/// 固定入口决定唯一验收场景；运行期间不切换，也不允许外部指定任意夹具规模。
#[derive(Clone, Copy)]
enum ProbeScenario {
    Full,
    SemanticSnapshot,
}

/// 保存固定场景、合成文件身份与当前生命周期阶段所需的唯一资源图。
struct ProbeIvars {
    path: PathBuf,
    scenario: ProbeScenario,
    started: Cell<bool>,
    pending_session: RefCell<Option<crate::NativeMarkdownRuntimeProbeSession>>,
    pending_release: RefCell<Option<crate::NativeMarkdownRuntimeProbeCompletion>>,
    release_deadline: RefCell<Option<Retained<objc2_foundation::NSTimer>>>,
    result: RefCell<Option<Result<(), String>>>,
    running: Cell<bool>,
    deferred_table_terminal: Cell<Option<i16>>,
    expected_table_terminal: Cell<Option<i16>>,
}

/// 原请求回调只发送声明的后继事件，不升级owner或推进任何Core/布局工作。
pub(crate) fn post_runtime_table_cell_terminal(stage: i16) {
    let mtm = MainThreadMarker::new().expect("表格终点位于真实主线程");
    post_probe_event(stage, None, mtm);
}

define_class!(
    // SAFETY: NSObject has no additional subclass invariants. Every callback and native handle
    // stays on the process's actual AppKit main thread, while the application owns this delegate.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ProbeIvars]
    struct OmniMarkdownProbeDelegate;

    // SAFETY: the initialized NSObject subclass keeps identity and thread confinement unchanged.
    unsafe impl NSObjectProtocol for OmniMarkdownProbeDelegate {}
    // SAFETY: the retained delegate outlives NSApplication.run and implements its launch callback.
    unsafe impl NSApplicationDelegate for OmniMarkdownProbeDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _notification: &NSNotification) {
            // A repeated native notification cannot create another editing session.
            if self.ivars().started.replace(true) {
                return;
            }
            post_probe_event(100, None, self.mtm());
        }
    }
);

impl OmniMarkdownProbeDelegate {
    /// Advances one finite phase without unwinding a Rust panic through an Objective-C callback.
    fn run_stage(&self, stage: i16) {
        if self.ivars().result.borrow().is_some() {
            return;
        }
        if self.ivars().running.get() {
            if self.ivars().expected_table_terminal.get() == Some(stage)
                && self
                    .ivars()
                    .deferred_table_terminal
                    .replace(Some(stage))
                    .is_none()
            {
                return; // Native析构的嵌套loop只暂存这一次真实终点，owner放回后才重投。
            }
            self.finish_probe(
                Err("原生表格阶段在owner借出期间重复或乱序交付".into()),
                self.mtm(),
            );
            return;
        }
        if matches!(stage, 111..=113) {
            if self.ivars().expected_table_terminal.take() != Some(stage) {
                self.finish_probe(Err("原生表格终点与原阶段不符".into()), self.mtm());
                return;
            }
            self.cancel_release_failure_deadline();
        }
        self.ivars().running.set(true);
        self.run_stage_inner(stage);
        self.ivars().running.set(false);
        if self.ivars().result.borrow().is_some() {
            let session = self.ivars().pending_session.borrow_mut().take();
            if let Some(session) = session.as_ref() {
                session.cancel_table_exercise();
            }
            *self.ivars().pending_session.borrow_mut() = session;
        }
        let deferred = self.ivars().deferred_table_terminal.take();
        if self.ivars().result.borrow().is_none()
            && let Some(next) = deferred
        {
            post_probe_event(next, None, self.mtm());
        }
    }

    fn run_stage_inner(&self, stage: i16) {
        let mtm = self.mtm();
        // Local native temporaries drain before the stage result is published or the successor queued.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            autoreleasepool(|_| {
                match stage {
                    // Open owns the mounted graph until a later native event begins the input exercise.
                    100 => {
                        let session =
                            crate::run_native_markdown_runtime_probe(&self.ivars().path, mtm)?;
                        *self.ivars().pending_session.borrow_mut() = Some(session);
                        Ok(())
                    }
                    // Take ownership out of RefCell before reentrant AppKit input/layout operations.
                    101 => {
                        let session = self
                            .ivars()
                            .pending_session
                            .borrow_mut()
                            .take()
                            .ok_or("the mounted probe session is missing")?;
                        let result = match self.ivars().scenario {
                            // 既有完整合同仍使用短夹具，继续执行原输入、格式与表格阶段。
                            ProbeScenario::Full => session.exercise(),
                            // 长夹具只验证真实属性窗口替换，不借用完整合同的同步输入前提。
                            ProbeScenario::SemanticSnapshot => session.exercise_semantic_snapshot(),
                        };
                        *self.ivars().pending_session.borrow_mut() = Some(session);
                        result
                    }
                    110..=113 => {
                        if stage < 113 {
                            self.ivars().expected_table_terminal.set(Some(stage + 1));
                        }
                        let session = self
                            .ivars()
                            .pending_session
                            .borrow_mut()
                            .take()
                            .ok_or("表格回归owner缺失")?;
                        let result = session.exercise_table_stage(stage);
                        *self.ivars().pending_session.borrow_mut() = Some(session);
                        result
                    }
                    // Teardown runs after input callbacks and their event/temporary pools have returned.
                    102 => {
                        let session = self
                            .ivars()
                            .pending_session
                            .borrow_mut()
                            .take()
                            .ok_or("the exercised probe session is missing")?;
                        let completion = session.close()?;
                        let weak = dispatch2::MainThreadBound::new(Weak::from(self), mtm);
                        completion.when_native_objects_released(move || {
                            let mtm = MainThreadMarker::new()
                                .expect("native release signals use the main queue");
                            if weak.get(mtm).load().is_some() {
                                // Actual last-native-object deallocation grants one verification event, never an elapsed retry.
                                post_probe_event(103, None, mtm);
                            }
                        });
                        // Publish the weak token only after reentrant native teardown has returned.
                        *self.ivars().pending_release.borrow_mut() = Some(completion);
                        self.arm_release_failure_deadline(stage);
                        Ok(())
                    }
                    // Consume weak evidence once: a surviving graph fails immediately without retries.
                    103 => {
                        let completion = self
                            .ivars()
                            .pending_release
                            .borrow_mut()
                            .take()
                            .ok_or("the release token is missing")?;
                        // End RefMut before native weak loads can retain/release their observed objects.
                        completion.verify_released(mtm)
                    }
                    // The application router rejects every other fixture event subtype.
                    _ => Err("unknown native probe stage".into()),
                }
            })
        }))
        .unwrap_or_else(|_| {
            // Convert a caught Rust panic to failure before returning through the Objective-C callback.
            Err("The native probe panicked inside its AppKit callback".into())
        });
        if self.ivars().result.borrow().is_some() {
            return;
        } // 嵌套终点已失败，不再发布后继或新期限。
        let succeeded = result.is_ok(); // 只复制状态位，原Result仍由既有match原样消费。
        match result {
            // Every observed contract and weak graph assertion passed before the loop stops.
            Ok(()) if stage == 103 => {
                self.cancel_release_failure_deadline();
                match self.ivars().scenario {
                    ProbeScenario::Full => {
                        // 独立 TextKit 机制参考至此完成；九项真实资源场景由正式 Host 组合根逐案执行。
                        self.finish_probe(Ok(()), mtm);
                    }
                    ProbeScenario::SemanticSnapshot => {
                        // 只有上一阶段真实关闭且本阶段严格弱图验证成功，才发布独立场景收据。
                        eprintln!("omni_semantic_snapshot_close verified=true strict_release=true");
                        self.finish_probe(Ok(()), mtm);
                    }
                }
            }
            // Actual native cohort completion owns the only transition from close to strict verification.
            Ok(()) if stage == 102 => {}
            Ok(()) if matches!(stage, 110..=112) => {
                self.arm_release_failure_deadline(stage); // 只等真实激活终点，时钟不能授予成功。
            }
            Ok(()) if stage == 101 => {
                let next = match self.ivars().scenario {
                    ProbeScenario::Full => 110, // 保留既有四个原生表格阶段及其真实终点。
                    ProbeScenario::SemanticSnapshot => 102, // 独立属性回归完成后立即走同一真实关闭路径。
                };
                post_probe_event(next, None, mtm);
            }
            Ok(()) if stage == 113 => {
                post_probe_event(102, None, mtm); // 完整场景的最后一个表格终点才允许关闭。
            }
            // Hand through the actual main queue so earlier AppKit initialization/input work runs
            // before the next input event; no nested run loop, sleep, poll or conditional retry exists.
            Ok(()) => dispatch2::DispatchQueue::main().exec_async(move || {
                // This real main-queue turn precedes the next application event without retaining a graph.
                let mtm =
                    MainThreadMarker::new().expect("the dispatch main queue is the AppKit thread");
                trace_probe_stage(None, stage + 1, ProbeStageBoundary::DispatchEntered, None);
                post_probe_event(stage + 1, None, mtm);
            }),
            // Failures preserve their exact phase and stop the real application loop immediately.
            Err(error) => self.finish_probe(Err(format!("stage {stage}: {error}")), mtm),
        }
        trace_probe_stage(
            None,
            stage,
            ProbeStageBoundary::StageReturned,
            Some(succeeded),
        );
    }

    /// 在每个已完成场景撤销唯一期限，不把历史 timer 留到后面的场景误报失败。
    fn cancel_release_failure_deadline(&self) {
        let deadline = self.ivars().release_deadline.borrow_mut().take();
        if let Some(deadline) = deadline {
            // borrow 已结束；AppKit 公开 invalidate 撤销后续回调权限。
            deadline.invalidate();
        }
    }

    /// Bound a missing native completion with one failure-only deadline, never a graph retry or acceptance delay.
    fn arm_release_failure_deadline(&self, originating_stage: i16) {
        let weak = dispatch2::MainThreadBound::new(Weak::from(self), self.mtm());
        let block = block2::RcBlock::new(move |_timer: NonNull<objc2_foundation::NSTimer>| {
            let mtm = MainThreadMarker::new().expect("the probe deadline uses the main run loop");
            let Some(delegate) = weak.get(mtm).load() else {
                // A completed standalone driver cannot be revived by a late failure-only timer delivery.
                return;
            };
            let unfinished = delegate.ivars().result.borrow().is_none();
            if unfinished {
                // Take weak evidence out of state before one terminal diagnostic can release temporary
                // native upgrades. An absent signal remains failure; there is no graph retry or allowance.
                let completion = delegate.ivars().pending_release.borrow_mut().take();
                let diagnostic = completion
                    .as_ref()
                    .map(|completion| completion.failure_diagnostic(mtm));
                // 期限同样用于单元格激活：必须带原阶段，不能把尚未关闭时的激活超时误报成资源泄漏。
                // 2秒门槛、一次终止和无成功重试规则均保持，诊断不授予任何产品权限。
                delegate.finish_probe(Err(format!("native probe stage={originating_stage} missed its original terminal within the probe's 2 s failure deadline; {}", diagnostic.as_deref().unwrap_or("terminal evidence missing"))), mtm);
            }
        });
        // SAFETY: the nonrepeating block captures only a main-thread-bound weak delegate, no native graph.
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                2.0, false, &block,
            )
        };
        *self.ivars().release_deadline.borrow_mut() = Some(timer);
    }

    /// Records one terminal result and wakes NSApplication's native event wait after stopping it.
    fn finish_probe(&self, result: Result<(), String>, mtm: MainThreadMarker) {
        if self.ivars().result.borrow().is_some() {
            return;
        }
        let failed = result.is_err();
        *self.ivars().result.borrow_mut() = Some(result); // 任何Native清理前先封闭终点，保最早失败。
        if failed {
            let session = self.ivars().pending_session.borrow_mut().take();
            if let Some(session) = session.as_ref() {
                session.cancel_table_exercise();
            }
            *self.ivars().pending_session.borrow_mut() = session;
        }
        let deadline = self.ivars().release_deadline.borrow_mut().take();
        if let Some(deadline) = deadline {
            // End the timer-state borrow before native invalidation releases its copied weak block.
            deadline.invalidate();
        }
        NSApplication::sharedApplication(mtm).stop(None);
        post_probe_event(0, None, mtm);
    }
}

// TLS只持Rust Rc包裹的Native弱引用；原生弱操作和临时Retained释放都在借用外。
thread_local! {
    static PROBE_DELEGATE: RefCell<Option<Rc<Weak<OmniMarkdownProbeDelegate>>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: NSApplication retains its standard initialization and real main-thread event loop.
    // The subclass has no ivars; system events are forwarded unchanged to its superclass.
    #[unsafe(super = NSApplication)]
    #[thread_kind = MainThreadOnly]
    struct OmniMarkdownProbeApplication;

    impl OmniMarkdownProbeApplication {
        #[unsafe(method(sendEvent:))]
        fn send_probe_event(&self, event: &NSEvent) {
            // Only four isolated fixture events belong to the test; every stage executes once.
            if event.r#type() == NSEventType::ApplicationDefined && ((100..=103).contains(&event.subtype().0) || (110..=113).contains(&event.subtype().0)) {
                let weak_owner = PROBE_DELEGATE.with_borrow(|slot| {
                    // 仅增加Rust Rc计数；不克隆Native Weak、不升级或释放任何Native对象。
                    slot.as_ref().map(Rc::clone)
                });
                // TLS借用已经结束，原生弱升级即使同步重入也不会撞到该slot借用。
                let delegate = weak_owner.as_ref().and_then(|owner| owner.load());
                drop(weak_owner); // 最后一个Rc可能销毁Native Weak，必须同样在借用外。
                let Some(delegate) = delegate else {
                    // A fixture event queued after cleanup has no authority to revive the finished driver.
                    return;
                };
                // The router borrow ended before callbacks can reenter native or project state.
                let stage = event.subtype().0;
                let case = None;
                trace_probe_stage(case, stage, ProbeStageBoundary::DeliveryEnter, None);
                delegate.run_stage(stage);
                return;
            }
            // SAFETY: the live event retains its Cocoa ABI and ownership throughout this matching
            // main-thread superclass sendEvent: call; ordinary system delivery stays unchanged.
            unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
        }
    }
);

/// Enqueues one synthetic application event without retaining any editor, document or timer.
fn post_probe_event(subtype: i16, case: Option<usize>, mtm: MainThreadMarker) {
    // The payload contains a finite phase identifier and no actual keyboard or document input.
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined, NSPoint::new(0.0, 0.0), NSEventModifierFlags::empty(), 0.0, 0, None, subtype, 0, 0,
    ).expect("AppKit constructs the fixture application event");
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
    trace_probe_stage(case, subtype, ProbeStageBoundary::PostReturned, None);
}

/// 使用共享原生驱动执行既有完整编辑与九个资源场景合同。
///
/// 调用者提供独立合成文件。实际应用事件依次完成输入、表格、关闭和严格弱图验证，
/// 再进入原资源场景；仅真实析构信号可授予关闭成功，不引入轮询、重试或残留对象豁免。
/// 原有两秒期限只报告缺少信号，不能把界面不可用或事件未交付转换成通过。
///
/// # Safety
///
/// 仅可在专用验收进程的真实 AppKit 主线程调用，且必须早于任何其他
/// `NSApplication::sharedApplication` 访问或应用构造。首个原生应用必须是本模块的私有子类，
/// 否则继承的单例访问器可能返回不兼容对象。不得在运行中的产品、Rust 工作线程测试框架
/// 或嵌套应用循环内调用；文件必须是测试自有合成数据，不得使用用户文档或剪贴板。
///
/// # Errors
///
/// 返回具体阶段的合同或资源释放错误；可用图形会话和真实事件交付是必要前提。
pub unsafe fn run_native_markdown_runtime_probe_application(
    document_path: &Path,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    // SAFETY: 调用者承担本入口列出的唯一应用、真实主线程和合成数据义务；私有驱动不放宽它们。
    unsafe { run_native_markdown_probe_application(document_path, mtm, ProbeScenario::Full) }
}

/// 使用同一原生驱动独立执行属性窗口快照回归及真实关闭后的严格弱图验证。
///
/// 本场景固定使用独立长夹具，不进入完整编辑合同或九个资源场景；关闭与释放门禁完全复用。
///
/// # Safety
///
/// 仅可在专用验收进程的真实 AppKit 主线程调用，且必须早于任何其他
/// `NSApplication::sharedApplication` 访问或应用构造。首个原生应用必须是本模块的私有子类。
/// 不得在运行中的产品、Rust 工作线程测试框架或嵌套应用循环内调用；文件必须是测试自有
/// 合成数据，不得使用用户文档或剪贴板。一次进程只能选择一个验收入口。
///
/// # Errors
///
/// 返回属性窗口回归、真实事件交付或严格资源释放的阶段错误，不跳过界面前提。
pub unsafe fn run_native_markdown_semantic_snapshot_probe_application(
    document_path: &Path,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    // SAFETY: 调用者承担与完整入口相同的唯一应用和线程义务，场景只改变预先固定的验收步骤。
    unsafe {
        run_native_markdown_probe_application(document_path, mtm, ProbeScenario::SemanticSnapshot)
    }
}

/// 复用唯一应用、事件路由、关闭与弱图释放驱动，场景值在 delegate 初始化后不再改变。
///
/// # Safety
///
/// 调用者必须满足两个公开验收入口共同列出的唯一应用、真实主线程和合成文件前提。
unsafe fn run_native_markdown_probe_application(
    document_path: &Path,
    mtm: MainThreadMarker,
    scenario: ProbeScenario,
) -> Result<(), String> {
    // 安全合同要求专用进程唯一入口；在任何Native构造之前只建立Rust单调时钟。
    PROBE_STAGE_TRACE.with_borrow_mut(|slot| {
        *slot = Some(ProbeStageTrace {
            origin: Instant::now(),
            budget: ProbeStageBudget::default(),
        });
    });
    // SAFETY: 公开入口已将首个应用和真实主线程义务传入；子类保留系统初始化与单例所有权。
    let application: Retained<OmniMarkdownProbeApplication> =
        unsafe { msg_send![OmniMarkdownProbeApplication::class(), sharedApplication] };
    // 固定场景与文件身份只初始化一次，delegate 仅拥有当前阶段资源与唯一终点结果。
    let delegate = OmniMarkdownProbeDelegate::alloc(mtm).set_ivars(ProbeIvars {
        path: document_path.to_owned(),
        scenario,
        started: Cell::new(false),
        pending_release: RefCell::new(None),
        pending_session: RefCell::new(None),
        release_deadline: RefCell::new(None),
        result: RefCell::new(None),
        running: Cell::new(false),
        deferred_table_terminal: Cell::new(None),
        expected_table_terminal: Cell::new(None),
    });
    // SAFETY: 所有 Rust ivar 已完整初始化，NSObject 的公开初始化仍在其真实主线程执行。
    let delegate: Retained<OmniMarkdownProbeDelegate> = unsafe { msg_send![super(delegate), init] };
    // Native Weak构造先完成，slot内只移动已初始化的Rust Rc，不持有driver强引用。
    let weak_owner = Rc::new(Weak::from_retained(&delegate));
    let previous = PROBE_DELEGATE.with_borrow_mut(|slot| slot.replace(weak_owner));
    drop(previous); // 替换掉的Native弱引用只在RefMut结束后释放。
    application.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    application.run();
    // Stop native routing before releasing the Rust delegate/result and all remaining phase owners.
    application.setDelegate(None);
    let previous = PROBE_DELEGATE.with_borrow_mut(|slot| {
        // 先撤销router入口；仅将Rc移出，不能在slot借用内销毁Native弱引用。
        slot.take()
    });
    drop(previous); // 所有Native weak disposal及其潜在同步重入均在借用外。
    // Transfer the terminal result after routing stops; the delegate's remaining phase owners then drop.
    delegate
        .ivars()
        .result
        .borrow_mut()
        .take()
        .ok_or_else(|| "native launch callback produced no terminal probe result".to_owned())?
}

/// 保留完整编辑合同的原始 320 行合成夹具，不读取用户笔记或工作区内容。
#[must_use]
pub fn native_markdown_runtime_probe_fixture() -> String {
    native_markdown_probe_fixture(ProbeScenario::Full)
}

/// 建立固定 640 行分段长文，仅用于真实属性窗口 A/B 替换及严格释放回归。
#[must_use]
pub fn native_markdown_semantic_snapshot_probe_fixture() -> String {
    native_markdown_probe_fixture(ProbeScenario::SemanticSnapshot)
}

/// 两个公开入口只选择固定夹具合同，不向测试调用者暴露任意行数或分段选项。
fn native_markdown_probe_fixture(scenario: ProbeScenario) -> String {
    let line_count = match scenario {
        ProbeScenario::Full => 320, // 原完整合同依赖短夹具的同步格式与模式输入前提。
        ProbeScenario::SemanticSnapshot => 640, // 将两张表分隔到不同真实属性窗口，保持独立长文覆盖。
    };
    let long_document_prefix = (0..line_count)
        .map(|line| {
            let ending = match scenario {
                ProbeScenario::Full => "\n", // 完整合同恢复原始每行一个换行，禁止长夹具改变既有门禁。
                ProbeScenario::SemanticSnapshot if line % 16 == 15 => {
                    "\n\n" // 定期闭合段落，避免整块扩展吞入远端表；实际窗口不相交仍由原生断言确认。
                }
                ProbeScenario::SemanticSnapshot => "\n", // 其余行继续覆盖多行段落，不人为切成单行块。
            };
            format!("第 {line:03} 行用于验证长文档末尾表格的视口布局与滚动恢复。{ending}")
        })
        .collect::<String>();
    let fixture = format!(
        "# Probe\n\n| 任务编号 (ID) | 模块名称 (Module) | 运行状态 (Status) | 优先级 (Priority) | 响应时间 (Latency) | 详细说明 (Notes) |\n| :--- | :--- | :---: | :---: | ---: | :--- |\n| `#101` | **用户鉴权中心** `Auth` | 🟢 正常 | `P0` | `12 ms` | 支持 OAuth 2.0 与 JWT 校验 |\n| `#102` | **数据同步通道** `Sync` | 🟡 排队中 | `P1` | `145 ms` | 正在等待跨区域节点就绪 |\n| `#103` | **推送网关** `Push` | 🔴 异常 | `P0` | `--` | 触发下游接口超时熔断 |\n| `#104` | **报表导出** `Export` | ⚪ 已完成 | `P2` | `3,420 ms` | 生成文件大小约 *12.5 MB* |\n\n[link](https://example.test) H~2~O\n\n列表任务定位起点。\n\n- [x] 已完成任务\n- [ ] 待完成任务\n\n- 普通短横标记\n+ 普通加号标记\n* 普通星号标记\n\n<u>underlined</u>\n\n---\n\n{long_document_prefix}\n## 8. 实施里程碑与上线风险\n\n| 阶段 | 交付核心物料 | 周期 | 风险预案 |\n| :--- | :--- | :--- | :--- |\n| **Phase 1: 基础设施与 L1/L2 跑通** | 抓取网关、GCS 存储管道、Trafilatura/Crawl4AI 容器镜像 | 1.5 周 | 提前准备海外 Clean IP 节点，避免初始节点受限。 |\n| **Phase 2: L3 兜底与 GCP 向量化打通** | Wayback 自动切换链路、Vertex AI / Qdrant 索引管道 | 1 周 | 快照服务偶发限流，内置本地 Token Bucket 节流器。 |\n| **Phase 3: GEO 归因与 Batch 批处理上线** | Gemini Flash 归因 Prompt 调优、批量异步 ETL 管道 | 1 周 | 开启 Structured Outputs 保证返回严格的 JSON 格式。 |\n| **Phase 4: 压测与业务看板验收** | 全链路压测（500 QPS）、Grafana 监控看板上线 | 0.5 周 | 对高频域名提前注入缓存规则。 |\n"
    );
    // 夹具只交给测试自有的临时文件；闲置插件目录不会保存该 String。
    fixture
}
