//! 当前已选输入法在原四MiB映射正文的远端视口组合；不切换输入源，也不调用插字接口。

#[path = "../../src/native_surface/probe/system_ime/input.rs"]
mod shared_input;
use shared_input::{FocusNotifications, SoftwareInput};
#[path = "../../src/native_surface/probe/system_ime/validation.rs"]
mod shared_validation;

mod native_types {
    pub(super) use super::{
        NativeMarkdownPagedBody, NativeMarkdownPagedBodyPaintObserver,
        NativeMarkdownPagedInputCommit, NativeMarkdownPagedInputState, NativeMarkdownPagedSceneKey,
        NativeMarkdownPagedTextView,
    };
}

use super::*;
use objc2::{ClassType, define_class, msg_send, rc::Retained};
use objc2_app_kit::{
    NSEvent, NSEventModifierFlags, NSEventType, NSFloatingWindowLevel, NSScreen,
    NSTextInputContext, NSWindowCollectionBehavior,
};
use objc2_foundation::{NSContainsRect, NSObjectProtocol, NSPointInRect, NSTimer};
use omni_markdown_core::MarkdownTextSelection;
use omni_md_plugin::native_surface::{
    paged_body::{NativeMarkdownPagedBodyInputObserver, NativeMarkdownPagedBodyResourceReceipts},
    paged_input::{NativeMarkdownPagedInputCommit, NativeMarkdownPagedInputState},
    paged_text_view::NativeMarkdownPagedTextView,
};

const INSERTION: usize = 70_001;
// 只观察本合成文稿一次提交的至多128B纯UTF16值，不按字面词假定当前Rime使用全拼。
const SOFTWARE_INPUT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
/// 软件模式验证当前IME真实组合/提交及实际面板几何；不据此声称物理按键或候选内容已通过。
pub(super) enum Mode {
    Manual,
    Software,
}

/// 真实输入等待的单次期限；只停止当前标准run，不持有正文、输入context或候选文字。
struct InputDeadline(Retained<NSTimer>);

impl InputDeadline {
    /// 超时与实际提交共用stop出口；不是轮询任务，也不改变任何输入或产品快捷键。
    fn new(app: &NSApplication, seconds: f64) -> Self {
        let weak = std::sync::Arc::new(dispatch2::MainThreadBound::new(Weak::from(app), app.mtm()));
        let block = block2::RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
            let mtm = MainThreadMarker::new().unwrap();
            println!("system_ime stage=deadline_expired");
            std::io::stdout().flush().unwrap();
            if let Some(app) = weak.get(mtm).load() {
                stop(&app);
            }
        });
        // SAFETY: 当前主线程default run loop独占计时；可跨线程block只携MainThreadBound弱票。
        Self(unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, false, &block)
        })
    }
}

impl Drop for InputDeadline {
    /// 提前成功或panic也取消计时，计时回调不可能误停后继释放验收。
    fn drop(&mut self) {
        self.0.invalidate();
    }
}

/// 唯一应用内唤醒事件只结束原等待，不经过文本协议或改变全局键盘/热键。
fn wake(app: &NSApplication) {
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined, NSPoint::ZERO, NSEventModifierFlags::empty(),
        0.0, 0, None, 0x4f49, 0, 0,
    ).unwrap();
    app.postEvent_atStart(&event, false);
}

/// stop在异步通知中可能仍等下一事件；唯一无文本wake只让当前标准run及时返回。
fn stop(app: &NSApplication) {
    app.stop(None);
    wake(app);
}

define_class!(
    // SAFETY: 只有主线程验收拥有此无标题栏窗口；签名与NSWindow公开焦点合同相同。
    #[unsafe(super(NSWindow))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct ImeWindow;
    unsafe impl NSObjectProtocol for ImeWindow {}
    impl ImeWindow {
        // 无边框验收窗也必须成为真实键盘客户，否则系统中文候选不会送到此view。
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool { true }
        // 主窗口资格只作用于本轮临时窗，不改变产品窗口或其他应用的焦点规则。
        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool { true }
    }
);

