//! Owns viewport-sized line-number/fold controls without owning document text or editor lifetime.
//!
//! Geometry is prepared in the existing coalesced TextKit transaction. Painting reads a bounded
//! snapshot only; fold state contains canonical intervals and is invalidated by any text generation.

use super::*;
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSRulerOrientation, NSRulerView,
    NSTextLayoutFragmentEnumerationOptions,
};
use omni_markdown_core::markdown_folding::{markdown_fold_region, markdown_line_may_fold};

#[cfg(feature = "native-paged-body-probe")]
mod labels;
#[cfg(feature = "native-paged-body-probe")]
pub(super) mod paged;
#[cfg(feature = "native-raster-surface-probe")]
mod raster;

/// A finite viewport cap bounds ruler metadata even with unusually small or overlapping line heights.
const MAX_GUTTER_LINES: usize = 256;
/// Explicit nested folds are presentation metadata, bounded independently of document size.
const MAX_ACTIVE_FOLDS: usize =
    omni_markdown_core::markdown_folding::anchor_mapping::MAX_MARKDOWN_FOLD_ANCHORS;

/// One logical source line has one number, irrespective of how many wrapped visual rows it occupies.
#[derive(Clone, PartialEq)]
struct GutterLine {
    number: usize,
    document_y: f64,
    height: f64,
    may_fold: bool,
    folded: bool,
}

/// 缓存只借用本次已验证片段的身份与有限几何；弱引用不延长 TextKit 或正文寿命。
struct GutterFragmentProof {
    fragment: Weak<objc2_app_kit::NSTextLayoutFragment>,
    source: std::ops::Range<usize>,
    frame: NSRect,
}

/// 行号不能仅凭同一 Core 代次继续使用：滚动与 Find 可在代次不变时替换整批 Native 几何。
/// 每个 ruler 至多保留当前视口的 256 片元数据，没有正文副本、原生强引用或第二布局索引。
struct GutterGeometryProof {
    generation: u64,
    geometry_epoch: u64,
    semantic_epoch: Option<u64>,
    font_epoch: u64,
    fold_revision: u64,
    visible: NSRect,
    origin: NSPoint,
    viewport_source: std::ops::Range<usize>,
    manager: Option<Weak<NSTextLayoutManager>>,
    content: Option<Weak<objc2_app_kit::NSTextContentManager>>,
    fragments: Vec<GutterFragmentProof>,
}

#[cfg(feature = "native-raster-surface-probe")]
thread_local! {
    // 整个主线程进程最多十六次失败；日志不保留视图、正文、Native 对象或后继调度。
    static GUTTER_STALE_DIAGNOSTIC_COUNT: Cell<u8> = const { Cell::new(0) };
}

/// 只复用原校验已经取得的固定标量；None 表示原短路路径未读取，不能为日志补做 getter。
#[derive(Clone, Copy, Debug, Default)]
struct GutterProofDiagnostic {
    epochs: Option<(u64, u64, Option<u64>, u64, u64)>,
    viewport: Option<(NSRect, NSPoint)>,
    source: Option<(usize, usize)>,
    fragment_index: Option<usize>,
    expected_source: Option<(usize, usize)>,
    state: Option<usize>,
    frame: Option<NSRect>,
    expected_frame: Option<NSRect>,
    identity_matches: Option<bool>,
}

/// 失败后只打印原结果；固定 trace 开关关闭时不改变证明、计数或任何 Native 调用。
fn report_gutter_proof_failure(
    site: &'static str,
    reason: &'static str,
    operation: &'static str,
    proof: &GutterGeometryProof,
    facts: GutterProofDiagnostic,
) {
    #[cfg(feature = "native-raster-surface-probe")]
    native_markdown_trace! {
        let ordinal = GUTTER_STALE_DIAGNOSTIC_COUNT.with(|count| {
            if count.get() == 16 { return None; } // 预算耗尽只停日志，不制造后续成功或重试。
            let next = count.get() + 1;
            count.set(next);
            Some(next)
        });
        if let Some(ordinal) = ordinal {
            eprintln!("native_raster_surface event=gutter_proof_stale ordinal={ordinal} site={site} reason={reason} operation={operation} expected_epochs_g_geometry_semantic_font_fold={:?} expected_viewport={:?} expected_origin={:?} expected_source={}..{} observed={facts:?} formal=false",
                (proof.generation, proof.geometry_epoch, proof.semantic_epoch, proof.font_epoch, proof.fold_revision), proof.visible, proof.origin, proof.viewport_source.start, proof.viewport_source.end);
        }
    }
    #[cfg(not(feature = "native-raster-surface-probe"))]
    let _ = (site, reason, operation, proof, facts); // 普通构建没有计数器、输出或额外 Native 观测。
}

#[cfg(feature = "native-raster-surface-probe")]
/// 一次原标签循环的有限事实；仅几何/计数，不缓存正文、标签字符串或另一个行数组。
struct GutterDrawFacts {
    dirty: NSRect,
    viewport: NSRect,
    first_line: Option<(usize, f64, f64, f64)>,
    outside_viewport: usize,
    outside_damage: usize,
}

/// Only the scroll surface owns the ruler; the backlink never prolongs its editor or canonical model.
pub(super) struct GutterIvars {
    text_view: Weak<PieceBackedMarkdownTextView>,
    lines: RefCell<Vec<GutterLine>>,
    // lines 与本证明只在无 Native 调用的同一发布段更换；失败刷新先撤证明，不重新签旧行。
    geometry: RefCell<Option<Rc<GutterGeometryProof>>>,
    attributes: RefCell<Option<Rc<Retained<MarkdownTextAttributes>>>>,
    // 标量在 super initializer 前固定；只有实际正文 Raster 存在时才走 CPU layer 路径。
    #[cfg(feature = "native-raster-surface-probe")]
    raster_enabled: bool,
    #[cfg(feature = "native-raster-surface-probe")]
    raster: RefCell<Option<Rc<raster::GutterRaster>>>,
}

