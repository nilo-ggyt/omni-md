//! 独立真实AppKit事件工作负载；时钟/成功收据属于driver，内存峰值由进程外采样器测量。

use super::*;
use crate::native_surface::NativeMarkdownMemoryWorkload;
use objc2_foundation::NSTimer;
use std::str::FromStr;

mod fixture;
pub use fixture::{
    write_native_markdown_giant_paragraph_fixture, write_native_markdown_memory_fixture,
};

/// 未编译真实 primitive 计数时明确缺证据；功能驱动通过不能补签这一项。
const STRING_READ_EVIDENCE: &str = if cfg!(feature = "native-block-fragment-diagnostic") {
    "enabled"
} else {
    "unavailable"
};

#[cfg(feature = "native-table-row-append")]
mod table_append;
#[cfg(feature = "native-table-row-append")]
pub(crate) use table_append::post_table_append_terminal;
#[cfg(feature = "native-table-row-append")]
pub(crate) use table_append::post_table_cell_activation_terminal;
#[cfg(feature = "native-table-row-append")]
pub use table_append::run_native_markdown_table_append_workload_application;

/// 只允许预先声明的已验证前缀；等待窗口不能让未完成资源或定位转为成功。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NativeMarkdownMemoryStopAfter {
    #[default]
    All,
    Open,
    Source,
    FindShown,
    QueryTable,
    FindTable,
    PreviewTable,
    SourceTable,
    QueryCode,
    FindCode,
    PreviewCode,
    FindCodePreview,
}

impl NativeMarkdownMemoryStopAfter {
    fn stage(self) -> Option<i16> {
        match self {
            Self::All => None,
            Self::Open => Some(401),
            Self::Source => Some(403),
            Self::FindShown => Some(405),
            Self::QueryTable => Some(407),
            Self::FindTable => Some(409),
            Self::PreviewTable => Some(411),
            Self::SourceTable => Some(413),
            Self::QueryCode => Some(415),
            Self::FindCode => Some(417),
            Self::PreviewCode => Some(419),
            Self::FindCodePreview => Some(421),
        }
    }
}

impl FromStr for NativeMarkdownMemoryStopAfter {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "all" => Ok(Self::All),
            "open" => Ok(Self::Open),
            "source" => Ok(Self::Source),
            "find_shown" => Ok(Self::FindShown),
            "query_table" => Ok(Self::QueryTable),
            "find_table" => Ok(Self::FindTable),
            "preview_table" => Ok(Self::PreviewTable),
            "source_table" => Ok(Self::SourceTable),
            "query_code" => Ok(Self::QueryCode),
            "find_code" => Ok(Self::FindCode),
            "preview_code" => Ok(Self::PreviewCode),
            "find_code_preview" => Ok(Self::FindCodePreview),
            _ => Err("stop_after必须是声明的工作负载阶段"), // 未知前缀不能默认为完整或较短动作。
        }
    }
}

fn stage_name(stage: i16) -> &'static str {
    #[cfg(feature = "native-table-row-append")]
    if (500..=523).contains(&stage) {
        return table_append::stage_name(stage);
    }
    match stage {
        400 => "open",
        401 => "verify_open",
        402 => "source",
        403 => "verify_source",
        404 => "find_shown",
        405 => "verify_find_shown",
        406 => "query_table",
        407 => "verify_query_table",
        408 => "find_table",
        409 => "verify_find_table",
        410 => "preview_table",
        411 => "verify_preview_table",
        412 => "source_table",
        413 => "verify_source_table",
        414 => "query_code",
        415 => "verify_query_code",
        416 => "find_code",
        417 => "verify_find_code",
        418 => "preview_code",
        419 => "verify_preview_code",
        420 => "find_code_preview",
        421 => "verify_find_code_preview",
        422 => "return",
        423 => "strict_close",
        424 => "terminal",
        425 => "highlight_source",
        426 => "highlight_source_origin",
        427 => "highlight_chinese_input",
        428 => "verify_highlight_chinese",
        429 => "highlight_non_bmp_input",
        430 => "verify_highlight_non_bmp",
        431 => "verify_highlight_composition",
        432 => "highlight_scroll",
        433 => "verify_highlight_scroll",
        434 => "highlight_preview_input",
        435 => "verify_highlight_preview_hide",
        _ => "invalid",
    }
}

