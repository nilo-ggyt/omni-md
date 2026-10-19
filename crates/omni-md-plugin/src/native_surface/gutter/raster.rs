//! 原 ruler 仍负责全部背景、标签和折叠标记；这里只替换有限绘图目标与 backing 发布。

use super::*;
use crate::native_surface::raster_surface::{
    NativeMarkdownRasterAuxiliaryResources, NativeMarkdownRasterOriginPermit,
    NativeMarkdownRasterSurface,
};
use objc2_app_kit::{NSViewLayerContentsRedrawPolicy, NSWindow};
use objc2_core_foundation::CGAffineTransform;
use objc2_core_graphics::{CGContext, CGImage};
use objc2_quartz_core::{CALayer, CATransaction, kCAContentsFormatRGBA8Uint, kCAGravityResize};

/// 只有实际启用了同一正文 Raster 才安装；没有正文、图像或另一个资源计数器。
/// 原 contents 仅为恢复既有 Native 关系保留，原来为 nil 时不制造占位图。
pub(super) struct GutterRaster {
    ruler: Weak<NativeMarkdownGutter>,
    view: Weak<PieceBackedMarkdownTextView>,
    folds: std::rc::Weak<NativeMarkdownFoldState>,
    owner: std::rc::Weak<NativeMarkdownRasterSurface>,
    resources: NativeMarkdownRasterAuxiliaryResources,
    previous_layer: Option<Weak<CALayer>>,
    installed_layer: Option<Weak<CALayer>>,
    previous_contents: Option<Retained<AnyObject>>,
    previous_wants_layer: bool,
    previous_redraw: NSViewLayerContentsRedrawPolicy,
    previous_format: Option<Retained<NSString>>,
    previous_gravity: Option<Retained<NSString>>,
    previous_scale: Option<f64>,
    previous_flatten_subviews: bool,
    finished: Cell<bool>,
    painting: Cell<bool>,
    paint_logs: Cell<u8>,
    entry_logs: Cell<u8>,
    pending_logs: Cell<u8>,
    geometry_logs: Cell<u8>,
    active: RefCell<Option<Rc<PaintAuthority>>>,
}

/// 只存在于一次同步 updateLayer 栈；持有的 Native 值在当前调用退出前全部归还。
/// 原 OriginPermit 同时约束正文、字体、模式、选择和几何事件，绝不补领较新票。
struct PaintAuthority {
    lease: Rc<GutterRaster>,
    owner: Rc<NativeMarkdownRasterSurface>,
    permit: NativeMarkdownRasterOriginPermit,
    view: Retained<PieceBackedMarkdownTextView>,
    capture: NativeMarkdownSelectionCapture,
    lines: Vec<GutterLine>,
    // 固定刷新时的真实文档视口/片段批次，不能让同代次旧 labels 借新绘制票重新发布。
    proof: Rc<GutterGeometryProof>,
    attributes: Rc<Retained<MarkdownTextAttributes>>,
    bounds: NSRect,
    viewport: NSRect,
    generation: u64,
    window: Retained<NSWindow>,
    layer: Retained<CALayer>,
    scale: f64,
    draw_completed: Cell<bool>,
    drawn_labels: Cell<usize>,
    published_image: Cell<Option<NonNull<CGImage>>>,
}

/// graphics slot 是线程局部原生状态；失败撤权也必须恢复原值，不能把 CPU context 留给窗口。
struct GraphicsRestore(Option<Retained<NSGraphicsContext>>);
impl Drop for GraphicsRestore {
    fn drop(&mut self) {
        NSGraphicsContext::setCurrentContext(self.0.as_deref());
    }
}

/// active 只借给同步的原 drawHashMarks 回调；先移出槽，再释放全部 Native 临时 claim。
struct ActivePaint<'a>(&'a GutterRaster);
impl Drop for ActivePaint<'_> {
    fn drop(&mut self) {
        let previous = self.0.active.borrow_mut().take();
        drop(previous);
        self.0.painting.set(false);
    }
}

/// 即使中途撤权也要配平 begin/commit；该清理不据此签任何发布成功收据。
struct Transaction;
impl Drop for Transaction {
    fn drop(&mut self) {
        CATransaction::commit();
    }
}