/// 只为物理键盘验收允许无边框窗口取得系统焦点，不为产品增加标题栏或关闭动作。
pub(super) fn window(frame: NSRect, mtm: MainThreadMarker) -> Retained<NSWindow> {
    let allocated = ImeWindow::alloc(mtm).set_ivars(());
    // SAFETY: 原NSWindow initializer接收完整主线程参数，Retained独占所得+1对象。
    let window: Retained<ImeWindow> = unsafe {
        msg_send![super(allocated), initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered, defer: false]
    };
    let window = Retained::into_super(window);
    window.setTitle(&NSString::from_str("Omni MD 中文输入法验收"));
    // 临时人工输入窗口随激活进入当前桌面；all-Spaces窗口的onActiveSpace事实可能迟于显示通知。
    // 只修改本用例的窗口归属，正式启动器继续沿自己的原层级和Spaces策略。
    window.setLevel(NSFloatingWindowLevel);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::MoveToActiveSpace
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    // 首次上屏前以本人当前鼠标所在的真实显示器定位；不能让初始(0,0)窗口决定另一块屏幕。
    // 鼠标、screen.frame与window.frame都沿AppKit全局坐标，不混入Quartz的左上原点bounds。
    let pointer = NSEvent::mouseLocation();
    let screens = NSScreen::screens(mtm);
    let screen = (0..screens.count())
        .map(|index| screens.objectAtIndex(index))
        .find(|screen| NSPointInRect(pointer, screen.frame()))
        .expect("实际鼠标必须位于本次人工验收的可用显示器");
    let visible = screen.visibleFrame();
    let size = window.frame().size;
    window.setFrameOrigin(NSPoint::new(
        visible.origin.x + (visible.size.width - size.width) * 0.5,
        visible.origin.y + (visible.size.height - size.height) * 0.5,
    ));
    // 实际640×240窗口须完整留在选中屏幕的可用区域；小屏或系统移位不能冒充可见验收。
    let actual = window.frame();
    assert!(
        NSContainsRect(visible, actual),
        "实际输入法验收窗口未完整位于当前显示器可用区域：visible={visible:?} actual={actual:?}"
    );
    println!(
        "system_ime stage=initial_window_placement pointer={pointer:?} selected_screen={} screen_frame={:?} visible_frame={visible:?} actual_window_frame={actual:?} appkit_coordinates=true before_first_order_front=true",
        screen.localizedName(),
        screen.frame(),
    );
    window
}

/// 只保存真实提交的标量事实；不保文稿、候选文字、NSTextInputContext或第二正文。
struct InputObservation {
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    marked_seen: Cell<bool>,
    finished: Cell<bool>,
    // 只保实际提交的至多128B UTF16纯值，用于流式原文与历史对照；不保Source或Native强边。
    committed_utf16: RefCell<Option<Vec<u16>>>,
    application: Retained<NSApplication>,
    software: RefCell<Option<Rc<SoftwareInput>>>,
    started: Cell<bool>,
    deadline: RefCell<Option<InputDeadline>>,
}

impl InputObservation {
    /// 原焦点真正到达后才开始唯一输入阶段，不退出再重启应用主循环或提前切换输入源。
    fn start(&self, mode: Mode, window: &NSWindow, view: &NativeMarkdownPagedTextView) {
        let focus_deadline = self.deadline.borrow_mut().take();
        drop(focus_deadline); // 焦点期限先归还；它不能迟到停止正在进行的实际中文组合。
        let started = Instant::now();
        let software = if matches!(mode, Mode::Software) {
            match SoftwareInput::new(
                &self.application,
                window,
                view,
                self.body.clone(),
                self.document.clone(),
                started + SOFTWARE_INPUT_TIMEOUT,
                Rc::new({
                    let app = self.application.clone();
                    move || stop(&app)
                }),
            ) {
                Ok(driver) => Some(driver),
                Err(cause) => {
                    eprintln!("system_ime stage=input_start_failed reason={cause}");
                    stop(&self.application);
                    return;
                }
            }
        } else {
            None
        };
        *self.software.borrow_mut() = software.clone();
        self.started.set(true);
        let timeout = if software.is_some() {
            SOFTWARE_INPUT_TIMEOUT
        } else {
            Duration::from_secs(300)
        };
        let seconds = timeout.as_secs_f64();
        // Driver构造与真实焦点查询也消耗原输入阶段期限；外层终止timer不能从这里另加30秒。
        let remaining = timeout.saturating_sub(started.elapsed()).as_secs_f64();
        *self.deadline.borrow_mut() = Some(InputDeadline::new(&self.application, remaining));
        println!(
            "system_ime stage=actual_focus_ready visible=true key=true app_active=true original_context_active=true on_active_space={} window_server_onscreen=true occlusion_visible=true software_events={} physical_keyboard={}",
            window.isOnActiveSpace(),
            software.is_some(),
            software.is_none(),
        );
        let stage = if software.is_some() {
            "awaiting_system_software_input"
        } else {
            "awaiting_physical_input"
        };
        println!(
            "system_ime stage={stage} expected=actual_han_candidate location={INSERTION} window_number={} visible={} key={} app_active={} software_events={} physical_keyboard={} deadline_seconds={seconds} injected_text=false whole_product=false formal=false",
            window.windowNumber(),
            window.isVisible(),
            window.isKeyWindow(),
            self.application.isActive(),
            software.is_some(),
            software.is_none(),
        );
        std::io::stdout().flush().unwrap();
        if let Some(driver) = software {
            driver.prepare_next();
        }
    }

