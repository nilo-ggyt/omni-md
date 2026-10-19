//! 原表格字段的真实历史分组oracle；只调生产构造、输入协议与既有严格关闭，不替换输入算法。

use super::*;
use objc2::{DefinedClass, MainThreadOnly, define_class};
use objc2_app_kit::{NSApplicationDelegate, NSEventModifierFlags, NSEventType};
use objc2_foundation::{NSNotFound, NSObject, NSObjectProtocol};

const EVENT: i16 = 17_431;
const START: isize = 0;
const ACTIVATED: isize = 1;
const STEP: isize = 2;
const CLOSE: isize = 3;
const RELEASED: isize = 4;
const HISTORY_LIMIT: usize = 16;

// 返回值只在当前事件栈借出真实字段、同会话editor与实际owned manager，不增加跨事件强owner。
type ActualCellEditor = (
    Retained<NSTextField>,
    Retained<NSTextView>,
    Retained<objc2_foundation::NSUndoManager>,
);

thread_local! {
    // 仅真实主线程driver的弱身份；排队事件不保图，也不能创建另一产品owner。
    static DRIVER: RefCell<Option<Rc<Weak<HistoryDriver>>>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug)]
enum Case {
    Ime,
    Typing,
    SelectionMove,
    BreakCoalescing,
}

impl Case {
    const ALL: [Self; 4] = [
        Self::Ime,
        Self::Typing,
        Self::SelectionMove,
        Self::BreakCoalescing,
    ];