impl GutterRaster {
    pub(super) fn install(
        ruler: &NativeMarkdownGutter,
        view: &PieceBackedMarkdownTextView,
        owner: &Rc<NativeMarkdownRasterSurface>,
        ledger: &NativeMarkdownOwnedLedger,
    ) -> Result<Rc<Self>, String> {
        let resources = owner.auxiliary_resources();
        let current = || {
            resources.is_open()
                && !view.ivars().textkit_graph_teardown.get()
                && view
                    .ivars()
                    .raster_surface
                    .borrow()
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, owner))
        };
        // 构造尚未绑定活 session，不能伪造选择票；这里仅检查原构造仍拥有同一 view/Raster。
        let previous_wants_layer = construction_step(&current, || ruler.wantsLayer())?;
        let previous_redraw = construction_step(&current, || ruler.layerContentsRedrawPolicy())?;
        let previous_layer = construction_step(&current, || ruler.layer())?;
        let previous_weak = match previous_layer.as_deref() {
            Some(layer) => Some(construction_step(&current, || Weak::from(layer))?),
            None => None, // 原 nil 关系最终必须恢复为 nil。
        };
        let previous_contents = match previous_layer.as_deref() {
            // SAFETY: 当前主线程 CALayer 的公开 contents 值只为恢复原关系保留；不解释其类型。
            Some(layer) => construction_step(&current, || unsafe { layer.contents() })?,
            None => None,
        };
        let previous_format = match previous_layer.as_ref() {
            Some(layer) => Some(construction_step(&current, || layer.contentsFormat())?),
            None => None, // 原无层时没有需要恢复的层配置，不能为快照另造一个CALayer。
        };
        let previous_gravity = match previous_layer.as_ref() {
            Some(layer) => Some(construction_step(&current, || layer.contentsGravity())?),
            None => None,
        };
        let previous_scale = match previous_layer.as_ref() {
            Some(layer) => Some(construction_step(&current, || layer.contentsScale())?),
            None => None,
        };
        let previous_flatten_subviews =
            construction_step(&current, || ruler.canDrawSubviewsIntoLayer())?;
        let mut lease = Self {
            ruler: construction_step(&current, || Weak::from(ruler))?,
            view: construction_step(&current, || Weak::from(view))?,
            folds: Rc::downgrade(&view.ivars().folds),
            owner: Rc::downgrade(owner),
            resources: resources.clone(),
            previous_layer: previous_weak,
            installed_layer: None,
            previous_contents,
            previous_wants_layer,
            previous_redraw,
            previous_format,
            previous_gravity,
            previous_scale,
            previous_flatten_subviews,
            finished: Cell::new(false),
            painting: Cell::new(false),
            paint_logs: Cell::new(0),
            entry_logs: Cell::new(0),
            pending_logs: Cell::new(0),
            geometry_logs: Cell::new(0),
            active: RefCell::new(None),
        };
        // 必须先且仅由wantsLayer让AppKit创建真正layer-backed层。先setLayer再wantsLayer会成为
        // layer-hosting view，AppKit不再负责updateLayer；这正是缓存已刷新但74pt栏一直空白的原因。
        // 此处不调用setLayer安装自有层，关闭恢复仍只操作下方捕获的真实backing身份。
        construction_step(&current, || ruler.setWantsLayer(true))?;
        let layer = construction_step(&current, || ruler.layer())?
            .ok_or("源码行号未取得AppKit实际backing层")?;
        lease.installed_layer = Some(construction_step(&current, || Weak::from_retained(&layer))?);
        if !same_layer(Some(&layer), previous_layer.as_deref()) {
            construction_step(&current, || {
                ledger.include("editor.raster.gutter-layer", &layer)
            })??;
        }
        // 公开合同规定该flag为true会忽略wantsUpdateLayer并建立drawRect显示列表；此独立栏明确保留
        // 自己的CPU contents入口，不把下层窗口绘制重新引回accelerated-CG。
        construction_step(&current, || ruler.setCanDrawSubviewsIntoLayer(false))?;
        construction_step(&current, || {
            ruler.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay)
        })?;
        let actual = construction_step(&current, || ruler.layer())?;
        if !actual
            .as_deref()
            .is_some_and(|actual| std::ptr::eq::<CALayer>(actual, &*layer))
        {
            return Err("源码行号没有保留本次安装的实际 backing 层".into());
        }
        drop(actual);
        drop(layer);
        drop(previous_layer);
        if !current() {
            return Err("源码行号 backing 构造结束时已撤权".into());
        }
        Ok(Rc::new(lease))
    }

    pub(super) fn invalidate_geometry(&self) {
        if !self.finished.get() {
            // 与正文共用原几何代次；只撤原票，不增加调度器或第二个几何 authority。
            self.resources.invalidate_geometry();
        }
    }

    pub(super) fn active(&self) -> bool {
        self.active.borrow().is_some()
    }

    /// 原绘制函数逐 Native 原语调用本入口；没有同步 CPU 原票时禁止走窗口绘图旁路。
    pub(super) fn drawing_step<T>(
        &self,
        ruler: &NativeMarkdownGutter,
        work: impl FnOnce() -> T,
    ) -> Option<T> {
        let auth = self.active.borrow().clone();
        let auth = auth?; // 只接受当前原 super drawRect 同步回调，不能自行新建权限。
        match auth.step(ruler, work) {
            Ok(value) => Some(value),
            Err(error) => {
                self.resources.fail();
                eprintln!(
                    "native_raster_surface event=gutter_draw_cancelled error={error:?} formal=false"
                );
                None
            }
        }
    }

    pub(super) fn mark_draw_complete(&self, ruler: &NativeMarkdownGutter, facts: GutterDrawFacts) {
        let auth = self.active.borrow().clone();
        if let Some(auth) = auth
            && auth.validate_geometry(ruler).is_ok()
        {
            let count = self.geometry_logs.get();
            if count < 8 {
                self.geometry_logs.set(count + 1);
                native_markdown_trace! {
                    eprintln!("native_raster_surface event=gutter_draw_geometry dirty={:?} viewport={:?} first_line_number_document_y_ruler_y_height={:?} cached_lines={} outside_viewport={} outside_damage={} drawn_labels={} formal=false", facts.dirty, facts.viewport, facts.first_line, auth.lines.len(), facts.outside_viewport, facts.outside_damage, auth.drawn_labels.get());
                }
            }
            // 只标记原循环完整返回；实际标签/屏外交集另作事实输出，不能据此宣称可见覆盖正确。
            auth.draw_completed.set(true);
        }
    }

    /// 只累计本次真实 drawAtPoint 已沿原票返回的标签，不把缓存长度冒充已画行数。
    pub(super) fn record_drawn_label(&self) {
        if let Some(auth) = self.active.borrow().as_ref() {
            auth.drawn_labels.set(auth.drawn_labels.get() + 1);
        } // 没有同步 CPU 原绘图栈时不记录后继或窗口旁路的绘制。
    }

    fn pending(&self, reason: &'static str) -> Result<bool, String> {
        let count = self.pending_logs.get();
        if count < 16 {
            self.pending_logs.set(count + 1);
            native_markdown_trace! { eprintln!("native_raster_surface event=gutter_pending reason={reason} formal=false"); }
        }
        Ok(false) // 只解释本次没有绘制/发布，绝不是完成收据，也不追加timer或重试任务。
    }

    pub(super) fn paint(
        self: &Rc<Self>,
        ruler: &NativeMarkdownGutter,
        entry: &'static str,
    ) -> Result<bool, String> {
        let count = self.entry_logs.get();
        if count < 8 {
            self.entry_logs.set(count + 1);
            native_markdown_trace! { eprintln!("native_raster_surface event=gutter_callback_received entry={entry} formal=false"); }
        }
        if self.finished.get() || !self.resources.is_open() {
            return self.pending("closed"); // accepted 后迟到的空壳回调不能恢复任何业务资源。
        }
        if self.painting.replace(true) {
            return Err("源码行号光栅绘制发生同步重入".into());
        }
        let _active = ActivePaint(self);
        let _operation = self.resources.operation()?;
        // 首个 Native Weak.load 前固定原纯 Rust 选择/owner 票和有限缓存，不靠 getter 重签。
        let Some(folds) = self.folds.upgrade() else {
            return self.pending("fold_owner_unavailable");
        };
        let Ok(capture) = folds.capture_selection() else {
            return self.pending("source_owner_not_current");
        };
        drop(folds);
        let lines = ruler.ivars().lines.borrow().clone();
        let Some(proof) = ruler.ivars().geometry.borrow().clone() else {
            return self.pending("visible_geometry_proof_unavailable");
        };
        let Some(attributes) = ruler.ivars().attributes.borrow().clone() else {
            return self.pending("gutter_attributes_unavailable");
        };
        if lines.is_empty() {
            return self.pending("visible_line_cache_empty"); // 没有原逻辑行时不发布一张缺标签的空图。
        }
        if lines.len() > MAX_GUTTER_LINES {
            return Err("源码行号缓存超出原有限上限".into());
        }
        let view = selection_native_step(&capture, || self.view.load())
            .map_err(|_| "源码行号原票在取得 view 时失效")?
            .ok_or("源码行号原 view 已释放")?;
        let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        if !ruler.cached_geometry_is_current(&view, &capture, &proof, "raster_admission") {
            return self.pending("visible_geometry_proof_stale"); // 只拒绝本次发布，不清空旧图像或假装刷新完成。
        }
        let Some(owner) = self.owner.upgrade() else {
            return self.pending("raster_owner_unavailable");
        };
        let permit = selection_native_step(&capture, || owner.begin(&view))
            .map_err(|_| "源码行号 begin 不能补领较新正文权限")??;
        let Some(permit) = permit else {
            return self.pending("original_permit_unavailable");
        };
        let Some((viewport, generation)) =
            owner.auxiliary_step(&view, &permit, || ruler.source_viewport(&view))?
        else {
            return self.pending("source_viewport_unavailable"); // Preview/未挂窗/空交集都不签已画好的结果。
        };
        let bounds = owner.auxiliary_step(&view, &permit, || ruler.bounds())?;
        let Some(window) = owner.auxiliary_step(&view, &permit, || ruler.window())? else {
            return self.pending("window_unavailable");
        };
        if !owner.auxiliary_step(&view, &permit, || window.isVisible())? {
            return self.pending("window_not_visible");
        }
        let scale = owner.auxiliary_step(&view, &permit, || window.backingScaleFactor())?;
        let layer = owner
            .auxiliary_step(&view, &permit, || {
                self.installed_layer.as_ref().and_then(Weak::load)
            })?
            .ok_or("源码行号 backing 已释放")?;
        let authority_view = owner.auxiliary_step(&view, &permit, || view.clone())?;
        let auth = Rc::new(PaintAuthority {
            lease: Rc::clone(self),
            owner,
            permit,
            view: authority_view,
            capture,
            lines,
            proof,
            attributes,
            bounds,
            viewport,
            generation,
            window,
            layer,
            scale,
            draw_completed: Cell::new(false),
            drawn_labels: Cell::new(0),
            published_image: Cell::new(None),
        });
        auth.validate_geometry(ruler)?;
        let bitmap = auth.step(ruler, || self.resources.bitmap(bounds, scale))??;
        let context = bitmap.context();
        auth.step(ruler, || {
            CGContext::concat_ctm(
                Some(context),
                CGAffineTransform {
                    a: scale,
                    b: 0.0,
                    c: 0.0,
                    d: -scale,
                    tx: -scale * bounds.origin.x,
                    ty: scale * (bounds.origin.y + bounds.size.height),
                },
            )
        })?;
        auth.step(ruler, || CGContext::clip_to_rect(Some(context), bounds))?;
        *self.active.borrow_mut() = Some(Rc::clone(&auth));
        // 临时 NSGraphicsContext 必须在 finish 前真实归还 CG context；原 provider 要求冻结后无写者。
        autoreleasepool(|_| -> Result<(), String> {
            let raster = auth.step(ruler, || {
                NSGraphicsContext::graphicsContextWithCGContext_flipped(context, true)
            })?;
            let restore = auth.step(ruler, || {
                GraphicsRestore(NSGraphicsContext::currentContext())
            })?;
            auth.step(ruler, || {
                NSGraphicsContext::setCurrentContext(Some(&raster))
            })?;
            auth.step(ruler, || ruler.draw_native_for_raster(bounds))?;
            drop(restore); // 先恢复线程原槽，再归还临时 CPU context。
            drop(raster);
            auth.validate_geometry(ruler)
        })?;
        let active = self.active.borrow_mut().take();
        drop(active); // super 的所有同步回调已经结束，不让 active 槽延长 Native claim。
        if !auth.draw_completed.get() {
            return self.pending("native_hash_marks_incomplete"); // 原绘制提前退出时不发布缺少标签的图像。
        }
        if !auth.step(ruler, || {
            ruler.cached_geometry_is_current(
                &auth.view,
                &auth.capture,
                &auth.proof,
                "raster_draw_end",
            )
        })? {
            return self.pending("visible_geometry_changed_during_draw");
        }
        let image = auth.step(ruler, || bitmap.finish())??;
        publish(&auth, ruler, &image)?;
        drop(image);
        auth.validate_geometry(ruler)?;
        if !auth.step(ruler, || {
            ruler.cached_geometry_is_current(
                &auth.view,
                &auth.capture,
                &auth.proof,
                "raster_publish_end",
            )
        })? {
            return self.pending("visible_geometry_changed_during_publish");
        }
        let emitted = self.paint_logs.get();
        if emitted < 8 {
            self.paint_logs.set(emitted + 1);
            native_markdown_trace! {
                // 只复用已验证的绘制与发布结果；这不是截图、命中或整产品功能通过收据。
                eprintln!("native_raster_surface event=gutter_published ordinal={} drawn_labels={} cached_lines={} bounds={:?} scale={} formal=false", emitted + 1, auth.drawn_labels.get(), auth.lines.len(), auth.bounds, auth.scale);
            }
        }
        Ok(true)
    }

    pub(super) fn report_failure(&self, error: &str) {
        self.resources.fail();
        eprintln!("native_raster_surface event=gutter_raster_failed error={error:?} formal=false");
    }

    pub(super) fn restore(&self) -> Result<(), String> {
        if self.finished.replace(true) {
            return Ok(()); // 幂等仅约束本同步清理；真实 nil/像素收据仍由原门禁判断。
        }
        let result = self.restore_once();
        if result.is_err() {
            self.resources.fail();
        }
        result
    }

    fn restore_once(&self) -> Result<(), String> {
        if self.active.borrow().is_some() || self.painting.get() {
            return Err("源码行号仍有同步绘制，不能提前恢复 backing".into());
        }
        let ruler = self.ruler.load().ok_or("恢复源码行号时原 ruler 已释放")?;
        let view = self.view.load().ok_or("恢复源码行号时原正文已释放")?;
        let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        let current = || self.resources.is_open() || view.ivars().textkit_graph_teardown.get();
        let original = match self.previous_layer.as_ref() {
            Some(layer) => Some(
                construction_step(&current, || layer.load())?
                    .ok_or("原 ruler 层已释放，不能伪造恢复")?,
            ),
            None => None,
        };
        let installed = match self.installed_layer.as_ref() {
            Some(layer) => Some(
                construction_step(&current, || layer.load())?
                    .ok_or("本次 ruler backing 已失去原身份")?,
            ),
            None => None, // 尚未取得AppKit层的构造失败没有可以冒认的已安装身份。
        };
        let actual = construction_step(&current, || ruler.layer())?;
        // 构造尚未设置层就失败时，actual 可能仍是精确原层；只接受这两个预先固定的身份。
        if !same_layer(actual.as_deref(), installed.as_deref())
            && !same_layer(actual.as_deref(), original.as_deref())
        {
            return Err("源码行号 backing 已被替换，不覆盖后继 Native 关系".into());
        }
        if let Some(layer) = actual.as_deref().filter(|_| installed.is_some()) {
            // SAFETY: 只恢复本次原层的 contents；不会修改图像/provider 或让仍存活的不可变值变空。
            construction_step(&current, || unsafe {
                layer.setContents(self.previous_contents.as_deref())
            })?;
            if let Some(format) = self.previous_format.as_ref() {
                construction_step(&current, || layer.setContentsFormat(format))?;
            }
            if let Some(gravity) = self.previous_gravity.as_ref() {
                construction_step(&current, || layer.setContentsGravity(gravity))?;
            }
            if let Some(scale) = self.previous_scale {
                construction_step(&current, || layer.setContentsScale(scale))?;
            }
            // SAFETY: 只比较原 contents 身份；任意原 CF 值都不接收额外 NSObject 消息。
            let contents = construction_step(&current, || unsafe { layer.contents() })?;
            if !same_contents(contents.as_deref(), self.previous_contents.as_deref()) {
                return Err("源码行号原 contents 身份未精确恢复".into());
            }
        }
        construction_step(&current, || {
            ruler.setCanDrawSubviewsIntoLayer(self.previous_flatten_subviews)
        })?;
        construction_step(&current, || ruler.setWantsLayer(self.previous_wants_layer))?;
        construction_step(&current, || ruler.setLayer(original.as_deref()))?;
        construction_step(&current, || {
            ruler.setLayerContentsRedrawPolicy(self.previous_redraw)
        })?;
        if construction_step(&current, || ruler.wantsLayer())? != self.previous_wants_layer
            || !same_layer(
                construction_step(&current, || ruler.layer())?.as_deref(),
                original.as_deref(),
            )
            || construction_step(&current, || ruler.layerContentsRedrawPolicy())?
                != self.previous_redraw
            || construction_step(&current, || ruler.canDrawSubviewsIntoLayer())?
                != self.previous_flatten_subviews
        {
            return Err("源码行号原 backing 关系未精确恢复".into());
        }
        Ok(())
    }
}

