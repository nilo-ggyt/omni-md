//! 同一真实 NSTextInputClient 的焦点、软件键和候选面板观察；不拥有应用事件循环。
//! 独立控制与正式 Host 共用此实现，调用方各自负责完成通知、历史验证和原窗口收尾。

use super::INSERTION;
use super::native_types::{
    NativeMarkdownPagedBody, NativeMarkdownPagedBodyPaintObserver, NativeMarkdownPagedInputCommit,
    NativeMarkdownPagedInputState, NativeMarkdownPagedSceneKey, NativeMarkdownPagedTextView,
};
use objc2::{
    ClassType, MainThreadOnly,
    rc::{Retained, Weak, autoreleasepool},
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{
    NSApplication, NSApplicationDidBecomeActiveNotification, NSApplicationDidUpdateNotification,
    NSEvent, NSEventType, NSTextInputClient, NSTextInputContext, NSWindow,
    NSWindowDidBecomeKeyNotification, NSWindowDidChangeOcclusionStateNotification,
    NSWindowDidUpdateNotification, NSWindowOcclusionState, NSWorkspace,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_graphics::{
    CGEvent, CGRectMakeWithDictionaryRepresentation, CGWindowListCreateDescriptionFromArray,
    kCGWindowAlpha, kCGWindowBounds, kCGWindowIsOnscreen, kCGWindowNumber, kCGWindowOwnerPID,
};
use objc2_foundation::{
    MainThreadMarker, NSNotFound, NSNotification, NSNotificationCenter, NSObjectProtocol, NSRange,
    NSRect, NSString, NSTimer,
};
use omni_markdown_core::{MarkdownDocumentBuffer, MarkdownTextSelection};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Instant,
};
#[path = "panel.rs"]
mod panel;

/// 原应用焦点和窗口实际显示分别确认；仅有AppKit可见标记不能证明本人能看见正文。
pub(super) struct FocusNotifications {
    center: Retained<NSNotificationCenter>,
    tokens: [Retained<ProtocolObject<dyn NSObjectProtocol>>; 5],
}

impl FocusNotifications {
    /// 一次真实焦点交接启动输入阶段，整个用例只进入一次NSApplication.run。
    pub(super) fn new(
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
        // 每次既有通知独立查询原单窗；focused为false不能把“尚未查询”写成WindowServer的false。
        // 这只补齐真实诊断，最终准入仍同时要求原AppKit焦点、Visible位和实际上屏事实。
        let server_onscreen = original_window_is_onscreen(window);
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

/// 本会话只持一对待投递的软件down/up；真实marked才准备下一press，系统队列负责实际分发。
pub(super) struct SoftwareInput {
    weak: std::rc::Weak<Self>,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    view: Weak<NativeMarkdownPagedTextView>,
    application: Retained<NSApplication>,
    // 完成只交还原调用方；独立控制可停止自己的 run，Host 则继续原事件循环收尾。
    on_stop: Rc<dyn Fn()>,
    context: Retained<NSTextInputContext>,
    initial_source: Retained<NSString>,
    // 与外层输入阶段共用唯一绝对期限；面板轮询、后继Space或Host通知均不能重新起算。
    pub(super) deadline: Instant,
    window_owner: Weak<NSWindow>,
    window: isize,
    pub(super) next: Cell<usize>,
    pending: RefCell<Option<(usize, [Retained<NSEvent>; 2])>>,
    pub(super) waiting: Cell<Option<(usize, u64)>>,
    // 最后一个字母只等待该完整Input的真实CPU；面板结果仅保纯身份/几何，不保活系统窗口。
    panel_input: Cell<Option<NativeMarkdownPagedInputState>>,
    // 最后一个字母的原帧从首次观察一直守到Space真正交付；Missing不能换新帧补权。
    // key只有Source/Scene弱身份，不保正文；Space取走本槽后，Host通知不能再查旧组合。
    panel_frame: RefCell<Option<(NativeMarkdownPagedSceneKey, usize)>>,
    // 另一个进程上屏不会必然更新本Host。只在原组合等待期保这一份标准timer；
    // block只持driver Weak，确认、失败和关闭均先invalidate，不保正文或候选窗。
    panel_timer: RefCell<Option<Retained<NSTimer>>>,
    // firstRect/系统查询可同步发Host通知；只阻止本次Native栈的递归查询，不代表面板完成。
    panel_querying: Cell<bool>,
    panel_observation: Cell<Option<panel::PanelObservation>>,
    pub(super) failed: Cell<Option<&'static str>>,
    pub(super) stopped: Cell<bool>,
}

impl SoftwareInput {
    /// 只消费原候选观察的终点事实；不向外暴露系统窗口或输入法进程资源。
    pub(super) fn candidate_observed(&self) -> bool {
        self.panel_observation.get().is_some()
    }

    /// 只记录原context此刻已选的输入源；用户未启用中文或当前键序不能产出目标词都保留真实失败。
    /// 这里不枚举、选择或恢复输入源，也不借全局TIS绕过原客户的输入权限。
    pub(super) fn new(
        app: &Retained<NSApplication>,
        window: &NSWindow,
        view: &NativeMarkdownPagedTextView,
        body: std::rc::Weak<NativeMarkdownPagedBody>,
        document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
        deadline: Instant,
        on_stop: Rc<dyn Fn()>,
    ) -> Result<Rc<Self>, &'static str> {
        let context = view.inputContext().ok_or("原正文系统输入 context 已释放")?;
        // nil不能证明后继软件key仍交给最初输入源；拒绝本次验收，而不是替用户选择一个输入法。
        let initial_source = context
            .selectedKeyboardInputSource()
            .ok_or("原 context 当前输入源为空，无法绑定实际按键")?;
        let input = view.input_state().map_err(|_| "原输入状态已撤销")?;
        if input.marked.is_some() || input.selection != MarkdownTextSelection::caret(INSERTION) {
            return Err("原远端光标已变化或仍有未完成组合");
        }
        let active =
            NSTextInputContext::currentInputContext(view.mtm()).ok_or("原输入 context 尚未激活")?;
        if !std::ptr::eq(&*active, &*context) {
            return Err("当前输入 context 已不是原正文");
        }
        let driver = Rc::new_cyclic(|weak| Self {
            weak: weak.clone(),
            body,
            document,
            view: Weak::from(view),
            application: app.clone(),
            on_stop,
            context,
            initial_source,
            deadline,
            window_owner: Weak::from(window),
            window: window.windowNumber(),
            next: Cell::new(0),
            pending: RefCell::new(None),
            waiting: Cell::new(None),
            panel_input: Cell::new(None),
            panel_frame: RefCell::new(None),
            panel_timer: RefCell::new(None),
            panel_querying: Cell::new(false),
            panel_observation: Cell::new(None),
            failed: Cell::new(None),
            stopped: Cell::new(false),
        });
        // 只记录刚才已绑定的系统输入源标识；不能从首键失败推测输入模式，也不额外查询或选择源。
        println!(
            "system_ime stage=current_input_source_bound source_id={} input_source_selected=false software_events=true physical_keyboard=false",
            *driver.initial_source,
        );
        Ok(driver)
    }

    /// 本地槽只持下一个press的完整down/up；marked退栈后投递，不能递归分发事件。
    pub(super) fn prepare_next(&self) {
        if self.stopped.get() || self.failed.get().is_some() {
            return;
        }
        if Instant::now() >= self.deadline {
            self.failed
                .set(Some("原三十秒输入期限已到，不能准备后继按键"));
            self.stop_sequence();
            return;
        }
        let index = self.next.get();
        // 当前Rime可以是双拼或形码，不能把全拼zhongwen当共同协议；只固定试一次a，绝不轮换键串。
        // 真实marked和可见候选出现后才以Space确认；这里只列虚拟键码与应有ASCII，不注入汉字。
        let presses: [(u16, u8); 2] = [(0x00, b'a'), (0x31, b' ')];
        assert!(index < presses.len());
        assert!(self.pending.borrow().is_none() && self.waiting.get().is_none());
        // CG-backed身份和合法ASCII自身都不证明输入法已组合或面板已显示，仍须消费实际后态。
        // NULL source 仅创建本地 CGEvent，不调用 CGEventPost/PostToPid、不申请全局事件权限，
        // 也不切换当前输入源或模式；真正处理结果仍由原 marked、中文和面板收据证明。
        let (keycode, ascii) = presses[index];
        let created = (|| -> Result<[Retained<NSEvent>; 2], &'static str> {
            let create = |key_down: bool| -> Result<Retained<NSEvent>, &'static str> {
                let raw = CGEvent::new_keyboard_event(None, keycode, key_down)
                    .ok_or("系统拒绝创建本次本地CG键盘事件")?;
                let event = NSEvent::eventWithCGEvent(&raw).ok_or("系统拒绝包装本次CG键盘事件")?;
                drop(raw); // NSEvent 按公开合同保留同一 CGEvent；本地额外 claim 不跨入后继队列。
                let cg_backed = event.CGEvent().is_some();
                let kind = if key_down {
                    NSEventType::KeyDown
                } else {
                    NSEventType::KeyUp
                };
                if event.r#type() != kind || event.keyCode() != keycode || !cg_backed {
                    return Err("实际CG包装事件的类型、键码或原事件对象不符");
                } // 原系统包装失败不补建纯文字事件，也不静默改用另一条输入路径。
                // 当前 Squirrel 的 keyDown 先读取此字符再处理虚拟键码；仅有 CGEvent
                // 不能证明这个前提。只核本次固定 ASCII 键，不写 Unicode 载荷或改变输入源。
                let characters = event
                    .charactersIgnoringModifiers()
                    .ok_or("实际CG包装事件缺少本次ASCII按键字符")?;
                if characters.length() != 1 || characters.characterAtIndex(0) != u16::from(ascii) {
                    return Err("实际CG包装事件的字符与本次ASCII按键不符");
                } // down/up 均须带同一次预期字符；缺失或不符不交 pending，也不冒充有效字母。
                println!(
                    "system_ime stage=software_key_prepared index={index} key_down={key_down} event_type={:?} event_key_code={} event_modifier_flags={:?} event_window_number={} timestamp_seconds={} cg_backed={cg_backed} unicode_payload_set=false global_event_posted=false transport=nsapplication_event_queue software_events=true physical_keyboard=false",
                    event.r#type(),
                    event.keyCode(),
                    event.modifierFlags(),
                    event.windowNumber(),
                    event.timestamp(),
                );
                Ok(event)
            };
            // 一个keycode的按下与释放必须全部创建成功才交pending；up失败时down也归还，
            // 不能先交付半个press，再用后继构造或重试补其释放。
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
                self.stop_sequence();
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
        // 退原marked/CPU回调栈后只交付这个press；此时没有Surface租约、文稿或本地槽借用。
        // 一次性续体只取本地槽，晚到时弱票/停止状态不能复活输入会话，也不自行重放事件。
        dispatch2::DispatchQueue::main().exec_async(move || {
            autoreleasepool(|_| {
                let mtm = MainThreadMarker::new().unwrap();
                if let Some(driver) = weak.get(mtm).upgrade()
                    && let Some(view) = driver.view.load()
                {
                    driver.post_pending(&view);
                }
            });
        });
    }

    /// 核原客户后把完整press交原应用队列，由唯一run正常取出；入队不代表IME消费。
    fn post_pending(&self, view: &NativeMarkdownPagedTextView) -> bool {
        let Some((index, [down, up])) = self.pending.borrow_mut().take() else {
            return false;
        }; // 槽借用先结束，系统分发后的真实marked回调可安全准备后继槽。
        if self.stopped.get() || self.failed.get().is_some() {
            return true; // 已撤权的本地key直接归还，不发送给后继客户。
        }
        // Space只接收最后一个字母的原CPU票；真实面板已经被同票观察后才会准备这个按键。
        // 先把弱帧移到本同步栈再做Native查询；否则pending.take后的重入通知会再次看到
        // “尚未发送Space”的旧槽，既可能重复准备按键，也可能把正常确认后的换代误报撤权。
        let composition_frame = if index == 1 {
            self.panel_frame.borrow_mut().take()
        } else {
            None
        };
        // 该press绑定原文稿此刻的完整Input；后续Native焦点查询不能替旧按键重采新选择。
        // 请求只持弱文稿身份，所有读取在入队前退栈，不跨系统回调借文稿或Body。
        let original_input = match view.capture_scene_request() {
            Ok(request) if std::rc::Weak::ptr_eq(&request.document, &self.document) => {
                request.input
            }
            _ => {
                if self.failed.get().is_none() {
                    self.failed.set(Some("原软件key文稿身份或完整Input已失效"));
                }
                self.stop_sequence();
                return true;
            }
        };
        let admitted = || {
            let current = NSTextInputContext::currentInputContext(self.application.mtm());
            let client = self.context.client();
            let Some(original_window) = self.window_owner.load() else {
                return false;
            };
            // 实际上屏单独查询；既不拿失败的焦点布尔代写WS结果，也不放松Visible位。
            // 此有限查询不读图像、正文或别的窗口，不能为本地同步路由省掉原显示门。
            let server_onscreen = original_window_is_onscreen(&original_window);
            std::ptr::eq(
                Retained::as_ptr(&client).cast::<AnyObject>(),
                std::ptr::from_ref(view).cast::<AnyObject>(),
            ) && self.application.isActive()
                && original_window.isVisible()
                && original_window
                    .occlusionState()
                    .contains(NSWindowOcclusionState::Visible)
                && server_onscreen
                && view
                    .window()
                    .is_some_and(|actual| std::ptr::eq(&*actual, &*original_window))
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
                && self.body.upgrade().is_some_and(|body| {
                    body.view().is_ok_and(|owner| {
                        std::ptr::eq::<NativeMarkdownPagedTextView>(&**owner, view)
                    })
                })
                && view
                    .capture_scene_request()
                    .is_ok_and(|request| std::rc::Weak::ptr_eq(&request.document, &self.document))
        };
        if !admitted() {
            if self.failed.get().is_none() {
                self.failed
                    .set(Some("原软件key交付前已失去窗口/输入context焦点权限"));
            }
            self.waiting.set(None);
            self.stop_sequence();
            return true; // 本地owner随栈结束；不重投此key，也不拦截任何其他真实事件。
        }
        // 焦点正确不代表输入源仍相同。每键紧前核原标识；变化只结束本序列，绝不切回旧源。
        if self.context.selectedKeyboardInputSource().as_deref() != Some(&*self.initial_source) {
            if self.failed.get().is_none() {
                self.failed
                    .set(Some("当前输入源已变化，本次软件输入序列失效"));
            }
            self.waiting.set(None);
            self.stop_sequence();
            return true;
        }
        // 原始CG包装事件的windowNumber可以为0；只交原应用队列，不私写窗口号或直投NSWindow。
        // 入队前仍核原窗口/context/输入源，实际分发由AppKit的原唯一run承担。
        // 这里不预言排队后的IME处理或焦点；后继marked和最终中文仍须属于原客户才可通过。
        if !admitted()
            || self.context.selectedKeyboardInputSource().as_deref() != Some(&*self.initial_source)
        {
            if self.failed.get().is_none() {
                self.failed
                    .set(Some("软件key交付前原窗口或输入context已撤销"));
            }
            self.stop_sequence();
            return true;
        }
        if index == 1 {
            // 面板观察后还有一次真实主队列间隔；交付前不能给新输入或新几何重新捕generation。
            // 原窗口的Native查询结束后最后核同Input/Scene/CPU，撤权只终止而不重试Space。
            if composition_frame
                .as_ref()
                .is_none_or(|(key, paints)| !self.panel_current(view, key, *paints))
            {
                if self.failed.get().is_none() {
                    self.failed
                        .set(Some("面板观察后的原Input或几何已撤销，不能确认候选"));
                }
                self.stop_sequence();
                return true;
            }
        }
        if !view
            .input_state()
            .is_ok_and(|input| input == original_input)
        {
            if self.failed.get().is_none() {
                self.failed
                    .set(Some("软件key焦点查询期间原完整Input已变化"));
            }
            self.stop_sequence();
            return true;
        }
        // 所有可重入焦点/Source查询都已归还；本press只能在原期限内入队，真正提交仍有原末端期限门。
        // 不能因面板较晚上屏而把30秒以后的确认或中文提交算作成功。
        if Instant::now() >= self.deadline {
            self.failed
                .set(Some("原三十秒输入期限已到，不能交付后继按键"));
            self.stop_sequence();
            return true;
        }
        let generation = original_input.generation;
        self.waiting.set(Some((index, generation)));
        self.next.set(index + 1);
        // waiting先于入队登记；不能在GCD续体里直接sendEvent，跳过原run的取事件阶段。
        // 两个原事件按down/up顺序追加到同一FIFO，由AppKit保活，既不走全局Quartz也不写中文。
        // 下一字母仍只由真实marked回调准备；它入队时本press的up已经在队列中，不反插到队首。
        self.application.postEvent_atStart(&down, false);
        self.application.postEvent_atStart(&up, false);
        // 返回仅说明已交原本地队列，不证明分发、释放键或IME处理完成；这些不能由本日志补签。
        // marked、候选面板、中文提交和原文/历史仍沿原独立实际终点严格验收。
        println!(
            "system_ime stage=software_key_queued index={index} queued_events_for_press=2 before_generation={generation} original_input_source=true cg_backed=true unicode_payload_set=false global_event_posted=false transport=nsapplication_event_queue software_events=true physical_keyboard=false"
        );
        true
    }

    /// 当前票只接受真正后继marked变化；一个key的多个通知不能重复发送下一字符。
    pub(super) fn marked(&self, commit: &NativeMarkdownPagedInputCommit) {
        if self.stopped.get() || self.failed.get().is_some() {
            return; // 同一IME事件可先insert再marked；首错之后的回调不能登记CPU或覆盖失败。
        }
        if let Some((0, generation)) = self.waiting.get()
            && commit.state.generation != generation
            && (commit.invalidation.characters_changed || commit.invalidation.marked_changed)
        {
            self.waiting.set(None);
            println!(
                "system_ime stage=software_marked index=0 generation={} software_events=true physical_keyboard=false",
                commit.state.generation,
            );
            // Squirrel先回调setMarkedText才显示面板；此栈不能把旧面板或CPU排队当真实位置。
            // 本次唯一字母先登记原提交对应的实际CPU终点，面板贴近后才能准备Space。
            // Body原观察槽只保driver的Weak，不为取得候选而延长文稿生命或补入另一字母。
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
                self.stop_sequence();
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
                && Instant::now() < self.deadline
                && self.next.get() == 1
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

    /// 仅原组合仍待确认时接受真实Host更新；Space的准备、交付和后继换代均不再查旧帧。
    pub(super) fn host_updated(&self) {
        if self.stopped.get()
            || self.failed.get().is_some()
            || self.panel_observation.get().is_some()
            || self.panel_querying.get()
            || self.next.get() != 1
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

    /// 首次Missing后等待真实外部面板上屏；所有采样仍消费原完整Input与同一CPU帧。
    /// timer不是完成收据，也不触发排版、输入或窗口显示；到期只失败，不自我延长。
    fn wait_for_panel(&self) {
        if self.stopped.get()
            || self.failed.get().is_some()
            || self.panel_observation.get().is_some()
            || self.panel_timer.borrow().is_some()
        {
            return;
        }
        if Instant::now() >= self.deadline {
            self.failed
                .set(Some("原三十秒输入期限内未观察到实际候选面板"));
            self.stop_sequence();
            return;
        }
        let weak = dispatch2::MainThreadBound::new(self.weak.clone(), self.application.mtm());
        let block = block2::RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
            autoreleasepool(|_| {
                let mtm = MainThreadMarker::new().expect("面板观察timer仅在原主线程交付");
                if let Some(driver) = weak.get(mtm).upgrade() {
                    if driver.stopped.get() || driver.failed.get().is_some() {
                        driver.cancel_panel_wait();
                    } else if Instant::now() >= driver.deadline {
                        driver
                            .failed
                            .set(Some("原三十秒输入期限内未观察到实际候选面板"));
                        driver.stop_sequence();
                    } else {
                        driver.host_updated(); // 同一原票重查，不续订CPU、不投递任何按键。
                    }
                }
            });
        });
        // SAFETY: 原主线程default runloop只登记这一份100ms标准timer；block跨轮次仅持Weak。
        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.1, true, &block) };
        if self.stopped.get() || self.failed.get().is_some() {
            timer.invalidate(); // Native构造重入后的取消不能留下没有owner的重复回调。
            return;
        }
        *self.panel_timer.borrow_mut() = Some(timer);
    }

    /// 先移出唯一timer槽，再执行Native取消；回调即使正在退栈也不持正文或后继输入权限。
    fn cancel_panel_wait(&self) {
        let timer = self.panel_timer.borrow_mut().take();
        if let Some(timer) = timer {
            timer.invalidate();
        }
    }

    /// 软件输入失败只通知原拥有者；候选观察先结束，后继关闭再归还原 context 和 mmap。
    pub(super) fn stop_sequence(&self) {
        self.cancel_panel_wait();
        (self.on_stop)();
    }

    /// 真实key/CPU退栈后的首次查询、Host更新和有限timer均共用原Input/CPU权限。
    /// 面板尚未上屏保留原组合与CPU票；其余拒绝立即失败，不在确认后补签组合期几何。
    fn observe_panel(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        if self.stopped.get()
            || self.failed.get().is_some()
            || self.panel_observation.get().is_some()
            || self.next.get() != 1
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
            // Missing只保留这次未完成事实，不能提前发送Space关闭仍在上屏的候选面板。
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
            Ok(Some(observation)) => {
                // 只有同原Input/CPU且真正可见、贴近组合起点的面板才授权准备Space。
                // 该纯收据不保活系统窗口；最终中文、历史与面板检查仍各自必须通过。
                self.panel_observation.set(Some(observation));
                self.cancel_panel_wait();
                println!(
                    "system_ime stage=composition_confirmation_admitted panel_observed=true original_input_cpu=true final_panel_gate_preserved=true deadline_unchanged=true software_events=true physical_keyboard=false",
                );
                // 创建事件也可同步通知；保护守到pending真实安装，防止同一Host更新重复准备Space。
                self.prepare_next();
            }
            Ok(None) => {
                // 不取走panel_frame、不改原Input、不建立确认按键；外部显示可以晚于Host更新。
                // 唯一标准timer只沿同一个key/paints重查，绝不重采CPU或延长原30秒期限。
                self.wait_for_panel();
                println!(
                    "system_ime stage=awaiting_composition_panel panel_observed=false original_input_cpu=true metadata_wait=host_update_or_standard_timer confirmation_dispatched=false deadline_unchanged=true software_events=true physical_keyboard=false"
                );
            }
            Err(cause) => {
                if self.failed.get().is_none() {
                    self.failed.set(Some(cause));
                }
                self.stop_sequence();
            }
        }
        self.panel_querying.set(false); // 本同步查询及按键准备已退栈，不把保护当面板完成状态。
    }

    /// 一个IME事件可先insertText再setMarkedText，未标记通知不是原按键的解释终点。
    /// 字母仍等真实marked；确认键若没有提交目标中文才失败，不补造文字或重放输入。
    pub(super) fn unexpected_commit(&self, commit: &NativeMarkdownPagedInputCommit) {
        if let Some((index, _)) = self.waiting.get()
            && (commit.invalidation.characters_changed || commit.invalidation.marked_changed)
            && self.failed.get().is_none()
        {
            if index == 0 {
                // Squirrel同次rimeUpdate可先消费已提交文字，再显示后继组合；此时立即
                // stop会让后面的真实marked被首错门挡掉。保留原waiting，不发送下一字母；
                // 若原事件始终没有marked，仍由既有30秒期限失败，不续期或另排检查任务。
                println!(
                    "system_ime stage=intermediate_unmarked index={index} generation={} utf16_len={} marked={:?} waiting_preserved=true next_press_dispatched=false deadline_unchanged=true software_events=true physical_keyboard=false",
                    commit.state.generation, commit.state.utf16_len, commit.state.marked,
                );
                return;
            }
            self.failed.set(Some("实际系统IME没有按本序列提交目标中文"));
            self.stop_sequence();
        }
    }

    /// 先撤本地软件key，再只清理仍属于原客户、原输入源和原活动context的组合。
    /// 输入源变化或客户失焦时不操作系统组合；无法证明归属、或正常辞任后仍有marked，都返回失败。
    pub(super) fn cleanup(&self) -> bool {
        let already_stopped = self.stopped.replace(true);
        self.cancel_panel_wait(); // 先撤重复系统回调，后面的Native组合清理不能再次启动观察。
        self.waiting.set(None);
        self.panel_input.set(None);
        self.panel_observation.set(None);
        let panel_frame = self.panel_frame.borrow_mut().take();
        drop(panel_frame); // 原弱Scene票也随本次输入结束，不留给后来客户或后继Space。
        let pending = self.pending.borrow_mut().take();
        // 本地槽只含尚未交付的key，归还它不会撤销已经同步分发的事件；这里不丢弃用户事件。
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
            self.stop_sequence();
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
                    println!("system_ime stage=awaiting_composition_panel original_cpu=true metadata_wait=host_update_or_standard_timer deadline_unchanged=true software_events=true physical_keyboard=false");
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