    fn expected(self) -> &'static str {
        match self {
            Self::Ime => "你",
            Self::Typing | Self::BreakCoalescing => "baseabc",
            Self::SelectionMove => "xbaseab",
        }
    }

    fn operations(self) -> &'static [Operation] {
        match self {
            Self::Ime => &[
                Operation::Mark("n", 1),
                Operation::Mark("ni", 2),
                Operation::Insert("你"),
            ],
            Self::Typing => &[
                Operation::Insert("a"),
                Operation::Insert("b"),
                Operation::Insert("c"),
            ],
            Self::SelectionMove => &[
                Operation::Insert("a"),
                Operation::Insert("b"),
                Operation::Move(0),
                Operation::Insert("x"),
            ],
            Self::BreakCoalescing => &[
                Operation::Insert("a"),
                Operation::Insert("b"),
                Operation::Break,
                Operation::Insert("c"),
            ],
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Operation {
    Mark(&'static str, usize),
    Insert(&'static str),
    Move(usize),
    Break,
}

#[derive(Clone, Copy)]
enum Phase {
    Input,
    Undo,
    Redo,
    EndClient,
    VerifyClientEnd,
}

struct EndingClient {
    field: Weak<NSTextField>,
    manager: Weak<objc2_foundation::NSUndoManager>,
}

struct OracleRun {
    session: NativeMarkdownRuntimeProbeSession,
    receipt: Option<Rc<NativeMarkdownTableCellReceipt>>,
    case: usize,
    operation: usize,
    phase: Phase,
    undos: usize,
    redos: usize,
    ending: Option<EndingClient>,
}

impl OracleRun {
    fn begin_case(&mut self) -> Result<(), String> {
        let view = &self.session.surface.backend.text_view;
        // 与原完整probe相同地消费实际挂窗layout/display transaction，再保存既有弱样本。
        complete_native_markdown_probe_layout_transaction(
            &self.session.application,
            &self.session.panel,
            &self.session.container_view,
            &self.session.surface,
        )?;
        let observations = capture_probe_paragraph_observations(&self.session.surface);
        let previous = self
            .session
            .paragraph_observations
            .replace(Some(observations));
        drop(previous); // 只替换原有限弱样本；不保留paragraph或原attributedString强边。
        let cell = {
            let cache = view.ivars().visible_table_layout_cache.borrow();
            cache
                .as_ref()
                .and_then(|cache| {
                    cache
                        .tables
                        .iter()
                        .flat_map(|table| &table.cells)
                        .find(|cell| {
                            cell.coordinate.row_index == 1 && cell.coordinate.column_index == 0
                        })
                        .cloned()
                })
                .ok_or("历史oracle缺少真实首格矩阵")?
        };
        let receipt = view.probe_prepare_table_cell_receipt(
            self.session.surface.document(),
            cell.coordinate,
            activated,
        )?;
        if !view.probe_begin_table_cell_editing(cell, Rc::clone(&receipt)) {
            return Err("历史oracle原行请求未被接受".into());
        }
        self.receipt = Some(receipt);
        self.operation = 0;
        self.phase = Phase::Input;
        self.undos = 0;
        self.redos = 0;
        Ok(()) // 只有原Activated回调会投递后继；不按轮次或经过时间猜测字段已就绪。
    }

    fn field(&self) -> Result<ActualCellEditor, String> {
        let field = self
            .session
            .surface
            .backend
            .text_view
            .existing_table_cell_editor()
            .ok_or("原表格字段不存在")?;
        let editor = field
            .currentEditor()
            .and_then(|editor| editor.downcast::<NSTextView>().ok())
            .ok_or("原字段没有实际shared editor")?;
        let controller = &self.session.controller;
        if !editor
            .downcast_ref::<NativeMarkdownSessionFieldEditor>()
            .is_some_and(|editor| {
                editor.belongs_to(&controller.ivars().undo, &controller.ivars().owned)
            })
        {
            return Err("历史oracle未使用原生产会话field editor".into());
        }
        let manager = editor
            .undoManager()
            .ok_or("实际editor没有会话UndoManager")?;
        if !controller.ivars().undo.owns_observed_manager(&manager) {
            return Err("实际editor历史不属于原生产会话".into());
        }
        Ok((field, editor, manager))
    }

    fn activated(&mut self) -> Result<(), String> {
        let receipt = self.receipt.as_ref().ok_or("历史oracle缺少原激活收据")?;
        if receipt.outcome() != Some(NativeMarkdownTableCellOutcome::Activated)
            || !receipt.current_for(&self.session.surface.backend.text_view)
        {
            return Err(format!(
                "历史oracle没有取得真实激活终点：{:?}",
                receipt.outcome()
            ));
        }
        let (_, editor, manager) = self.field()?;
        if !manager.groupsByEvent() || manager.canUndo() || manager.canRedo() {
            return Err("原客户开始时的自动分组或空历史不成立".into());
        }
        let selection = if matches!(Case::ALL[self.case], Case::Ime) {
            NSRange::new(0, 4)
        } else {
            NSRange::new(4, 0)
        };
        editor.setSelectedRange(selection);
        if self.observe("begin", &editor, &manager)? != "base" {
            return Err("历史oracle初始格不是预声明的base".into());
        }
        println!(
            "native_cell_history case={:?} point=selector_support editor_undo={} editor_redo={} actual_manager_owned=true formal=false",
            Case::ALL[self.case],
            editor.respondsToSelector(sel!(undo:)),
            editor.respondsToSelector(sel!(redo:)),
        );
        Ok(())
    }

    fn observe(
        &self,
        label: &'static str,
        editor: &NSTextView,
        manager: &objc2_foundation::NSUndoManager,
    ) -> Result<String, String> {
        let value = editor.string();
        if value.length() > 32 {
            return Err("历史oracle超出预声明的32UTF16短样本".into());
        }
        let text = autoreleasepool(|pool| {
            // SAFETY: 真实长度已经限制为32UTF16；池内借用只复制本测试短合成样本，不读取用户正文。
            unsafe { value.to_str(pool) }.to_owned()
        });
        let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(editor);
        println!(
            "native_cell_history case={:?} point={label} text={text:?} selected={:?} marked={} marked_range={:?} can_undo={} can_redo={} grouping={} coalescing={} formal=false",
            Case::ALL[self.case],
            editor.selectedRange(),
            input.hasMarkedText(),
            input.markedRange(),
            manager.canUndo(),
            manager.canRedo(),
            manager.groupingLevel(),
            editor.isCoalescingUndo(),
        );
        expect_document_text(&self.session.surface, &self.session.initial_text)?;
        let document = self.session.surface.document().borrow();
        if document.generation() != 0
            || document.is_dirty()
            || document.can_undo()
            || document.can_redo()
        {
            return Err("原field局部历史意外改变canonical正文或历史".into());
        }
        Ok(text)
    }

    fn step(&mut self) -> Result<bool, String> {
        if matches!(self.phase, Phase::VerifyClientEnd) {
            return self.verify_client_end(); // 原命令栈已结束；下一真实事件仅核验一次，不轮询客户状态。
        }
        let case = Case::ALL[self.case];
        let (field, editor, manager) = self.field()?;
        match self.phase {
            Phase::Input => {
                let action = case.operations()[self.operation];
                let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
                match action {
                    Operation::Mark(text, caret) => {
                        let text = NSString::from_str(text);
                        // SAFETY: 同一实际shared editor按NSTextInputClient ABI同步接收短NSString及合法相对选区。
                        unsafe {
                            input.setMarkedText_selectedRange_replacementRange(
                                &text,
                                NSRange::new(caret, 0),
                                NSRange::new(NSNotFound as usize, 0),
                            );
                        }
                        if !input.hasMarkedText() {
                            return Err("真实setMarked没有保留marked".into());
                        }
                    }
                    Operation::Insert(text) => {
                        let text = NSString::from_str(text);
                        // SAFETY: 本次真实输入只含短合成NSString；NSNotFound保留AppKit原marked/选择替换规则。
                        unsafe {
                            input.insertText_replacementRange(
                                &text,
                                NSRange::new(NSNotFound as usize, 0),
                            );
                        }
                    }
                    Operation::Move(at) => editor.setSelectedRange(NSRange::new(at, 0)),
                    Operation::Break => editor.breakUndoCoalescing(),
                }
                self.observe("input", &editor, &manager)?;
                self.operation += 1;
                if self.operation == case.operations().len() {
                    if self.observe("input_complete", &editor, &manager)? != case.expected()
                        || input.hasMarkedText()
                    {
                        return Err("真实字段最终输入或marked结束不符合固定样本".into());
                    }
                    self.phase = Phase::Undo;
                }
            }
            Phase::Undo => {
                if manager.canUndo() {
                    if self.undos >= HISTORY_LIMIT {
                        return Err("真实field Undo超出有限观察上界".into());
                    }
                    manager_history_operation(&manager, false)?;
                    self.undos += 1;
                    self.observe("undo", &editor, &manager)?;
                } else {
                    if self.undos == 0
                        || self.observe("undo_exhausted", &editor, &manager)? != "base"
                    {
                        return Err("真实field Undo没有恢复原短草稿".into());
                    }
                    self.phase = Phase::Redo;
                }
            }
            Phase::Redo => {
                if manager.canRedo() {
                    if self.redos >= HISTORY_LIMIT {
                        return Err("真实field Redo超出有限观察上界".into());
                    }
                    manager_history_operation(&manager, true)?;
                    self.redos += 1;
                    self.observe("redo", &editor, &manager)?;
                } else {
                    if self.redos != self.undos
                        || self.observe("redo_exhausted", &editor, &manager)? != case.expected()
                    {
                        return Err("真实field Redo没有完整往返原输入组".into());
                    }
                    self.phase = Phase::EndClient;
                }
            }
            Phase::EndClient => {
                self.ending = Some(EndingClient {
                    field: Weak::from_retained(&field),
                    manager: Weak::from_retained(&manager),
                });
                let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
                // 原editor解释已存在的Escape语义，交生产control delegate处理；不直调内部取消，
                // 不伪造didEnd通知，也不手调finished helper或removeAllActions取得空历史。
                objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
                    // SAFETY: 同一活主线程editor实现公开输入协议；selector是原生产支持的cancelOperation:。
                    unsafe { input.doCommandBySelector(sel!(cancelOperation:)) };
                }))
                .map_err(|exception| {
                    format!("真实原field取消命令抛出Objective-C异常：{exception:?}")
                })?;
                self.phase = Phase::VerifyClientEnd;
            }
            Phase::VerifyClientEnd => return Err("历史oracle客户结束核验重复进入输入栈".into()),
        }
        Ok(false)
    }

    fn verify_client_end(&mut self) -> Result<bool, String> {
        let case = Case::ALL[self.case];
        let ending = self.ending.take().ok_or("客户结束缺少原弱身份")?;
        let field = ending.field.load().ok_or("原客户字段在核验前已消失")?;
        let manager = ending
            .manager
            .load()
            .ok_or("原客户UndoManager在核验前已消失")?;
        if !self
            .session
            .controller
            .ivars()
            .undo
            .owns_observed_manager(&manager)
        {
            return Err("原客户结束后的UndoManager已不是原owned manager".into());
        }
        let borrowed = self.session.controller.ivars().borrowed.borrow().clone();
        // 一次记录全部真实边界；首项失败不跳过其余只读事实，也不补做清理使断言通过。
        let editor_attached = field.currentEditor().is_some();
        let can_undo = manager.canUndo();
        let can_redo = manager.canRedo();
        let borrowed_failed = borrowed.failure().is_some();
        if editor_attached || can_undo || can_redo || borrowed_failed {
            return Err(format!(
                "原客户结束没有真实清空自有历史：case={case:?} editor_attached={editor_attached} can_undo={can_undo} can_redo={can_redo} borrowed_failed={borrowed_failed} diagnostic={}",
                borrowed.failure_diagnostic()
            ));
        }
        self.session.controller.ivars().undo.preflight()?;
        expect_document_text(&self.session.surface, &self.session.initial_text)?;
        println!(
            "native_cell_history case={case:?} point=client_end undo_groups={} redo_groups={} actual_editor_detached=true actual_history_empty=true canonical_unchanged=true formal=false",
            self.undos, self.redos
        );
        self.case += 1;
        self.receipt = None;
        Ok(self.case == Case::ALL.len())
    }
}

