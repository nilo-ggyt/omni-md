//! 只观察当前 Squirrel 的真实组合面板；不读取标题、候选文字或图像，也不驱动输入法。

use objc2::{MainThreadMarker, rc::autoreleasepool, runtime::AnyObject};
use objc2_app_kit::{NSRunningApplication, NSScreen};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_graphics::{
    CGRectMakeWithDictionaryRepresentation, CGShieldingWindowLevel, CGWindowListCopyWindowInfo,
    CGWindowListCreateDescriptionFromArray, CGWindowListOption, kCGWindowAlpha, kCGWindowBounds,
    kCGWindowIsOnscreen, kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerPID,
};
use objc2_foundation::{NSBundle, NSDictionary, NSRect, NSString, ns_string};

// 官方 1.1.2 的 SquirrelTheme.offsetHeight 固定为 5 点。仅用这个定位间隔加原客户
// 一行字符框高度容纳 IMK lineHeightRectangle 的行内基线差，不复制输入法的主题布局。
// https://github.com/rime/squirrel/blob/1.1.2/sources/SquirrelTheme.swift
const PANEL_OFFSET_POINTS: f64 = 5.0;

type WindowDescriptions = CFArray<CFDictionary<CFString, CFType>>;

/// 运行实例的纯身份；PID 配合真实启动时刻，不能把同名字进程或 PID 复用视为原输入法。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SquirrelProcessIdentity {
    pub(super) pid: i32,
    pub(super) launch_unix_seconds: f64,
}

/// 这里只返回实际窗口与坐标事实；没有 Source、Font、Reader、AX 或 Native 控件强边。
/// 面板同时承载预编辑和候选，故此结果不宣称识别了 AX 候选列表或验证了候选文字。
#[derive(Clone, Copy, Debug)]
pub(super) struct PanelObservation {
    pub(super) process: SquirrelProcessIdentity,
    pub(super) window_id: u32,
    pub(super) panel_rect: NSRect,
    pub(super) anchor_rect: NSRect,
    pub(super) screen_rect: NSRect,
    pub(super) distance_points: f64,
    pub(super) maximum_distance_points: f64,
    pub(super) near_anchor: bool,
    pub(super) same_screen: bool,
    pub(super) touches_screen_edge: bool,
}

