//! 当前已选输入法在原四MiB映射正文的远端视口组合；不切换输入源，也不调用插字接口。

#[path = "ime/panel.rs"]
mod panel;

use super::*;
use objc2::{ClassType, define_class, msg_send, rc::Retained};
use objc2_app_kit::{
    NSApplicationDidBecomeActiveNotification, NSApplicationDidUpdateNotification, NSEvent,
    NSEventModifierFlags, NSEventType, NSFloatingWindowLevel, NSScreen, NSTextInputContext,
    NSWindowCollectionBehavior, NSWindowDidBecomeKeyNotification,
    NSWindowDidChangeOcclusionStateNotification, NSWindowDidUpdateNotification,
    NSWindowOcclusionState, NSWorkspace,
};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFNumber, CFString, CFType};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGRectMakeWithDictionaryRepresentation,
    CGWindowListCreateDescriptionFromArray, kCGWindowAlpha, kCGWindowBounds, kCGWindowIsOnscreen,
    kCGWindowNumber, kCGWindowOwnerPID,
};
use objc2_foundation::{
    NSContainsRect, NSNotification, NSNotificationCenter, NSObjectProtocol, NSPointInRect,
    NSProcessInfo, NSTimer,
};
use omni_markdown_core::MarkdownTextSelection;
use omni_md_plugin::native_surface::{
    paged_body::{NativeMarkdownPagedBodyInputObserver, NativeMarkdownPagedBodyResourceReceipts},
    paged_input::{NativeMarkdownPagedInputCommit, NativeMarkdownPagedInputState},
    paged_text_view::NativeMarkdownPagedTextView,
};

const INSERTION: usize = 70_001;
const EXPECTED: [u16; 2] = [0x4e2d, 0x6587];

#[derive(Clone, Copy)]
/// 软件模式验证当前IME真实组合/提交及实际面板几何；不据此声称物理按键或候选内容已通过。
pub(super) enum Mode {
    Manual,
    Software,
}

/// 原应用焦点和窗口实际显示分别确认；仅有AppKit可见标记不能证明本人能看见正文。
struct FocusNotifications {
    center: Retained<NSNotificationCenter>,
    tokens: [Retained<ProtocolObject<dyn NSObjectProtocol>>; 5],
}

impl FocusNotifications {
    /// 一次真实焦点交接启动输入阶段，整个用例只进入一次NSApplication.run。
    fn new(
        app: &NSApplication,
        window: &NSWindow,
        view: &NativeMarkdownPagedTextView,
        on_ready: impl FnOnce() + 'static,
        on_update: impl Fn() + 'static,
    ) -> (Self, impl FnOnce()) {
        let center = NSNotificationCenter::defaultCenter();
        let application = Weak::from(app);
        let window_weak = Weak::from(window);
        let view = Weak::from(view);
        let callback = RefCell::new(Some(on_ready));
        let attempt = std::sync::Arc::new(dispatch2::MainThreadBound::new(
            move || {
                if callback.borrow().is_none() {
                    return false;
                }
                let (app, window, view) =
                    match (application.load(), window_weak.load(), view.load()) {
                        (Some(app), Some(window), Some(view)) => (app, window, view),
                        (app, window, view) => {
                            println!(
                                "system_ime stage=focus_owner_released app={} window={} view={}",
                                app.is_some(),
                                window.is_some(),
                                view.is_some(),
                            );
                            return false; // 原客户已释放，不能以迟到通知重新取得一次输入权限。
                        }
                    };
                let ready = Self::ready(&app, &window, &view);
                println!("system_ime stage=focus_check ready={ready}");
                if !ready {
                    return true; // 当前真实通知尚在AppKit交接栈内，退栈后允许一次同源检查。
                }
                let callback = callback.borrow_mut().take();
                if let Some(callback) = callback {
                    callback(); // 槽借用先结束，真实输入客户的同步通知不能再次开始同一序列。
                }
                false
            },
            app.mtm(),
        ));
        let notification = attempt.clone();
        let block =
            block2::RcBlock::new(move |_notification: std::ptr::NonNull<NSNotification>| {
                let mtm = MainThreadMarker::new().expect("原AppKit焦点通知只能在主线程交付");
                if notification.get(mtm)() {
                    let settled = notification.clone();
                    // 每个真实通知至多续一次；不自行重排续体，也不在已消费回调后制造更新循环。
                    dispatch2::DispatchQueue::main().exec_async(move || {
                        let mtm = MainThreadMarker::new().unwrap();
                        settled.get(mtm)();
                    });
                }
            });
        // 焦点回调消费后，原窗口/应用仍会收到真实更新通知；只在这些通知里交付面板等待。
        // MainThreadBound内仅有同一焦点block和弱输入观察者，不让通知中心保活正文或客户。
        // 此block自身不排后继面板任务，Host更新也不是Squirrel面板已可见的完成证明。
        let focus = std::sync::Arc::new(dispatch2::MainThreadBound::new(block.clone(), app.mtm()));
        let updated = std::sync::Arc::new(dispatch2::MainThreadBound::new(on_update, app.mtm()));
        let update_block =
            block2::RcBlock::new(move |notification: std::ptr::NonNull<NSNotification>| {
                let mtm = MainThreadMarker::new().expect("原AppKit更新通知只能在主线程交付");
                focus.get(mtm).call((notification,));
                updated.get(mtm)();
            });
        // SAFETY: 五个通知均属原主线程AppKit对象；可发送block只持MainThreadBound弱客户和原回调。
        let tokens = unsafe {
            [
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSApplicationDidBecomeActiveNotification),
                    Some(app),
                    None,
                    &block,
                ),
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWindowDidBecomeKeyNotification),
                    Some(window),
                    None,
                    &block,
                ),
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWindowDidChangeOcclusionStateNotification),
                    Some(window),
                    None,
                    &block,
                ),
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWindowDidUpdateNotification),
                    Some(window),
                    None,
                    &update_block,
                ),
                // 应用完成真实窗口更新时再检查一次，覆盖早期key/active通知之后才激活输入客户的次序。
                // 由标准run交付实际生命周期通知，不增加定时器、轮询或第二次事件循环。
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSApplicationDidUpdateNotification),
                    Some(app),
                    None,
                    &update_block,
                ),
            ]
        };
        // 初次检查由调用方在最后一次真实show/activate之后交付；不依赖更早已发出的通知。
        // 若AppKit尚在交接栈，只沿同一检查续一次主队列，不轮询或再次进入事件循环。
        let first = move || {
            let mtm = MainThreadMarker::new().unwrap();
            if attempt.get(mtm)() {
                dispatch2::DispatchQueue::main().exec_async(move || {
                    let mtm = MainThreadMarker::new().unwrap();
                    attempt.get(mtm)();
                });
            }
        };
        (Self { center, tokens }, first)
    }

    /// 同一原窗口与输入客户的真实权限必须同时成立；其他context不能冒充当前正文。
    fn ready(app: &NSApplication, window: &NSWindow, view: &NativeMarkdownPagedTextView) -> bool {
        // 启动通知可能先于run状态位更新；实际焦点和输入客户已成立即可准备本次输入。
        let active = app.isActive();
        let visible = window.isVisible();
        let key = window.isKeyWindow();
        let on_space = window.isOnActiveSpace();
        let occlusion = window.occlusionState();
        // AppKit活动状态与系统前台应用是独立事实；此处只记录原PID是否匹配，
        // 不读取或输出其他应用身份，也不把这项诊断增添为本轮焦点准入条件。
        // SAFETY: getpid无指针前提，只读取本进程的实际PID，不访问或控制其他进程。
        let original_pid = unsafe { libc::getpid() };
        let system_frontmost_is_original = NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .is_some_and(|application| application.processIdentifier() == original_pid);
        let original_responder = window.firstResponder().is_some_and(|responder| {
            std::ptr::eq::<objc2_app_kit::NSResponder>(&*responder, view.as_super().as_super())
        });
        let original_context = NSTextInputContext::currentInputContext(view.mtm())
            .zip(view.inputContext())
            .is_some_and(|(current, original)| std::ptr::eq(&*current, &*original));
        let focused = active
            && visible
            && key
            && occlusion.contains(NSWindowOcclusionState::Visible)
            && original_responder
            && original_context;
        // Spaces状态可迟于真实显示通知；以WindowServer的原单窗显示事实配合原输入焦点授权。
        // 仅在实际焦点已经齐备的这次通知取一份描述，不增加轮询、计时或另一事件循环。
        let server_onscreen = focused && original_window_is_onscreen(window);
        // 记录同一次真实交接缺少的具体权限；主循环退出后的状态不能反推早期通知已就绪。
        println!(
            "system_ime stage=focus_permissions active={active} visible={visible} key={key} on_space={on_space} occlusion={occlusion:?} original_responder={original_responder} original_context={original_context} window_server_onscreen={server_onscreen} system_frontmost_is_original={system_frontmost_is_original}",
        );
        focused && server_onscreen
    }
}