fn manager_history_operation(
    manager: &objc2_foundation::NSUndoManager,
    redo: bool,
) -> Result<(), String> {
    // 外来异常只在这个原生primitive周围由Objective-C catch截获；不能让Rust catch_unwind
    // 尝试解释ObjC异常。失败保原文字/分组预期，外层仍执行原生产关闭与实际析构门。
    objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
        if redo {
            // 与生产routing相同，只恢复实际editor登记的自有manager；不要求NSTextView实现redo:。
            manager.redo();
        } else {
            // 标准manager自行处理顶层event group；不手工结束或重建分组，也不给editor补undo:。
            manager.undo();
        }
    }))
    .map_err(|exception| match exception {
        Some(exception) => format!("真实会话manager历史原语抛出Objective-C异常：{exception}"),
        None => "真实会话manager历史原语抛出nil Objective-C异常".into(),
    })
}

fn activated(_: NativeMarkdownTableCellOutcome) {
    post(ACTIVATED);
}

fn post(stage: isize) {
    let mtm = MainThreadMarker::new().expect("历史oracle只在真实主线程投递事件");
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined, NSPoint::ZERO, NSEventModifierFlags::empty(), 0.0, 0, None,
        EVENT, stage, 0,
    ).expect("构造固定历史oracle应用事件");
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
}

