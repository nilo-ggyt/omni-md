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

/// Keeps the bounded fixture and only the graph needed by the current native lifecycle phase.
struct ProbeIvars {
    path: PathBuf,
    started: Cell<bool>,
    pending_session: RefCell<Option<crate::NativeMarkdownRuntimeProbeSession>>,
    pending_release: RefCell<Option<crate::NativeMarkdownRuntimeProbeCompletion>>,
    release_deadline: RefCell<Option<Retained<objc2_foundation::NSTimer>>>,
    result: RefCell<Option<Result<(), String>>>,
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
            post_probe_event(100, self.mtm());
        }
    }
);

impl OmniMarkdownProbeDelegate {
    /// Advances one finite phase without unwinding a Rust panic through an Objective-C callback.
    fn run_stage(&self, stage: i16) {
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
                        let result = session.exercise();
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
                                post_probe_event(103, mtm);
                            }
                        });
                        // Publish the weak token only after reentrant native teardown has returned.
                        *self.ivars().pending_release.borrow_mut() = Some(completion);
                        self.arm_release_failure_deadline();
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
        match result {
            // Every observed contract and weak graph assertion passed before the loop stops.
            Ok(()) if stage == 103 => self.finish_probe(Ok(()), mtm),
            // Actual native cohort completion owns the only transition from close to strict verification.
            Ok(()) if stage == 102 => {}
            // Hand through the actual main queue so earlier AppKit initialization/input work runs
            // before the next input event; no nested run loop, sleep, poll or conditional retry exists.
            Ok(()) => dispatch2::DispatchQueue::main().exec_async(move || {
                // This real main-queue turn precedes the next application event without retaining a graph.
                let mtm =
                    MainThreadMarker::new().expect("the dispatch main queue is the AppKit thread");
                post_probe_event(stage + 1, mtm);
            }),
            // Failures preserve their exact phase and stop the real application loop immediately.
            Err(error) => self.finish_probe(Err(format!("stage {stage}: {error}")), mtm),
        }
    }

    /// Bound a missing native completion with one failure-only deadline, never a graph retry or acceptance delay.
    fn arm_release_failure_deadline(&self) {
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
                delegate.finish_probe(Err(format!("native deallocation did not signal within the probe's 2 s failure deadline; {}", diagnostic.as_deref().unwrap_or("release evidence missing"))), mtm);
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
        let deadline = self.ivars().release_deadline.borrow_mut().take();
        if let Some(deadline) = deadline {
            // End the timer-state borrow before native invalidation releases its copied weak block.
            deadline.invalidate();
        }
        *self.ivars().result.borrow_mut() = Some(result);
        NSApplication::sharedApplication(mtm).stop(None);
        post_probe_event(0, mtm);
    }
}

// The router stores no strong delegate or native graph owner between actual application events.
thread_local! {
    static PROBE_DELEGATE: RefCell<Option<Weak<OmniMarkdownProbeDelegate>>> = const { RefCell::new(None) };
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
            if event.r#type() == NSEventType::ApplicationDefined && (100..=103).contains(&event.subtype().0) {
                let delegate = PROBE_DELEGATE.with_borrow(|weak| {
                    // The global router stores a weak link, never an extra delegate/editor owner.
                    weak.as_ref().and_then(|weak| {
                        // Upgrade only while the standalone driver still owns its live delegate.
                        weak.load()
                    })
                });
                let Some(delegate) = delegate else {
                    // A fixture event queued after cleanup has no authority to revive the finished driver.
                    return;
                };
                // The router borrow ended before callbacks can reenter native or project state.
                delegate.run_stage(event.subtype().0);
                return;
            }
            // SAFETY: the live event retains its Cocoa ABI and ownership throughout this matching
            // main-thread superclass sendEvent: call; ordinary system delivery stays unchanged.
            unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
        }
    }
);

/// Enqueues one synthetic application event without retaining any editor, document or timer.
fn post_probe_event(subtype: i16, mtm: MainThreadMarker) {
    // The payload contains a finite phase identifier and no actual keyboard or document input.
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined, NSPoint::new(0.0, 0.0), NSEventModifierFlags::empty(), 0.0, 0, None, subtype, 0, 0,
    ).expect("AppKit constructs the fixture application event");
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
}