/// 缺少身份、真实窗口或几何就拒绝，不能以 CPU Ready、marked 或公开 firstRect 补造面板。
#[derive(Clone, Copy, Debug)]
pub(super) enum PanelError {
    CallerRevoked,
    MainThreadRequired,
    SquirrelInstanceNotUnique,
    SquirrelIdentityUnavailable,
    SquirrelBundleMismatch,
    SquirrelVersionMismatch,
    InputSourceNotInBundle,
    WindowServerUnavailable,
    InvalidWindowMetadata,
    PanelMissing,
    PanelNotUnique,
    PanelChanged,
    InvalidGeometry,
    ScreenUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PanelWindow {
    id: u32,
    // WindowServer 使用主菜单屏左上原点的 points，返回前统一转换到原 firstRect 坐标系。
    server_rect: NSRect,
}

/// caller 必须先等原 keyDown 退栈和同 Input 的真实 CPU 帧；每次真实通知只观察一次，
/// 调用方沿同一原帧与期限等待，不以查询动作创建后继通知或延长输入期限。
/// current 核原 Doc/Window/View/Context、源身份、完整 Input 与几何；这里不重采其权限。
/// 所有 Native 查询都在调用方借用结束后进行，且各边界前后复核 current，不能跨重入保权。
pub(super) fn observe(
    input_source: &NSString,
    first_rect: NSRect,
    current: impl Fn() -> bool,
) -> Result<PanelObservation, PanelError> {
    let observation = autoreleasepool(|_| {
        if !current() {
            return Err(PanelError::CallerRevoked);
        }
        let mtm = MainThreadMarker::new().ok_or(PanelError::MainThreadRequired)?;
        if !valid_rect(first_rect, true) {
            return Err(PanelError::InvalidGeometry);
        }

        // 仅查固定输入法 bundle 的公开运行实例，不用应用显示名或输入源字符串前缀猜 PID。
        let bundle_id = ns_string!("im.rime.inputmethod.Squirrel");
        let applications = NSRunningApplication::runningApplicationsWithBundleIdentifier(bundle_id);
        if !current() {
            return Err(PanelError::CallerRevoked);
        }
        if applications.len() != 1 {
            // 只记录本次已有查询的数量；LS应用数组为空与多实例都不是原PID已退出的证明。
            println!(
                "system_ime stage=squirrel_instance_count phase=initial count={} process_exit_proven=false",
                applications.len(),
            );
            return Err(PanelError::SquirrelInstanceNotUnique);
        }
        let application = applications.objectAtIndex(0);
        let process = squirrel_identity(&application, input_source)?;
        if !current() {
            return Err(PanelError::CallerRevoked);
        }

        // 屏幕数组第一项是主菜单屏，不能用会随 key window 改变的 NSScreen.main 翻转 Y。
        // 选中的实际屏幕还须完整容纳原字符框；不会把离屏位置夹成另一个有效输入锚点。
        let screens = NSScreen::screens(mtm);
        let menu_screen = screens.firstObject().ok_or(PanelError::ScreenUnavailable)?;
        let menu_rect = menu_screen.frame();
        let mut screen_rect = None;
        for screen in screens.iter() {
            let frame = screen.frame();
            if valid_rect(frame, false) && contains(frame, first_rect) {
                screen_rect = Some(frame);
                break;
            }
        }
        let screen_rect = screen_rect.ok_or(PanelError::ScreenUnavailable)?;
        let shielding = CGShieldingWindowLevel();
        if !current() {
            return Err(PanelError::CallerRevoked);
        }

        // WindowServer 没有按 PID 列窗的入口。先只读 owner PID，再仅解码本输入法
        // 的有限窗口元数据；从不访问其他应用或本输入法窗口的标题、文字、图像字段。
        // 同一份列表包含隐藏窗口，便于区别尚未显示与根本没有原进程窗口；这不授予候选身份，
        // 下方panel_window仍严格要求原PID、shielding层级、onscreen和非零alpha。
        let windows = CGWindowListCopyWindowInfo(
            CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements,
            0,
        )
        .ok_or(PanelError::WindowServerUnavailable)?;
        // SAFETY: 公开 CopyWindowInfo 合同返回 CFDictionary 数组；字段在下层逐一检查实际 CF 类型。
        let windows: CFRetained<WindowDescriptions> =
            unsafe { CFRetained::cast_unchecked(windows) };
        if !current() {
            return Err(PanelError::CallerRevoked);
        }
        let mut found = None;
        let mut diagnostic_windows = 0;
        for description in windows.iter() {
            // 只借这一次系统返回的原字典，最多报告八个原输入法窗口；这不是第二次列窗，
            // 也不从日志重建候选或改变下方筛选。字段缺失保留 None，不能替它补默认层级。
            // SAFETY: 固定公开键均为系统常量；先核原PID，再读取其余有限标量和几何。
            let owner_key = unsafe { kCGWindowOwnerPID };
            let owner = description
                .get(owner_key)
                .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i32));
            if owner == Some(process.pid) && diagnostic_windows < 8 {
                // 不匹配层级的原进程窗口也须留事实，才能区分“没有面板”和“实际层级不同”。
                // 输出上限只约束日志；下方仍检查所有原记录，不能漏掉第九项的重复候选。
                // SAFETY: 这里只读已确认属于原Squirrel的窗口；不访问窗口标题或图像键。
                let (number, layer, onscreen, alpha, bounds) = unsafe {
                    (
                        kCGWindowNumber,
                        kCGWindowLayer,
                        kCGWindowIsOnscreen,
                        kCGWindowAlpha,
                        kCGWindowBounds,
                    )
                };
                let window_id = description
                    .get(number)
                    .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i32))
                    .map(|number| number as u32);
                let layer = description
                    .get(layer)
                    .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i32));
                let onscreen = description
                    .get(onscreen)
                    .and_then(|value| value.downcast_ref::<CFBoolean>().map(CFBoolean::as_bool));
                let alpha = description
                    .get(alpha)
                    .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_f64));
                // 诊断解码失败只打印None；合法性与可见性的拒绝仍完全由panel_window负责，
                // 这里不创建PanelWindow，也不会因另一层窗口缺少几何提前终止正式筛选。
                let bounds = description.get(bounds).and_then(|value| {
                    // CF 容器借用只活在原 retained 值的栈中；诊断仅交出 Copy 坐标。
                    let bounds = value.downcast_ref::<CFDictionary>()?;
                    let mut rect = NSRect::ZERO;
                    // SAFETY: 仅同步解码已有原字典的公开CGRect值；失败如实保留None。
                    unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds), &mut rect) }
                        .then_some(rect)
                });
                println!(
                    "system_ime stage=squirrel_window_metadata pid={} window_id={window_id:?} layer={layer:?} onscreen={onscreen:?} alpha={alpha:?} bounds={bounds:?} expected_shielding_layer={shielding} coordinate_space=window_server_points title_read=false image_read=false",
                    process.pid,
                );
                diagnostic_windows += 1;
            }
            if let Some(window) = panel_window(&description, process.pid, shielding)?
                && found.replace(window).is_some()
            {
                return Err(PanelError::PanelNotUnique);
            }
        }
        let panel = found.ok_or(PanelError::PanelMissing)?;

        // 初次已经确定唯一原 ID；后继只查这个 ID，不在它消失后找另一个同类窗口补票。
        if !current() {
            return Err(PanelError::CallerRevoked);
        }
        let confirmed = exact_window(panel.id, process.pid, shielding)?;
        if !current() {
            return Err(PanelError::CallerRevoked);
        }
        if confirmed != panel {
            return Err(PanelError::PanelChanged);
        }

        // NSRunningApplication 文档要求以对象相等性辨认实例；再核 PID/启动时刻和实际
        // bundle/version/source 表，避免查询期间退出重启或把新的输入法实例当原客户的面板。
        let remaining = NSRunningApplication::runningApplicationsWithBundleIdentifier(bundle_id);
        if remaining.len() != 1 {
            // 原窗口已实测后仍不从另一实例补票；记录真实末端数量，不放宽面板或存活判断。
            println!(
                "system_ime stage=squirrel_instance_count phase=after_window count={} original_pid={} process_exit_proven=false",
                remaining.len(),
                process.pid,
            );
            return Err(PanelError::SquirrelInstanceNotUnique);
        }
        let still_running = remaining.objectAtIndex(0);
        if application != still_running
            || squirrel_identity(&still_running, input_source)? != process
        {
            return Err(PanelError::SquirrelIdentityUnavailable);
        }
        if !current() {
            return Err(PanelError::CallerRevoked);
        }

        // firstRect 是 AppKit 屏幕 points；仅对 WindowServer 的左上原点翻 Y，不乘 backing scale。
        let mut panel_rect = panel.server_rect;
        panel_rect.origin.y = menu_rect.origin.y + menu_rect.size.height
            - panel.server_rect.origin.y
            - panel.server_rect.size.height;
        if !valid_rect(panel_rect, false) {
            return Err(PanelError::InvalidGeometry);
        }
        let distance_points = distance(first_rect, panel_rect);
        let maximum_distance_points = PANEL_OFFSET_POINTS + first_rect.size.height;
        if !distance_points.is_finite() || !maximum_distance_points.is_finite() {
            return Err(PanelError::InvalidGeometry);
        }
        // 屏边贴靠只报告事实，不因 clamp 就把远处面板放行为 near；调用方仍须同时核
        // near_anchor/same_screen，随后才允许真实 Space 确认并完成原中文值与 Undo 验证。
        let same_screen = contains(screen_rect, panel_rect);
        let touches_screen_edge = panel_rect.origin.x <= screen_rect.origin.x
            || panel_rect.origin.y <= screen_rect.origin.y
            || panel_rect.origin.x + panel_rect.size.width
                >= screen_rect.origin.x + screen_rect.size.width
            || panel_rect.origin.y + panel_rect.size.height
                >= screen_rect.origin.y + screen_rect.size.height;
        Ok(PanelObservation {
            process,
            window_id: panel.id,
            panel_rect,
            anchor_rect: first_rect,
            screen_rect,
            distance_points,
            maximum_distance_points,
            near_anchor: distance_points <= maximum_distance_points,
            same_screen,
            touches_screen_edge,
        })
    });
    // 本地 Foundation/CF 元数据已归还再作最后一次客户守卫；池的退出也不留作输入权限。
    if !current() {
        return Err(PanelError::CallerRevoked);
    }
    observation
}