impl Drop for GutterRaster {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            self.report_failure(&error);
        }
    }
}

impl PaintAuthority {
    fn metadata_current(&self, ruler: &NativeMarkdownGutter) -> bool {
        !self.lease.finished.get()
            && self.lease.resources.is_open()
            && self.capture.current()
            && ruler.proof_is_current(&self.proof)
            && ruler
                .ivars()
                .raster
                .borrow()
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, &self.lease))
            && *ruler.ivars().lines.borrow() == self.lines
            && ruler
                .ivars()
                .attributes
                .borrow()
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, &self.attributes))
    }

    /// 先后均核原纯票；当前函数不直接读取 Native，因而不会递归进入几何校验。
    fn primitive<T>(
        &self,
        ruler: &NativeMarkdownGutter,
        work: impl FnOnce() -> T,
    ) -> Result<T, String> {
        if !self.metadata_current(ruler) {
            return Err("源码行号原绘制缓存或权限已改变".into());
        }
        let result = self.owner.auxiliary_step(&self.view, &self.permit, work)?;
        if !self.metadata_current(ruler) {
            return Err("源码行号 Native 返回后原缓存已改变".into());
        }
        Ok(result)
    }

    fn validate_geometry(&self, ruler: &NativeMarkdownGutter) -> Result<(), String> {
        if self.primitive(ruler, || ruler.bounds())? != self.bounds {
            return Err("源码行号真实 bounds 在绘制中改变".into());
        }
        let window = self
            .primitive(ruler, || ruler.window())?
            .ok_or("源码行号已离开原窗口")?;
        if !std::ptr::eq::<NSWindow>(&*window, &*self.window)
            || self.primitive(ruler, || window.backingScaleFactor())? != self.scale
        {
            return Err("源码行号窗口或 backing scale 已改变".into());
        }
        let layer = self
            .primitive(ruler, || ruler.layer())?
            .ok_or("源码行号实际 backing 缺失")?;
        if !std::ptr::eq::<CALayer>(&*layer, &*self.layer) {
            return Err("源码行号实际 backing 身份已改变".into());
        }
        if !self.primitive(ruler, || {
            self.view.gutter_generation_is_current(self.generation)
        })? {
            return Err("源码行号不再属于原 Source 代次".into());
        }
        // viewport 的原位置来自原 source_viewport；真实 frame/bounds/backing 事件撤同一原票。
        // 这里不递归重调 source_viewport，原绘制函数仍逐行核同一原 viewport。
        if self.viewport.size.width <= 0.0 || self.viewport.size.height <= 0.0 {
            return Err("源码行号没有真实的原 Source 视口".into());
        }
        if let Some(expected) = self.published_image.get() {
            // SAFETY: 发布后的每个原语仍检查当前层持有同一 CF 图像；不对 CF 值调用 NSObject 方法。
            let image = self.primitive(ruler, || unsafe { self.layer.contents() })?;
            if image.as_deref().is_none_or(|image| {
                std::ptr::from_ref(image).cast::<CGImage>() != expected.as_ptr()
            }) {
                return Err("源码行号已发布图像在后继 Native 原语中被替换".into());
            }
        }
        Ok(())
    }

    fn step<T>(&self, ruler: &NativeMarkdownGutter, work: impl FnOnce() -> T) -> Result<T, String> {
        self.validate_geometry(ruler)?;
        let result = self.primitive(ruler, work)?;
        self.validate_geometry(ruler)?;
        Ok(result)
    }
}