define_class!(
    // SAFETY: NSRulerView initialization and drawing stay on the main thread; all Rust ivars are
    // initialized before AppKit calls overrides, and the client backlink is weak.
    #[unsafe(super = NSRulerView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = GutterIvars]
    pub(super) struct NativeMarkdownGutter;

    // SAFETY: this initialized NSRulerView subclass inherits main-thread NSObject identity.
    unsafe impl NSObjectProtocol for NativeMarkdownGutter {}

    impl NativeMarkdownGutter {
        #[unsafe(method(isFlipped))]
        /// Use the editor's downward source-line coordinate convention for both paint and hit testing.
        fn is_flipped(&self) -> bool {
            true
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(wantsUpdateLayer))]
        /// 只有同一原正文 Raster 才改用真实 CALayer.contents；普通路径原样交还 NSRulerView。
        fn wants_update_layer(&self) -> bool {
            if self.ivars().raster_enabled {
                true
            } else {
                // SAFETY: 公开 NSView getter 的 ABI 与本覆写一致，接收者已完成全部 ivars 初始化。
                unsafe { msg_send![super(self), wantsUpdateLayer] }
            }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(updateLayer))]
        /// 原 AppKit 脏标记触发一次有限 CPU 绘制；取消或失败不伪造完成、不启动重试调度。
        fn update_layer(&self) {
            if !self.ivars().raster_enabled {
                // SAFETY: 未启用辅助画布时完整转发公开 NSView 生命周期方法。
                unsafe { let _: () = msg_send![super(self), updateLayer]; }
                return;
            }
            self.paint_raster_layer("updateLayer");
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(drawRect:))]
        /// layer 入口不再产生窗口 accelerated-CG 显示列表；原 drawRect 只在 CPU context 调用。
        fn draw_rect(&self, dirty: NSRect) {
            if self.ivars().raster_enabled {
                self.paint_raster_layer("drawRect");
            } else {
                // SAFETY: 普通原路径保持 NSRulerView 公开绘制 ABI 与原 dirty 范围。
                unsafe { let _: () = msg_send![super(self), drawRect: dirty]; }
            }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(setFrame:))]
        /// 真实几何事件先撤正文共用的原几何票，再调用原生 setter。
        fn set_frame_for_raster(&self, frame: NSRect) {
            self.invalidate_raster_geometry();
            // SAFETY: 原 NSView setter 接收同一 NSRect ABI，不改参数或布局策略。
            unsafe { let _: () = msg_send![super(self), setFrame: frame]; }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(setBounds:))]
        /// 滚动产生的真实 bounds 改变不能让旧 CPU 图继续签发布。
        fn set_bounds_for_raster(&self, bounds: NSRect) {
            self.invalidate_raster_geometry();
            // SAFETY: 与公开原 NSView setter ABI 一致，原几何参数完整转发。
            unsafe { let _: () = msg_send![super(self), setBounds: bounds]; }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(viewDidChangeBackingProperties))]
        /// 屏幕 scale 改变必须撤原票，不能把旧像素分辨率作为新 backing 发布。
        fn backing_changed_for_raster(&self) {
            self.invalidate_raster_geometry();
            // SAFETY: 公开 NSView 通知方法无参数，不增加观察者或隐藏视图。
            unsafe { let _: () = msg_send![super(self), viewDidChangeBackingProperties]; }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(viewDidMoveToWindow))]
        /// 同一 ruler 换窗也撤原票；不把后继窗口身份补签到进行中的绘制。
        fn window_changed_for_raster(&self) {
            self.invalidate_raster_geometry();
            // SAFETY: 保留公开原 NSView 生命周期调用顺序与 ABI。
            unsafe { let _: () = msg_send![super(self), viewDidMoveToWindow]; }
        }

        #[unsafe(method(drawHashMarksAndLabelsInRect:))]
        /// 只绘制同一活 Source 视口内的缓存行号；不在绘制中解析正文或触发布局。
        fn draw_hash_marks_and_labels(&self, dirty: NSRect) {
            #[cfg(feature = "native-raster-surface-probe")]
            if self.ivars().raster_enabled && !self.raster_draw_is_active() {
                return; // 只有本次真实 CPU super drawRect 可消费标签，不能旁路回到窗口 GPU context。
            }
            let Some(Some(view)) = self.paint_native_step(|| self.ivars().text_view.load()) else { return; };
            let _payload_use = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            if view.ivars().textkit_graph_teardown.get() { return; }
            let Ok(capture) = view.ivars().folds.capture_selection() else { return; };
            let Some(proof) = self.ivars().geometry.borrow().clone() else { return; };
            if !self.cached_geometry_is_current(&view, &capture, &proof, "draw_begin") { return; }
            let Some(attributes) = self.ivars().attributes.borrow().clone() else { return; };
            let Some((viewport, generation)) = self.source_viewport(&view) else { return; };
            let Some(damage) = native_markdown_paint_rect(viewport, dirty, viewport) else { return; };
            let Some(active_line) = self.paint_native_step(|| view.selected_source_line()) else { return; };
            // 原有有限缓存完整保留；结束借用后才调用可重入的 Native 坐标和标签绘制。
            let lines = self.ivars().lines.borrow().clone();
            #[cfg(feature = "native-markdown-runtime-probe")]
            crate::native_surface::probe_call_diagnostic::memory_point("gutter_draw_begin");
            #[cfg(feature = "native-raster-surface-probe")]
            let mut facts = GutterDrawFacts {
                dirty, viewport,
                first_line: None, outside_viewport: 0, outside_damage: 0,
            };
            let Some(_graphics) = self.paint_native_step(GutterGraphicsState::save) else { return; };
            if self.paint_native_step(|| NSBezierPath::clipRect(damage)).is_none() { return; }
            for line in lines {
                // 仍按原始正文基线转换，不把长逻辑行号挪到视口边缘。
                let Some(point) = self.paint_native_step(|| self.convertPoint_fromView(
                    NSPoint::new(0.0, line.document_y), Some(&view))) else { return; };
                if self.source_viewport(&view) != Some((viewport, generation)) { return; }
                #[cfg(feature = "native-raster-surface-probe")]
                if facts.first_line.is_none() {
                    facts.first_line = Some((line.number, line.document_y, point.y, line.height));
                }
                let Some(rect) = visible_gutter_line(point.y, line.height, viewport) else {
                    #[cfg(feature = "native-raster-surface-probe")]
                    { facts.outside_viewport += 1; }
                    continue; // 完全屏外逻辑行仍不挪到视口顶端，必须保留原折叠命中坐标。
                };
                let Some(paint) = native_markdown_paint_rect(rect, dirty, viewport) else {
                    #[cfg(feature = "native-raster-surface-probe")]
                    { facts.outside_damage += 1; }
                    continue; // 普通原dirty绘制仍保留既有局部更新语义。
                };
                if active_line == Some(line.number) {
                    // 与原路径相同的真实活动行交集；RAII 在任意撤权出口配平 graphics state。
                    let Some(_highlight) = self.paint_native_step(GutterGraphicsState::save) else { return; };
                    let Some(color) = self.paint_native_step(|| NSColor::colorWithSRGBRed_green_blue_alpha(0.35, 0.48, 0.72, 0.12)) else { return; };
                    if self.paint_native_step(|| color.setFill()).is_none()
                        || self.paint_native_step(|| NSBezierPath::fillRect(paint)).is_none() { return; }
                }
                let marker = if line.folded {
                    "▶" // 已折叠行继续显示原展开标记，点击仍走原 Core 折叠授权。
                } else if line.may_fold {
                    "▾" // 预提示不授予隐藏正文的权限。
                } else {
                    " " // 普通行只保留原行号。
                };
                let Some(string) = self.paint_native_step(|| NSString::from_str(&format!("{} {marker}", line.number + 1))) else { return; };
                // SAFETY: 原字典仍只含活字体/颜色与不可变键，临时有限标签同步归还。
                let Some(label) = self.paint_native_step(|| unsafe {
                    NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &string, Some(&attributes))
                }) else { return; };
                if self.source_viewport(&view) != Some((viewport, generation)) { return; }
                if self.paint_native_step(|| label.drawAtPoint(NSPoint::new(4.0, point.y))).is_none() { return; }
                #[cfg(feature = "native-raster-surface-probe")]
                if let Some(raster) = self.ivars().raster.borrow().as_ref() {
                    raster.record_drawn_label(); // 原实际标签调用已返回且原票有效，才记一次绘制事实。
                }
            }
            // 完整原循环后只复核一次片段批次，避免逐标签/逐 Native 原语重复遍历造成平方开销。
            if !self.cached_geometry_is_current(&view, &capture, &proof, "draw_end") { return; }
            drop(_graphics); // 先恢复 graphics state，再记原 draw-end 边界。
            #[cfg(feature = "native-raster-surface-probe")]
            self.complete_raster_draw(facts);
            #[cfg(feature = "native-markdown-runtime-probe")]
            crate::native_surface::probe_call_diagnostic::memory_point("gutter_draw_end");
        }

        #[unsafe(method(mouseDown:))]
        /// 只命中同一 Source 可见区域内的缓存逻辑行，遮挡区没有折叠热区。
        fn mouse_down(&self, event: &NSEvent) {
            let Some(view) = self.ivars().text_view.load() else {
                // AppKit 交付已排队事件时，原弱客户端可能已经释放。
                return;
            };
            // 鼠标命中测试保留几何后会调用 AppKit；其完整生命周期都阻止 accepted release。
            let _payload_use = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            if view.ivars().textkit_graph_teardown.get() {
                // 迟到点击不能解析旧折叠区域或提交任何草稿。
                return;
            }
            let Ok(capture) = view.ivars().folds.capture_selection() else { return; };
            let Some(proof) = self.ivars().geometry.borrow().clone() else { return; };
            if !self.cached_geometry_is_current(&view, &capture, &proof, "mouse_begin") { return; }
            let Some((viewport, generation)) = self.source_viewport(&view) else {
                return; // Preview、关闭或未挂载的 ruler 都不能消费旧折叠事件。
            };
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            if !gutter_contains_point(viewport, point) {
                return; // 与绘制使用同一正文 clip，Find/header 区域不会留下不可见热区。
            }
            let lines = self.ivars().lines.borrow().clone();
            let selected = lines.into_iter().find(|line| {
                // 保留长逻辑行的原命中高度，只裁掉正文实际不可见的部分。
                let top = self
                    .convertPoint_fromView(NSPoint::new(0.0, line.document_y), Some(&view))
                    .y;
                visible_gutter_line(top, line.height, viewport)
                    .is_some_and(|rect| gutter_contains_point(rect, point))
            });
            if self.source_viewport(&view) != Some((viewport, generation))
                || !self.cached_geometry_is_current(&view, &capture, &proof, "mouse_end") {
                // 原生转换若重入新布局/正文代次/关闭，旧点不能命中后继几何。
                return;
            }
            if let Some(line) = selected {
                // core 再核当前代次、实际语法并先提交可见草稿，点击不会绕过原折叠事务。
                if let Err(error) = view.toggle_source_fold(line.number) {
                    // 容量、提交或语法拒绝都保留原正文和已有折叠图。
                    present_native_markdown_error(&error);
                }
            }
        }
    }
);

/// 配平原同步绘图栈；即使权限撤销也必须恢复，不能把新 clip 留给下一个控件。
struct GutterGraphicsState;
impl GutterGraphicsState {
    fn save() -> Self {
        NSGraphicsContext::saveGraphicsState_class();
        Self
    }
}
impl Drop for GutterGraphicsState {
    fn drop(&mut self) {
        NSGraphicsContext::restoreGraphicsState_class();
    }
}

impl NativeMarkdownGutter {
    #[cfg(feature = "native-raster-surface-probe")]
    /// 两个公开 Native 入口共用普通 Rust 边界，避免直接调用带隐藏 selector 的覆写实现。
    fn paint_raster_layer(&self, entry: &'static str) {
        let raster = self.ivars().raster.borrow().clone();
        if let Some(raster) = raster
            && let Err(error) = raster.paint(self, entry)
        {
            raster.report_failure(&error);
        } // 构造尚未安装租约或 accepted 已取走时，不碰任何旧 Native 图。
    }

    /// CPU 回调沿同一原票逐原语核验；普通窗口路径保留完全相同的 Native 调用。
    fn paint_native_step<T>(&self, work: impl FnOnce() -> T) -> Option<T> {
        #[cfg(feature = "native-raster-surface-probe")]
        if self.ivars().raster_enabled {
            let raster = self.ivars().raster.borrow().clone();
            if let Some(raster) = raster
                && raster.active()
            {
                return raster.drawing_step(self, work);
            }
        }
        Some(work()) // 原鼠标命中与初始几何读取没有 CPU paint 权限，不新增绘制事务。
    }

