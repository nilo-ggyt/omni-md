//! 显式feature的有限观测，不引入observer复位、替换content subclass或默认日志成本。

use super::*;
use objc2_app_kit::{
    NSTextContentStorage, NSTextContentStorageUnsupportedAttributeAddedNotification,
    NSTextViewDidSwitchToNSLayoutManagerNotification,
    NSTextViewWillSwitchToNSLayoutManagerNotification,
};
use objc2_foundation::NSNotificationCenter;

#[cfg(feature = "native-markdown-runtime-probe")]
mod paint_receipts;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(in crate::native_surface) use paint_receipts::{
    CodePaintReceipt, MAX_PROBE_CODE_REQUEST_UTF16, NativeMarkdownPreviewBlockProbeSnapshot,
    ProbeBlockKey, cancel_code_paint_for_body, code_paint_sequence, code_snapshot_matches,
    matching_code_paint, observe_code_paint, painted_current, prepared,
};

thread_local! {
    static SEEN: Cell<u16> = const { Cell::new(0) };
}

/// 每类只记录一次有限标量，绝不记录正文、路径、地址或取得额外文本副本。
fn record(bit: u8, message: impl FnOnce()) {
    SEEN.with(|seen| {
        if seen.get() & (1 << bit) != 0 {
            return;
        } // 重复layout不增加日志或诊断工作。
        seen.set(seen.get() | (1 << bit));
        message(); // 不持TLS借用；闭包只格式化调用方已取得的标量。
    });
}

/// 接收selector过滤后的公开事件；每种最多一次，只有明确feature构建才存在。
pub(in crate::native_surface) fn notification(kind: u8) {
    record(kind, || {
        eprintln!("block_background_diagnostic notification={kind}")
    });
}

/// 仅输出正常draw已取得的geometry，不为日志主动布局或枚举。
pub(in crate::native_surface) fn painted(kind: u8, width: f64, height: f64) {
    record(3 + kind, || {
        eprintln!("block_background_diagnostic painted kind={kind} width={width} height={height}")
    });
}

/// 与可失败构造结束点绑定，过滤只接受这个surface的实际content/body view。
pub(in crate::native_surface) fn install(
    view: &PieceBackedMarkdownTextView,
    content: &NSTextContentStorage,
) {
    let center = NSNotificationCenter::defaultCenter();
    let observer = AsRef::<AnyObject>::as_ref(view);
    // SAFETY: 三个selector均接收NSNotification，surface保持view/content，Drop拆图前对称remove。
    unsafe {
        center.addObserver_selector_name_object(
            observer,
            sel!(blockUnsupported:),
            Some(NSTextContentStorageUnsupportedAttributeAddedNotification),
            Some(AsRef::<AnyObject>::as_ref(content)),
        );
        center.addObserver_selector_name_object(
            observer,
            sel!(blockWillSwitch:),
            Some(NSTextViewWillSwitchToNSLayoutManagerNotification),
            Some(observer),
        );
        center.addObserver_selector_name_object(
            observer,
            sel!(blockDidSwitch:),
            Some(NSTextViewDidSwitchToNSLayoutManagerNotification),
            Some(observer),
        );
    }
}

/// 只拆除本诊断三项，不清除框架或生产delegate的注册。
pub(in crate::native_surface) fn uninstall(
    view: &PieceBackedMarkdownTextView,
    content: &NSTextContentStorage,
) {
    let center = NSNotificationCenter::defaultCenter();
    let observer = AsRef::<AnyObject>::as_ref(view);
    // SAFETY: observer/name/object与install完全一致，调用时surface尚保持真实对象。
    unsafe {
        center.removeObserver_name_object(
            observer,
            Some(NSTextContentStorageUnsupportedAttributeAddedNotification),
            Some(AsRef::<AnyObject>::as_ref(content)),
        );
        center.removeObserver_name_object(
            observer,
            Some(NSTextViewWillSwitchToNSLayoutManagerNotification),
            Some(observer),
        );
        center.removeObserver_name_object(
            observer,
            Some(NSTextViewDidSwitchToNSLayoutManagerNotification),
            Some(observer),
        );
    }
}