fn publish(
    auth: &PaintAuthority,
    ruler: &NativeMarkdownGutter,
    image: &CGImage,
) -> Result<(), String> {
    let transaction = auth.step(ruler, || {
        CATransaction::begin();
        Transaction // step 后核验失败也会析构返回值，配平已发生的 begin。
    })?;
    auth.step(ruler, || CATransaction::setDisableActions(true))?;
    // SAFETY: 常量均为 QuartzCore 的公开进程期不可变字符串，图层仍是本次原票固定的真实层。
    auth.step(ruler, || unsafe {
        auth.layer.setContentsFormat(kCAContentsFormatRGBA8Uint)
    })?;
    // SAFETY: 同上；resize 只把已按实际 scale 绘制的完整 bounds 图像交给原层。
    auth.step(ruler, || unsafe {
        auth.layer.setContentsGravity(kCAGravityResize)
    })?;
    auth.step(ruler, || auth.layer.setContentsScale(auth.scale))?;
    // SAFETY: CALayer.contents 接收真实 CF CGImage；只桥接同一对象身份，不向 CF 对象发 NSObject 消息。
    auth.step(ruler, || unsafe {
        let object = &*std::ptr::from_ref(image).cast::<AnyObject>();
        auth.layer.setContents(Some(object));
        // 仅保存身份标量；同一次 setter 返回后的检查已要求实际 contents 为该对象。
        auth.published_image.set(Some(NonNull::from(image)));
    })?;
    verify_image(auth, ruler, image)?;
    drop(transaction); // commit 后还要核实际 contents，不能只以 setter 返回视为成功发布。
    verify_image(auth, ruler, image)
}