    #[cfg(feature = "native-raster-surface-probe")]
    fn complete_raster_draw(&self, facts: GutterDrawFacts) {
        let raster = self.ivars().raster.borrow().clone();
        if let Some(raster) = raster {
            raster.mark_draw_complete(self, facts);
        }
    }

    #[cfg(feature = "native-raster-surface-probe")]
    fn raster_draw_is_active(&self) -> bool {
        self.ivars()
            .raster
            .borrow()
            .as_ref()
            .is_some_and(|raster| raster.active())
    }

    #[cfg(feature = "native-raster-surface-probe")]
    fn invalidate_raster_geometry(&self) {
        let raster = self.ivars().raster.borrow().clone();
        if let Some(raster) = raster {
            raster.invalidate_geometry();
        }
    }

    #[cfg(feature = "native-raster-surface-probe")]
    fn draw_native_for_raster(&self, bounds: NSRect) {
        // SAFETY: 直接调用原 NSRulerView 公开绘制；它仍分派到原 drawHashMarks，context 已限定为 CPU。
        unsafe {
            let _: () = msg_send![super(self), drawRect: bounds];
        }
    }

    /// 纯 Rust 身份检查可放在逐 Native 原语前后，不重新遍历片段或升级正文 owner。
    fn proof_is_current(&self, proof: &Rc<GutterGeometryProof>) -> bool {
        self.ivars()
            .geometry
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, proof))
    }

    /// 只在绘制/点击准入与完整绘制结束检查一次原批次；过程中仍由原 Raster 票撤销几何权限。
    /// 所有 Weak/Native 查询都夹在原选择票与原 proof 身份检查之间，且没有 RefCell 借用跨调用。
    fn cached_geometry_is_current(
        &self,
        view: &PieceBackedMarkdownTextView,
        capture: &NativeMarkdownSelectionCapture,
        proof: &Rc<GutterGeometryProof>,
        site: &'static str,
    ) -> bool {
        let failure = Cell::new(None);
        let operation = Cell::new("begin");
        let facts = Cell::new(GutterProofDiagnostic::default());
        // 只记录原短路求值中的首个 false；后续清理/复核不能覆盖真正的失败原因。
        let predicate = |name: &'static str, value: bool| {
            if !value && failure.get().is_none() {
                failure.set(Some(name));
            }
            value
        };
        // 此宏仅给原调用贴固定名称，保留同一 guard、同一 getter 及原 .ok()? 短路。
        macro_rules! read {
            ($guard:expr, $name:literal, $work:expr) => {{
                operation.set($name);
                native_markdown_table_intent_step($guard, $work).ok()?
            }};
        }
        let result = (|| -> Option<()> {
            let original = || {
                predicate("selection_capture", capture.current())
                    && predicate("proof_identity", self.proof_is_current(proof))
                    && predicate("graph_open", !view.ivars().textkit_graph_teardown.get())
            };
            let storage = read!(&original, "view.storage_weak", || view
                .ivars()
                .text_storage
                .load())?;
            let current = || {
                original()
                    && predicate(
                        "source_mode",
                        storage.presentation_mode() == MarkdownPresentationMode::Source,
                    )
                    && predicate(
                        "geometry_epoch",
                        storage.ivars().table_geometry_epoch.get() == proof.geometry_epoch,
                    )
                    && predicate(
                        "semantic_epoch",
                        storage.ivars().presentation_semantic_epoch.get() == proof.semantic_epoch,
                    )
                    && predicate(
                        "font_epoch",
                        storage.ivars().font_run_scratch.epoch() == proof.font_epoch,
                    )
                    && predicate(
                        "fold_revision",
                        view.ivars().folds.revision() == proof.fold_revision,
                    )
            };
            operation.set("canonical.document");
            let document = storage.document()?;
            let generation = document.borrow().generation();
            drop(document); // 日志复用原借用已经取得的 generation，不升级额外正文 owner。
            let mut observed = facts.get();
            observed.epochs = Some((
                generation,
                storage.ivars().table_geometry_epoch.get(),
                storage.ivars().presentation_semantic_epoch.get(),
                storage.ivars().font_run_scratch.epoch(),
                view.ivars().folds.revision(),
            ));
            facts.set(observed); // 其余 epoch 都是现有校验读取的纯 Cell，不补做 Native getter。
            if !current() || !predicate("generation", generation == proof.generation) {
                return None;
            }
            let actual_viewport = read!(&current, "view.bounded_paint_viewport", || view
                .bounded_paint_viewport());
            let mut observed = facts.get();
            observed.viewport = actual_viewport;
            facts.set(observed);
            if !predicate(
                "document_viewport",
                actual_viewport == Some((proof.visible, proof.origin)),
            ) {
                return None; // 比较正文文档 clip，ruler 的固定零 origin 不构成来源证明。
            }
            let Some(manager_weak) = proof.manager.as_ref() else {
                operation.set("empty_document.shape");
                let document = storage.document()?;
                return (predicate("empty_viewport_source", proof.viewport_source.is_empty())
                    && predicate("empty_document_length", document.borrow().utf16_len() == 0)
                    && predicate("empty_fragment_inventory", proof.fragments.is_empty())
                    && current())
                .then_some(());
            };
            let manager = read!(&current, "proof.manager_weak", || manager_weak.load())?;
            let content = read!(&current, "proof.content_weak", || proof
                .content
                .as_ref()?
                .load())?;
            let actual_manager = read!(&current, "view.textLayoutManager", || view
                .textLayoutManager())?;
            let actual_content = read!(&current, "manager.textContentManager", || manager
                .textContentManager())?;
            if !predicate(
                "manager_identity",
                std::ptr::eq(&*actual_manager, &*manager),
            ) || !predicate(
                "content_identity",
                std::ptr::eq(&*actual_content, &*content),
            ) {
                return None;
            }
            let viewport = read!(&current, "manager.viewportController", || manager
                .textViewportLayoutController());
            let viewport_manager = read!(&current, "viewport.textLayoutManager", || viewport
                .textLayoutManager())?;
            if !predicate(
                "viewport_manager_identity",
                std::ptr::eq(&*viewport_manager, &*manager),
            ) {
                return None;
            }
            let range = read!(&current, "viewport.viewportRange", || viewport
                .viewportRange())?;
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
            let document_range = read!(&current, "provider.documentRange", || provider
                .documentRange());
            let start = read!(&current, "documentRange.location", || document_range
                .location());
            let source_range = |range: &NSTextRange| -> Option<std::ops::Range<usize>> {
                let location = read!(&current, "range.location", || range.location());
                let end = read!(&current, "range.endLocation", || range.endLocation());
                let from = usize::try_from(read!(&current, "content.startOffset", || content
                    .offsetFromLocation_toLocation(&start, &location)))
                .ok()?;
                let to = usize::try_from(read!(&current, "content.endOffset", || content
                    .offsetFromLocation_toLocation(&start, &end)))
                .ok()?;
                predicate("source_range_order", from <= to).then_some(from..to)
            };
            let actual_source = source_range(&range)?;
            let mut observed = facts.get();
            observed.source = Some((actual_source.start, actual_source.end));
            facts.set(observed);
            if !predicate(
                "viewport_source_range",
                actual_source == proof.viewport_source,
            ) {
                return None;
            }
            for (index, expected) in proof.fragments.iter().enumerate() {
                let mut observed = facts.get();
                observed.fragment_index = Some(index);
                observed.expected_source = Some((expected.source.start, expected.source.end));
                observed.expected_frame = Some(expected.frame);
                observed.source = None;
                observed.state = None;
                observed.frame = None;
                observed.identity_matches = None;
                facts.set(observed); // None 明确保留原短路后尚未读取的字段，不用诊断补读 frame。
                let fragment = read!(&current, "fragment.weak", || expected.fragment.load())?;
                let state = read!(&current, "fragment.state", || fragment.state());
                let mut observed = facts.get();
                observed.state = Some(state.0);
                facts.set(observed);
                if !predicate(
                    "fragment_state",
                    state == objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable,
                ) {
                    return None;
                }
                let frame = read!(&current, "fragment.layoutFragmentFrame", || fragment
                    .layoutFragmentFrame());
                let mut observed = facts.get();
                observed.frame = Some(frame);
                facts.set(observed);
                if !predicate("fragment_frame", frame == expected.frame) {
                    return None;
                }
                let fragment_manager = read!(&current, "fragment.textLayoutManager", || fragment
                    .textLayoutManager())?;
                if !predicate(
                    "fragment_manager_identity",
                    std::ptr::eq(&*fragment_manager, &*manager),
                ) {
                    return None;
                }
                let fragment_range = read!(&current, "fragment.rangeInElement", || fragment
                    .rangeInElement());
                let actual_source = source_range(&fragment_range)?;
                let mut observed = facts.get();
                observed.source = Some((actual_source.start, actual_source.end));
                facts.set(observed);
                if !predicate("fragment_source_range", actual_source == expected.source) {
                    return None;
                }
                let location = read!(&current, "fragmentRange.location", || fragment_range
                    .location());
                let actual = read!(&current, "manager.fragmentForLocation", || manager
                    .textLayoutFragmentForLocation(&location))?;
                let identity_matches = std::ptr::eq(&*actual, &*fragment);
                let mut observed = facts.get();
                observed.identity_matches = Some(identity_matches);
                facts.set(observed);
                if !predicate("fragment_lookup_identity", identity_matches) {
                    return None;
                }
            }
            current().then_some(()) // 原业务判断与 Native 序列不变；日志绝不推进布局或签发通过。
        })();
        // 原临时 Native claim 全部归还之后仍沿原票复核；只复用实际失败，不另做观测调用。
        let valid = result.is_some()
            && predicate("selection_after_claim_drop", capture.current())
            && predicate("proof_after_claim_drop", self.proof_is_current(proof));
        if !valid {
            report_gutter_proof_failure(
                site,
                failure.get().unwrap_or("native_result_unavailable"),
                operation.get(),
                proof,
                facts.get(),
            );
        }
        valid
    }

    /// 绘制和折叠点击共用实时可见区域，不缓存 Find 高度或手工补偿 header 偏移。
    fn source_viewport(&self, view: &PieceBackedMarkdownTextView) -> Option<(NSRect, u64)> {
        if view.ivars().textkit_graph_teardown.get() {
            return None; // accepted 后的空壳不升级 canonical 弱引用。
        }
        let proof = self.ivars().geometry.borrow().clone()?;
        let generation = {
            let storage = self.paint_native_step(|| view.ivars().text_storage.load())??;
            if storage.presentation_mode() != MarkdownPresentationMode::Source
                || storage.ivars().table_geometry_epoch.get() != proof.geometry_epoch
                || storage.ivars().presentation_semantic_epoch.get() != proof.semantic_epoch
                || storage.ivars().font_run_scratch.epoch() != proof.font_epoch
                || view.ivars().folds.revision() != proof.fold_revision
            {
                return None; // 隐藏 ruler 的迟到绘制和事件不能借 Preview 恢复 Source 权限。
            }
            let document = storage.document()?;
            document.borrow().generation()
        }; // canonical 借用及临时强 owner 均在坐标转换前结束。
        let (text_visible, origin) = self.paint_native_step(|| view.bounded_paint_viewport())??;
        if generation != proof.generation
            || text_visible != proof.visible
            || origin != proof.origin
            || !self.proof_is_current(&proof)
        {
            return None; // 同代次的新滚动/Find 视口不能给旧缓存重新签发绘制或折叠命中权限。
        }
        // ruler 独占横向条带；仅转换正文 clip 的上下沿，原生层级自行包含 Find/resize 偏移。
        let top = self
            .paint_native_step(|| {
                self.convertPoint_fromView(NSPoint::new(0.0, text_visible.origin.y), Some(view))
            })?
            .y;
        let bottom = self
            .paint_native_step(|| {
                self.convertPoint_fromView(
                    NSPoint::new(0.0, text_visible.origin.y + text_visible.size.height),
                    Some(view),
                )
            })?
            .y;
        let bounds = self.paint_native_step(|| self.bounds())?;
        let vertical_clip = NSRect::new(
            NSPoint::new(bounds.origin.x, top.min(bottom)),
            NSSize::new(bounds.size.width, (bottom - top).abs()),
        );
        let visible = self.paint_native_step(|| self.visibleRect())?;
        let viewport = native_markdown_paint_rect(visible, bounds, vertical_clip)?;
        // 所有可重入 getter 返回后再核活代次和 Source；非法/空交集不会被全文 bounds 替代。
        self.paint_native_step(|| view.gutter_generation_is_current(generation))?
            .then_some((viewport, generation))
    }

    /// 一个滚动面只挂接一个原 ruler；仅保留有限字体/标签和弱 editor，辅助层构造失败明确上报。
    pub(super) fn new(
        scroll: &NSScrollView,
        view: &PieceBackedMarkdownTextView,
        ledger: &NativeMarkdownOwnedLedger,
        mtm: MainThreadMarker,
    ) -> Result<Retained<Self>, String> {
        #[cfg(feature = "native-raster-surface-probe")]
        let raster_owner = view.ivars().raster_surface.borrow().clone();
        #[cfg(not(feature = "native-raster-surface-probe"))]
        let _ = ledger; // 普通构建不创建辅助层或第二 ledger。
        let font = NSFont::monospacedDigitSystemFontOfSize_weight(11.0, unsafe {
            // SAFETY: AppKit's regular font weight is an immutable exported scalar.
            NSFontWeightRegular
        });
        let color = NSColor::secondaryLabelColor();
        // SAFETY: AppKit exports immutable process-lifetime attribute key objects.
        let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
        let values: [&AnyObject; 2] = [&font, &color];
        let attributes = MarkdownTextAttributes::from_slices(&keys, &values);
        let allocated = Self::alloc(mtm).set_ivars(GutterIvars {
            text_view: Weak::from(view),
            lines: RefCell::new(Vec::new()),
            geometry: RefCell::new(None),
            // 绘制只克隆 Rust Rc，原生 retain/release 不与 RefCell 借用重叠。
            attributes: RefCell::new(Some(Rc::new(attributes))),
            #[cfg(feature = "native-raster-surface-probe")]
            raster_enabled: raster_owner.is_some(),
            #[cfg(feature = "native-raster-surface-probe")]
            raster: RefCell::new(None),
        });
        // SAFETY: the NSRulerView designated initializer receives the live main-thread scroll owner
        // after every Rust ivar is initialized; the declared selector matches its Objective-C ABI.
        let ruler: Retained<Self> = unsafe {
            msg_send![super(allocated), initWithScrollView: Some(scroll), orientation: NSRulerOrientation::VerticalRuler]
        };
        // macOS 14+ 普通 NSView 默认允许越界绘制；先限制 ruler 自身边界，正文 clip 仍单独约束绘制与点击。
        ruler.setClipsToBounds(true);
        ruler.setRuleThickness(74.0);
        ruler.setClientView(Some(view));
        ruler.setAccessibilityLabel(Some(ns_string!("源码行号与折叠控件")));
        #[cfg(feature = "native-raster-surface-probe")]
        if let Some(owner) = raster_owner {
            let raster = raster::GutterRaster::install(&ruler, view, &owner, ledger)?;
            *ruler.ivars().raster.borrow_mut() = Some(raster);
        }
        Ok(ruler)
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// Check bounded visible metadata without exposing or copying its source-line records.
    pub(super) fn verify_visible_labels_for_probe(&self) -> Result<(), String> {
        let count = self.ivars().lines.borrow().len();
        if count == 0 || count > MAX_GUTTER_LINES {
            // Missing labels or an exceeded viewport cap invalidates the actual native gutter contract.
            return Err(format!("源码行号的可见标签数量无效：{count}"));
        }
        Ok(())
    }

    /// Read current cached paragraph geometry for one logical source line, without native layout work.
    fn line_frame(
        &self,
        view: &PieceBackedMarkdownTextView,
        line: usize,
        width: f64,
    ) -> Option<NSRect> {
        let capture = view.ivars().folds.capture_selection().ok()?;
        let proof = self.ivars().geometry.borrow().clone()?;
        if !self.cached_geometry_is_current(view, &capture, &proof, "active_line_background") {
            return None; // 正文活动行背景也不能消费另一视口的旧行号几何。
        }
        self.ivars()
            .lines
            .borrow()
            .iter()
            .find(|entry| entry.number == line)
            .map(|entry| {
                // The returned rectangle is copied metadata, so all cache borrows end before painting.
                NSRect::new(
                    NSPoint::new(0.0, entry.document_y),
                    NSSize::new(width, entry.height.max(20.0)),
                )
            })
    }

    /// 逐字段证明 ruler 空壳不保留几何容量或属性所有权。
    pub(super) fn payload_is_empty(&self) -> bool {
        let metadata_empty = self.ivars().lines.borrow().capacity() == 0
            && self.ivars().geometry.borrow().is_none()
            && self.ivars().attributes.borrow().is_none();
        // clientView 原生 getter 前结束全部 RefCell 临时借用，关闭观察也允许重入。
        #[cfg(feature = "native-raster-surface-probe")]
        let metadata_empty = metadata_empty && self.ivars().raster.borrow().is_none();
        metadata_empty && self.clientView().is_none()
    }

    /// Drop bounded geometry immediately when detaching; the scroll view separately releases its ruler.
    pub(super) fn clear(&self) {
        #[cfg(feature = "native-raster-surface-probe")]
        {
            let raster = self.ivars().raster.borrow_mut().take();
            if let Some(raster) = raster {
                // 先取出唯一槽再调用可重入 Native；恢复原关系后归还所有辅助元数据。
                if let Err(error) = raster.restore() {
                    raster.report_failure(&error);
                }
                drop(raster);
            }
        }
        let lines = self.ivars().lines.replace(Vec::new());
        let geometry = self.ivars().geometry.borrow_mut().take();
        let attributes = self.ivars().attributes.borrow_mut().take();
        // 全部借用结束后释放容量及原生值，随后才调用可重入的 client detach。
        drop((lines, geometry, attributes));
        self.setClientView(None);
    }
}