/// 只请求原NSWindow的一个ID；WindowServer描述确认已上屏，不读取图像或枚举其他窗口。
fn original_window_is_onscreen(window: &NSWindow) -> bool {
    let observed = autoreleasepool(|_| -> Result<bool, &'static str> {
        let window_id = u32::try_from(window.windowNumber())
            .ok()
            .filter(|id| *id != 0)
            .ok_or("original_window_id_unavailable")?;
        // 此API的输入是原始CGWindowID槽，不是CFNumber对象；NULL callbacks禁止retain/解引用ID。
        // 用完整指针宽槽零扩展单个u32，不能把u32数组冒充64位CFArray的指针数组。
        let mut only_id = [std::ptr::without_provenance::<std::ffi::c_void>(
            window_id as usize,
        )];
        // SAFETY: 一个有效栈槽贯穿同步Create；NULL callbacks仅复制数值，不访问整数代表的地址。
        let ids = unsafe { CFArray::new(None, only_id.as_mut_ptr(), 1, std::ptr::null()) }
            .ok_or("single_window_id_array_unavailable")?;
        // SAFETY: ids只含上方原始CGWindowID；公开函数只查询所给窗口，不捕获屏幕或查找其他窗口。
        let descriptions = unsafe { CGWindowListCreateDescriptionFromArray(Some(&ids)) }
            .ok_or("window_server_description_unavailable")?;
        // SAFETY: CreateDescription的公开返回合同为CFDictionary数组，键为CFString、值为CFType。
        // 后面的每个字段仍按真实CF类型和原PID/ID读取，不把整个描述当作已显示证明。
        let descriptions: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
            unsafe { CFRetained::cast_unchecked(descriptions) };
        if descriptions.len() != 1 {
            return Err("original_window_description_not_present"); // 已消失的ID可返回空数组，不能补查全窗。
        }
        let description = descriptions
            .get(0)
            .ok_or("original_window_description_not_present")?;
        // SAFETY: 固定公开键自macOS10.5可用；只借键本身，不读标题、应用名或其他窗口元数据。
        let (number_key, owner_key, onscreen_key, alpha_key, bounds_key) = unsafe {
            (
                kCGWindowNumber,
                kCGWindowOwnerPID,
                kCGWindowIsOnscreen,
                kCGWindowAlpha,
                kCGWindowBounds,
            )
        };
        let number = description
            .get(number_key)
            .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i32))
            .map(|number| number as u32); // SDK以SInt32编码CGWindowID；比较同一32位身份而非有符号大小。
        let owner = description
            .get(owner_key)
            .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i64));
        // SAFETY: getpid只取本进程标量；原窗口身份必须同时匹配，错误描述不参与后续显示判断。
        if number != Some(window_id) || owner != Some(i64::from(unsafe { libc::getpid() })) {
            return Err("original_window_identity_mismatch");
        }
        // IsOnscreen是可选CFBoolean；SDK规定缺键即未上屏，不能用AppKit可见标记填补它。
        let onscreen = description
            .get(onscreen_key)
            .and_then(|value| value.downcast_ref::<CFBoolean>().map(CFBoolean::as_bool))
            .unwrap_or(false);
        let alpha = description
            .get(alpha_key)
            .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_f64))
            .ok_or("original_window_alpha_unavailable")?;
        let bounds_value = description
            .get(bounds_key)
            .ok_or("original_window_bounds_unavailable")?;
        let bounds_dictionary = bounds_value
            .downcast_ref::<CFDictionary>()
            .ok_or("original_window_bounds_not_dictionary")?;
        let mut bounds = NSRect::ZERO;
        // SAFETY: 系统Bounds值已按CFDictionary核类型；公开转换同步写入完整本地CGRect。
        if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds_dictionary), &mut bounds) }
        {
            return Err("original_window_bounds_not_rect");
        }
        let nonempty_bounds = [
            bounds.origin.x,
            bounds.origin.y,
            bounds.size.width,
            bounds.size.height,
            bounds.origin.x + bounds.size.width,
            bounds.origin.y + bounds.size.height,
        ]
        .into_iter()
        .all(f64::is_finite)
            && bounds.size.width > 0.0
            && bounds.size.height > 0.0;
        let ready = onscreen && alpha.is_finite() && alpha > 0.0 && nonempty_bounds;
        println!(
            "system_ime stage=original_window_server window_id={window_id} identity_matches=true onscreen={onscreen} alpha={alpha} bounds={bounds:?} ready={ready} exact_window_count=1 image_read=false all_windows_scanned=false",
        );
        Ok(ready)
    });
    match observed {
        Ok(ready) => ready,
        Err(reason) => {
            println!(
                "system_ime stage=original_window_server ready=false reason={reason} exact_window_count=1 image_read=false all_windows_scanned=false",
            );
            false // 不可用的实际描述保留失败；不请求录屏权限或以迟到Spaces状态代签。
        }
    }
}