/// 只读应用公开 bundle 元数据，不读取 Rime 方案或个人配置；原 source 必须是这份实际
/// bundle 声明的精确模式 ID，且运行实例及其可执行文件都来自这份 1.1.2 bundle。
fn squirrel_identity(
    application: &NSRunningApplication,
    input_source: &NSString,
) -> Result<SquirrelProcessIdentity, PanelError> {
    let pid = application.processIdentifier();
    let launch = application
        .launchDate()
        .ok_or(PanelError::SquirrelIdentityUnavailable)?
        .timeIntervalSince1970();
    if application.isTerminated() || pid <= 0 || !launch.is_finite() || launch <= 0.0 {
        return Err(PanelError::SquirrelIdentityUnavailable);
    }
    let bundle_url = application
        .bundleURL()
        .ok_or(PanelError::SquirrelBundleMismatch)?;
    let executable = application
        .executableURL()
        .ok_or(PanelError::SquirrelBundleMismatch)?;
    let bundle = NSBundle::bundleWithURL(&bundle_url).ok_or(PanelError::SquirrelBundleMismatch)?;
    let expected_id = ns_string!("im.rime.inputmethod.Squirrel");
    if application.bundleIdentifier().as_deref() != Some(expected_id)
        || bundle.bundleIdentifier().as_deref() != Some(expected_id)
        || bundle.executableURL().as_deref() != Some(&*executable)
    {
        return Err(PanelError::SquirrelBundleMismatch);
    }
    let version = bundle
        .objectForInfoDictionaryKey(ns_string!("CFBundleVersion"))
        .and_then(|value| value.downcast::<NSString>().ok())
        .ok_or(PanelError::SquirrelVersionMismatch)?;
    if &*version != ns_string!("1.1.2") {
        return Err(PanelError::SquirrelVersionMismatch);
    }
    let declared_source = bundle
        .objectForInfoDictionaryKey(ns_string!("ComponentInputModeDict"))
        .and_then(|value| value.downcast::<NSDictionary>().ok())
        .and_then(|modes| modes.objectForKey(ns_string!("tsInputModeListKey")))
        .and_then(|value| value.downcast::<NSDictionary>().ok())
        .and_then(|modes| modes.objectForKey(input_source))
        .and_then(|value| value.downcast::<NSDictionary>().ok())
        .and_then(|mode| mode.objectForKey(ns_string!("TISInputSourceID")))
        .and_then(|value: objc2::rc::Retained<AnyObject>| value.downcast::<NSString>().ok());
    if declared_source.as_deref() != Some(input_source) {
        return Err(PanelError::InputSourceNotInBundle);
    }
    Ok(SquirrelProcessIdentity {
        pid,
        launch_unix_seconds: launch,
    })
}