    /// 只读本合成文稿在原插入点的有限实际提交，不把候选内容、文稿或Core映射留到后续阶段。
    /// UTF16与Unicode Han分类都完整成功才交纯值；不从输入源标识推断方案或重建预期词。
    fn capture_han_commit(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> Result<Vec<u16>, &'static str> {
        shared_validation::capture_han(&self.document, &self.body, commit)
    }
}

impl NativeMarkdownPagedBodyInputObserver for InputObservation {
    /// 原业务提交通知在Core借用结束后交付，只有实际marked再提交目标中文才结束人工输入。
    fn committed(&self, commit: &NativeMarkdownPagedInputCommit) {
        let software = self.software.borrow().clone();
        if !self.started.get()
            || self.finished.get()
            || software.as_ref().is_some_and(|driver| driver.stopped.get())
        {
            return; // 原客户的组合清理回调没有权限补签已超时/失败的实际输入序列。
        }
        if commit.state.marked.is_some() {
            self.marked_seen.set(true);
            if let Some(driver) = &software {
                driver.marked(commit);
            }
            return;
        }
        if !self.marked_seen.get() {
            if let Some(driver) = &software {
                driver.unexpected_commit(commit);
            }
            return;
        }
        if let Some(driver) = &software
            && (driver.next.get() != 2
                || !driver.candidate_observed()
                || driver.waiting.get().is_none_or(|(index, _)| index != 1))
        {
            // 单键产生marked不代表确认完成；原可见面板和Space阶段缺一就不能接纳后继汉字。
            driver.unexpected_commit(commit);
            return;
        }
        let units = match self.capture_han_commit(commit) {
            Ok(units) => units,
            Err(cause) => {
                if let Some(driver) = &software {
                    if driver.failed.get().is_none() {
                        driver.failed.set(Some(cause));
                    }
                    driver.stop_sequence();
                }
                return; // 不合格值不补试其他键串，也不能以净长度或已入队次数宣称中文成功。
            }
        };
        if let Some(driver) = &software
            && Instant::now() >= driver.deadline
        {
            driver
                .failed
                .set(Some("实际中文提交未在原三十秒输入期限内完成"));
            driver.stop_sequence();
            return;
        }
        *self.committed_utf16.borrow_mut() = Some(units);
        self.finished.set(true);
        stop(&self.application); // 标准run在真实中文提交后退出，不造按键或输入法回调。
    }

    fn local_input_changed(&self) {
        // 这个独立原生客户只等待正文的实际中文提交；格内历史变化不能满足该验收结束条件。
    }
}

/// 每次4KiB读回全部canonical内容，确认远端插字未覆盖原四MiB，也没有把组合过程变成多笔历史。
fn verify(document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>, inserted: &[u16]) {
    shared_validation::verify(document, inserted).unwrap();
}

/// 远端输入必须走到原70001位置；沿既有远端目标验收核有限后继范围，不能套用文首百分之一界限。
fn deliver_visible(stage: &str, body: &NativeMarkdownPagedBody, frame: Rc<PaintDelivery>) {
    let progress = deliver_frame(stage, body, frame);
    let beyond_visible = SourceFixture::Cjk.edited_byte_end(INSERTION + 9000);
    assert!(!progress.complete && progress.source_total_bytes >= SOURCE_BYTES);
    assert!(progress.frontier_source_bytes < beyond_visible);
    assert!(progress.shaped_input_bytes < beyond_visible);
} // 原三秒帧通知及4096-byte/256-scalar量子仍由deliver_frame逐次核验。