impl Drop for FocusNotifications {
    /// 完成、系统拒绝和panic都撤销本次订阅，通知中心不能保留验收回调或应用弱票。
    fn drop(&mut self) {
        for token in &self.tokens {
            // SAFETY: token均是此center实际addObserver返回的登记；各撤销一次。
            unsafe { self.center.removeObserver((**token).as_ref()) };
        }
    }
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

/// 本会话只持一对待投递的软件down/up；真实marked才准备下一press，系统队列负责实际分发。
struct SoftwareInput {
    weak: std::rc::Weak<Self>,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    view: Weak<NativeMarkdownPagedTextView>,
    application: Retained<NSApplication>,
    context: Retained<NSTextInputContext>,
    initial_source: Retained<NSString>,
    window_owner: Weak<NSWindow>,
    window: isize,
    next: Cell<usize>,
    pending: RefCell<Option<(usize, [Retained<NSEvent>; 2])>>,
    waiting: Cell<Option<(usize, u64)>>,
    // 最后一个字母只等待该完整Input的真实CPU；面板结果仅保纯身份/几何，不保活系统窗口。
    panel_input: Cell<Option<NativeMarkdownPagedInputState>>,
    // 最后一个字母的原帧从首次观察一直守到Space真正post；Missing不能换新帧补权。
    // key只有Source/Scene弱身份，不保正文；Space取走本槽后，Host通知不能再查旧组合。
    panel_frame: RefCell<Option<(NativeMarkdownPagedSceneKey, usize)>>,
    // firstRect/系统查询可同步发Host通知；只阻止本次Native栈的递归查询，不代表面板完成。
    panel_querying: Cell<bool>,
    panel_observation: Cell<Option<panel::PanelObservation>>,
    failed: Cell<Option<&'static str>>,
    stopped: Cell<bool>,
}

impl SoftwareInput {
    /// 只记录原context此刻已选的输入源；用户未启用中文或当前键序不能产出目标词都保留真实失败。
    /// 这里不枚举、选择或恢复输入源，也不借全局TIS绕过原客户的输入权限。
    fn new(
        app: &Retained<NSApplication>,
        window: &NSWindow,
        view: &NativeMarkdownPagedTextView,
        body: std::rc::Weak<NativeMarkdownPagedBody>,
        document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    ) -> Rc<Self> {
        let context = view.inputContext().expect("原正文必须持有系统输入context");
        // nil不能证明后继软件key仍交给最初输入源；拒绝本次验收，而不是替用户选择一个输入法。
        let initial_source = context
            .selectedKeyboardInputSource()
            .expect("原context当前输入源为空，无法绑定本次软件输入序列");
        let input = view.input_state().unwrap();
        assert!(input.marked.is_none());
        assert_eq!(input.selection, MarkdownTextSelection::caret(INSERTION));
        let driver = Rc::new_cyclic(|weak| Self {
            weak: weak.clone(),
            body,
            document,
            view: Weak::from(view),
            application: app.clone(),
            context,
            initial_source,
            window_owner: Weak::from(window),
            window: window.windowNumber(),
            next: Cell::new(0),
            pending: RefCell::new(None),
            waiting: Cell::new(None),
            panel_input: Cell::new(None),
            panel_frame: RefCell::new(None),
            panel_querying: Cell::new(false),
            panel_observation: Cell::new(None),
            failed: Cell::new(None),
            stopped: Cell::new(false),
        });
        let active =
            NSTextInputContext::currentInputContext(view.mtm()).expect("原输入context尚未激活");
        assert!(std::ptr::eq(&*active, &*driver.context));
        // 只记录刚才已绑定的系统输入源标识；不能从首键失败推测输入模式，也不额外查询或选择源。
        println!(
            "system_ime stage=current_input_source_bound source_id={} input_source_selected=false software_events=true physical_keyboard=false",
            *driver.initial_source,
        );
        driver
    }

