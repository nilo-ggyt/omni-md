//! The reference ruler still owns backgrounds, labels, and fold markers; this explicit raster path replaces only its bounded drawing target and backing publication.

use super::*;
use crate::native_surface::raster_surface::{
    NativeMarkdownRasterAuxiliaryResources, NativeMarkdownRasterOriginPermit,
    NativeMarkdownRasterSurface,
};
use objc2_app_kit::{NSViewLayerContentsRedrawPolicy, NSWindow};
use objc2_core_foundation::CGAffineTransform;
use objc2_core_graphics::{CGContext, CGImage};
use objc2_quartz_core::{CALayer, CATransaction, kCAContentsFormatRGBA8Uint, kCAGravityResize};

/// Installs only with the same document's explicitly enabled Raster; retains no document text/image or independent resource counter.
/// Retains original contents only to restore the existing Native relationship; an originally nil value creates no placeholder image.
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

/// Exists on one synchronous updateLayer stack; all Native claims return before that call ends.
/// The original OriginPermit covers document, font, mode, selection, and geometry events without acquiring newer authority.
struct PaintAuthority {
    lease: Rc<GutterRaster>,
    owner: Rc<NativeMarkdownRasterSurface>,
    permit: NativeMarkdownRasterOriginPermit,
    view: Retained<PieceBackedMarkdownTextView>,
    capture: NativeMarkdownSelectionCapture,
    lines: Vec<GutterLine>,
    // Fixes the actual document viewport/fragment batch from refresh, so old same-generation labels cannot republish under a new paint intent.
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

/// The graphics slot is thread-local Native state; failure/revocation still restores its original value instead of leaving a CPU context for the window.
struct GraphicsRestore(Option<Retained<NSGraphicsContext>>);
impl Drop for GraphicsRestore {
    fn drop(&mut self) {
        NSGraphicsContext::setCurrentContext(self.0.as_deref());
    }
}

/// Lends active authority only to synchronous original drawHashMarks callbacks; takes the slot before releasing Native temporaries.
struct ActivePaint<'a>(&'a GutterRaster);
impl Drop for ActivePaint<'_> {
    fn drop(&mut self) {
        let previous = self.0.active.borrow_mut().take();
        drop(previous);
        self.0.painting.set(false);
    }
}

/// Balances begin/commit even after revocation; cleanup itself certifies no successful publication.
struct Transaction;
impl Drop for Transaction {
    fn drop(&mut self) {
        CATransaction::commit();
    }
}