/// 仅对归属本进程且层级匹配的窗口读取显示和几何；不把 AppKit styleMask 推测成 WS 属性。
fn panel_window(
    description: &CFDictionary<CFString, CFType>,
    pid: i32,
    shielding: i32,
) -> Result<Option<PanelWindow>, PanelError> {
    // SAFETY: 固定公开 CGWindow 键由系统提供；这里只取所需元数据，不遍历字典值或读取标题。
    let (owner, layer, onscreen, alpha, number, bounds) = unsafe {
        (
            kCGWindowOwnerPID,
            kCGWindowLayer,
            kCGWindowIsOnscreen,
            kCGWindowAlpha,
            kCGWindowNumber,
            kCGWindowBounds,
        )
    };
    let owner = description
        .get(owner)
        .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i32))
        .ok_or(PanelError::InvalidWindowMetadata)?;
    if owner != pid {
        return Ok(None);
    }
    let layer = description
        .get(layer)
        .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i32))
        .ok_or(PanelError::InvalidWindowMetadata)?;
    if layer != shielding {
        return Ok(None);
    }
    // IsOnscreen 按 SDK 缺键即 false；alpha=0 的用户主题不视为真正可见的组合面板。
    let onscreen = description
        .get(onscreen)
        .and_then(|value| value.downcast_ref::<CFBoolean>().map(CFBoolean::as_bool))
        .unwrap_or(false);
    let alpha = description
        .get(alpha)
        .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_f64))
        .ok_or(PanelError::InvalidWindowMetadata)?;
    if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
        return Err(PanelError::InvalidWindowMetadata);
    }
    if !onscreen || alpha == 0.0 {
        return Ok(None);
    }
    let id = description
        .get(number)
        .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i32))
        .map(|number| number as u32)
        .filter(|id| *id != 0)
        .ok_or(PanelError::InvalidWindowMetadata)?;
    let bounds = description
        .get(bounds)
        .ok_or(PanelError::InvalidWindowMetadata)?;
    let bounds = bounds
        .downcast_ref::<CFDictionary>()
        .ok_or(PanelError::InvalidWindowMetadata)?;
    let mut rect = NSRect::ZERO;
    // SAFETY: 已核为系统给出的 CFDictionary；公开转换只同步写入本地 CGRect，失败不产生坐标票。
    if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds), &mut rect) }
        || !valid_rect(rect, false)
    {
        return Err(PanelError::InvalidGeometry);
    }
    Ok(Some(PanelWindow {
        id,
        server_rect: rect,
    }))
}