    /// 本地槽只持下一个press的完整down/up；marked退栈后投递，不能递归分发事件。
    fn prepare_next(&self) {
        if self.stopped.get() || self.failed.get().is_some() {
            return;
        }
        let index = self.next.get();
        let keys = [0x06, 0x04, 0x1f, 0x2d, 0x05, 0x0d, 0x0e, 0x2d, 0x31];
        assert!(index < keys.len());
        assert!(self.pending.borrow().is_none() && self.waiting.get().is_none());
        // AppKit的keyEventWithType构造不保证走IME；由公开CG键盘事件形成真正backing，
        // 再桥接NSEvent。只给原虚拟键码，不设置Unicode文字、私有window字段或输入源。
        // 事件仍只post到本进程NSApp；这里绝不CGEventPost、PostToPid或模拟物理键盘。
        let created = (|| -> Result<[Retained<NSEvent>; 2], &'static str> {
            let create = |key_down: bool| -> Result<Retained<NSEvent>, &'static str> {
                let event = CGEvent::new_keyboard_event(None, keys[index], key_down)
                    .ok_or("系统拒绝创建本次Quartz软件键盘事件")?;
                CGEvent::set_flags(Some(&event), CGEventFlags::empty());
                CGEvent::set_integer_value_field(
                    Some(&event),
                    CGEventField::KeyboardEventAutorepeat,
                    0,
                );
                // 分别保留两个事件的真实创建时刻；CG以纳秒计，不人为制造按压时长或等待。
                let uptime = NSProcessInfo::processInfo().systemUptime();
                CGEvent::set_timestamp(Some(&event), (uptime * 1_000_000_000.0) as u64);
                let event = NSEvent::eventWithCGEvent(&event)
                    .ok_or("系统拒绝桥接本次Quartz软件键盘事件")?;
                if event.CGEvent().is_none() {
                    return Err("本次软件键盘事件没有真实CGEvent backing");
                }
                println!(
                    "system_ime stage=software_key_prepared index={index} key_down={key_down} event_window_number={} cg_backed=true timestamp_seconds={} software_events=true physical_keyboard=false",
                    event.windowNumber(),
                    event.timestamp(),
                );
                Ok(event) // NSEvent保留其真实CGEvent；局部CF owner随本同步构造栈归还。
            };
            // 一个keycode的按下与释放必须全部创建成功才交pending；up失败时down也归还，
            // 不能先post半个press，再用后继构造或重试补其释放。
            let down = create(true)?;
            let up = create(false)?;
            Ok([down, up])
        })();
        let events = match created {
            Ok(events) => events,
            Err(cause) => {
                if self.failed.get().is_none() {
                    self.failed.set(Some(cause));
                }
                stop(&self.application);
                return; // 构造拒绝也交父唯一严格关闭，不panic绕开弱票及mmap门。
            }
        };
        if self.stopped.get() || self.failed.get().is_some() {
            return; // Native构造期的迟到取消不能重新安装原本地pending槽。
        }
        *self.pending.borrow_mut() = Some((index, events));
        let weak = std::sync::Arc::new(dispatch2::MainThreadBound::new(
            self.weak.clone(),
            self.application.mtm(),
        ));
        // 标准run负责真实事件；一次性续体只取本地槽，晚到时弱票/停止状态不能复活输入会话。
        dispatch2::DispatchQueue::main().exec_async(move || {
            autoreleasepool(|_| {
                let mtm = MainThreadMarker::new().unwrap();
                if let Some(driver) = weak.get(mtm).upgrade()
                    && let Some(view) = driver.view.load()
                {
                    driver.dispatch_pending(&view);
                }
            });
        });
    }

    /// 原loop栈顶取走一个完整press，核原客户后按down/up顺序入队；不把posted写成delivered。
    fn dispatch_pending(&self, view: &NativeMarkdownPagedTextView) -> bool {
        let Some((index, [down, up])) = self.pending.borrow_mut().take() else {
            return false;
        }; // 槽借用先结束，系统分发后的真实marked回调可安全准备后继槽。
        if self.stopped.get() || self.failed.get().is_some() {
            return true; // 已撤权的本地key直接归还，不进入系统队列或发送给后继客户。
        }
        // Space只接收最后一个字母已实际发布的原CPU票，不以面板可见性作为确认前置。
        // 先把弱帧移到本同步栈再做Native查询；否则pending.take后的重入通知会再次看到
        // “尚未发送Space”的旧槽，既可能重复准备按键，也可能把正常确认后的换代误报撤权。
        let composition_frame = if index == 8 {
            self.panel_frame.borrow_mut().take()
        } else {
            None
        };
        let admitted = || {
            let current = NSTextInputContext::currentInputContext(self.application.mtm());
            let client = self.context.client();
            let Some(original_window) = self.window_owner.load() else {
                return false;
            };
            std::ptr::eq(
                Retained::as_ptr(&client).cast::<AnyObject>(),
                std::ptr::from_ref(view).cast::<AnyObject>(),
            ) && self.application.isActive()
                && self.application.keyWindow().is_some_and(|window| {
                    std::ptr::eq(&*window, &*original_window)
                        && window.windowNumber() == self.window
                        && window.isKeyWindow()
                        && window.firstResponder().is_some_and(|responder| {
                            std::ptr::eq(
                                Retained::as_ptr(&responder).cast::<AnyObject>(),
                                Retained::as_ptr(&client).cast::<AnyObject>(),
                            )
                        })
                })
                && current.is_some_and(|current| std::ptr::eq(&*current, &*self.context))
        };
        if !admitted() {
            if self.failed.get().is_none() {
                self.failed
                    .set(Some("原软件key交付前已失去窗口/输入context焦点权限"));
            }
            self.waiting.set(None);
            stop(&self.application);
            return true; // 本地owner随栈结束；不重投此key，也不拦截任何其他真实事件。
        }
        // 焦点正确不代表输入源仍相同。每键紧前核原标识；变化只结束本序列，绝不切回旧源。
        if self.context.selectedKeyboardInputSource().as_deref() != Some(&*self.initial_source) {
            if self.failed.get().is_none() {
                self.failed
                    .set(Some("当前输入源已变化，本次软件输入序列失效"));
            }
            self.waiting.set(None);
            stop(&self.application);
            return true;
        }
        // CG桥接事件的windowNumber可为0，系统将按真正keyWindow分发；Native查询后
        // 再核原窗口对象/context/输入源，不能把本事件改投到刚切换的客户或别的窗口。
        if !admitted()
            || self.context.selectedKeyboardInputSource().as_deref() != Some(&*self.initial_source)
        {
            if self.failed.get().is_none() {
                self.failed
                    .set(Some("软件key入队前原窗口或输入context已撤销"));
            }
            stop(&self.application);
            return true;
        }
        if index == 8 {
            // 面板观察后还有一次真实主队列间隔；投递前不能给新输入或新几何重新捕generation。
            // 原窗口的Native查询结束后最后核同Input/Scene/CPU，撤权只终止而不重试Space。
            if composition_frame
                .as_ref()
                .is_none_or(|(key, paints)| !self.panel_current(view, key, *paints))
            {
                if self.failed.get().is_none() {
                    self.failed
                        .set(Some("面板观察后的原Input或几何已撤销，不能确认候选"));
                }
                stop(&self.application);
                return true;
            }
        }
        let generation = view.input_state().unwrap().generation;
        self.waiting.set(Some((index, generation)));
        self.next.set(index + 1);
        // waiting先于入队安装且按press推进一次；系统后继marked仍决定下一press，
        // 不因up创建/入队就推进输入，也不把释放误当中文提交或面板显示完成。
        // currentEvent属于真正从队列取出的事件，不能用直接sendEvent假装它已经经过run。
        // CG桥接事件未必带windowNumber；只在原keyWindow/context仍属本客户时post，
        // 原run再沿NSApplication→原keyWindow/responder→输入context解释，
        // 不直接调用文本协议，也不在系统未产生marked时重试、切模式或替换输入源。
        // 同一主线程同步栈内相邻post，期间不插入getter、回调或等待。两事件都入原队尾，
        // 后继marked才能准备下一press，因此其down入队时这次up已经在前。
        // 仅记录实际posted；末SpaceDown提交即stop时，其up仍可能尚未由run取走。
        self.application.postEvent_atStart(&down, false);
        self.application.postEvent_atStart(&up, false);
        println!(
            "system_ime stage=software_key_posted index={index} posted_events_for_press=2 before_generation={generation} original_input_source=true software_events=true physical_keyboard=false"
        );
        true
    }

    /// 当前票只接受真正后继marked变化；一个key的多个通知不能重复发送下一字符。
    fn marked(&self, commit: &NativeMarkdownPagedInputCommit) {
        if self.stopped.get() || self.failed.get().is_some() {
            return; // 同一IME事件可先insert再marked；首错之后的回调不能登记CPU或覆盖失败。
        }
        if let Some((index, generation)) = self.waiting.get()
            && index < 8
            && commit.state.generation != generation
            && (commit.invalidation.characters_changed || commit.invalidation.marked_changed)
        {
            self.waiting.set(None);
            println!(
                "system_ime stage=software_marked index={index} generation={} software_events=true physical_keyboard=false",
                commit.state.generation,
            );
            if index == 7 {
                // Squirrel先回调setMarkedText才显示面板；此栈不能把旧面板或CPU排队当真实位置。
                // 在原提交尚未结束时登记下一实际CPU终点，避免退栈后才观察而漏掉已完成帧。
                // Body原观察槽只保本driver的Weak；原输入观察者继续唯一强持driver，不延长文稿生命。
                self.panel_input.set(Some(commit.state));
                let armed =
                    self.body
                        .upgrade()
                        .zip(self.weak.upgrade())
                        .is_some_and(|(body, driver)| {
                            let observer: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = driver;
                            body.observe_next_cpu_frame(Rc::downgrade(&observer))
                                .is_ok()
                        });
                if !armed {
                    self.panel_input.set(None);
                    if self.failed.get().is_none() {
                        self.failed.set(Some("无法观察原组合Input的下一实际CPU帧"));
                    }
                    stop(&self.application);
                }
            } else {
                self.prepare_next();
            }
        }
    }

    /// 面板查询只在原客户、同源完整Input及刚观察到的CPU仍当前时执行；Native查询后再纯核。
    /// 不把同generation或相同窗口号当身份，不从后继Ready重采一份授权给原组合。
    fn panel_current(
        &self,
        view: &NativeMarkdownPagedTextView,
        key: &NativeMarkdownPagedSceneKey,
        paints: usize,
    ) -> bool {
        // 实际CPU通知携带的key是唯一几何来源；原完整Input和nonce缺一都不能借新帧补权。
        // 字母已经全部投递、Space尚未创建才属于本观察阶段；清理或另一次输入会立即撤销。
        let pure = || {
            !self.stopped.get()
                && self.failed.get().is_none()
                && self.next.get() == 8
                && self.waiting.get().is_none()
                && self.pending.borrow().is_none()
                && key.request.input.marked.is_some()
                && std::rc::Weak::ptr_eq(&self.document, &key.request.document)
                && view
                    .input_state()
                    .is_ok_and(|state| state == key.request.input)
                && !view.is_busy()
                && paints > 0
                && view.cpu_paints() == Some(paints)
                && view.cpu_paint_pending() == Some(false)
                && view
                    .capture_ready_scene_key()
                    .is_ok_and(|actual| actual.same(key))
        };
        if !pure() {
            return false;
        }
        // Weak只保证不保活owner，不能单凭upgrade就认客户；仍核Body实际view是原通知客户。
        // 这些强借用只活本同步守卫，返回前归还，不进入后继主队列或系统窗口记录。
        let Some(body) = self.body.upgrade() else {
            return false;
        };
        if !body
            .view()
            .is_ok_and(|owner| std::ptr::eq::<NativeMarkdownPagedTextView>(&**owner, view))
        {
            return false;
        }
        // 焦点和当前context是两项不同系统事实：客户仍挂在原view，并不保证它还接收键盘。
        // Native getter均无RefCell/Doc借用；纯核在最后执行，重入的新选择或关闭不能蒙混通过。
        let context = NSTextInputContext::currentInputContext(self.application.mtm());
        let client = self.context.client();
        let Some(window) = self.window_owner.load() else {
            return false;
        };
        // 事件windowNumber只负责路由，不能代替窗口对象身份；原view也必须仍挂在同一窗口。
        // 弱窗口不会把关闭后的客户留到后来应用，号码复用也不能让新窗口接管本次观察。
        let original = std::ptr::eq(
            Retained::as_ptr(&client).cast::<AnyObject>(),
            std::ptr::from_ref(view).cast::<AnyObject>(),
        ) && context.is_some_and(|current| std::ptr::eq(&*current, &*self.context))
            && view
                .window()
                .is_some_and(|actual| std::ptr::eq(&*actual, &*window))
            && self.application.isActive()
            && self.application.keyWindow().is_some_and(|actual| {
                std::ptr::eq(&*actual, &*window)
                    && actual.windowNumber() == self.window
                    && actual.isKeyWindow()
                    && actual.firstResponder().is_some_and(|responder| {
                        std::ptr::eq(
                            Retained::as_ptr(&responder).cast::<AnyObject>(),
                            std::ptr::from_ref(view).cast::<AnyObject>(),
                        )
                    })
            });
        // 最后一个Native getter仍核原输入源；后置纯核防查询期间新输入、滚动或关闭补签。
        original
            && self.context.selectedKeyboardInputSource().as_deref() == Some(&*self.initial_source)
            && pure()
    }

    /// 仅原组合仍待确认时接受真实Host更新；Space的准备、入队和后继换代均不再查旧帧。
    fn host_updated(&self) {
        if self.stopped.get()
            || self.failed.get().is_some()
            || self.panel_observation.get().is_some()
            || self.panel_querying.get()
            || self.next.get() != 8
            || self.waiting.get().is_some()
            || self.pending.borrow().is_some()
        {
            return;
        }
        let frame = self.panel_frame.borrow().clone();
        if let Some((key, paints)) = frame {
            self.observe_panel(&key, paints); // 槽借用已结束，Native可同步交付另一个真实通知。
        }
    }

    /// 真实key与CPU均退栈后的首次查询，或一次真实Host更新，共用同一原Input/CPU权限。
    /// 面板与中文提交分别留下真实结果；Missing不阻断原Space，其余拒绝仍立即失败。
    /// 原期限与固定键序不变，不为缺失面板重试、补wake或在确认后补签组合期几何。
    fn observe_panel(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        if self.stopped.get()
            || self.failed.get().is_some()
            || self.panel_observation.get().is_some()
            || self.next.get() != 8
            || self.waiting.get().is_some()
            || self.pending.borrow().is_some()
            || self.panel_querying.replace(true)
        {
            return;
        }
        let observed = (|| -> Result<Option<panel::PanelObservation>, &'static str> {
            let view = self.view.load().ok_or("原组合客户已释放")?;
            let current = || self.panel_current(&view, key, paints);
            if !current() {
                return Err("原组合Input/CPU/焦点已变化，不能观察面板");
            }
            // Squirrel1.1.2以attributes(index:0)查询inline session起点；真实系统桥接
            // 已观测为marked.start的首字符，不是当前caret。只沿原CPU票取该组合起点，
            // 不能从第三方面板位置倒推字符，也不能以marked尾端的caret另造期望锚点。
            let marked = key
                .request
                .input
                .marked
                .filter(|marked| marked.length > 0)
                .ok_or("原Input没有可查询首字符的实际组合范围")?;
            let range = NSRange::new(marked.location, 1);
            let marked_end = marked
                .location
                .checked_add(marked.length)
                .ok_or("原组合范围末端溢出")?;
            let requested_end = range
                .location
                .checked_add(range.length)
                .ok_or("原组合首字符末端溢出")?;
            let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*view);
            let mut actual = NSRange::new(NSNotFound as usize, 0);
            // SAFETY: 同一原客户、组合首字符及栈上完整out范围；真实协议可扩展到完整字素。
            let rect = unsafe { client.firstRectForCharacterRange_actualRange(range, &mut actual) };
            // 请求从marked起点开始，返回值也必须从该起点覆盖所求首字符且不越marked末端。
            // 实际Native端点用checked_add核算；允许真实GCB扩展，不接受空范围或溢出补票。
            if !current()
                || actual.location != marked.location
                || actual
                    .location
                    .checked_add(actual.length)
                    .is_none_or(|end| end < requested_end || end > marked_end)
            {
                return Err("原组合起点没有同Input且完全位于组合范围内的实际firstRect");
            }
            // helper只读真实窗口。Host的CPU完成不能保证另一个进程的面板已发布；
            // Missing仅记录这项尚未观察到，仍须完整原Input与几何才能独立确认中文。
            let observed = match panel::observe(&self.initial_source, rect, current) {
                Ok(observed) => observed,
                Err(panel::PanelError::PanelMissing) => {
                    if !self.panel_current(&view, key, paints) {
                        return Err("面板等待期间原组合Input/CPU/焦点已变化");
                    }
                    return Ok(None);
                }
                Err(cause) => {
                    println!(
                        "system_ime stage=composition_panel_failed cause={cause:?} anchor=composition_start software_events=true physical_keyboard=false"
                    );
                    return Err("无法确认当前Squirrel真实组合面板");
                }
            };
            // 只输出原窗口号、进程和points几何；不读取标题、候选文字、窗口图像或AX树。
            // touches_screen_edge仅报告边缘事实，绝不能替near/same_screen的独立位置要求。
            println!(
                "system_ime stage=composition_panel pid={} launch_unix_seconds={} window_id={} panel_rect={:?} anchor_rect={:?} screen_rect={:?} distance_points={} maximum_distance_points={} near_anchor={} same_screen={} touches_screen_edge={} anchor=composition_start requested_range={range:?} actual_range={actual:?} actual_input_cpu=true candidate_contents_verified=false software_events=true physical_keyboard=false",
                observed.process.pid,
                observed.process.launch_unix_seconds,
                observed.window_id,
                observed.panel_rect,
                observed.anchor_rect,
                observed.screen_rect,
                observed.distance_points,
                observed.maximum_distance_points,
                observed.near_anchor,
                observed.same_screen,
                observed.touches_screen_edge,
            );
            if !observed.near_anchor
                || !observed.same_screen
                || !self.panel_current(&view, key, paints)
            {
                return Err("真实组合面板没有在原组合起点同屏附近");
            }
            Ok(Some(observed))
        })();
        match observed {
            Ok(observation) => {
                // Some只代表当次真实可见且贴近组合起点的面板；None不能由后继中文补签。
                // 两项结果独立：先走原Space/内容/历史验证，最终仍严格要求面板收据。
                self.panel_observation.set(observation);
                println!(
                    "system_ime stage=composition_confirmation_admitted panel_observed={} original_input_cpu=true panel_not_a_precondition=true final_panel_gate_preserved=true deadline_unchanged=true software_events=true physical_keyboard=false",
                    observation.is_some(),
                );
                // 创建CG事件也可能同步通知；原递归保护要守到pending真实安装，不能在
                // Missing分支提前放开而让同一Host更新再次创建Space。它不跨异步队列。
                self.prepare_next();
            }
            Err(cause) => {
                if self.failed.get().is_none() {
                    self.failed.set(Some(cause));
                }
                stop(&self.application);
            }
        }
        self.panel_querying.set(false); // 本同步查询及按键准备已退栈，不把保护当面板完成状态。
    }

    /// 错源直出英文或空格确认了其他词都是真失败；只唤醒原loop，不补造目标中文。
    fn unexpected_commit(&self, commit: &NativeMarkdownPagedInputCommit) {
        if self.waiting.get().is_some()
            && (commit.invalidation.characters_changed || commit.invalidation.marked_changed)
            && self.failed.get().is_none()
        {
            self.failed.set(Some("实际系统IME没有按本序列提交目标中文"));
            stop(&self.application);
        }
    }

    /// 先撤本地软件key，再只清理仍属于原客户、原输入源和原活动context的组合。
    /// 输入源变化或客户失焦时不操作系统组合；无法证明归属、或正常辞任后仍有marked，都返回失败。
    fn cleanup(&self) -> bool {
        let already_stopped = self.stopped.replace(true);
        self.waiting.set(None);
        self.panel_input.set(None);
        self.panel_observation.set(None);
        let panel_frame = self.panel_frame.borrow_mut().take();
        drop(panel_frame); // 原弱Scene票也随本次输入结束，不留给后来客户或后继Space。
        let pending = self.pending.borrow_mut().take();
        // 本地槽只含尚未post的key，归还它不会撤销已入系统队列的事件；这里不丢弃用户事件。
        // stopped先撤后继投递权，原窗口/context的真实关闭继续由本用例既有资源收尾负责。
        drop(pending);
        let source_unchanged =
            self.context.selectedKeyboardInputSource().as_deref() == Some(&*self.initial_source);
        // context不保活其client；先取得原view的真实强借用，再读client，不能向已释放客户发Native消息。
        let Some(original_view) = self.view.load() else {
            println!(
                "system_ime stage=local_input_cleanup cleaned=false original_client=false reason=original_view_released input_source_selected=false software_events=true physical_keyboard=false"
            );
            return false;
        };
        let client = self.context.client();
        let original_client = std::ptr::eq(
            Retained::as_ptr(&client).cast::<AnyObject>(),
            Retained::as_ptr(&original_view).cast::<AnyObject>(),
        );
        let mut resigned_local_client = None;
        if !already_stopped
            && original_client
            && client.hasMarkedText()
            && let Some(window) = self.window_owner.load()
        {
            // discard只通知系统转换，并不保证客户Core组合已结束。沿正式窗口辞任调用
            // 原客户的生产resign流程，让真实IME提交或既有Core收尾完成，不直调unmark协议。
            // view/context/window仍由本同步栈保护；所有RefCell借用先结束才调用Native。
            let original_focus = self.application.isActive()
                && self.application.keyWindow().is_some_and(|actual| {
                    std::ptr::eq(&*actual, &*window)
                        && actual.windowNumber() == self.window
                        && actual.isKeyWindow()
                        && actual.firstResponder().is_some_and(|responder| {
                            std::ptr::eq(
                                Retained::as_ptr(&responder).cast::<AnyObject>(),
                                Retained::as_ptr(&client).cast::<AnyObject>(),
                            )
                        })
                });
            if source_unchanged
                && original_focus
                && NSTextInputContext::currentInputContext(self.application.mtm())
                    .is_some_and(|current| std::ptr::eq(&*current, &*self.context))
                && self.context.selectedKeyboardInputSource().as_deref()
                    == Some(&*self.initial_source)
            {
                // 仅残余组合需要辞任；已经真正提交中文的成功路径保持原客户和后续历史动作。
                resigned_local_client = Some(window.makeFirstResponder(None));
            }
        }
        // 辞任返回也不是文字完成收据；仍核同原view的公开marked及真实Input，不能补签中文成功。
        let marked_cleared = original_client
            && !client.hasMarkedText()
            && original_view
                .input_state()
                .is_ok_and(|state| state.marked.is_none());
        // 辞任和客户查询可同步进入Native；返回后仍核最初源，发生变化只报告失败而不修复用户选择。
        let source_unchanged = source_unchanged
            && self.context.selectedKeyboardInputSource().as_deref() == Some(&*self.initial_source);
        if !source_unchanged && self.failed.get().is_none() {
            self.failed
                .set(Some("当前输入源已变化，本次软件输入序列失效"));
        }
        let pending_released = self.pending.borrow().is_none();
        let cleaned = pending_released
            && source_unchanged
            && marked_cleared
            && resigned_local_client != Some(false);
        println!(
            "system_ime stage=local_input_cleanup cleaned={cleaned} input_source_unchanged={source_unchanged} original_client={original_client} resigned_local_client={resigned_local_client:?} marked_cleared={marked_cleared} local_pending_released={pending_released} input_source_selected=false software_events=true physical_keyboard=false"
        );
        cleaned
    }
}