struct MemoryDriverIvars {
    root: PathBuf,
    origin: Instant,
    stop_after: NativeMarkdownMemoryStopAfter,
    started: Cell<bool>,
    expected: Cell<i16>,
    workload: RefCell<Option<NativeMarkdownMemoryWorkload>>,
    #[cfg(feature = "native-table-row-append")]
    append_workload: RefCell<Option<crate::native_surface::NativeMarkdownTableAppendWorkload>>,
    #[cfg(feature = "native-table-row-append")]
    append_case: crate::native_surface::NativeMarkdownTableAppendCase,
    #[cfg(feature = "native-table-row-append")]
    append_running: Cell<bool>,
    #[cfg(feature = "native-table-row-append")]
    append_signal: Cell<Option<i16>>,
    first_failure: RefCell<Option<String>>,
    result: RefCell<Option<Result<(), String>>>,
    release_deadline: RefCell<Option<Retained<NSTimer>>>,
    measurement_timer: RefCell<Option<Retained<NSTimer>>>,
}

define_class!(
    // SAFETY: 标准NSObject子类仅在真实AppKit主线程存取ivars，application持有的delegate存活至run返回。
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = MemoryDriverIvars]
    struct OmniMarkdownMemoryDriver;

    // SAFETY: NSObject公开身份与线程约束不变。
    unsafe impl NSObjectProtocol for OmniMarkdownMemoryDriver {}
    // SAFETY: 本driver专用进程只用一次真实launch callback，不模拟或嵌套NSApplication.run。
    unsafe impl NSApplicationDelegate for OmniMarkdownMemoryDriver {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_launch(&self, _notification: &NSNotification) {
            if self.ivars().started.replace(true) {
                return;
            } // 重复系统通知不能重复挂载。
            post_probe_event(self.ivars().expected.get(), None, self.mtm());
        }
    }
);

thread_local! {
    // TLS只复制Rc；objc weak构造/load/drop全部发生在TLS借用之外。
    static MEMORY_DRIVER: RefCell<Option<Rc<Weak<OmniMarkdownMemoryDriver>>>> = const { RefCell::new(None) };
}

impl OmniMarkdownMemoryDriver {
    fn trace(&self, stage: i16, event: &'static str, success: Option<bool>) {
        #[cfg(feature = "native-block-fragment-diagnostic")]
        match event {
            "begin" => crate::native_surface::string_read_diagnostic::begin(stage), // 声明原阶段边界。
            "pool_drained" => crate::native_surface::string_read_diagnostic::report(stage), // 仅输出已有统计。
            _ => {} // sync_return等既有日志不重置或重复输出该阶段。
        }
        println!(
            "native_memory_stage stage={stage} action={} mono_ns={} event={event} ok={success:?}",
            stage_name(stage),
            self.ivars().origin.elapsed().as_nanos()
        );
        // 阶段名仍由上方原日志给出；仅补同一同步边界的标量快照，不改变工作负载推进。
        let phase = match event {
            "begin" => "workload_stage_begin", // Native 调用前，观察前一事件已留下的资源。
            "sync_return" => "workload_stage_return", // 当前动作已经返回，临时 autorelease 尚可能存在。
            "pool_drained" => "workload_stage_drained", // 原有 pool 归还后，不另加清理或等待。
            _ => return, // 未声明事件不产生新采样点，也不重置全进程预算。
        };
        crate::native_surface::probe_call_diagnostic::memory_point(phase);
    }

