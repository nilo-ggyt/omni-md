//! 只用固定TLS标量标注原Native调用；不读取几何、不改变原票或产生后继任务。

use std::cell::Cell;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Phase {
    Idle,
    SourceAnchor,
    BoundedVisible,
    BoundedOrigin,
    BoundedDropScroll,
    BoundedDropWindow,
    ReadVisible,
    ReadStorage,
    ReadManager,
    ReadExpectedManager,
    ReadLayoutQueue,
    ReadContent,
    ReadEditing,
    ReadOrigin,
    ReadBounds,
    ReadClip,
    RequestKey,
    KeyStorage,
    KeyVisible,
    KeyBounds,
    KeyRead,
    FrameCallback,
    FrameRectCallback,
    FrameRectSuper,
    BoundsRectCallback,
    BoundsRectSuper,
    FrameStorage,
    FrameTraceBefore,
    FrameTraceAfter,
    FrameSuper,
    FrameSchedule,
    #[cfg(feature = "native-raster-surface-probe")]
    GeometryCallback,
    // 只有光栅对照路径注册通知；其他诊断组合不生成不存在的事件标记。
    #[cfg(feature = "native-raster-surface-probe")]
    NoticeLoad,
    #[cfg(feature = "native-raster-surface-probe")]
    NoticeLayout,
    InvalidateBody,
    InvalidateClip,
    InvalidateDisplay,
}

impl Phase {
    fn is_native_operation(self) -> bool {
        #[cfg(feature = "native-raster-surface-probe")]
        if self == Self::GeometryCallback {
            return false; // 通知本身只建立回调边界，不是引发该回调的Native操作。
        }
        !matches!(
            self,
            Self::Idle
                | Self::SourceAnchor
                | Self::ReadVisible
                | Self::RequestKey
                | Self::KeyRead
                | Self::FrameCallback
                | Self::FrameRectCallback
                | Self::BoundsRectCallback
                | Self::FrameSchedule
                | Self::InvalidateBody
        )
    }
}

#[derive(Clone, Copy)]
struct State {
    phase: Phase,
    parent: Phase,
    depth: u16,
    exhausted: bool,
    source_anchor: bool,
}

thread_local! {
    // TLS只存固定phase、深度及范围标量；没有Vec、Native/Doc owner或正文。
    static STATE: Cell<State> = const { Cell::new(State {
        phase: Phase::Idle, parent: Phase::Idle, depth: 0, exhausted: false, source_anchor: false,
    }) };
}
// 即使错误调用来自其他线程，也共用固定16条和每phase一次预算，不把TLS预算当全进程预算。
static SEEN: AtomicU64 = AtomicU64::new(0);
static LOGS: AtomicU8 = AtomicU8::new(0);
static RETURN_LOGS: AtomicU8 = AtomicU8::new(0);

fn enabled() -> bool {
    // SAFETY: 固定NUL结尾环境名只检查存在性，与既有trace门相同；不复制值或修改环境。
    !unsafe { libc::getenv(c"OMNI_NATIVE_PROBE_TRACE".as_ptr()) }.is_null()
}

/// 只公开当前同步范围的纯标量，供原失败出口保留未被全进程去重隐藏的证据。
pub(super) fn source_anchor_active() -> bool {
    enabled() && STATE.with(|state| state.get().source_anchor)
}

pub(super) struct Scope {
    previous: Option<State>,
    source_boundary: bool,
}

impl Drop for Scope {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            if self.source_boundary {
                return_scalar("scope_end", None); // 只描述原同步scope结束；预算耗尽不能伪造未打印的终点。
            }
            STATE.with(|state| state.set(previous)); // 同步重入退出精确还原外层，不把内层phase留给后继事件。
        }
    }
}

pub(super) fn enter(phase: Phase) -> Scope {
    if !enabled() {
        return Scope {
            previous: None,
            source_boundary: false,
        }; // 普通探针不初始化诊断TLS。
    }
    let previous = STATE.with(Cell::get);
    let depth = previous.depth.checked_add(1);
    STATE.with(|state| {
        state.set(State {
            phase,
            parent: previous.phase,
            depth: depth.unwrap_or(previous.depth),
            exhausted: previous.exhausted || depth.is_none(),
            source_anchor: previous.source_anchor,
        })
    });
    Scope {
        previous: Some(previous),
        source_boundary: false,
    }
}

/// 只延续原stamp.before纯值；不读取Native、文档或重新获取模式/选择权限。
pub(super) fn source_anchor_scope(source: bool) -> Scope {
    let mut scope = enter(Phase::SourceAnchor);
    if scope.previous.is_some() {
        STATE.with(|state| {
            let mut actual = state.get();
            actual.source_anchor = source;
            state.set(actual);
        });
        scope.source_boundary = source;
        if source {
            return_scalar("scope_begin", None);
        }
    }
    scope
}

