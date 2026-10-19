//! 500系列只驱动独立合成小表；复用同一个NSApplication和driver，不进入原400长文状态。

use super::*;
use crate::native_surface::{NativeMarkdownTableAppendCase, NativeMarkdownTableAppendWorkload};

pub(crate) fn post_table_cell_activation_terminal() {
    // 真实activation的成功/取消/失败都只投507，由原Rc与原工作负载判定；回调不读取产品。
    let mtm = MainThreadMarker::new().expect("单格激活终点位于主队列");
    post_probe_event(507, None, mtm);
}

pub(crate) fn post_table_append_terminal() {
    // 回调只投递事件；结果与原收据由503同原工作负载核验，不在此采样或驱动Native。
    let mtm = MainThreadMarker::new().expect("append终点位于主队列");
    post_probe_event(503, None, mtm);
}

pub(super) fn stage_name(stage: i16) -> &'static str {
    match stage {
        500 => "append_open",
        501 => "append_verify_open",
        502 => "append_begin_activation",
        507 => "append_activate_and_begin",
        503 => "append_verify_and_undo",
        504 => "append_verify_undo_and_redo",
        505 => "append_verify_redo_and_undo",
        506 => "append_verify_baseline",
        508 => "toolbar_verify_format_or_second_field_input",
        509 => "toolbar_verify_undo_or_local_field_undo",
        510 => "toolbar_verify_redo_or_field_undo",
        511 => "toolbar_field_verify_redo_and_undo",
        512 => "toolbar_field_verify_undo_and_redo",
        513 => "toolbar_field_verify_redo_and_commit",
        514 => "toolbar_field_verify_commit_and_undo",
        515 => "toolbar_field_verify_canonical_undo_and_redo",
        516 => "toolbar_field_verify_canonical_redo_and_save",
        517 => "conflict_external_save_geometry_and_cancel",
        518 => "conflict_verify_cancel_and_copy",
        519 => "conflict_verify_copy_and_return_original",
        520 => "conflict_verify_aba_and_reload",
        521 => "conflict_verify_real_reload",
        522 => "append_return",
        523 => "append_strict_close",
        _ => "invalid",
    }
}