/// Run the standalone editor contract using the same driver in plugin and pinned Host checks.
///
/// The caller supplies an existing synthetic fixture. Four actual application events perform open,
/// input, close and one strict weak-release verification after an actual deallocation cohort signal.
/// This function adds no polling, sleep, retry or survivor allowance. One 2 s failure-only deadline
/// terminates a missing native signal without inspecting or accepting a surviving graph. It returns failure rather than converting
/// unavailable GUI/event delivery to success.
///
/// # Safety
///
/// Call only in a dedicated probe process, before ANY other `NSApplication::sharedApplication`
/// access or application construction. The first native application must be this private subclass;
/// its inherited shared-application accessor otherwise could return an incompatible existing class.
/// Keep this process on the actual AppKit main thread. Do not invoke from a running product, Rust
/// worker-thread test harness or nested application loop; no user document or clipboard belongs here.
///
/// # Errors
///
/// Returns a stage-specific contract or graph-release failure. A GUI session and actual native event
/// delivery are prerequisites, not optional conditions that callers may skip.
pub unsafe fn run_native_markdown_runtime_probe_application(
    document_path: &Path,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    // SAFETY: the caller guarantees this is the process's first application construction on the
    // real main thread. The subclass preserves initialization and Cocoa owns its process singleton.
    let application: Retained<OmniMarkdownProbeApplication> =
        unsafe { msg_send![OmniMarkdownProbeApplication::class(), sharedApplication] };
    // The delegate owns only current phase resources, fixture identity and one terminal result.
    let delegate = OmniMarkdownProbeDelegate::alloc(mtm).set_ivars(ProbeIvars {
        path: document_path.to_owned(),
        started: Cell::new(false),
        pending_release: RefCell::new(None),
        pending_session: RefCell::new(None),
        release_deadline: RefCell::new(None),
        result: RefCell::new(None),
    });
    // SAFETY: all Rust ivars are initialized before NSObject's public initializer runs on its thread.
    let delegate: Retained<OmniMarkdownProbeDelegate> = unsafe { msg_send![super(delegate), init] };
    PROBE_DELEGATE.with_borrow_mut(|weak| {
        // The event router receives a weak link and never becomes a second phase-resource owner.
        *weak = Some(Weak::from_retained(&delegate));
    });
    application.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    application.run();
    // Stop native routing before releasing the Rust delegate/result and all remaining phase owners.
    application.setDelegate(None);
    PROBE_DELEGATE.with_borrow_mut(|weak| {
        // No queued fixture event can upgrade a completed driver's delegate.
        weak.take();
    });
    // Transfer the terminal result after routing stops; the delegate's remaining phase owners then drop.
    delegate
        .ivars()
        .result
        .borrow_mut()
        .take()
        .ok_or_else(|| "native launch callback produced no terminal probe result".to_owned())?
}

/// Create the shared synthetic Unicode/table fixture without reading any user or workspace content.
#[must_use]
pub fn native_markdown_runtime_probe_fixture() -> String {
    let long_document_prefix = (0..320)
        .map(|line| {
            // Synthetic Unicode lines place a second table outside the first viewport.
            format!("第 {line:03} 行用于验证长文档末尾表格的视口布局与滚动恢复。\n")
        })
        .collect::<String>();
    let fixture = format!(
        "# Probe\n\n| 任务编号 (ID) | 模块名称 (Module) | 运行状态 (Status) | 优先级 (Priority) | 响应时间 (Latency) | 详细说明 (Notes) |\n| :--- | :--- | :---: | :---: | ---: | :--- |\n| `#101` | **用户鉴权中心** `Auth` | 🟢 正常 | `P0` | `12 ms` | 支持 OAuth 2.0 与 JWT 校验 |\n| `#102` | **数据同步通道** `Sync` | 🟡 排队中 | `P1` | `145 ms` | 正在等待跨区域节点就绪 |\n| `#103` | **推送网关** `Push` | 🔴 异常 | `P0` | `--` | 触发下游接口超时熔断 |\n| `#104` | **报表导出** `Export` | ⚪ 已完成 | `P2` | `3,420 ms` | 生成文件大小约 *12.5 MB* |\n\n[link](https://example.test)\n\n<u>underlined</u>\n\n---\n\n{long_document_prefix}\n## 8. 实施里程碑与上线风险\n\n| 阶段 | 交付核心物料 | 周期 | 风险预案 |\n| :--- | :--- | :--- | :--- |\n| **Phase 1: 基础设施与 L1/L2 跑通** | 抓取网关、GCS 存储管道、Trafilatura/Crawl4AI 容器镜像 | 1.5 周 | 提前准备海外 Clean IP 节点，避免初始节点受限。 |\n| **Phase 2: L3 兜底与 GCP 向量化打通** | Wayback 自动切换链路、Vertex AI / Qdrant 索引管道 | 1 周 | 快照服务偶发限流，内置本地 Token Bucket 节流器。 |\n| **Phase 3: GEO 归因与 Batch 批处理上线** | Gemini Flash 归因 Prompt 调优、批量异步 ETL 管道 | 1 周 | 开启 Structured Outputs 保证返回严格的 JSON 格式。 |\n| **Phase 4: 压测与业务看板验收** | 全链路压测（500 QPS）、Grafana 监控看板上线 | 0.5 周 | 对高频域名提前注入缓存规则。 |\n"
    );
    // Transfer this test-owned fixture to its temporary-file caller; no runtime catalog retains it.
    fixture
}