/// 原逻辑行只裁掉屏外部分；标签仍用未裁剪基线绘制，不把行号挪到视口顶端。
fn visible_gutter_line(top: f64, height: f64, viewport: NSRect) -> Option<NSRect> {
    if !top.is_finite() || !height.is_finite() {
        return None; // 无法认证的 native 行几何不能产生绘制或折叠命中权限。
    }
    let line = NSRect::new(
        NSPoint::new(viewport.origin.x, top),
        NSSize::new(viewport.size.width, height.max(16.0)),
    );
    native_markdown_paint_rect(line, viewport, viewport)
}

/// 使用左闭右开的同一可见区域，防止边缘事件落入相邻 Find/header 控件。
fn gutter_contains_point(rect: NSRect, point: NSPoint) -> bool {
    point.x.is_finite()
        && point.y.is_finite()
        && point.x >= rect.origin.x
        && point.x < rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y < rect.origin.y + rect.size.height
}

impl PieceBackedMarkdownTextView {
    /// TextKit/坐标调用返回后重新验证派生数据仍属于同一活 canonical 代次。
    pub(super) fn projection_generation_is_current(&self, expected: u64) -> bool {
        let sealed = self.ivars().textkit_graph_teardown.get();
        let current = if sealed {
            // 已封闭时连 canonical weak 都不升级，不延长业务生命周期。
            None
        } else {
            self.ivars()
                .text_storage
                .load()
                .and_then(|storage| storage.document())
                .map(|document| document.borrow().generation())
        };
        projection_can_publish(sealed, expected, current)
    }