fn verify_image(
    auth: &PaintAuthority,
    ruler: &NativeMarkdownGutter,
    image: &CGImage,
) -> Result<(), String> {
    // SAFETY: 只取得实际原层公开 contents 并比较 CF 身份，不假设任何 NSObject 子类或文本内容。
    let actual = auth.step(ruler, || unsafe { auth.layer.contents() })?;
    if actual.as_deref().is_none_or(|actual| {
        std::ptr::from_ref(actual).cast::<CGImage>() != std::ptr::from_ref(image)
    }) {
        return Err("源码行号层 contents 没有接收同一实际 CGImage".into());
    }
    Ok(())
}

fn same_contents(actual: Option<&AnyObject>, expected: Option<&AnyObject>) -> bool {
    match (actual, expected) {
        (Some(actual), Some(expected)) => std::ptr::eq(actual, expected),
        (None, None) => true,
        _ => false,
    }
}

fn same_layer(actual: Option<&CALayer>, expected: Option<&CALayer>) -> bool {
    match (actual, expected) {
        (Some(actual), Some(expected)) => std::ptr::eq(actual, expected),
        (None, None) => true,
        _ => false, // 精确 nil 与一个仍存活的不同层不是同一原关系。
    }
}

fn construction_step<T>(
    current: &impl Fn() -> bool,
    work: impl FnOnce() -> T,
) -> Result<T, String> {
    if !current() {
        return Err("源码行号 backing 操作前原阶段已撤销".into());
    }
    let value = work();
    if !current() {
        return Err("源码行号 backing 操作后原阶段已撤销".into());
    }
    Ok(value)
}