#[derive(Clone, Copy, Debug)]
pub(super) enum GeometryKind {
    FrameSize,
    #[cfg(feature = "native-raster-surface-probe")]
    Notice,
    FrameRect,
    BoundsRect,
}

pub(super) struct GeometryCallback {
    _scope: Scope,
    outer: Option<State>,
    kind: GeometryKind,
    size: Option<(f64, f64)>,
    rect_origin: Option<(f64, f64)>,
    reported: Cell<bool>,
}

pub(super) fn geometry_callback(kind: GeometryKind, size: Option<(f64, f64)>) -> GeometryCallback {
    let phase = match kind {
        GeometryKind::FrameSize => Phase::FrameCallback,
        #[cfg(feature = "native-raster-surface-probe")]
        GeometryKind::Notice => Phase::GeometryCallback,
        GeometryKind::FrameRect => Phase::FrameRectCallback,
        GeometryKind::BoundsRect => Phase::BoundsRectCallback,
    };
    let scope = enter(phase);
    let outer = scope.previous;
    GeometryCallback {
        _scope: scope,
        outer,
        kind,
        size,
        rect_origin: None,
        reported: Cell::new(false),
    }
}

pub(super) fn geometry_rect_callback(
    kind: GeometryKind,
    rect: objc2_foundation::NSRect,
) -> GeometryCallback {
    let mut callback = geometry_callback(kind, Some((rect.size.width, rect.size.height)));
    callback.rect_origin = Some((rect.origin.x, rect.origin.y));
    callback
}

impl GeometryCallback {
    pub(super) fn record(&self, scheduled: bool) {
        if self.reported.replace(true) {
            return; // 同一实际回调只能输出一次，不因多次Weak观察复用日志预算。
        }
        let Some(outer) = self.outer else { return };
        let public_rect = matches!(
            self.kind,
            GeometryKind::FrameRect | GeometryKind::BoundsRect
        );
        if !outer.source_anchor || (!public_rect && !outer.phase.is_native_operation()) {
            return; // 未捕获的事件不证明无重入；startup/普通Idle不消耗Source锚诊断预算。
        }
        let bit = 1_u64 << (outer.phase as u8);
        if !public_rect && SEEN.fetch_or(bit, Ordering::Relaxed) & bit != 0 {
            return; // 同一固定phase只记录首次真实回调，不保留无限最近历史。
        }
        if LOGS
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |logs| {
                (logs < 16).then_some(logs + 1)
            })
            .is_err()
        {
            return;
        } // 全进程固定上限；耗尽不回绕、也不改变业务执行。
        let (width, height) = self.size.unwrap_or((0.0, 0.0));
        let (x, y) = self.rect_origin.unwrap_or((0.0, 0.0));
        eprintln!(
            "native_viewport_reentry scope=source_mode_anchor event={:?} phase={:?} parent={:?} depth={} depth_exhausted={} size_present={} new_width={width} new_height={height} rect_present={} new_x={x} new_y={y} scheduled={scheduled} formal=false",
            self.kind,
            outer.phase,
            outer.parent,
            outer.depth,
            outer.exhausted,
            self.size.is_some(),
            self.rect_origin.is_some(),
        );
    }
}

pub(super) fn returned_rect(rect: objc2_foundation::NSRect) {
    return_scalar(
        "rect",
        Some((
            rect.origin.x,
            rect.origin.y,
            rect.size.width,
            rect.size.height,
        )),
    );
}

pub(super) fn returned_point(point: objc2_foundation::NSPoint) {
    return_scalar("point", Some((point.x, point.y, 0.0, 0.0)));
}

fn return_scalar(kind: &'static str, value: Option<(f64, f64, f64, f64)>) {
    if !enabled() {
        return; // 默认诊断关闭时不初始化返回值TLS，也不为日志查询Native。
    }
    let state = STATE.with(Cell::get);
    if !state.source_anchor {
        return; // 没有原Source锚scope就不输出；不读取任何Native来推断当前模式。
    }
    let Ok(previous) = RETURN_LOGS.try_update(Ordering::Relaxed, Ordering::Relaxed, |logs| {
        (logs < 24).then_some(logs + 1)
    }) else {
        return;
    }; // 全进程24条时序记录，包含scope边界；没有轮询或无限历史。
    let sequence = previous + 1;
    let (x, y, width, height) = value.unwrap_or((0.0, 0.0, 0.0, 0.0));
    eprintln!(
        "native_viewport_return scope=source_mode_anchor sequence={sequence} phase={:?} parent={:?} depth={} depth_exhausted={} kind={kind} value_present={} x={x} y={y} width={width} height={height} formal=false",
        state.phase,
        state.parent,
        state.depth,
        state.exhausted,
        value.is_some(),
    );
}