impl NativeMarkdownPagedBodyPaintObserver for SoftwareInput {
    /// 原Body已验证并发布完整CPU；只携同key弱身份再退一层，面板查询不穿过绘图回调栈。
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        // 只消费此前最后一个真实marked的完整Input；后继发布不能复活已经结束的等待。
        // 不因旧帧失效而反复重订阅或延长原输入期限，也不替换原输入观察者。
        let Some(input) = self.panel_input.take() else {
            return;
        };
        if self.stopped.get() || self.failed.get().is_some() {
            return;
        }
        if key.request.input != input {
            self.failed.set(Some("原组合CPU终点已被后继Input替代"));
            stop(&self.application);
            return;
        }
        // key只携原Source/Scene弱身份；跨队列不保存view、Doc、Reader或候选窗Native强边。
        // 再退一次主队列后仍检查view未处于输入回调，不能把Native嵌套交付冒充keyDown退栈。
        let weak = dispatch2::MainThreadBound::new(self.weak.clone(), self.application.mtm());
        let key = dispatch2::MainThreadBound::new(key.clone(), self.application.mtm());
        dispatch2::DispatchQueue::main().exec_async(move || {
            autoreleasepool(|_| {
                let mtm = MainThreadMarker::new().unwrap();
                if let Some(driver) = weak.get(mtm).upgrade() {
                    if driver.stopped.get() || driver.failed.get().is_some() {
                        return;
                    }
                    // 唯一原CPU终点只在本次退栈续体安装。更早的Host通知看不到等待槽，
                    // 后来的通知也只能重用此key/paints，不能重订阅或采新Input给旧组合补票。
                    *driver.panel_frame.borrow_mut() = Some((key.get(mtm).clone(), paints));
                    println!("system_ime stage=awaiting_composition_panel original_cpu=true actual_host_updates_only=true deadline_unchanged=true software_events=true physical_keyboard=false");
                    driver.observe_panel(key.get(mtm), paints);
                }
            });
        });
    }
}