/// 原窗口事件循环接收系统输入法的真实组合/提交；软件按键与人工输入分别标记，超时不计通过。
pub(super) fn run(
    app: &Retained<NSApplication>,
    window: &NSWindow,
    body: &Rc<NativeMarkdownPagedBody>,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    mode: Mode,
) -> Result<(), String> {
    let target_frame = observe(body, Instant::now());
    let target = body
        .request_source_target(NSRange::new(INSERTION, 0))
        .unwrap();
    deliver_visible("system_ime_remote_viewport", body, target_frame);
    body.take_source_target_visible(&target).unwrap().unwrap();
    // 通过同一个原生视图的真实命中移动caret；不直接修改文稿选择或给输入法手造replacementRange。
    let caret = view.screen_rect(NSRange::new(INSERTION, 0)).unwrap().rect;
    let point = window.convertPointFromScreen(NSPoint::new(
        caret.origin.x,
        caret.origin.y + caret.size.height * 0.5,
    ));
    let click_frame = observe(body, Instant::now());
    for kind in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind, point, NSEventModifierFlags::empty(), 1.0, window.windowNumber(), None, 1, 1, 1.0,
        ).unwrap();
        // SAFETY: 原窗口坐标的实际公开鼠标签名，事件只在同步调用期间存活。
        unsafe {
            if kind == NSEventType::LeftMouseDown {
                let _: () = msg_send![view, mouseDown: &*event];
            } else {
                let _: () = msg_send![view, mouseUp: &*event];
            }
        }
    }
    deliver_visible("system_ime_remote_caret", body, click_frame);
    assert_eq!(
        view.input_state().unwrap().selection,
        MarkdownTextSelection::caret(INSERTION)
    );
    assert!(window.makeFirstResponder(Some(view)));
    let observer = Rc::new(InputObservation {
        body: Rc::downgrade(body),
        document: document.clone(),
        marked_seen: Cell::new(false),
        finished: Cell::new(false),
        committed_utf16: RefCell::new(None),
        application: app.clone(),
        software: RefCell::new(None),
        started: Cell::new(false),
        deadline: RefCell::new(Some(InputDeadline::new(app, 30.0))),
    });
    let erased: Rc<dyn NativeMarkdownPagedBodyInputObserver> = observer.clone();
    body.observe_input(Rc::downgrade(&erased)).unwrap();
    drop(erased); // Body只持原观察者Weak，焦点和输入阶段共用该客户，不另建文稿或输入图。
    let weak = Rc::downgrade(&observer);
    let updates = Rc::downgrade(&observer);
    let original_window = Weak::from(window);
    let original_view = Weak::from(view);
    let (focus, first_focus) = FocusNotifications::new(
        app,
        window,
        view,
        move || {
            if let (Some(observer), Some(window), Some(view)) =
                (weak.upgrade(), original_window.load(), original_view.load())
            {
                observer.start(mode, &window, &view);
            }
        },
        move || {
            if let Some(observer) = updates.upgrade()
                && observer.started.get()
            {
                let driver = observer.software.borrow().clone();
                if let Some(driver) = driver {
                    // 只借原通知owner的弱客户；RefCell先归还再进入Native，清理后无槽就早返。
                    autoreleasepool(|_| driver.host_updated());
                }
            }
        },
    );
    window.makeKeyAndOrderFront(None);
    app.activate();
    window.orderFrontRegardless();
    first_focus(); // 在真实显示调用之后检查原客户；不向正文发送按键或制造输入法回调。
    app.run(); // 唯一标准主循环同时负责首次显示、真实输入和超时，不stop后再次run。
    let deadline = observer.deadline.borrow_mut().take();
    drop(deadline);
    drop(focus); // 主循环退出时同步撤真实通知；后继历史与资源验收不会重启输入阶段。
    // run退出即撤观察者的输入接纳权；cleanup可同步unmark，不能把该次清理补签成成功输入。
    // 原已投递事件只属于这一窗口；失败返回后由父用例关闭它，不再run或挂载后继客户。
    let started = observer.started.replace(false);
    let completed_before_cleanup = observer.finished.get();
    let mut failure = (!started).then(|| {
        format!(
            "系统未完成原窗口真实显示与输入焦点交接：active={} visible={} key={} on_space={} occlusion={:?} original_responder={} original_context={}",
            app.isActive(),
            window.isVisible(),
            window.isKeyWindow(),
            window.isOnActiveSpace(),
            window.occlusionState(),
            window.firstResponder().is_some_and(|responder| {
                std::ptr::eq::<objc2_app_kit::NSResponder>(&*responder, view.as_super().as_super())
            }),
            NSTextInputContext::currentInputContext(view.mtm())
                .zip(view.inputContext())
                .is_some_and(|(current, original)| std::ptr::eq(&*current, &*original)),
        )
    });
    let software = observer.software.borrow_mut().take();
    // 面板收据只复制纯结果，必须在cleanup撤槽前保存；不保留系统窗口或旧组合CPU票。
    // None是人工路径没有本软件观察，Some(false)才是软件路径尚缺的最终面板证据。
    let panel_observed = software.as_ref().map(|driver| driver.candidate_observed());
    if let Some(driver) = &software {
        // next在入队前登记以接实际marked，故只表示已开始的press，不能推算真实分发或keyup次数。
        // 成功仍同时要求原marked、中文和面板终点；software_key_queued不能替代其中任何一项。
        let presses_started = driver.next.get();
        let cleaned = driver.cleanup();
        let original_failure = driver.failed.get(); // 清理发现源身份变化也须保留为本次真实失败。
        if failure.is_none()
            && !(completed_before_cleanup
                && original_failure.is_none()
                && presses_started == 2
                && cleaned)
        {
            failure = Some(format!(
                "当前IME软件事件序列失败：completed={completed_before_cleanup} original_failure={original_failure:?} presses_started={presses_started} panel_observed={panel_observed:?} local_input_cleaned={cleaned}",
            ));
        }
    }
    if failure.is_none() && !completed_before_cleanup {
        failure = Some("没有收到实际中文输入法marked到中文提交，不能计作真实IME验收".into());
    }
    let marked_seen = observer.marked_seen.get();
    let committed = observer.committed_utf16.borrow_mut().take();
    drop(observer); // 先撤输入弱客户，成功后的历史与失败后的关闭都不能重启输入阶段。
    drop(software); // 原context与本地软件key owner不能跨入父用例的严格释放终点。
    if let Some(failure) = failure {
        return Err(failure); // 父用例先走原唯一关闭/弱票/mmap门，再报告这里的首个真实失败。
    }
    assert!(marked_seen);
    let committed = committed.expect("真实中文成功始终保存同一次有限提交值");
    verify(document, &committed);
    assert_eq!(
        view.input_state().unwrap().selection,
        MarkdownTextSelection::caret(INSERTION + committed.len())
    );
    for (command, inserted) in [
        (sel!(undo:), false),
        (sel!(redo:), true),
        (sel!(undo:), false),
    ] {
        let frame = observe(body, Instant::now());
        view.command(command).unwrap();
        deliver_visible("system_ime_exact_history", body, frame);
        // 每次仍流式核完整四MiB；Redo只恢复同一次实际值，Undo必须精确归还整个原prefix/suffix。
        verify(document, if inserted { committed.as_slice() } else { &[] });
        let state = view.input_state().unwrap();
        assert!(state.marked.is_none());
        assert_eq!(
            state.selection,
            MarkdownTextSelection::caret(INSERTION + if inserted { committed.len() } else { 0 })
        ); // 撤销/重做也要恢复真实输入前后caret，不能只让净文字看起来相同。
        receipts.push(body.resource_receipts().unwrap());
    }
    // 原验收仍从文首继续全部编辑和释放流程；只走既有Cocoa导航，不留下额外正文历史。
    let top = observe(body, Instant::now());
    view.command(sel!(moveToBeginningOfDocument:)).unwrap();
    deliver_visible("system_ime_return", body, top);
    // 真实中文与一次历史事务已经完成；仍独立核最终面板收据，由父用例唯一严格关闭。
    // 不能因为文字正确而把未观察到的候选位置签成通过，也不能让这项缺证吞掉历史结果。
    let full_result = panel_observed != Some(false);
    println!(
        "system_ime stage=full_ime_result passed={full_result} marked_then_chinese=true one_undo=true exact_four_mebibytes=true viewport_lazy=true panel_observed={panel_observed:?} software_events={} physical_keyboard={} whole_product=false formal=false",
        matches!(mode, Mode::Software),
        matches!(mode, Mode::Manual),
    );
    if !full_result {
        return Err("实际中文提交与撤销重做已验证，但未观察到原组合起点附近的真实可见面板，完整IME验收仍失败".into());
    }
    Ok(())
}