thread_local! {
    // 进程总上限16次切换，每次3个阶段；多个note不能无限放大观测日志。
    static MODE_TRANSITIONS: Cell<u8> = const { Cell::new(0) };
}

/// 固定标量统计，不持Native对象/文档/路径；只有显式feature改变类布局。
#[derive(Clone, Copy)]
struct ModeQueries {
    transition: u8,
    reported: u8,
    viewport_start: usize,
    viewport_end: usize,
    queries: u64,
    minimum: usize,
    maximum: usize,
    maximum_request: usize,
    outside_viewport: u64,
}

/// 一个storage只保最近一次有限观测，默认产品不含计数器或日志分支。
#[derive(Default)]
pub(in crate::native_surface) struct ModeAttributeDiagnostic(Cell<Option<ModeQueries>>);

impl ModeAttributeDiagnostic {
    /// 明确模式动作开始时捕获已存在视口，不在属性getter内查询几何或布局。
    pub(in crate::native_surface) fn begin(&self, viewport: std::ops::Range<usize>) {
        let transition = MODE_TRANSITIONS.with(|counter| {
            let next = counter.get();
            if next >= 16 {
                return None; // 全进程预算耗尽后永久静默，不重置到另一个note继续记录。
            }
            counter.set(next + 1);
            Some(next)
        });
        self.0.set(transition.map(|transition| ModeQueries {
            transition,
            reported: 0,
            viewport_start: viewport.start,
            viewport_end: viewport.end,
            queries: 0,
            minimum: usize::MAX,
            maximum: 0,
            maximum_request: 0,
            outside_viewport: 0,
        }));
    }
    /// 每次真实attributesAtIndex入口只增加标量，不读取正文或Native状态。
    pub(in crate::native_surface) fn query(&self, index: usize) {
        if let Some(mut state) = self.0.get() {
            state.queries = state.queries.saturating_add(1);
            state.minimum = state.minimum.min(index);
            state.maximum = state.maximum.max(index.saturating_add(1));
            if index < state.viewport_start || index >= state.viewport_end {
                state.outside_viewport = state.outside_viewport.saturating_add(1); // 只描述所捕获视口，不推断查询原因。
            }
            self.0.set(Some(state));
        }
    }
    /// 实际4Ki属性窗口准备点记录请求大小，不能把它解释为Foundation总分配。
    pub(in crate::native_surface) fn request(&self, length: usize) {
        if let Some(mut state) = self.0.get() {
            state.maximum_request = state.maximum_request.max(length);
            self.0.set(Some(state));
        }
    }
    /// 每阶段最多一次；输出前结束Cell访问，分阶段清计数以区分同步通知/queue/paint。
    pub(in crate::native_surface) fn report(&self, stage: u8) {
        let Some(mut state) = self.0.get() else {
            return; // 非模式切换不输出，即使产品正常初始layout很多次。
        };
        if stage >= 3 || state.reported & (1 << stage) != 0 {
            return; // 48条硬上限，不轮询也不重复打印空阶段。
        }
        let snapshot = state;
        state.reported |= 1 << stage;
        state.queries = 0;
        state.minimum = usize::MAX;
        state.maximum = 0;
        state.maximum_request = 0;
        state.outside_viewport = 0;
        self.0.set(Some(state));
        let minimum = (snapshot.queries != 0).then_some(snapshot.minimum);
        eprintln!(
            "block_background_mode transition={} stage={} queries={} min={minimum:?} max={} max_request={} outside_viewport={} viewport_start={} viewport_end={}",
            snapshot.transition,
            stage,
            snapshot.queries,
            snapshot.maximum,
            snapshot.maximum_request,
            snapshot.outside_viewport,
            snapshot.viewport_start,
            snapshot.viewport_end
        );
    }
}