    /// 接受一次声明阶段；每个操作有begin/sync_return/pool_drained三个不同边界，不声称present。
    fn run_stage(&self, stage: i16) {
        #[cfg(feature = "native-table-row-append")]
        if (500..=523).contains(&stage) {
            self.run_append_stage(stage);
            return; // 独立小表状态不读写原400长文facts或前缀协议。
        }
        if self.ivars().result.borrow().is_some() {
            return;
        } // stop后的迟到事件无执行权限。
        if self.ivars().expected.get() != stage {
            self.stop(Err("工作负载阶段重复或顺序错误".into()));
            return; // 不在错误event上重播最后一个成功动作。
        }
        self.trace(stage, "begin", None);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            autoreleasepool(|_| {
                let result = self.operate(stage);
                self.trace(stage, "sync_return", Some(result.is_ok()));
                result
            })
        }))
        .unwrap_or_else(|_| Err("工作负载原生回调中发生Rust panic".into()));
        self.trace(stage, "pool_drained", Some(result.is_ok()));
        if let Err(error) = result {
            let failure = format!("阶段{stage}：{error}");
            if (stage < 422 || (425..=435).contains(&stage))
                && self.ivars().workload.borrow().is_some()
            {
                // 动作失败也只进行真实返回清理；first_failure保留，关闭成功不能冲销功能失败。
                if self.ivars().first_failure.borrow().is_none() {
                    *self.ivars().first_failure.borrow_mut() = Some(failure);
                }
                self.enqueue(422);
            } else {
                // 本次关闭或strict错误单独交给stop合并，不能取走首因后丢弃实际清理失败。
                self.stop(Err(failure));
            }
            return;
        }
        match stage {
            422 => self.arm_release_deadline(), // 只等实际finish_close投递423，不按时间推定成功。
            423 => {
                self.cancel_timer(&self.ivars().release_deadline);
                let workload = self.ivars().workload.borrow_mut().take();
                drop(workload); // 产品已经严格完成，portable会话和弱收据同样在测量idle前释放。
                self.ivars().expected.set(424);
                println!(
                    "native_memory_measurement_idle mono_ns={} seconds=5 strict_close_passed=true",
                    self.ivars().origin.elapsed().as_nanos()
                );
                self.arm_measurement_timer(424);
            }
            424 => {
                self.cancel_timer(&self.ivars().measurement_timer);
                let failure = self.ivars().first_failure.borrow_mut().take();
                self.stop(failure.map_or(Ok(()), Err));
            }
            _ if self.ivars().stop_after.stage() == Some(stage) => {
                // fresh-prefix只在原验证PASS之后暂停；一次timer仅延长采样窗口，不重查资源或定位。
                self.ivars().expected.set(422);
                println!(
                    "native_memory_prefix_hold stage={stage} mono_ns={} seconds=5 verified=true",
                    self.ivars().origin.elapsed().as_nanos()
                );
                self.arm_measurement_timer(422);
            }
            421 => self.enqueue(425), // 原完整前缀已通过，才在同一窗口附加真实高亮像素回归。
            435 => self.enqueue(422), // 附加回归通过仍消费原Return及严格关闭收据。
            _ => self.enqueue(stage + 1), // 固定下一事件，没有延迟重试或完成轮询。
        }
    }

    fn operate(&self, stage: i16) -> Result<(), String> {
        if stage == 400 {
            let weak = dispatch2::MainThreadBound::new(Weak::from(self), self.mtm());
            let workload = NativeMarkdownMemoryWorkload::start(&self.ivars().root, move || {
                let mtm = MainThreadMarker::new().expect("工作负载finish_close位于主线程");
                if weak.get(mtm).load().is_some() {
                    post_probe_event(423, None, mtm);
                }
            })?;
            *self.ivars().workload.borrow_mut() = Some(workload);
            return Ok(());
        }
        if stage == 424 {
            return Ok(());
        } // strict已通过，此事件只结束外部测量窗口。
        let workload = self
            .ivars()
            .workload
            .borrow_mut()
            .take()
            .ok_or("工作负载owner缺失")?;
        // 结束RefMut后才进入任何Native操作；闭包返回后先放回原owner再传播错误。
        let result = match stage {
            401 => workload.verify_open(),
            402 | 412 => workload.change_mode(true),
            403 | 413 => workload.verify_mode(true),
            404 => workload.show_find(),
            405 => workload.verify_find_shown(),
            406 => workload.enter_query(false),
            407 => workload.verify_query(false),
            408 | 416 | 420 => workload.find_next(),
            409 => workload.verify_find(false, false),
            410 | 418 => workload.change_mode(false),
            411 | 419 => workload.verify_mode(false),
            414 => workload.enter_query(true),
            415 => workload.verify_query(true),
            417 => workload.verify_find(true, false),
            421 => workload.verify_find(true, true),
            422 => workload.request_return(),
            423 => workload.finish(),
            425..=435 => workload.exercise_find_highlights(stage),
            _ => Err("未知工作负载阶段".into()),
        };
        *self.ivars().workload.borrow_mut() = Some(workload);
        result
    }

    fn enqueue(&self, stage: i16) {
        self.ivars().expected.set(stage);
        #[cfg(feature = "native-block-fragment-diagnostic")]
        if stage == 421 {
            // 原Find terminal与paint各有一次真实终点；该分支不先排一个会抢跑paint的验证事件。
            let weak = dispatch2::MainThreadBound::new(Weak::from(self), self.mtm());
            let workload = self.ivars().workload.borrow_mut().take();
            let observed = match workload.as_ref() {
                Some(workload) => workload.observe_preview_find_paint(move || {
                    let Some(mtm) = MainThreadMarker::new() else {
                        return;
                    };
                    let Some(driver) = weak.get(mtm).load() else {
                        return;
                    };
                    if driver.ivars().expected.get() == 421
                        && driver.ivars().result.borrow().is_none()
                    {
                        // 回调来自同一原paint成功或明确取消；只投递一次独立验证，不重放动作。
                        post_probe_event(421, None, mtm);
                    }
                }),
                None => Err("代码背景终点观察缺少原工作负载owner".to_owned()),
            };
            *self.ivars().workload.borrow_mut() = workload; // 先归还原owner，再处理同步注册失败。
            if let Err(error) = observed {
                if self.ivars().first_failure.borrow().is_none() {
                    *self.ivars().first_failure.borrow_mut() =
                        Some(format!("阶段421终点注册：{error}"));
                }
                self.enqueue(422); // 注册已拒绝时没有未来信号，走既有真实Return/strict，不重试。
            }
            return;
        }
        dispatch2::DispatchQueue::main().exec_async(move || {
            let mtm = MainThreadMarker::new().expect("工作负载后继事件位于主队列");
            post_probe_event(stage, None, mtm); // 不捕获driver强引用或Native业务图。
        });
    }

    fn cancel_timer(&self, slot: &RefCell<Option<Retained<NSTimer>>>) {
        let timer = slot.borrow_mut().take();
        if let Some(timer) = timer {
            timer.invalidate();
        } // 原生调用和最终drop均离开借用。
    }

    fn arm_measurement_timer(&self, stage: i16) {
        self.cancel_timer(&self.ivars().measurement_timer);
        let weak = dispatch2::MainThreadBound::new(Weak::from(self), self.mtm());
        let block = block2::RcBlock::new(move |_timer: NonNull<NSTimer>| {
            let mtm = MainThreadMarker::new().expect("采样窗口timer位于主线程");
            let Some(driver) = weak.get(mtm).load() else {
                return;
            };
            if driver.ivars().expected.get() == stage && driver.ivars().result.borrow().is_none() {
                post_probe_event(stage, None, mtm); // 一次交付，不检查或重试任何失败功能。
            }
        });
        // SAFETY: timer非重复，callback只含主线程约束弱driver，不持产品窗口/正文。
        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(5.0, false, &block) };
        *self.ivars().measurement_timer.borrow_mut() = Some(timer);
    }

    fn arm_release_deadline(&self) {
        // 关闭事件已返回；唯一期限只将缺失strict回执变为失败，绝不延迟后再接受survivor。
        self.ivars().expected.set(423);
        let weak = dispatch2::MainThreadBound::new(Weak::from(self), self.mtm());
        let block = block2::RcBlock::new(move |_timer: NonNull<NSTimer>| {
            let mtm = MainThreadMarker::new().expect("工作负载关闭期限位于主线程");
            let Some(driver) = weak.get(mtm).load() else {
                return;
            };
            if driver.ivars().expected.get() == 423 && driver.ivars().result.borrow().is_none() {
                #[cfg(all(
                    feature = "native-raster-surface-probe",
                    feature = "native-close-stage-diagnostic"
                ))]
                {
                    // 原期限已经失败；只借出原 workload 做一次弱角色观察，不排事件或延后门槛。
                    let mut workload = driver.ivars().workload.borrow_mut().take();
                    if let Some(workload) = workload.as_ref() {
                        workload.diagnose_raster_close_timeout();
                    }
                    // 弱观察可重入：仅原阶段仍在等待且槽仍空时归还，不能覆盖后继新 owner。
                    if driver.ivars().expected.get() == 423
                        && driver.ivars().result.borrow().is_none()
                    {
                        let mut slot = driver.ivars().workload.borrow_mut();
                        if slot.is_none() {
                            *slot = workload.take();
                        }
                    }
                    drop(workload); // 已失去原槽归还权时在借用外释放旧 owner，保留后继状态。
                }
                driver.stop(Err("真实返回未在2秒失败期限内签发严格关闭收据".into()));
            }
        });
        // SAFETY: 与原probe同一公开failure-only timer用法，捕获的仅为弱driver。
        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(2.0, false, &block) };
        *self.ivars().release_deadline.borrow_mut() = Some(timer);
    }

    fn stop(&self, result: Result<(), String>) {
        self.cancel_timer(&self.ivars().release_deadline);
        self.cancel_timer(&self.ivars().measurement_timer);
        if self.ivars().result.borrow().is_some() {
            return;
        }
        let first = self.ivars().first_failure.borrow_mut().take();
        let result = match (first, result) {
            (Some(first), Err(cleanup)) => Err(format!("{first}；终止阶段：{cleanup}")),
            (Some(first), Ok(())) => Err(first), // 后来的严格清理成功不能冲销原功能首因。
            (None, result) => result,
        };
        *self.ivars().result.borrow_mut() = Some(result);
        NSApplication::sharedApplication(self.mtm()).stop(None);
        post_probe_event(0, None, self.mtm()); // 唤醒原生事件等待，不执行应用退出或业务关闭替代。
    }
}