fn later(stage: isize) {
    // 只投递下一个已声明动作；没有timer、重复成功探测或持Native对象的排队闭包。
    dispatch2::DispatchQueue::main().exec_async(move || post(stage));
}

struct DriverIvars {
    path: std::path::PathBuf,
    run: RefCell<Option<OracleRun>>,
    completion: RefCell<Option<NativeMarkdownRuntimeProbeCompletion>>,
    failure: RefCell<Option<String>>,
    closing: Cell<bool>,
    result: RefCell<Option<Result<(), String>>>,
}

define_class!(
    // SAFETY: NSObject delegate只在专用进程主线程初始化；内部图只在有限事件之间持有。
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = DriverIvars]
    struct HistoryDriver;
    // SAFETY: 保留NSObject基础身份，所有Native调用与析构均在主线程。
    unsafe impl NSObjectProtocol for HistoryDriver {}
    // SAFETY: 公开delegate仅投递固定起始事件，不跨借用创建或操作文档。
    unsafe impl NSApplicationDelegate for HistoryDriver {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn finished_launching(&self, _: &NSNotification) {
            later(START);
        }
    }
);

impl HistoryDriver {
    fn deliver(&self, stage: isize) {
        if self.ivars().result.borrow().is_some()
            || (self.ivars().closing.get() && stage != RELEASED && stage != CLOSE)
        {
            return;
        }
        // 不能让测试断言或Rust展开跨Objective-C回调；失败仍先走原图关闭再返回首因。
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            autoreleasepool(|_| self.advance(stage))
        }))
        .unwrap_or_else(|_| Err("历史oracle在Native事件中发生Rust展开".into()));
        if let Err(error) = result {
            // 先输出原事件首因；后续严格关闭或测试driver析构失败不能把此原因覆盖掉。
            eprintln!(
                "native_cell_history point=event_failure stage={stage} error={error:?} formal=false"
            );
            self.ivars().failure.borrow_mut().get_or_insert(error);
            if stage == CLOSE || stage == RELEASED {
                self.finish();
            } else {
                later(CLOSE);
            }
        }
    }

    fn advance(&self, stage: isize) -> Result<(), String> {
        match stage {
            START => {
                let session = run_native_markdown_runtime_probe(&self.ivars().path, self.mtm())?;
                *session.table_owner_lease.borrow_mut() =
                    Some(ProbeEditingOwnerLease::new(&session.controller)?);
                let mut run = OracleRun {
                    session,
                    receipt: None,
                    case: 0,
                    operation: 0,
                    phase: Phase::Input,
                    undos: 0,
                    redos: 0,
                    ending: None,
                };
                let result = run.begin_case();
                self.ivars().run.replace(Some(run));
                result
            }
            ACTIVATED | STEP => {
                let mut run = self
                    .ivars()
                    .run
                    .borrow_mut()
                    .take()
                    .ok_or("历史oracle缺少原session")?;
                let result = if stage == ACTIVATED {
                    run.activated().map(|()| false)
                } else {
                    run.step()
                };
                let done = result.as_ref().is_ok_and(|done| *done);
                let begin_next = result.is_ok() && !done && run.receipt.is_none();
                let result =
                    result.and_then(|_| if begin_next { run.begin_case() } else { Ok(()) });
                self.ivars().run.replace(Some(run));
                result?;
                if done {
                    later(CLOSE);
                } else if !begin_next {
                    later(STEP);
                }
                Ok(())
            }
            CLOSE => {
                if self.ivars().closing.replace(true) {
                    return Ok(());
                }
                let run = self
                    .ivars()
                    .run
                    .borrow_mut()
                    .take()
                    .ok_or("历史oracle关闭没有原session")?;
                // 失败也取消原活动字段；所有局部actual editor/manager claim已在上一事件池退出。
                run.session
                    .surface
                    .backend
                    .text_view
                    .cancel_active_table_cell();
                let lease = run.session.table_owner_lease.borrow_mut().take();
                drop(lease);
                let completion = run.session.close()?;
                completion.when_native_objects_released(|| post(RELEASED));
                self.ivars().completion.replace(Some(completion));
                Ok(()) // 只等原实际析构信号；不按时间/队列轮数观察weak变空。
            }
            RELEASED => {
                let completion = self
                    .ivars()
                    .completion
                    .borrow_mut()
                    .take()
                    .ok_or("历史oracle没有严格关闭收据")?;
                completion.verify_released(self.mtm())?;
                println!(
                    "native_cell_history point=product_release actual_completion_verified=true formal=false"
                );
                self.finish();
                Ok(())
            }
            _ => Err("未知历史oracle事件".into()),
        }
    }

    fn finish(&self) {
        let error = self.ivars().failure.borrow_mut().take();
        self.ivars().result.replace(Some(error.map_or(Ok(()), Err)));
        NSApplication::sharedApplication(self.mtm()).stop(None);
        post(-1); // 唤醒原run返回；result已封闭，迟到事件不能重建图或覆盖失败。
    }
}