    /// ruler 仅属于 Source；Native 回调中切到 preview 也使旧几何失去发布权限。
    fn gutter_generation_is_current(&self, expected: u64) -> bool {
        self.projection_generation_is_current(expected)
            && self.ivars().text_storage.load().is_some_and(|storage| {
                storage.presentation_mode() == MarkdownPresentationMode::Source
            })
    }

    /// Resolve the settled native caret's logical line using canonical tree aggregates.
    fn selected_source_line(&self) -> Option<usize> {
        let storage = self.ivars().text_storage.load()?;
        if storage.presentation_mode() != MarkdownPresentationMode::Source {
            // Inline preview owns no active source-line/gutter highlight.
            return None;
        }
        let selection = self.selectedRange();
        let document = storage.document()?;
        let document = document.borrow();
        document
            .byte_offset_for_utf16(selection.location)
            .and_then(|byte| document.line_for_byte(byte))
            .ok()
    }

    /// Paint a cached active source line before normal text drawing, with no element enumeration.
    pub(super) fn draw_source_active_line(&self, dirty: NSRect) {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // 独立绘制入口同样不能在 accepted 后取得正文或属性。
            return;
        }
        let Some(line) = self.selected_source_line() else {
            // Preview/disconnected input has no source-line background to draw.
            return;
        };
        let Some(ruler) = self.ivars().gutter.get().and_then(Weak::load) else {
            // Native storage-only adapters have no prepared source-line geometry.
            return;
        };
        let Some((visible, _)) = self.bounded_paint_viewport() else {
            return; // 首次隐藏/卸载不绘制；原生挂载/draw事件会沿既有调度恢复。
        };
        let Some(frame) = ruler.line_frame(self, line, self.bounds().size.width) else {
            // Offscreen/unlaid caret geometry is deferred; drawing never forces whole-document layout.
            return;
        };
        if let Some(paint) = native_markdown_paint_rect(frame, dirty, visible) {
            // The subtle native source highlight uses the same one-line range as the gutter.
            NSGraphicsContext::saveGraphicsState_class();
            NSColor::colorWithSRGBRed_green_blue_alpha(0.35, 0.48, 0.72, 0.08).setFill();
            NSBezierPath::fillRect(paint);
            NSGraphicsContext::restoreGraphicsState_class();
        }
    }

    /// Mark only existing ruler backing as damaged after a settled selection change.
    pub(super) fn invalidate_source_gutter_highlight(&self) {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // 迟到选择通知不重新请求空壳绘制。
            return;
        }
        if let Some(ruler) = self.ivars().gutter.get().and_then(Weak::load) {
            // Selection changes reuse cached label geometry and create no timer or extra layout request.
            ruler.setNeedsDisplay(true);
        }
    }

    /// 显式全部展开、模式切换及teardown保留清空策略；正常编辑改由真实ChangeSet映射。
    pub(super) fn unfold_source(&self) {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // 已 take/drop 的折叠状态不能被旧输入入口重新写入。
            return;
        }
        if self.ivars().folds.borrow_mut().is_empty() {
            // The normal unfolded editor needs no layout invalidation or native transaction.
            return;
        }
        self.ivars().folds.borrow_mut().clear();
        self.ivars().folds.advance(); // 显式展开撤权；无效化不再重复推进提交票据。
        self.invalidate_fold_layout();
    }

    /// Invalidate only derived layout; canonical bytes, selection history and saved baseline are untouched.
    fn invalidate_fold_layout(&self) {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() || !self.ivars().folds.owner_is_current() {
            // 原生 layout 调用必须保持同一活 owner，不能请求已断开空壳恢复派生图。
            return;
        }
        let Ok(capture) = self.ivars().folds.capture_selection() else {
            return;
        };
        // 原owner/capture确认后，本次无效化已承担当前dirty；finish入口预先take时再取为空无副作用。
        // 仅在首次Native释放/调用之前确认一次，之后重入新置的dirty须保留，不能在返回时清掉。
        self.ivars().folds.take_layout_dirty();
        self.advance_block_fold_epoch(); // 同text generation的fold变化也必须撤销旧派生语义。
        let _ = (|| -> Result<(), NativeMarkdownFoldError> {
            // 释放原生cache与任一getter均可重入；只沿开始时的同一fold票据推进。
            selection_native_step(&capture, || self.invalidate_visible_table_layout_cache())?;
            let Some(manager) = selection_native_step(&capture, || self.textLayoutManager())?
            else {
                return Ok(());
            };
            let Some(content) = selection_native_step(&capture, || manager.textContentManager())?
            else {
                return Ok(());
            };
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
            let range = selection_native_step(&capture, || provider.documentRange())?;
            selection_native_step(&capture, || manager.invalidateLayoutForRange(&range))?;
            selection_native_step(&capture, || self.setNeedsDisplay(true))?;
            selection_native_step(&capture, || self.schedule_visible_table_layout_update())?;
            Ok(())
        })(); // 失去权限时已有新状态负责绘制，不重新领票或排重试。
    }

    /// 已提交变化先换纯fold坐标，派生Native无效化在结束document借用后只消费一次。
    pub(super) fn finish_fold_layout_update(&self) {
        if self.ivars().folds.take_layout_dirty() {
            self.invalidate_fold_layout();
        }
    }

    /// Resolve a single explicit user fold while preserving the canonical source and full text coordinates.
    pub(super) fn toggle_source_fold(&self, line: usize) -> Result<(), String> {
        if !self.ivars().folds.owner_is_current() {
            return Err("笔记折叠权限已变更".into());
        }
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // probe 或其他直接调用也必须遵守同一 sealed 边界。
            return Err("笔记折叠控件已关闭".into());
        }
        self.commit_active_table_cell()?;
        self.commit_composition_before_release()?;
        if self
            .ivars()
            .text_storage
            .load()
            .is_none_or(|storage| storage.presentation_mode() != MarkdownPresentationMode::Source)
        {
            // Folding belongs to literal source mode; preview retains its normal inline/table presentation.
            return Err("请切换到源码模式后折叠文档块".into());
        }
        let document = self
            .ivars()
            .text_storage
            .load()
            .and_then(|storage| storage.document())
            .ok_or("编辑器已关闭")?;
        let generation = document.borrow().generation();
        if self.ivars().folds.generation.get() != generation {
            // An intervening canonical edit invalidates every saved source coordinate.
            self.ivars().folds.synchronize_generation(generation);
        }
        let existing = self
            .ivars()
            .folds
            .borrow()
            .iter()
            .position(|fold| fold.header_line == line);
        if let Some(index) = existing {
            // Expanding removes only this fold; nested independent fold choices remain view-local.
            self.ivars().folds.borrow_mut().remove(index);
            self.ivars().folds.advance();
        } else {
            // Query before changing presentation: a rejected/nonfoldable source line stays fully visible.
            let region = markdown_fold_region(&document.borrow(), line)
                .map_err(|error| error.to_string())?;
            let Some(region) = region else {
                // A cheap gutter hint never authorizes hiding a context-dependent continuation line.
                return Ok(());
            };
            if self.ivars().folds.borrow().len() >= MAX_ACTIVE_FOLDS {
                // The explicit metadata cap cannot silently replace an earlier user fold choice.
                return Err("请先展开折叠块，再添加新的折叠".into());
            }
            let selected = self
                .primary_text_selection()
                .map_err(|error| error.to_string())?;
            if !selected.belongs_to(&document) || !selected.current_in(&document.borrow()) {
                return Err("折叠前文本选择或笔记已变更".into());
            }
            let move_to_header = (selected.selection.head_utf16 > region.hidden_utf16_range.start
                && selected.selection.head_utf16 < region.hidden_utf16_range.end)
                .then_some(region.hidden_utf16_range.start);
            // 先在纯栈发布本次有限选择，再调用Native setter；不能在其重入后追加旧region。
            self.ivars().folds.borrow_mut().push(region);
            self.ivars().folds.advance();
            if let Some(header) = move_to_header {
                self.set_semantic_native_selection(MarkdownTextSelection::caret(header));
            }
        }
        self.invalidate_fold_layout();
        Ok(())
    }

    /// Rebuild at most one viewport of logical line labels from existing TextKit 2 fragment geometry.
    pub(super) fn refresh_source_gutter(&self) {
        self.finish_fold_layout_update();
        // 所有调用方（含 probe 直接调用）都覆盖 canonical Rc 和枚举 block 的完整寿命。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // 清理后的 Vec 容量不能因迟到刷新被重建。
            return;
        }
        let Some(ruler) = self.ivars().gutter.get().and_then(Weak::load) else {
            // Isolated storage/probe adapters may intentionally have no scroll-owned ruler.
            return;
        };
        // 刷新开始即撤销上一批证明；同代次新视口若准备失败，旧 lines 也不能借新票继续画或命中。
        // 先移出 RefCell 再 drop Weak 元数据，所有后续 Native 调用都不携带槽借用。
        let previous_geometry = ruler.ivars().geometry.borrow_mut().take();
        drop(previous_geometry);
        if self
            .ivars()
            .text_storage
            .load()
            .is_none_or(|storage| storage.presentation_mode() != MarkdownPresentationMode::Source)
        {
            // Preview has no ruler labels or fold controls, matching its uncluttered baseline presentation.
            let previous = ruler.ivars().lines.replace(Vec::new());
            drop(previous);
            return;
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            return;
        };
        let Some(document) = storage.document() else {
            // 已断开正文既没有来源，也不能继续保留可命中的旧标签。
            let previous = ruler.ivars().lines.replace(Vec::new());
            drop(previous);
            return;
        };
        let (document_length, generation) = {
            let document = document.borrow();
            (document.utf16_len(), document.generation())
        };
        if self.ivars().folds.generation.get() != generation {
            // 只有绕过正式observer的未知新代次才展开；已提交映射由其同代次票据继续授权。
            self.ivars().folds.synchronize_generation(generation);
        }
        let Ok(capture) = self.ivars().folds.capture_selection() else {
            return; // 刷新只消费原前台笔记票；不能在 Native 回调后重新领取较新 owner。
        };
        let geometry_epoch = storage.ivars().table_geometry_epoch.get();
        let semantic_epoch = storage.ivars().presentation_semantic_epoch.get();
        let font_epoch = storage.ivars().font_run_scratch.epoch();
        let fold_revision = self.ivars().folds.revision();
        let current = || {
            capture.current()
                && !self.ivars().textkit_graph_teardown.get()
                && storage.presentation_mode() == MarkdownPresentationMode::Source
                && storage.ivars().table_geometry_epoch.get() == geometry_epoch
                && storage.ivars().presentation_semantic_epoch.get() == semantic_epoch
                && storage.ivars().font_run_scratch.epoch() == font_epoch
                && self.ivars().folds.revision() == fold_revision
        };
        let is_empty = document_length == 0;
        if is_empty {
            // 空笔记依旧只有第一行，不为它请求片段或构造假的 source 覆盖。
            let Ok(Some((visible, origin))) =
                native_markdown_table_intent_step(&current, || self.bounded_paint_viewport())
            else {
                return;
            };
            let prepared = vec![GutterLine {
                number: 0,
                document_y: origin.y,
                height: 20.0,
                may_fold: false,
                folded: false,
            }];
            let proof = Rc::new(GutterGeometryProof {
                generation,
                geometry_epoch,
                semantic_epoch,
                font_epoch,
                fold_revision,
                visible,
                origin,
                viewport_source: 0..0,
                manager: None,
                content: None,
                fragments: Vec::new(),
            });
            if !publish_gutter_lines(&ruler.ivars().lines, prepared, current()) {
                return;
            }
            // 发布期间没有 Native 调用，不能出现新 lines 仍带旧 proof 的可重入观察窗口。
            *ruler.ivars().geometry.borrow_mut() = Some(proof);
            ruler.setNeedsDisplay(true);
            return;
        }
        let prepared = (|| -> Option<(Vec<GutterLine>, Rc<GutterGeometryProof>)> {
            // 每个 Native getter 都在同一原票前后检查；正文借用不跨 Native 或同步枚举。
            let manager =
                native_markdown_table_intent_step(&current, || self.textLayoutManager()).ok()??;
            let registered = native_markdown_table_intent_step(&current, || {
                self.ivars().text_layout_manager.get().and_then(Weak::load)
            })
            .ok()??;
            if !std::ptr::eq(&*manager, &*registered) {
                return None; // 同长度、同坐标的另一 manager 也不能成为本 ruler 的源。
            }
            let content =
                native_markdown_table_intent_step(&current, || manager.textContentManager())
                    .ok()??;
            let actual_content =
                native_markdown_table_intent_step(&current, || self.textContentStorage())
                    .ok()??;
            if !std::ptr::eq::<objc2_app_kit::NSTextContentManager>(
                &*content,
                actual_content.as_super(),
            ) {
                return None; // 保持原 view/content/storage 图的真实身份，不从点坐标猜关联。
            }
            let actual_storage =
                native_markdown_table_intent_step(&current, || actual_content.textStorage())
                    .ok()??;
            if !std::ptr::eq::<NSTextStorage>(&*actual_storage, storage.as_super()) {
                return None;
            }
            let (visible, origin) =
                native_markdown_table_intent_step(&current, || self.bounded_paint_viewport())
                    .ok()??;
            let viewport = native_markdown_table_intent_step(&current, || {
                manager.textViewportLayoutController()
            })
            .ok()?;
            let viewport_manager =
                native_markdown_table_intent_step(&current, || viewport.textLayoutManager())
                    .ok()??;
            if !std::ptr::eq(&*viewport_manager, &*manager) {
                return None; // 原 controller 的真实范围只对其实际 manager 生效。
            }
            let range =
                native_markdown_table_intent_step(&current, || viewport.viewportRange()).ok()??;
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
            let document_range =
                native_markdown_table_intent_step(&current, || provider.documentRange()).ok()?;
            let document_start =
                native_markdown_table_intent_step(&current, || document_range.location()).ok()?;
            let source_range = |range: &NSTextRange| -> Option<std::ops::Range<usize>> {
                let start =
                    native_markdown_table_intent_step(&current, || range.location()).ok()?;
                let end =
                    native_markdown_table_intent_step(&current, || range.endLocation()).ok()?;
                let from = usize::try_from(
                    native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(&document_start, &start)
                    })
                    .ok()?,
                )
                .ok()?;
                let to = usize::try_from(
                    native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(&document_start, &end)
                    })
                    .ok()?,
                )
                .ok()?;
                if from > to || to > document_length {
                    return None; // 不把外来/越界 location clamp 成合法 canonical 范围。
                }
                for (location, offset) in [(&start, from), (&end, to)] {
                    let offset = isize::try_from(offset).ok()?;
                    let reconstructed = native_markdown_table_intent_step(&current, || {
                        content.locationFromLocation_withOffset(&document_start, offset)
                    })
                    .ok()??;
                    if native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(location, &reconstructed)
                    })
                    .ok()?
                        != 0
                    {
                        return None; // 必须能由原 provider 精确重建端点，不能仅凭整数范围相信来源。
                    }
                }
                Some(from..to)
            };
            if source_range(&document_range)? != (0..document_length) {
                return None;
            }
            let viewport_source = source_range(&range)?;
            let terminal_viewport =
                viewport_source.is_empty() && viewport_source.end == document_length;
            if viewport_source.is_empty() && !terminal_viewport {
                return None; // 暂未布局的内部空 viewport 没有当前正文覆盖，不能猜上一片的位置。
            }
            // 点查询可能先返回旧 outflow 的 LayoutAvailable 片段；只有当前 viewport.location
            // 的公开源查询是本批首片。无需 ensureLayout，也不从 0 访问任何离屏正文。
            let seed = native_markdown_table_intent_step(&current, || range.location()).ok()?;
            let first = native_markdown_table_intent_step(&current, || {
                manager.textLayoutFragmentForLocation(&seed)
            })
            .ok()??;
            let lines = RefCell::new(Vec::<GutterLine>::new());
            let fragments = RefCell::new(Vec::<GutterFragmentProof>::new());
            let visited = Cell::new(0_usize);
            let covered = Cell::new(None);
            let previous_bottom = Cell::new(None);
            let failed = Cell::new(false);
            let block =
                block2::RcBlock::new(|pointer: NonNull<objc2_app_kit::NSTextLayoutFragment>| {
                    if !current() || visited.get() >= MAX_GUTTER_LINES {
                        failed.set(true);
                        return objc2::runtime::Bool::NO; // 预算限制片段数，不让同一逻辑行的重复片绕过上限。
                    }
                    let first_callback = visited.get() == 0;
                    visited.set(visited.get() + 1);
                    let next = (|| -> Option<bool> {
                        // SAFETY: 公开同步枚举仅在 callback 内借出活片段；裸指针不保存到缓存。
                        let fragment = unsafe { pointer.as_ref() };
                        if first_callback && !std::ptr::eq(fragment, &*first) {
                            return None; // 枚举若仍返回另一个旧首片，不能跳过它而把洞包装成完整覆盖。
                        }
                        if native_markdown_table_intent_step(&current, || fragment.state()).ok()?
                            != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable
                        {
                            return None; // estimated/unknown 不能读取并发布 frame，绘制也不负责排版。
                        }
                        let fragment_manager = native_markdown_table_intent_step(&current, || {
                            fragment.textLayoutManager()
                        })
                        .ok()??;
                        let element =
                            native_markdown_table_intent_step(&current, || fragment.textElement())
                                .ok()??;
                        let element_content = native_markdown_table_intent_step(&current, || {
                            element.textContentManager()
                        })
                        .ok()??;
                        if !std::ptr::eq(&*fragment_manager, &*manager)
                            || !std::ptr::eq(&*element_content, &*content)
                        {
                            return None; // 每片仍须来自原 manager/content，不能混合两次布局的 Native 图。
                        }
                        let fragment_range = native_markdown_table_intent_step(&current, || {
                            fragment.rangeInElement()
                        })
                        .ok()?;
                        let source = source_range(&fragment_range)?;
                        let location = native_markdown_table_intent_step(&current, || {
                            fragment_range.location()
                        })
                        .ok()?;
                        let actual = native_markdown_table_intent_step(&current, || {
                            manager.textLayoutFragmentForLocation(&location)
                        })
                        .ok()??;
                        if !std::ptr::eq(&*actual, fragment) {
                            return None; // 已被原 manager 替代的旧 outflow 片段即使 state==3 也没有标签权限。
                        }
                        let frame = native_markdown_table_intent_step(&current, || {
                            fragment.layoutFragmentFrame()
                        })
                        .ok()?;
                        let (frontier, bottom) = if terminal_viewport {
                            // EOF 可以有真正已排版的空末行，也可能公开 lookup 返回唯一实际末片；
                            // 只接受其真实 source.end==len 的这一片，不向前搜索、不合成基线。
                            if !first_callback || source.end != document_length {
                                return None;
                            }
                            (source.end, gutter_frame_bottom(frame, None)?)
                        } else {
                            gutter_viewport_fragment_frontier_with_hidden(
                                &viewport_source,
                                covered.get(),
                                &source,
                                previous_bottom.get(),
                                frame,
                                &|gap| {
                                    self.ivars().folds.hidden_range_is_covered(
                                        gap,
                                        generation,
                                        fold_revision,
                                        &current,
                                    )
                                },
                            )?
                        };
                        // 先证明连续源和单调几何，再取逻辑行元数据；完全屏外与折叠零高片仍只推进覆盖。
                        let top = frame.origin.y + origin.y;
                        let bottom_in_document = bottom + origin.y;
                        if !top.is_finite() || !bottom_in_document.is_finite() {
                            return None;
                        }
                        if frame.size.height > 0.0
                            && top < visible.origin.y + visible.size.height
                            && bottom_in_document > visible.origin.y
                        {
                            let metadata = {
                                let document = document.borrow();
                                document
                                    .byte_offset_for_utf16(source.start)
                                    .and_then(|byte| document.line_for_byte(byte))
                                    .map(|number| {
                                        (
                                            number,
                                            markdown_line_may_fold(&document, number)
                                                .unwrap_or(false),
                                        )
                                    })
                            }
                            .ok()?;
                            let (number, hint) = metadata;
                            if lines
                                .borrow()
                                .last()
                                .is_none_or(|line| line.number != number)
                            {
                                let folded = self
                                    .ivars()
                                    .folds
                                    .borrow()
                                    .iter()
                                    .any(|fold| fold.header_line == number);
                                lines.borrow_mut().push(GutterLine {
                                    number,
                                    document_y: top,
                                    height: frame.size.height,
                                    may_fold: hint,
                                    folded,
                                });
                            } // 软换行片段共享原逻辑行号，不把行号移到视口顶端。
                        }
                        let fragment_weak =
                            native_markdown_table_intent_step(&current, || Weak::from(fragment))
                                .ok()?;
                        fragments.borrow_mut().push(GutterFragmentProof {
                            fragment: fragment_weak,
                            source,
                            frame,
                        });
                        covered.set(Some(frontier));
                        previous_bottom.set(Some(bottom));
                        Some(frontier < viewport_source.end) // 精确覆盖真实 viewport 后立停，不消费相邻 outflow。
                    })();
                    if next.is_none() || !current() {
                        failed.set(true);
                        return objc2::runtime::Bool::NO;
                    }
                    next.unwrap_or(false).into()
                });
            let enumerated = native_markdown_table_intent_step(&current, || {
                manager.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
                    Some(&seed),
                    NSTextLayoutFragmentEnumerationOptions::empty(),
                    &block,
                )
            })
            .ok()?;
            drop(enumerated);
            drop(block); // 同步枚举与所有 callback 借用结束后，才做整批发布前复核。
            let covered_to_end = covered.get().is_some_and(|end| {
                end >= viewport_source.end
                    || self.ivars().folds.hidden_range_is_covered(
                        &(end..viewport_source.end),
                        generation,
                        fold_revision,
                        &current,
                    )
            });
            if failed.get() || !covered_to_end {
                return None; // 不发布截断前缀，也不用 frame 高度补造源覆盖。
            }
            let final_range =
                native_markdown_table_intent_step(&current, || viewport.viewportRange()).ok()??;
            let final_manager =
                native_markdown_table_intent_step(&current, || self.textLayoutManager()).ok()??;
            let final_content =
                native_markdown_table_intent_step(&current, || manager.textContentManager())
                    .ok()??;
            if source_range(&final_range)? != viewport_source
                || !std::ptr::eq(&*final_manager, &*manager)
                || !std::ptr::eq(&*final_content, &*content)
                || native_markdown_table_intent_step(&current, || self.bounded_paint_viewport())
                    .ok()?
                    != Some((visible, origin))
            {
                return None; // 查找/滚动/模式重入改变原 viewport 或图时，不提交跨帧混合结果。
            }
            let manager_weak =
                native_markdown_table_intent_step(&current, || Weak::from_retained(&manager))
                    .ok()?;
            let content_weak =
                native_markdown_table_intent_step(&current, || Weak::from_retained(&content))
                    .ok()?;
            let proof = Rc::new(GutterGeometryProof {
                generation,
                geometry_epoch,
                semantic_epoch,
                font_epoch,
                fold_revision,
                visible,
                origin,
                viewport_source,
                manager: Some(manager_weak),
                content: Some(content_weak),
                fragments: fragments.into_inner(),
            });
            Some((lines.into_inner(), proof))
        })();
        let Some((prepared, proof)) = prepared else {
            return; // 保留原失败策略；没有完整当前几何时不把不完整前缀伪装成可见标签。
        };
        if !publish_gutter_lines(
            &ruler.ivars().lines,
            prepared,
            current() && self.gutter_generation_is_current(generation),
        ) {
            // 最后一次 native callback 返回后才提交；关闭或 canonical 改变均丢弃本次容量。
            return;
        }
        *ruler.ivars().geometry.borrow_mut() = Some(proof);
        ruler.setNeedsDisplay(true);
    }
}