/// 只有原长文和附加高亮425..435事件进入memory路由；原9case和系统输入仍走原路径。
pub(super) fn route_event(event: &NSEvent) -> bool {
    let accepted = (400..=435).contains(&event.subtype().0);
    #[cfg(feature = "native-table-row-append")]
    let accepted = accepted || (500..=523).contains(&event.subtype().0);
    if event.r#type() != NSEventType::ApplicationDefined || !accepted {
        return false;
    }
    let owner = MEMORY_DRIVER.with_borrow(|slot| slot.as_ref().map(Rc::clone));
    let driver = owner.as_ref().and_then(|owner| owner.load());
    drop(owner); // objc弱runtime操作与可能最终析构均不跨TLS借用。
    if let Some(driver) = driver {
        driver.run_stage(event.subtype().0);
    }
    true
}

/// 在独立真实NSApplication中运行可审的合成只读序列；driver开销计入该进程但不自报内存峰值。
///
/// # Safety
/// 必须在真实主线程、第一次NSApplication访问之前调用，root只能是本测试独占的合成临时库。
/// 不得嵌套产品run loop，不得把此独立插件进程当作Host全图、物理IME或present端点验收。
pub unsafe fn run_native_markdown_memory_workload_application(
    root: &Path,
    stop_after: NativeMarkdownMemoryStopAfter,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    let origin = Instant::now(); // 原生application构造前固定时钟，输出均为该原点的单调ns。
    // SAFETY: 调用者保证此为进程首次application构造，继承共享入口返回该唯一真实子类。
    let application: Retained<OmniMarkdownProbeApplication> =
        unsafe { msg_send![OmniMarkdownProbeApplication::class(), sharedApplication] };
    let driver = OmniMarkdownMemoryDriver::alloc(mtm).set_ivars(MemoryDriverIvars {
        root: root.to_owned(),
        origin,
        stop_after,
        started: Cell::new(false),
        expected: Cell::new(400),
        workload: RefCell::new(None),
        #[cfg(feature = "native-table-row-append")]
        append_workload: RefCell::new(None),
        #[cfg(feature = "native-table-row-append")]
        append_case: crate::native_surface::NativeMarkdownTableAppendCase::Positive,
        #[cfg(feature = "native-table-row-append")]
        append_running: Cell::new(false),
        #[cfg(feature = "native-table-row-append")]
        append_signal: Cell::new(None),
        first_failure: RefCell::new(None),
        result: RefCell::new(None),
        release_deadline: RefCell::new(None),
        measurement_timer: RefCell::new(None),
    });
    // SAFETY: 所有Rust ivars已初始化，匹配主线程NSObject的标准init。
    let driver: Retained<OmniMarkdownMemoryDriver> = unsafe { msg_send![super(driver), init] };
    let weak = Rc::new(Weak::from_retained(&driver));
    let previous = MEMORY_DRIVER.with_borrow_mut(|slot| slot.replace(weak));
    drop(previous);
    println!(
        "native_memory_workload begin=true mono_ns={} stop_after={stop_after:?} product_scope=plugin_only present=false physical_ime=false string_read_evidence={STRING_READ_EVIDENCE} zero_copy_certified=false",
        origin.elapsed().as_nanos()
    );
    application.setDelegate(Some(ProtocolObject::from_ref(&*driver)));
    application.run();
    application.setDelegate(None);
    let previous = MEMORY_DRIVER.with_borrow_mut(Option::take);
    drop(previous); // 先撤router，再释放driver剩余资源，不能由迟到事件重新挂载。
    let result = driver
        .ivars()
        .result
        .borrow_mut()
        .take()
        .ok_or("工作负载没有产生终点结果")?;
    // 成功终点只在真实 Return、finish_close/strict 和最后采样 idle 完成后产生。
    // 此时才合并累计读失败，避免诊断发现全文读取时提前拆图或跳过既有严格关闭。
    #[cfg(feature = "native-block-fragment-diagnostic")]
    let result = merge_read_gate_result(
        result,
        crate::native_surface::string_read_diagnostic::verify_process_read_gate(),
    );
    #[cfg(not(feature = "native-block-fragment-diagnostic"))]
    println!(
        "native_string_read_gate scope=process_bulk_primitive status=unavailable zero_copy_certified=false"
    );
    // 原应用循环与累计读门禁都已结束，单次 lifetime 快照覆盖关闭尾部；它仍不是整产品收据。
    crate::native_surface::probe_call_diagnostic::memory_point("workload_terminal");
    println!(
        "native_memory_workload finished=true mono_ns={} passed={} string_read_evidence={STRING_READ_EVIDENCE} zero_copy_certified=false",
        origin.elapsed().as_nanos(),
        result.is_ok()
    );
    result
}

