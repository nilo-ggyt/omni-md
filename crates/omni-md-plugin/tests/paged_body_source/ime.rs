//! 当前已选输入法在原四MiB映射正文的远端视口组合；不切换输入源，也不调用插字接口。

use super::*;
use objc2::{ClassType, define_class, msg_send, rc::Retained};
use objc2_app_kit::{
    NSApplicationDidBecomeActiveNotification, NSApplicationDidUpdateNotification, NSEvent,
    NSEventModifierFlags, NSEventType, NSFloatingWindowLevel, NSScreen, NSTextInputContext,
    NSWindowCollectionBehavior, NSWindowDidBecomeKeyNotification,
    NSWindowDidChangeOcclusionStateNotification, NSWindowDidUpdateNotification,
    NSWindowOcclusionState,
};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFNumber, CFString, CFType};
use objc2_core_graphics::{
    CGRectMakeWithDictionaryRepresentation, CGWindowListCreateDescriptionFromArray, kCGWindowAlpha,
    kCGWindowBounds, kCGWindowIsOnscreen, kCGWindowNumber, kCGWindowOwnerPID,
};
use objc2_foundation::{
    NSContainsRect, NSNotification, NSNotificationCenter, NSObjectProtocol, NSPointInRect,
    NSProcessInfo, NSTimer,
};
use omni_markdown_core::MarkdownTextSelection;
use omni_md_plugin::native_surface::{
    paged_body::{NativeMarkdownPagedBodyInputObserver, NativeMarkdownPagedBodyResourceReceipts},
    paged_input::NativeMarkdownPagedInputCommit,
    paged_text_view::NativeMarkdownPagedTextView,
};

const INSERTION: usize = 70_001;
const EXPECTED: [u16; 2] = [0x4e2d, 0x6587];

#[derive(Clone, Copy)]
/// 软件模式只验证AppKit到当前IME的真实软件事件；不能据此声称物理按键或候选位置已通过。
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
        // SAFETY: 五个通知均属原主线程AppKit对象；可发送block只持MainThreadBound弱客户和一次回调。
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
                    &block,
                ),
                // 应用完成真实窗口更新时再检查一次，覆盖早期key/active通知之后才激活输入客户的次序。
                // 由标准run交付实际生命周期通知，不增加定时器、轮询或第二次事件循环。
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSApplicationDidUpdateNotification),
                    Some(app),
                    None,
                    &block,
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
            "system_ime stage=focus_permissions active={active} visible={visible} key={key} on_space={on_space} occlusion={occlusion:?} original_responder={original_responder} original_context={original_context} window_server_onscreen={server_onscreen}",
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

/// 本会话只持一个待交付的软件key；真实marked才准备下一键，不向系统事件队列投递文本事件。
struct SoftwareInput {
    weak: std::rc::Weak<Self>,
    view: Weak<NativeMarkdownPagedTextView>,
    application: Retained<NSApplication>,
    context: Retained<NSTextInputContext>,
    initial_source: Retained<NSString>,
    window: isize,
    next: Cell<usize>,
    pending: RefCell<Option<(usize, Retained<NSEvent>)>>,
    waiting: Cell<Option<(usize, u64)>>,
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
            view: Weak::from(view),
            application: app.clone(),
            context,
            initial_source,
            window: window.windowNumber(),
            next: Cell::new(0),
            pending: RefCell::new(None),
            waiting: Cell::new(None),
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

    /// 本地槽只持下一个有限key；主队列弱续体等marked回调退栈后交付，不能递归sendEvent。
    fn prepare_next(&self) {
        if self.stopped.get() || self.failed.get().is_some() {
            return;
        }
        let index = self.next.get();
        let keys = [
            ("z", 0x06),
            ("h", 0x04),
            ("o", 0x1f),
            ("n", 0x2d),
            ("g", 0x05),
            ("w", 0x0d),
            ("e", 0x0e),
            ("n", 0x2d),
            (" ", 0x31),
        ];
        assert!(index < keys.len());
        assert!(self.pending.borrow().is_none() && self.waiting.get().is_none());
        let (text, code) = keys[index];
        let text = NSString::from_str(text);
        // 按系统启动时钟记录真正创建时刻；零时间戳不代表当前输入源之后到达的这次软件事件。
        let timestamp = NSProcessInfo::processInfo().systemUptime();
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown, NSPoint::ZERO, NSEventModifierFlags::empty(), timestamp,
            self.window, None, &text, &text, false, code,
        ).expect("系统拒绝创建本窗口的软件key事件");
        *self.pending.borrow_mut() = Some((index, event));
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