impl OmniMarkdownMemoryDriver {
    pub(super) fn run_append_stage(&self, stage: i16) {
        if self.ivars().result.borrow().is_some() {
            return;
        } // 迟到事件无复活权限。
        let expected = self.ivars().expected.get();
        if matches!(stage, 503 | 507 | 518..=521) && expected >= 522 {
            return; // 已进入真实关闭，旧激活/append终点不得占用523的恢复交付槽。
        }
        if matches!(stage, 503 | 507 | 518..=521 | 523) && self.ivars().append_running.get() {
            // 真实终点可由嵌套event loop交付；原动作恢复并放回owner后只重投该已收到的信号一次。
            if self.ivars().append_signal.replace(Some(stage)).is_some() {
                self.stop(Err("追加终点信号重复".into()));
            }
            return;
        }
        if expected != stage {
            self.stop(Err("追加工作负载阶段重复或顺序错误".into()));
            return;
        }
        self.trace(stage, "begin", None);
        self.ivars().append_running.set(true);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            autoreleasepool(|_| {
                let result = self.operate_append(stage);
                self.trace(stage, "sync_return", Some(result.is_ok()));
                result
            })
        }))
        .unwrap_or_else(|_| Err("追加工作负载Native回调发生Rust panic".into()));
        self.trace(stage, "pool_drained", Some(result.is_ok()));
        // 阶段尾的timer建立、取消及owner析构也会进入Native；真实终点仍须暂存到这些操作全部返回。
        self.finish_append_stage(stage, result);
        self.ivars().append_running.set(false);
        if self.ivars().result.borrow().is_some() {
            self.ivars().append_signal.take(); // 终点已写入，迟到信号没有重新驱动owner的权限。
            return;
        }
        self.deliver_deferred_append_signal(self.ivars().expected.get());
    }

    fn finish_append_stage(&self, stage: i16, result: Result<(), String>) {
        if self.ivars().result.borrow().is_some() {
            return; // 嵌套错误已终止driver，不能再建立阶段期限或发布后继动作。
        }
        if let Err(error) = result {
            let failure = format!("阶段{stage}：{error}");
            if stage < 522 && self.ivars().append_workload.borrow().is_some() {
                if self.ivars().first_failure.borrow().is_none() {
                    *self.ivars().first_failure.borrow_mut() = Some(failure);
                }
                if matches!(
                    self.ivars().append_signal.get(),
                    Some(503 | 507 | 518..=521)
                ) {
                    // 已失败的激活/append旧终点无需再验证；真实strict-close的523绝不能被清除。
                    self.ivars().append_signal.take();
                }
                self.cancel_timer(&self.ivars().release_deadline);
                self.enqueue(522); // 功能失败仍执行正式返回，不能用清理成功冲掉首因。
            } else {
                self.stop(Err(failure));
            }
            return;
        }
        match stage {
            502 if self.ivars().append_case
                == NativeMarkdownTableAppendCase::FindReplacementComposition =>
            {
                self.enqueue(508); // 新场景只走原有限事件驱动，不伪造表格激活或追加完成。
            }
            502 => {
                self.ivars().expected.set(507);
                self.arm_append_deadline(507, 10.0, "原单格激活没有在失败期限内签发终点");
            } // 502只发起请求；只有原真实Activated可进入507，不按queue轮次推进。
            507 => {
                if self.ivars().append_case == NativeMarkdownTableAppendCase::PointerSelection {
                    self.cancel_timer(&self.ivars().release_deadline);
                    self.enqueue(508); // 真实 Ready 已核原 Pointer；后继事件才开始下一组实际鼠标输入。
                    return;
                }
                if matches!(
                    self.ivars().append_case,
                    NativeMarkdownTableAppendCase::ToolbarFormatSelection
                        | NativeMarkdownTableAppendCase::ToolbarFieldHistory
                ) {
                    self.cancel_timer(&self.ivars().release_deadline); // 原Activated已消费，不把激活期限留给三个实际按钮完成阶段。
                    self.enqueue(508); // 原按钮回调先入同一主队列；后继事件只认证结果，不再次点击粗体。
                    return;
                }
                if matches!(
                    self.ivars().append_case,
                    NativeMarkdownTableAppendCase::FormatUnchanged
                        | NativeMarkdownTableAppendCase::FormatSelection
                ) {
                    self.enqueue(522); // 已消费真实 Activated 与格式结果，直接走原 Return，不伪发追加终点。
                    return;
                }
                self.ivars().expected.set(503);
                self.arm_append_deadline(503, 10.0, "原追加请求没有在失败期限内签发终点");
            } // 只等真实terminal通知，不按event次数推进或poll。
            503 => {
                self.cancel_timer(&self.ivars().release_deadline);
                if matches!(
                    self.ivars().append_case,
                    NativeMarkdownTableAppendCase::Positive
                        | NativeMarkdownTableAppendCase::CommitResigned
                ) {
                    self.enqueue(504); // 原正例继续一次Undo/Redo/Undo，负例不能混入此历史断言。
                } else {
                    self.enqueue(522); // 两个未提交负例已完成原终点断言，直接真实Return与strict close。
                }
            }
            506 => self.enqueue(522),
            508 if self.ivars().append_case == NativeMarkdownTableAppendCase::PointerSelection => {
                let done = self
                    .ivars()
                    .append_workload
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .pointer_is_finished();
                if done {
                    self.enqueue(522); // 全部固定业务场景完成后仍消费原真实 Return/strict close。
                } else {
                    self.ivars().expected.set(507);
                    self.arm_append_deadline(507, 10.0, "原鼠标动作没有交付真实 Ready");
                }
            }
            510 if self.ivars().append_case
                == NativeMarkdownTableAppendCase::ToolbarFormatSelection =>
            {
                self.enqueue(522)
            } // 真正粗体/Undo/Redo与生产保存都完成，才走原Return/strict close。
            514 if self.ivars().append_case
                == NativeMarkdownTableAppendCase::FindReplacementComposition =>
            {
                self.enqueue(522); // 真实导航、拒绝、提交、替换及保存全部认证后仍走原严格关闭。
            }
            516 => self.enqueue(517), // 原字段/IME/唯一canonical及保存断言全部完成后才进入冲突条用例。
            517..=520 => {
                let next = stage + 1;
                self.ivars().expected.set(next);
                self.arm_append_deadline(next, 10.0, "原冲突对话框或真实笔记安装没有交付终点");
            } // 取消由真实modal栈退出交付；另存/切回/重载只由真实后继安装交付。
            521 => {
                self.cancel_timer(&self.ivars().release_deadline);
                self.enqueue(522);
            }
            522 => {
                self.ivars().expected.set(523);
                self.arm_append_deadline(523, 2.0, "真实返回未在失败期限内签发strict close");
            }
            523 => {
                self.cancel_timer(&self.ivars().release_deadline);
                let workload = self.ivars().append_workload.borrow_mut().take();
                drop(workload); // strict已经完成，再释放portable会话和弱收据。
                self.stop(Ok(()));
            }
            _ => self.enqueue(stage + 1), // 固定历史动作；502/507分别等原激活/append真实终点。
        }
    }

    fn deliver_deferred_append_signal(&self, expected: i16) {
        if let Some(signal) = self.ivars().append_signal.take() {
            if matches!(signal, 503 | 507 | 518..=521) && expected >= 522 {
                return; // 清理期间新到的旧激活/append终点都不能占用真实523。
            }
            if signal != expected {
                self.stop(Err("追加终点信号与原阶段不一致".into()));
                return;
            }
            post_probe_event(signal, None, self.mtm()); // 原栈恢复后的真实信号交付，不轮询结果。
        }
    }

    fn operate_append(&self, stage: i16) -> Result<(), String> {
        if stage == 500 {
            let weak = dispatch2::MainThreadBound::new(Weak::from(self), self.mtm());
            let terminal_owner = dispatch2::MainThreadBound::new(Weak::from(self), self.mtm());
            let workload = NativeMarkdownTableAppendWorkload::start(
                &self.ivars().root,
                self.ivars().append_case,
                move || {
                    let mtm = MainThreadMarker::new().expect("append finish_close位于主线程");
                    if weak.get(mtm).load().is_some() {
                        post_probe_event(523, None, mtm);
                    }
                },
                move |stage| {
                    let mtm = MainThreadMarker::new().expect("冲突验收终点沿原主线程交付");
                    if terminal_owner.get(mtm).load().is_some() {
                        post_probe_event(stage, None, mtm);
                    }
                },
            )?;
            *self.ivars().append_workload.borrow_mut() = Some(workload);
            return Ok(());
        }
        let workload = self
            .ivars()
            .append_workload
            .borrow_mut()
            .take()
            .ok_or("追加工作负载owner缺失")?;
        // 结束RefMut后才操作产品；所有错误路径也先放回唯一owner，再走正式清理。
        let result = match stage {
            501 => workload.verify_open(),
            502 if self.ivars().append_case
                == NativeMarkdownTableAppendCase::FindReplacementComposition =>
            {
                workload.begin_find_replacement_composition()
            }
            502 if self.ivars().append_case == NativeMarkdownTableAppendCase::PointerSelection => {
                workload.begin_pointer_case()
            }
            502 => workload.begin_append(),
            507 => {
                if self.ivars().append_case == NativeMarkdownTableAppendCase::PointerSelection {
                    workload.verify_pointer_ready()
                } else if self.ivars().append_case
                    == NativeMarkdownTableAppendCase::ToolbarFieldHistory
                {
                    workload.advance_toolbar_field_history(0)
                } else if matches!(
                    self.ivars().append_case,
                    NativeMarkdownTableAppendCase::FormatUnchanged
                        | NativeMarkdownTableAppendCase::FormatSelection
                        | NativeMarkdownTableAppendCase::ToolbarFormatSelection
                ) {
                    workload.verify_format() // 原真实字段/菜单路径，各场景核对应字符和历史结果。
                } else {
                    workload.finish_cell_activation_and_begin_append()
                }
            }
            503 => {
                if matches!(
                    self.ivars().append_case,
                    NativeMarkdownTableAppendCase::Positive
                        | NativeMarkdownTableAppendCase::CommitResigned
                ) {
                    workload.verify_append_and_undo()
                } else {
                    workload.verify_negative()
                }
            }
            504 => workload.verify_history(true, false),
            505 => workload.verify_history(false, false),
            506 => workload.verify_history(false, true),
            508..=514
                if self.ivars().append_case
                    == NativeMarkdownTableAppendCase::FindReplacementComposition =>
            {
                workload.advance_find_replacement_composition((stage - 507) as u8)
            }
            508 if self.ivars().append_case == NativeMarkdownTableAppendCase::PointerSelection => {
                workload.advance_pointer_case()
            }
            508 if self.ivars().append_case
                == NativeMarkdownTableAppendCase::ToolbarFieldHistory =>
            {
                workload.advance_toolbar_field_history(1)
            }
            509 if self.ivars().append_case
                == NativeMarkdownTableAppendCase::ToolbarFieldHistory =>
            {
                workload.advance_toolbar_field_history(2)
            }
            510 if self.ivars().append_case
                == NativeMarkdownTableAppendCase::ToolbarFieldHistory =>
            {
                workload.advance_toolbar_field_history(3)
            }
            511 => workload.advance_toolbar_field_history(4),
            512 => workload.advance_toolbar_field_history(5),
            513 => workload.advance_toolbar_field_history(6),
            514 => workload.verify_toolbar_format_history(0),
            515 => workload.verify_toolbar_format_history(1),
            516 => workload.verify_toolbar_format_history(2),
            517 => workload.begin_conflict_bar_case(),
            518 => workload.verify_conflict_cancel_and_copy(),
            519 => workload.verify_conflict_copy_and_return(),
            520 => workload.verify_conflict_aba_and_reload(),
            521 => workload.verify_conflict_reload(),
            508 => workload.verify_toolbar_format_history(0),
            509 => workload.verify_toolbar_format_history(1),
            510 => workload.verify_toolbar_format_history(2),
            522 => workload.request_return(self.ivars().first_failure.borrow().is_some()),
            523 => workload.finish(),
            _ => Err("未知追加工作负载阶段".into()),
        };
        *self.ivars().append_workload.borrow_mut() = Some(workload);
        result
    }

    fn arm_append_deadline(&self, expected: i16, seconds: f64, error: &'static str) {
        self.cancel_timer(&self.ivars().release_deadline);
        let weak = dispatch2::MainThreadBound::new(Weak::from(self), self.mtm());
        let block = block2::RcBlock::new(move |_timer: NonNull<NSTimer>| {
            let mtm = MainThreadMarker::new().expect("append失败期限位于主线程");
            let Some(driver) = weak.get(mtm).load() else {
                return;
            };
            if driver.ivars().expected.get() != expected || driver.ivars().result.borrow().is_some()
            {
                return; // 原终点已推进，迟到timer不读取后继资源。
            }
            if matches!(expected, 503 | 507 | 518..=521) {
                if driver.ivars().first_failure.borrow().is_none() {
                    *driver.ivars().first_failure.borrow_mut() = Some(error.to_owned());
                }
                driver.enqueue(522); // timeout只失败并关闭，不重试或接受迟到成功。
            } else {
                driver.stop(Err(error.into()));
            }
        });
        // SAFETY: 一次failure-only timer仅持弱driver；不保持产品对象、重试或宣布成功。
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, false, &block)
        };
        if self.ivars().expected.get() != expected || self.ivars().result.borrow().is_some() {
            // 构造期限时若失败终点已撤销原阶段，只取消本地timer；此时没有任何RefMut存活。
            timer.invalidate();
            return;
        }
        *self.ivars().release_deadline.borrow_mut() = Some(timer);
    }
}