/// CGWindowID 数组是原始整数槽，不是 CFNumber；只查询初次唯一匹配的这个 ID。
fn exact_window(id: u32, pid: i32, shielding: i32) -> Result<PanelWindow, PanelError> {
    let mut only_id = [std::ptr::without_provenance::<std::ffi::c_void>(
        id as usize,
    )];
    // SAFETY: NULL callbacks 仅复制整数槽；同步调用期间不会解引用 ID 所代表的地址。
    let ids = unsafe { CFArray::new(None, only_id.as_mut_ptr(), 1, std::ptr::null()) }
        .ok_or(PanelError::WindowServerUnavailable)?;
    // SAFETY: 数组仅含上述一个原始 CGWindowID，符合公开 CreateDescriptionFromArray 输入合同。
    let descriptions = unsafe { CGWindowListCreateDescriptionFromArray(Some(&ids)) }
        .ok_or(PanelError::WindowServerUnavailable)?;
    // SAFETY: 公开函数返回 CFDictionary 数组；本文件仍逐字段核 CF 类型和原 PID/layer/ID。
    let descriptions: CFRetained<WindowDescriptions> =
        unsafe { CFRetained::cast_unchecked(descriptions) };
    if descriptions.len() != 1 {
        return Err(PanelError::PanelChanged);
    }
    let description = descriptions.get(0).ok_or(PanelError::PanelChanged)?;
    panel_window(&description, pid, shielding)?
        .filter(|window| window.id == id)
        .ok_or(PanelError::PanelChanged)
}

/// 允许原 caret 字符框宽为零；实际屏幕和面板必须有面积，所有外部坐标及末端都须有限。
fn valid_rect(rect: NSRect, caret: bool) -> bool {
    [
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
        rect.origin.x + rect.size.width,
        rect.origin.y + rect.size.height,
    ]
    .into_iter()
    .all(f64::is_finite)
        && (if caret {
            rect.size.width >= 0.0
        } else {
            rect.size.width > 0.0
        })
        && rect.size.height > 0.0
}

/// 同一个 points 坐标域的实际包含关系；不夹点、不放大屏幕，也不改变候选原窗口。
fn contains(outer: NSRect, inner: NSRect) -> bool {
    inner.origin.x >= outer.origin.x
        && inner.origin.y >= outer.origin.y
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height
}

/// 两个完整矩形的最近距离保留竖排/上翻面板的真实方位，不假定某个角点必须对齐。
fn distance(anchor: NSRect, panel: NSRect) -> f64 {
    let dx = (anchor.origin.x - panel.origin.x - panel.size.width)
        .max(panel.origin.x - anchor.origin.x - anchor.size.width)
        .max(0.0);
    let dy = (anchor.origin.y - panel.origin.y - panel.size.height)
        .max(panel.origin.y - anchor.origin.y - anchor.size.height)
        .max(0.0);
    dx.hypot(dy)
}