define_class!(
    // SAFETY: 首个NSApplication单例必须是此私有子类；调用者在独立进程承担该前提。
    #[unsafe(super = NSApplication)]
    #[thread_kind = MainThreadOnly]
    struct HistoryApplication;
    // SAFETY: 只拦截本驱动固定ApplicationDefined标记，其他事件按原AppKit ABI转发一次。
    unsafe impl NSObjectProtocol for HistoryApplication {}
    impl HistoryApplication {
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            if event.r#type() == NSEventType::ApplicationDefined && event.subtype().0 == EVENT {
                let weak = DRIVER.with_borrow(|slot| slot.as_ref().map(Rc::clone));
                if let Some(driver) = weak.and_then(|weak| weak.load()) { driver.deliver(event.data1()); }
                return;
            }
            // SAFETY: 仍活event沿同一公开sendEvent ABI转发，不修改键盘、关闭或编辑语义。
            unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
        }
    }
);

/// 在原产品单格字段上观察真实IME/typing/selection/coalescing与Undo/Redo，再消费原严格关闭收据。
///
/// # Safety
/// 必须是独立验收进程的真实主线程，且早于任何NSApplication访问；路径只可指向本测试创建的
/// 固定短合成表格。不得在产品应用、嵌套run loop或Rust测试工作线程调用。
pub unsafe fn run_native_markdown_cell_history_oracle(
    path: &Path,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    // 测试自身的Native构造与run有明确池边界；原产品每个事件仍用自己的有限池。
    // 只在该池实际退出后检查driver，不能把测试启动阶段的autorelease claim当成产品幸存者。
    let (result, weak) = autoreleasepool(|_| {
        // SAFETY: 调用者保证首个应用实例及真实主线程；此子类仅增加固定事件路由。
        let application: Retained<HistoryApplication> =
            unsafe { msg_send![HistoryApplication::class(), sharedApplication] };
        let driver = HistoryDriver::alloc(mtm).set_ivars(DriverIvars {
            path: path.to_owned(),
            run: RefCell::new(None),
            completion: RefCell::new(None),
            failure: RefCell::new(None),
            closing: Cell::new(false),
            result: RefCell::new(None),
        });
        // SAFETY: 已完整初始化NSObject子类ivar，公开init返回同一具体driver。
        let driver: Retained<HistoryDriver> = unsafe { msg_send![super(driver), init] };
        let weak = Rc::new(Weak::from_retained(&driver));
        let previous = DRIVER.with_borrow_mut(|slot| slot.replace(weak));
        drop(previous);
        application.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
        application.setDelegate(Some(ProtocolObject::from_ref(&*driver)));
        application.run();
        application.setDelegate(None);
        let previous = DRIVER.with_borrow_mut(Option::take);
        drop(previous);
        // 原事件结果先独立移出；缺终点也保留为失败，不提前返回而跳过driver的解链和Drop。
        let result = driver
            .ivars()
            .result
            .borrow_mut()
            .take()
            .unwrap_or_else(|| Err("历史oracle没有真实终点".to_owned()));
        let weak = Weak::from_retained(&driver);
        drop(driver);
        (result, weak)
    });
    if weak.load().is_some() {
        // 不豁免driver幸存；若原client/close已失败，则同一错误同时保留两项事实。
        let release = "历史oracle driver在真实run和自身池退出后仍有原生强边";
        return match result {
            Ok(()) => Err(release.into()),
            Err(cause) => Err(format!("{cause}；另有测试释放失败：{release}")),
        };
    }
    println!(
        "native_cell_history point=driver_release actual_weak_nil=true outcome_success={} formal=false",
        result.is_ok()
    );
    result
}