/// 在独立进程中运行ACTIVE小表程序动作，不声称physical Tab或用户CmdZ/IME已通过。
///
/// # Safety
/// 必须在真实主线程且首次NSApplication访问之前调用；root必须是调用者独占的合成临时库。
pub unsafe fn run_native_markdown_table_append_workload_application(
    root: &Path,
    case: NativeMarkdownTableAppendCase,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    let origin = Instant::now();
    // SAFETY: 调用者保证首次application构造，复用原真实NSApplication子类和router。
    let application: Retained<OmniMarkdownProbeApplication> =
        unsafe { msg_send![OmniMarkdownProbeApplication::class(), sharedApplication] };
    let driver = OmniMarkdownMemoryDriver::alloc(mtm).set_ivars(MemoryDriverIvars {
        root: root.to_owned(),
        origin,
        stop_after: NativeMarkdownMemoryStopAfter::All,
        started: Cell::new(false),
        expected: Cell::new(500),
        workload: RefCell::new(None),
        append_workload: RefCell::new(None),
        append_case: case,
        append_running: Cell::new(false),
        append_signal: Cell::new(None),
        first_failure: RefCell::new(None),
        result: RefCell::new(None),
        release_deadline: RefCell::new(None),
        measurement_timer: RefCell::new(None),
    });
    // SAFETY: Rust ivars全初始化，匹配NSObject主线程init。
    let driver: Retained<OmniMarkdownMemoryDriver> = unsafe { msg_send![super(driver), init] };
    let weak = Rc::new(Weak::from_retained(&driver));
    let previous = MEMORY_DRIVER.with_borrow_mut(|slot| slot.replace(weak));
    drop(previous);
    println!(
        "native_table_append_workload begin=true active=true case={case:?} physical_tab=false physical_ime=false"
    );
    application.setDelegate(Some(ProtocolObject::from_ref(&*driver)));
    application.run();
    application.setDelegate(None);
    let previous = MEMORY_DRIVER.with_borrow_mut(Option::take);
    drop(previous); // 先撤router，再让driver剩余载荷析构。
    let result = driver
        .ivars()
        .result
        .borrow_mut()
        .take()
        .ok_or("追加工作负载没有终点结果")?;
    println!(
        "native_table_append_workload finished=true passed={}",
        result.is_ok()
    );
    result
}