/// 同一真实 viewport 的下一片必须连续推进源，且排版 frame 不能倒退或重叠。
/// 只检查有限标量；零高折叠片仍可推进覆盖，真正离屏/隐藏标签由原绘制交集决定。
#[cfg(test)]
fn gutter_viewport_fragment_frontier(
    viewport: &std::ops::Range<usize>,
    covered: Option<usize>,
    source: &std::ops::Range<usize>,
    previous_bottom: Option<f64>,
    frame: NSRect,
) -> Option<(usize, f64)> {
    gutter_viewport_fragment_frontier_with_hidden(
        viewport,
        covered,
        source,
        previous_bottom,
        frame,
        &|_| false,
    )
}

/// Native可以完全省略显式折叠片；只有同原票证明hidden联合覆盖才跨源空洞。
fn gutter_viewport_fragment_frontier_with_hidden(
    viewport: &std::ops::Range<usize>,
    covered: Option<usize>,
    source: &std::ops::Range<usize>,
    previous_bottom: Option<f64>,
    frame: NSRect,
    hidden: &impl Fn(&std::ops::Range<usize>) -> bool,
) -> Option<(usize, f64)> {
    if viewport.start >= viewport.end || source.start >= source.end {
        return None; // 普通视口不接受空范围；真正的 EOF 单片由调用方独立核原 document length。
    }
    let bottom = gutter_frame_bottom(frame, previous_bottom)?;
    if frame.size.height == 0.0 && !hidden(source) {
        return None; // 零高不能无条件吞掉正文；只有真实显式fold可省略这整个源范围。
    }
    let continuous = match covered {
        None => {
            (source.start <= viewport.start && viewport.start < source.end)
                || (source.start > viewport.start && hidden(&(viewport.start..source.start)))
        }
        Some(end) => {
            end < viewport.end
                && (source.start == end || (source.start > end && hidden(&(end..source.start))))
        }
    };
    continuous.then_some((source.end, bottom)) // 普通源仍精确相接，只有已证明的hidden空洞例外。
}