impl Drop for SoftwareInput {
    /// 测试断言、系统异常或超时也撤掉本地发送权；不能以自动切回输入源冒充资源清理。
    fn drop(&mut self) {
        if !self.cleanup() {
            eprintln!(
                "system_ime stage=local_input_cleanup_failed software_events=true physical_keyboard=false"
            );
            // run已把真实清理失败返回父用例；Drop不能再panic抢掉首错、绕过唯一严格关闭。
            // 这里只撤本地发送权，尚存的原系统组合由同一Body的正常关闭继续归还。
        }
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
        let software = matches!(mode, Mode::Software).then(|| {
            SoftwareInput::new(
                &self.application,
                window,
                view,
                self.body.clone(),
                self.document.clone(),
            )
        });
        *self.software.borrow_mut() = software.clone();
        self.started.set(true);
        let seconds = if software.is_some() { 30.0 } else { 300.0 };
        *self.deadline.borrow_mut() = Some(InputDeadline::new(&self.application, seconds));
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
            "system_ime stage={stage} expected=中文 location={INSERTION} window_number={} visible={} key={} app_active={} software_events={} physical_keyboard={} deadline_seconds={seconds} injected_text=false whole_product=false formal=false",
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
}

impl NativeMarkdownPagedBodyInputObserver for InputObservation {
    /// 原业务提交通知在Core借用结束后交付，只有实际marked再提交目标中文才结束人工输入。
    fn committed(&self, commit: &NativeMarkdownPagedInputCommit) {
        let software = self.software.borrow().clone();
        if !self.started.get() || software.as_ref().is_some_and(|driver| driver.stopped.get()) {
            return; // 原客户的组合清理回调没有权限补签已超时/失败的实际输入序列。
        }
        if commit.state.marked.is_some() {
            self.marked_seen.set(true);
            if let Some(driver) = &software {
                driver.marked(commit);
            }
            return;
        }
        if !self.marked_seen.get()
            || commit.state.utf16_len != SourceFixture::Cjk.source_utf16() + 2
        {
            if let Some(driver) = &software {
                driver.unexpected_commit(commit);
            }
            return;
        }
        let original = self.document.upgrade().unwrap();
        let mut units = [0; 2];
        original
            .borrow()
            .copy_utf16_code_units(INSERTION, &mut units)
            .unwrap();
        if units != EXPECTED {
            if let Some(driver) = &software {
                driver.unexpected_commit(commit);
            }
            return; // 错词、英文或粘贴不能替代中文候选确认；仍保留实际失败/未完成事实。
        }
        self.finished.set(true);
        stop(&self.application); // 标准run在真实中文提交后退出，不造按键或输入法回调。
    }