    /// 原loop栈顶取走一个本地key，每键核真实焦点后只send一次；这仍是软件事件而非物理键盘。
    fn dispatch_pending(&self, view: &NativeMarkdownPagedTextView) -> bool {
        let Some((index, event)) = self.pending.borrow_mut().take() else {
            return false;
        }; // 槽借用先结束，Native交付与同步marked回调可安全准备后继槽。
        if self.stopped.get() || self.failed.get().is_some() {
            return true; // 已撤权的本地key直接归还，不进入系统队列或发送给后继客户。
        }
        let current = NSTextInputContext::currentInputContext(self.application.mtm());
        let client = self.context.client();
        let admitted = std::ptr::eq(
            Retained::as_ptr(&client).cast::<AnyObject>(),
            std::ptr::from_ref(view).cast::<AnyObject>(),
        ) && self.application.isActive()
            && self.application.keyWindow().is_some_and(|window| {
                window.windowNumber() == self.window
                    && window.isKeyWindow()
                    && window.firstResponder().is_some_and(|responder| {
                        std::ptr::eq(
                            Retained::as_ptr(&responder).cast::<AnyObject>(),
                            Retained::as_ptr(&client).cast::<AnyObject>(),
                        )
                    })
            })
            && current.is_some_and(|current| std::ptr::eq(&*current, &*self.context));
        if !admitted {
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
        let generation = view.input_state().unwrap().generation;
        self.waiting.set(Some((index, generation)));
        self.next.set(index + 1);
        // waiting先于真实sendEvent安装，系统同步或异步marked都只消费这次输入；
        // 系统仍沿NSApplication→原responder→真实NSTextInputContext解释，不调用文本协议造中文。
        self.application.sendEvent(&event);
        println!(
            "system_ime stage=software_key_delivered index={index} before_generation={generation} original_input_source=true software_events=true physical_keyboard=false"
        );
        true
    }

    /// 当前票只接受真正后继marked变化；一个key的多个通知不能重复发送下一字符。
    fn marked(&self, commit: &NativeMarkdownPagedInputCommit) {
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
            self.prepare_next();
        }
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
    /// 输入源变化或客户失焦时不操作系统组合；无法证明归属、或公开discard后仍有marked，都返回失败。
    fn cleanup(&self) -> bool {
        let already_stopped = self.stopped.replace(true);
        self.waiting.set(None);
        let pending = self.pending.borrow_mut().take();
        drop(pending); // 软件key从未入系统队列；先撤发送权，再在无槽借用时实际归还本地Native owner。
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
        let mut discarded_local_marked = false;
        if !already_stopped && original_client && client.hasMarkedText() {
            // discard会回调输入客户，故不持本地RefCell借用；此时只准原激活context及其原焦点客户。
            let current = NSTextInputContext::currentInputContext(self.application.mtm());
            let original_focus = self.application.isActive()
                && self.application.keyWindow().is_some_and(|window| {
                    window.windowNumber() == self.window
                        && window.isKeyWindow()
                        && window.firstResponder().is_some_and(|responder| {
                            std::ptr::eq(
                                Retained::as_ptr(&responder).cast::<AnyObject>(),
                                Retained::as_ptr(&client).cast::<AnyObject>(),
                            )
                        })
                });
            if source_unchanged
                && original_focus
                && current.is_some_and(|current| std::ptr::eq(&*current, &*self.context))
            {
                self.context.discardMarkedText();
                discarded_local_marked = true;
            }
        }
        // SDK要求客户自行清除marked；调用discard本身不是完成收据，必须核原客户与真实Input状态。
        let marked_cleared = original_client
            && !client.hasMarkedText()
            && original_view
                .input_state()
                .is_ok_and(|state| state.marked.is_none());
        // discard和客户查询可同步进入Native；返回后仍核最初源，发生变化只报告失败而不修复用户选择。
        let source_unchanged = source_unchanged
            && self.context.selectedKeyboardInputSource().as_deref() == Some(&*self.initial_source);
        if !source_unchanged && self.failed.get().is_none() {
            self.failed
                .set(Some("当前输入源已变化，本次软件输入序列失效"));
        }
        let pending_released = self.pending.borrow().is_none();
        let cleaned = pending_released && source_unchanged && marked_cleared;
        println!(
            "system_ime stage=local_input_cleanup cleaned={cleaned} input_source_unchanged={source_unchanged} original_client={original_client} discarded_local_marked={discarded_local_marked} marked_cleared={marked_cleared} local_pending_released={pending_released} input_source_selected=false software_key_queued=false software_events=true physical_keyboard=false"
        );
        cleaned
    }
}

impl Drop for SoftwareInput {
    /// 测试断言、系统异常或超时也撤掉本地发送权；不能以自动切回输入源冒充资源清理。
    fn drop(&mut self) {
        if !self.cleanup() {
            eprintln!(
                "system_ime stage=local_input_cleanup_failed software_events=true physical_keyboard=false"
            );
            assert!(std::thread::panicking(), "原软件输入会话未能安全清理");
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
        let software = matches!(mode, Mode::Software)
            .then(|| SoftwareInput::new(&self.application, window, view));
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
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    mode: Mode,
) {
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
    let original_window = Weak::from(window);
    let original_view = Weak::from(view);
    let (focus, first_focus) = FocusNotifications::new(app, window, view, move || {
        if let (Some(observer), Some(window), Some(view)) =
            (weak.upgrade(), original_window.load(), original_view.load())
        {
            observer.start(mode, &window, &view);
        }
    });
    window.makeKeyAndOrderFront(None);
    app.activate();
    window.orderFrontRegardless();
    first_focus(); // 在真实显示调用之后检查原客户；不向正文发送按键或制造输入法回调。
    app.run(); // 唯一标准主循环同时负责首次显示、真实输入和超时，不stop后再次run。
    let deadline = observer.deadline.borrow_mut().take();
    drop(deadline);
    drop(focus); // 主循环退出时同步撤真实通知；后继历史与资源验收不会重启输入阶段。
    assert!(
        observer.started.get(),
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
    );
    let software = observer.software.borrow().clone();
    let completed_before_cleanup = observer.finished.get();
    if let Some(driver) = &software {
        let keys_delivered = driver.next.get();
        let cleaned = driver.cleanup();
        let original_failure = driver.failed.get(); // 清理发现源身份变化也须保留为本次真实失败。
        assert!(
            completed_before_cleanup
                && original_failure.is_none()
                && keys_delivered == 9
                && cleaned,
            "当前IME软件事件序列失败：completed={completed_before_cleanup} original_failure={original_failure:?} keys_delivered={keys_delivered} local_input_cleaned={cleaned}",
        );
        assert!(driver.pending.borrow().is_none());
    }
    assert!(
        completed_before_cleanup,
        "没有收到实际中文输入法marked到中文提交，不能计作真实IME验收"
    );
    assert!(observer.marked_seen.get());
    verify(document, true);
    assert_eq!(
        view.input_state().unwrap().selection,
        MarkdownTextSelection::caret(INSERTION + 2)
    );
    drop(observer); // 真实输入阶段结束即撤弱客户；后续自动历史动作不能补签人工中文输入。
    drop(software); // 原context与本地软件key owner只属于这次输入阶段，不进入历史/释放验收。
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
    println!(
        "system_ime marked_then_chinese=true one_undo=true exact_four_mebibytes=true viewport_lazy=true software_events={} physical_keyboard={} whole_product=false formal=false",
        matches!(mode, Mode::Software),
        matches!(mode, Mode::Manual),
    );
}