/// 原功能或清理首因始终保留；累计读失败不能被后来的严格关闭成功冲销。
#[cfg(any(feature = "native-block-fragment-diagnostic", test))]
fn merge_read_gate_result(
    workload: Result<(), String>,
    read_gate: Result<(), String>,
) -> Result<(), String> {
    match (workload, read_gate) {
        (Ok(()), Ok(())) => Ok(()), // 只完成两项必要检查，不签发未测的完整零拷贝或内存预算。
        (Err(original), Ok(())) => Err(original), // 无读取违规不能消除先前功能或清理失败。
        (Ok(()), Err(read_failure)) => Err(read_failure), // 严格关闭成功也必须保留真实全文读取失败。
        (Err(original), Err(read_failure)) => Err(format!("{original}；{read_failure}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_gate_failure_cannot_be_masked_by_successful_cleanup_or_replace_original_failure() {
        assert_eq!(merge_read_gate_result(Ok(()), Ok(())), Ok(()));
        assert_eq!(
            merge_read_gate_result(Ok(()), Err("已观测全文读取".into())),
            Err("已观测全文读取".into())
        );
        assert_eq!(
            merge_read_gate_result(Err("原功能失败".into()), Ok(())),
            Err("原功能失败".into())
        );
        assert_eq!(
            merge_read_gate_result(Err("原功能失败".into()), Err("非法读取范围".into())),
            Err("原功能失败；非法读取范围".into())
        );
    }

    #[test]
    fn prefixes_only_stop_at_verified_stages_and_reject_unknown_labels() {
        // 输入动作本身未获验证不能成为prefix完成点，所有有效前缀均对应独立检查事件。
        for value in [
            "open",
            "source",
            "find_shown",
            "query_table",
            "find_table",
            "preview_table",
            "source_table",
            "query_code",
            "find_code",
            "preview_code",
            "find_code_preview",
        ] {
            let stage = value
                .parse::<NativeMarkdownMemoryStopAfter>()
                .unwrap()
                .stage()
                .unwrap();
            assert!((401..=421).contains(&stage) && stage % 2 == 1);
        }
        assert_eq!(
            NativeMarkdownMemoryStopAfter::default(),
            NativeMarkdownMemoryStopAfter::All
        );
        assert!(
            "all"
                .parse::<NativeMarkdownMemoryStopAfter>()
                .unwrap()
                .stage()
                .is_none()
        );
        assert!(
            "source_retry"
                .parse::<NativeMarkdownMemoryStopAfter>()
                .is_err()
        );
        assert!("".parse::<NativeMarkdownMemoryStopAfter>().is_err());
    }
}