    fn local_input_changed(&self) {
        // 这个独立原生客户只等待正文的实际中文提交；格内历史变化不能满足该验收结束条件。
    }
}

/// 每次4KiB读回全部canonical内容，确认远端插字未覆盖原四MiB，也没有把组合过程变成多笔历史。
fn verify(document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>, inserted: bool) {
    let original = document.upgrade().unwrap();
    let original = original.borrow();
    let extra = if inserted { 2 } else { 0 };
    assert_eq!(
        original.utf16_len(),
        SourceFixture::Cjk.source_utf16() + extra
    );
    assert_eq!(
        original.byte_len(),
        SourceFixture::Cjk.edited_source_bytes() + extra * 3
    );
    let mut units = [0; 2048];
    for start in (0..original.utf16_len()).step_by(units.len()) {
        let count = units.len().min(original.utf16_len() - start);
        original
            .copy_utf16_code_units(start, &mut units[..count])
            .unwrap();
        for (offset, &actual) in units[..count].iter().enumerate() {
            let at = start + offset;
            let expected = if at == 0 {
                b'X' as u16
            } else if inserted && (INSERTION..INSERTION + 2).contains(&at) {
                EXPECTED[at - INSERTION]
            } else {
                0x4e2d
            };
            assert_eq!(actual, expected, "真实中文输入修改了非目标位置{at}");
        }
    }
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
    let panel_observed = software
        .as_ref()
        .map(|driver| driver.panel_observation.get().is_some());
    if let Some(driver) = &software {
        let presses_posted = driver.next.get();
        let events_posted = presses_posted * 2; // 原固定九个press，每次只在同一同步栈投递一对。
        let cleaned = driver.cleanup();
        let original_failure = driver.failed.get(); // 清理发现源身份变化也须保留为本次真实失败。
        if failure.is_none()
            && !(completed_before_cleanup
                && original_failure.is_none()
                && presses_posted == 9
                && cleaned)
        {
            failure = Some(format!(
                "当前IME软件事件序列失败：completed={completed_before_cleanup} original_failure={original_failure:?} presses_posted={presses_posted} events_posted={events_posted} panel_observed={panel_observed:?} local_input_cleaned={cleaned}",
            ));
        }
    }
    if failure.is_none() && !completed_before_cleanup {
        failure = Some("没有收到实际中文输入法marked到中文提交，不能计作真实IME验收".into());
    }
    let marked_seen = observer.marked_seen.get();
    drop(observer); // 先撤输入弱客户，成功后的历史与失败后的关闭都不能重启输入阶段。
    drop(software); // 原context与本地软件key owner不能跨入父用例的严格释放终点。
    if let Some(failure) = failure {
        return Err(failure); // 父用例先走原唯一关闭/弱票/mmap门，再报告这里的首个真实失败。
    }
    assert!(marked_seen);
    verify(document, true);
    assert_eq!(
        view.input_state().unwrap().selection,
        MarkdownTextSelection::caret(INSERTION + 2)
    );
    for (command, inserted) in [
        (sel!(undo:), false),
        (sel!(redo:), true),
        (sel!(undo:), false),
    ] {
        let frame = observe(body, Instant::now());
        view.command(command).unwrap();
        deliver_visible("system_ime_exact_history", body, frame);
        verify(document, inserted);
        let state = view.input_state().unwrap();
        assert!(state.marked.is_none());
        assert_eq!(
            state.selection,
            MarkdownTextSelection::caret(INSERTION + if inserted { 2 } else { 0 })
        ); // 撤销/重做也要恢复真实输入前后caret，不能只让净文字看起来相同。
        receipts.push(body.resource_receipts().unwrap());
    }
    // 原验收仍从文首继续全部编辑和释放流程；只走既有Cocoa导航，不留下额外正文历史。
    let top = observe(body, Instant::now());
    view.command(sel!(moveToBeginningOfDocument:)).unwrap();
    deliver_visible("system_ime_return", body, top);
    // 真实中文与一次历史事务已经独立完成；仍缺面板就返回失败，由父用例唯一严格关闭。
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