/// 普通片与真实 EOF 片共享有限 frame 校验；不通过改 y、排序或估计高度修补失效布局。
fn gutter_frame_bottom(frame: NSRect, previous_bottom: Option<f64>) -> Option<f64> {
    if ![
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
    ]
    .into_iter()
    .all(f64::is_finite)
        || frame.size.width < 0.0
        || frame.size.height < 0.0
    {
        return None; // 空源、未知/溢出几何没有覆盖能力，不能由旧坐标猜可见逻辑行。
    }
    let bottom = frame.origin.y + frame.size.height;
    if !bottom.is_finite() || previous_bottom.is_some_and(|previous| frame.origin.y < previous) {
        return None; // 源顺序正确但 y 倒退同样是混合布局；不能重新排序坐标掩盖重叠。
    }
    Some(bottom)
}

/// 只允许活的同代次投影发布，窗口可见性和已经排队均不是权限。
fn projection_can_publish(sealed: bool, expected: u64, current: Option<u64>) -> bool {
    !sealed && current == Some(expected)
}

/// 有限纯 metadata 的最后提交点；拒收时 prepared Vec 在无状态借用处完整 drop。
fn publish_gutter_lines(
    target: &RefCell<Vec<GutterLine>>,
    prepared: Vec<GutterLine>,
    current: bool,
) -> bool {
    if !current || prepared.len() > MAX_GUTTER_LINES {
        // 不截断过大的或已失效的本地结果，更不能覆盖 accepted 清理后的零容量槽。
        return false;
    }
    let previous = target.replace(prepared);
    drop(previous);
    true
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    fn line(number: usize) -> GutterLine {
        GutterLine {
            number,
            document_y: number as f64,
            height: 20.0,
            may_fold: false,
            folded: false,
        }
    }

    #[test]
    fn actual_viewport_excludes_old_outflow_and_rejects_backwards_geometry() {
        let viewport = 11_763..13_520;
        let frame = |y, height| NSRect::new(NSPoint::new(0.0, y), NSSize::new(600.0, height));
        // 实际回归的旧 point 查询先给出三条离开 viewport 的 source；state==3 也不能授予覆盖。
        for (source, y, height) in [
            (11_504..11_560, 6_443.0, 17.0),
            (11_560..11_707, 6_460.0, 51.0),
            (11_707..11_763, 6_511.0, 17.0),
        ] {
            assert!(
                gutter_viewport_fragment_frontier(&viewport, None, &source, None, frame(y, height))
                    .is_none()
            );
        }
        let first = gutter_viewport_fragment_frontier(
            &viewport,
            None,
            &(11_763..11_819),
            None,
            frame(6_456.0, 17.0),
        )
        .unwrap();
        assert_eq!(first, (11_819, 6_473.0));
        assert!(
            gutter_viewport_fragment_frontier(
                &viewport,
                Some(first.0),
                &(11_819..11_875),
                Some(first.1),
                frame(6_460.0, 17.0)
            )
            .is_none()
        );
        assert_eq!(
            gutter_viewport_fragment_frontier(
                &viewport,
                Some(first.0),
                &(11_819..11_875),
                Some(first.1),
                frame(6_473.0, 17.0)
            ),
            Some((11_875, 6_490.0))
        );
    }

    #[test]
    fn viewport_coverage_keeps_wrapped_first_and_folded_zero_height_but_never_bridges_holes() {
        let viewport = 100..300;
        let frame = |y, height| NSRect::new(NSPoint::new(0.0, y), NSSize::new(0.0, height));
        // 首片本身可跨视口起点；折叠片源仍存在，零高只不画标签，不删除 canonical 覆盖。
        let first =
            gutter_viewport_fragment_frontier(&viewport, None, &(80..160), None, frame(0.0, 34.0))
                .unwrap();
        let folded = gutter_viewport_fragment_frontier_with_hidden(
            &viewport,
            Some(first.0),
            &(160..240),
            Some(first.1),
            frame(34.0, 0.0),
            &|gap| *gap == (160..240),
        )
        .unwrap();
        assert_eq!(
            gutter_viewport_fragment_frontier(
                &viewport,
                Some(folded.0),
                &(240..320),
                Some(folded.1),
                frame(34.0, 17.0)
            ),
            Some((320, 51.0))
        );
        for source in [159..240, 161..240, 160..160] {
            assert!(
                gutter_viewport_fragment_frontier(
                    &viewport,
                    Some(first.0),
                    &source,
                    Some(first.1),
                    frame(34.0, 17.0)
                )
                .is_none()
            );
        }
        assert!(
            gutter_viewport_fragment_frontier(
                &viewport,
                Some(320),
                &(320..340),
                Some(51.0),
                frame(51.0, 17.0)
            )
            .is_none()
        );
        assert!(
            gutter_viewport_fragment_frontier(
                &(300..300),
                None,
                &(300..301),
                None,
                frame(0.0, 17.0)
            )
            .is_none()
        );
        for bad in [
            frame(f64::NAN, 17.0),
            frame(0.0, -1.0),
            frame(f64::MAX, f64::MAX),
        ] {
            assert!(
                gutter_viewport_fragment_frontier(&viewport, None, &(80..160), None, bad).is_none()
            );
        }
    }

    #[test]
    fn native_omitted_fold_requires_exact_hidden_gap_and_keeps_frame_order() {
        let frame = |y, height| NSRect::new(NSPoint::new(0.0, y), NSSize::new(780.0, height));
        let advance = |source: std::ops::Range<usize>,
                       y,
                       hidden: &dyn Fn(&std::ops::Range<usize>) -> bool| {
            gutter_viewport_fragment_frontier_with_hidden(
                &(0..490),
                Some(51),
                &source,
                Some(85.0),
                frame(y, 17.0),
                &|gap| hidden(gap),
            )
        };
        assert_eq!(
            advance(126..127, 85.0, &|gap| *gap == (51..126)),
            Some((127, 102.0))
        );
        assert!(advance(126..127, 85.0, &|_| false).is_none());
        assert!(advance(127..128, 85.0, &|gap| *gap == (51..126)).is_none());
        assert!(advance(126..127, 84.0, &|gap| *gap == (51..126)).is_none());
        assert!(
            gutter_viewport_fragment_frontier(
                &(0..490),
                Some(51),
                &(51..126),
                Some(85.0),
                frame(85.0, 0.0),
            )
            .is_none()
        ); // 零高本身不能作为隐藏证明。
    }

    #[test]
    fn native_callback_close_cannot_republish_prepared_geometry_capacity() {
        let target = RefCell::new(vec![line(0)]);
        let prepared: Vec<_> = (0..MAX_GUTTER_LINES).map(line).collect();
        // 模拟 Native 枚举返回前接受清理：目标已 take/drop，而栈内旧 geometry 尚未结束。
        let previous = target.replace(Vec::new());
        drop(previous);
        assert!(!publish_gutter_lines(
            &target,
            prepared,
            projection_can_publish(true, 7, Some(7))
        ));
        assert!(target.borrow().is_empty());
        assert_eq!(target.borrow().capacity(), 0);
    }

    #[test]
    fn native_callback_revision_change_or_detach_preserves_the_last_live_snapshot() {
        let target = RefCell::new(vec![line(3)]);
        for current in [Some(9), None] {
            // 入口的第 8 代不授权第 9 代/已断开结果；只检查入口会错误写回旧值。
            assert!(!publish_gutter_lines(
                &target,
                vec![line(8)],
                projection_can_publish(false, 8, current)
            ));
            assert_eq!(target.borrow()[0].number, 3);
        }
        assert!(publish_gutter_lines(
            &target,
            vec![line(9)],
            projection_can_publish(false, 9, Some(9))
        ));
        assert_eq!(target.borrow()[0].number, 9);
    }

    #[test]
    fn publication_rejects_oversized_native_geometry_without_a_truncated_prefix() {
        let target = RefCell::new(Vec::new());
        assert!(!publish_gutter_lines(
            &target,
            (0..=MAX_GUTTER_LINES).map(line).collect(),
            true
        ));
        assert_eq!(target.borrow().capacity(), 0);
        assert!(publish_gutter_lines(
            &target,
            (0..MAX_GUTTER_LINES).map(line).collect(),
            true
        ));
        assert_eq!(target.borrow().len(), MAX_GUTTER_LINES);
    }

    #[test]
    fn partially_visible_long_line_keeps_only_its_real_visible_fold_target() {
        // 长行从 Find 区域上方延续到正文下方，原完整行高会错误接受两侧遮挡区的点击。
        let viewport = NSRect::new(NSPoint::new(0.0, 100.0), NSSize::new(74.0, 60.0));
        let visible = visible_gutter_line(40.0, 200.0, viewport).unwrap();
        assert_eq!(visible, viewport);
        assert!(!gutter_contains_point(visible, NSPoint::new(10.0, 80.0)));
        assert!(gutter_contains_point(visible, NSPoint::new(10.0, 120.0)));
        assert!(!gutter_contains_point(visible, NSPoint::new(10.0, 170.0)));
        // 完全屏外的行没有命中矩形，不能仅依赖 Native dirtyRect 代替可见区。
        assert!(visible_gutter_line(10.0, 20.0, viewport).is_none());
    }

    #[test]
    fn resized_viewport_edges_and_invalid_geometry_cannot_expand_fold_hits() {
        let before = NSRect::new(NSPoint::new(0.0, 20.0), NSSize::new(74.0, 160.0));
        let after = NSRect::new(NSPoint::new(0.0, 80.0), NSSize::new(74.0, 100.0));
        let old_line = visible_gutter_line(60.0, 40.0, before).unwrap();
        let new_line = visible_gutter_line(60.0, 40.0, after).unwrap();
        // Find/resize 收紧后的真实区域立即拒绝旧热区，但保留同一逻辑行仍可见的部分。
        assert!(gutter_contains_point(old_line, NSPoint::new(10.0, 70.0)));
        assert!(!gutter_contains_point(new_line, NSPoint::new(10.0, 70.0)));
        assert!(gutter_contains_point(new_line, NSPoint::new(0.0, 80.0)));
        assert!(!gutter_contains_point(new_line, NSPoint::new(74.0, 80.0)));
        assert!(!gutter_contains_point(new_line, NSPoint::new(0.0, 100.0)));
        assert!(visible_gutter_line(f64::NAN, 20.0, after).is_none());
        assert!(visible_gutter_line(80.0, f64::INFINITY, after).is_none());
        assert!(!gutter_contains_point(
            new_line,
            NSPoint::new(f64::NAN, 90.0)
        ));
    }
}