impl GutterRaster {
    /// Captures original public backing settings before installing this reference-only CPU target;
    /// partial construction remains owned by the local lease and uses the same restoration path.
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
        // Construction has no live session selection intent; checks only that the original construction still owns the same view/Raster.
        let previous_wants_layer = construction_step(&current, || ruler.wantsLayer())?;
        let previous_redraw = construction_step(&current, || ruler.layerContentsRedrawPolicy())?;
        let previous_layer = construction_step(&current, || ruler.layer())?;
        let previous_weak = match previous_layer.as_deref() {
            Some(layer) => Some(construction_step(&current, || Weak::from(layer))?),
            None => None, // The originally nil relationship must ultimately return to nil.
        };
        let previous_contents = match previous_layer.as_deref() {
            // SAFETY: Retains this main-thread CALayer's public contents solely for relationship restoration, without interpreting its type.
            Some(layer) => construction_step(&current, || unsafe { layer.contents() })?,
            None => None,
        };
        let previous_format = match previous_layer.as_ref() {
            Some(layer) => Some(construction_step(&current, || layer.contentsFormat())?),
            None => None, // An originally absent layer has no settings to restore; snapshotting must not create another CALayer.
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
        // Lets wantsLayer alone create actual AppKit layer backing. Calling setLayer first would create a
        // layer-hosting view without AppKit updateLayer delivery, which caused the refreshed 74-point ruler to remain blank.
        // Installs no custom layer here; closure restores only the actual backing identities captured below.
        construction_step(&current, || ruler.setWantsLayer(true))?;
        let layer = construction_step(&current, || ruler.layer())?
            .ok_or("源码行号未取得AppKit实际backing层")?;
        lease.installed_layer = Some(construction_step(&current, || Weak::from_retained(&layer))?);
        if !same_layer(Some(&layer), previous_layer.as_deref()) {
            construction_step(&current, || {
                ledger.include("editor.raster.gutter-layer", &layer)
            })??;
        }
        // A true flag would bypass wantsUpdateLayer and build a drawRect display list; this separate ruler
        // keeps its CPU contents entry instead of routing window drawing back through accelerated Core Graphics.
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
            // Shares the document's original geometry epoch, revoking authority without another scheduler or geometry owner.
            self.resources.invalidate_geometry();
        }
    }

    pub(super) fn active(&self) -> bool {
        self.active.borrow().is_some()
    }

    /// The original drawing loop guards each Native primitive here; absent synchronous CPU authority, no window-drawing bypass is permitted.
    pub(super) fn drawing_step<T>(
        &self,
        ruler: &NativeMarkdownGutter,
        work: impl FnOnce() -> T,
    ) -> Option<T> {
        let auth = self.active.borrow().clone();
        let auth = auth?; // Accepts only the current original super drawRect callback, without creating authority.
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
            // Records only complete return of the original loop; actual label/clip facts are separate and this alone proves no correct visible coverage.
            auth.draw_completed.set(true);
        }
    }

    /// Counts only this intent's actually returned drawAtPoint calls, never substituting cache length for painted labels.
    pub(super) fn record_drawn_label(&self) {
        if let Some(auth) = self.active.borrow().as_ref() {
            auth.drawn_labels.set(auth.drawn_labels.get() + 1);
        } // Without the original synchronous CPU stack, records neither successor drawing nor a window bypass.
    }

    fn pending(&self, reason: &'static str) -> Result<bool, String> {
        let count = self.pending_logs.get();
        if count < 16 {
            self.pending_logs.set(count + 1);
            native_markdown_trace! { eprintln!("native_raster_surface event=gutter_pending reason={reason} formal=false"); }
        }
        Ok(false) // Reports no drawing/publication for this call; grants no completion receipt and schedules no timer or retry.
    }

    /// Paints one original geometry proof into a private bounded bitmap and publishes only after
    /// the actual draw completes; Pending grants no coverage and schedules no replacement work.
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
            return self.pending("closed"); // A late shell callback after acceptance cannot restore business resources.
        }
        if self.painting.replace(true) {
            return Err("源码行号光栅绘制发生同步重入".into());
        }
        let _active = ActivePaint(self);
        let _operation = self.resources.operation()?;
        // Fixes original pure Rust selection/owner authority and bounded cache before the first Native Weak load, without renewal from getters.
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
            return self.pending("visible_line_cache_empty"); // Without original logical lines, publishes no blank image missing its labels.
        }
        if lines.len() > MAX_GUTTER_LINES {
            return Err("源码行号缓存超出原有限上限".into());
        }
        let view = selection_native_step(&capture, || self.view.load())
            .map_err(|_| "源码行号原票在取得 view 时失效")?
            .ok_or("源码行号原 view 已释放")?;
        let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        if !ruler.cached_geometry_is_current(&view, &capture, &proof, "raster_admission") {
            return self.pending("visible_geometry_proof_stale"); // Rejects only this publication, without clearing the old image or claiming a completed refresh.
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
            return self.pending("source_viewport_unavailable"); // Preview, an unmounted window, or an empty intersection certifies no completed drawing.
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
        // Temporary NSGraphicsContext claims must return the CG context before finish; the original provider requires no writer after freezing.
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
            drop(restore); // Restores the thread's original slot before returning the temporary CPU context.
            drop(raster);
            auth.validate_geometry(ruler)
        })?;
        let active = self.active.borrow_mut().take();
        drop(active); // All synchronous superclass callbacks have ended; the active slot must not extend Native claims.
        if !auth.draw_completed.get() {
            return self.pending("native_hash_marks_incomplete"); // An early original-draw exit cannot publish an image missing labels.
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
                // Reports only already-validated drawing/publication facts, without claiming screenshot, hit-testing, or whole-product acceptance.
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
            return Ok(()); // Idempotence covers this synchronous cleanup only; the original boundary still verifies actual nil/pixel receipts.
        }
        let result = self.restore_once();
        if result.is_err() {
            self.resources.fail();
        }
        result
    }

    /// Restores only captured layer/view relationships. An active draw or externally replaced
    /// relationship remains failure rather than permission to overwrite another owner's graph.
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
            None => None, // Construction failing before acquiring an AppKit layer has no installed identity to claim.
        };
        let actual = construction_step(&current, || ruler.layer())?;
        // If construction failed before changing backing, actual may still be the exact original layer; accepts only these two captured identities.
        if !same_layer(actual.as_deref(), installed.as_deref())
            && !same_layer(actual.as_deref(), original.as_deref())
        {
            return Err("源码行号 backing 已被替换，不覆盖后继 Native 关系".into());
        }
        if let Some(layer) = actual.as_deref().filter(|_| installed.is_some()) {
            // SAFETY: Restores only this original layer's contents, without mutating the image/provider or emptying a live immutable value.
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
            // SAFETY: Compares original contents identity only; arbitrary CF values receive no extra NSObject messages.
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

    /// Checks original pure authority before and after work; this wrapper performs no independent Native geometry read, avoiding recursive validation.
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
        // Original viewport position comes from source_viewport; actual frame/bounds/backing events revoke that same intent.
        // Does not recursively call source_viewport; the original drawing loop still checks the same viewport per line.
        if self.viewport.size.width <= 0.0 || self.viewport.size.height <= 0.0 {
            return Err("源码行号没有真实的原 Source 视口".into());
        }
        if let Some(expected) = self.published_image.get() {
            // SAFETY: Post-publication primitives verify the layer still holds the same CF image, without NSObject methods on CF values.
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
        Transaction // The temporary is dropped even if step's post-check fails, balancing the actual begin.
    })?;
    auth.step(ruler, || CATransaction::setDisableActions(true))?;
    // SAFETY: QuartzCore constants are public immutable process-lifetime strings; the layer remains this intent's actual captured layer.
    auth.step(ruler, || unsafe {
        auth.layer.setContentsFormat(kCAContentsFormatRGBA8Uint)
    })?;
    // SAFETY: The same constant lifetime applies; resize passes the complete actual-scale bounds image to the original layer.
    auth.step(ruler, || unsafe {
        auth.layer.setContentsGravity(kCAGravityResize)
    })?;
    auth.step(ruler, || auth.layer.setContentsScale(auth.scale))?;
    // SAFETY: CALayer.contents receives an actual CF CGImage; bridges its identity without NSObject messages to CF.
    auth.step(ruler, || unsafe {
        let object = &*std::ptr::from_ref(image).cast::<AnyObject>();
        auth.layer.setContents(Some(object));
        // Stores identity only; the same setter's post-check requires actual contents to remain this object.
        auth.published_image.set(Some(NonNull::from(image)));
    })?;
    verify_image(auth, ruler, image)?;
    drop(transaction); // Rechecks actual contents after commit; setter return alone is not successful publication.
    verify_image(auth, ruler, image)
}

fn verify_image(
    auth: &PaintAuthority,
    ruler: &NativeMarkdownGutter,
    image: &CGImage,
) -> Result<(), String> {
    // SAFETY: Reads only the actual original layer's public contents and compares CF identity, assuming no NSObject subclass or text payload.
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
        _ => false, // Exact nil and a different live layer are not the same original relationship.
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
