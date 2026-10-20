//! 同一Ready scene只画入有限RGBA8像素；原窗口上下文不承接字形显示列表。

use super::*;
use crate::native_surface::viewport_pixels::{
    NativeMarkdownViewportPixels, PixelTile, TileFrame, TiledBitmap,
};
use objc2_core_foundation::CGAffineTransform;
use objc2_core_graphics::CGContext;
use objc2_quartz_core::{CALayer, CATransaction};

mod calibration;
mod candidate;
mod install;
mod layers;
mod scheduler;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) use install::probe::run as probe_installation_rollback;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) use layers::probe::run as probe_mask_slots;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) use scheduler::probe::nested_delivery as probe_nested_delivery;
use std::sync::Arc;

/// 只拥有本新视图经AppKit创建的真实backing和固定像素收据，没有正文、scene或view强回指。
pub(super) struct CpuSurface {
    layer: Retained<CALayer>,
    layers: layers::LayerPool,
    pixels: Arc<NativeMarkdownViewportPixels>,
    paints: Cell<usize>,
    painting: Cell<bool>,
    // 同步 Native 重入只合并一次重绘请求；不把较新 Scene/Input 或像素留在旧绘图栈中。
    repaint_requested: Cell<bool>,
    published: RefCell<Option<Rc<Published>>>,
    reuses: Cell<usize>,
    candidate: RefCell<Option<candidate::Candidate>>,
    active_serial: Cell<Option<u64>>,
    next_serial: Cell<u64>,
    scheduled: Cell<Option<u64>>,
    deferred: Cell<Option<u64>>,
    steps: Cell<usize>,
}

/// 单个已显示帧的弱身份和有限Native证明；不保Scene、Input、文件或正文载荷。
struct Published {
    key: NativeMarkdownPagedSceneKey,
    geometry: Geometry,
    publication: layers::Publication,
    frame: RefCell<Option<Rc<TileFrame>>>,
}
#[derive(Clone, Copy, PartialEq)]
struct Geometry {
    bounds: NSRect,
    backing: NSRect,
    scale: f64,
    width: usize,
    height: usize,
}

/// Native可重入，但同一surface只接受一个原始绘图栈；嵌套不能领取后继计数或覆盖候选。
struct PaintUse<'a>(&'a Cell<bool>);
impl<'a> PaintUse<'a> {
    fn enter(slot: &'a Cell<bool>) -> Result<Self> {
        if slot.get() {
            return Err(NativeMarkdownPagedTextViewError::Pending);
        }
        slot.set(true);
        Ok(Self(slot))
    }
}
impl Drop for PaintUse<'_> {
    fn drop(&mut self) {
        self.0.set(false); // 只归还本同步栈的准入，不排事件、不重采scene或重试较新状态。
    }
}

/// 失败出口也结束当前原事务；不因撤权继续发布contents。
struct Transaction;
impl Drop for Transaction {
    fn drop(&mut self) {
        CATransaction::commit();
    }
}

/// 唯一mount setter之后的失败不能假称未提交；只撤原attempt数据，并保持调用者原首错。
struct PublicationUse<'a> {
    surface: &'a CpuSurface,
    attempt: layers::Attempt,
    armed: bool,
}
impl<'a> PublicationUse<'a> {
    fn new(surface: &'a CpuSurface) -> Result<Self> {
        Ok(Self {
            surface,
            attempt: surface.layers.attempt()?,
            armed: true,
        })
    }
    fn commit(&mut self) {
        self.armed = false;
    }
}
impl Drop for PublicationUse<'_> {
    fn drop(&mut self) {
        if self.armed
            && self
                .surface
                .layers
                .revoke_attempt(&self.attempt, &self.surface.layer)
        {
            let metadata = self.surface.published.borrow_mut().take();
            drop(metadata); // 已失权原画面不能继续签Ready；不清其他serial的后继关系。
        }
    }
}

impl CpuSurface {
    pub(super) fn install(view: &NativeMarkdownPagedTextView) -> Result<Rc<Self>> {
        let origin = view.origin()?;
        if origin.step(view, || view.wantsLayer())? || origin.step(view, || view.layer())?.is_some()
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry); // 新视图必须保持原nil关系，不能夺取外部层。
        }
        // 先仅启用标准layer-backed路径，由AppKit建立和管理updateLayer的公开调度关系。
        // 先setLayer再wantsLayer会成为layer-hosting，不能靠直接调用updateLayer伪造实际绘图。
        let mut installation = install::Installation::new(view);
        let layer = installation.enable(&origin)?;
        let layers = layers::LayerPool::new(&|| origin.current(view))?;
        installation.mount(layers.mount_claim());
        origin.step(view, || layer.addSublayer(layers.mount()))?;
        let owner = Rc::new(Self {
            layer,
            layers,
            pixels: Arc::default(),
            paints: Cell::new(0),
            painting: Cell::new(false),
            repaint_requested: Cell::new(false),
            published: RefCell::new(None),
            reuses: Cell::new(0),
            candidate: RefCell::new(None),
            active_serial: Cell::new(None),
            next_serial: Cell::new(0),
            scheduled: Cell::new(None),
            deferred: Cell::new(None),
            steps: Cell::new(0),
        });
        origin.step(view, || {
            view.setLayerContentsRedrawPolicy(
                objc2_app_kit::NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay,
            )
        })?;
        owner.layer_current(view, &|| origin.current(view))?;
        installation.commit(); // 之后到唯一cpu槽发布之间没有Native回调；成功owner接管全部清理责任。
        Ok(owner)
    }

    pub(super) fn observer(&self) -> NativeMarkdownViewportPixelObserver {
        self.pixels.observer()
    }
    pub(super) fn track_close(&self, receipt: &NativeMarkdownPagedCloseReceipt) {
        receipt.track_pixels(&self.pixels); // 封闭原分配；异步provider仍归这份实际像素收据，不保surface。
    }
    pub(super) fn layer_weaks(&self) -> Vec<objc2::rc::Weak<AnyObject>> {
        self.layers.weaks()
    }
    pub(super) fn paints(&self) -> usize {
        self.paints.get()
    }

    /// 正式Source验收只借实际已发布key和原paint计数；不克隆像素、层或候选帧。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn published_frame(&self) -> Option<(NativeMarkdownPagedSceneKey, usize)> {
        self.published
            .borrow()
            .as_ref()
            .map(|published| (published.key.clone(), self.paints.get()))
    }

    pub(super) fn reuses(&self) -> usize {
        self.reuses.get()
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn reused_images(&self) -> Option<usize> {
        let published = self.published.borrow().clone();
        published.as_ref().and_then(|published| {
            published
                .frame
                .borrow()
                .as_ref()
                .map(|frame| frame.reused_images)
        })
    }

    fn layer_current(
        &self,
        view: &NativeMarkdownPagedTextView,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        let actual = checked(current, || view.layer())?;
        let matches = actual
            .as_deref()
            .is_some_and(|actual| std::ptr::eq::<CALayer>(actual, &*self.layer));
        drop(actual); // Native临时claim的析构也在后置纯票核之前完成。
        if !matches || !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }

    pub(super) fn paint(
        self: &Rc<Self>,
        view: &NativeMarkdownPagedTextView,
        scene: &SceneUse,
    ) -> Result<()> {
        let painting = match PaintUse::enter(&self.painting) {
            Ok(painting) => painting,
            Err(NativeMarkdownPagedTextViewError::Pending) => {
                // updateLayer 可在 CoreText/AppKit 的同步回调里重入。这里只拒绝第二个写入者，
                // 原候选仍由外层独占；健康的显示请求不应成为永久输入失败。
                let current = || scene.current(view);
                let result = (|| {
                    scene.scene.validate_ready(&current)?;
                    let layer = self.layer_current(view, &current);
                    scene.scene.validate_ready(&current)?;
                    layer
                })();
                #[cfg(feature = "native-close-stage-diagnostic")]
                self.trace_paint_failure("reentrant", view, scene, &result);
                result?; // 真实 Source、字体和 Native 所有权错误仍原样失败，不能被合并请求掩盖。
                self.repaint_requested.set(true);
                return Ok(());
            }
            Err(cause) => return Err(cause),
        };
        let result = self.paint_inner(view, scene);
        #[cfg(feature = "native-close-stage-diagnostic")]
        self.trace_paint_failure("paint_inner", view, scene, &result);
        drop(painting);
        let wake = self.wake_deferred(view); // 若真实嵌套主队列让出了延续，只在原绘图栈退出后重排同serial。
        #[cfg(feature = "native-close-stage-diagnostic")]
        self.trace_paint_failure("wake_deferred", view, scene, &wake);
        match result {
            Err(error) => Err(error),
            Ok(()) => wake,
        } // 原绘图/Source首错不能被后继调度拒绝替换。
    }
    #[cfg(feature = "native-close-stage-diagnostic")]
    fn trace_paint_failure(
        &self,
        stage: &str,
        view: &NativeMarkdownPagedTextView,
        scene: &SceneUse,
        result: &Result<()>,
    ) {
        if let Err(cause) = result {
            eprintln!(
                "native_cpu_failure stage={stage} cause={:?} original={:?} actual={:?} original_geometry={} actual_geometry={} origin_current={} scene_current={} depth={} active_serial={:?}",
                std::mem::discriminant(cause),
                scene.origin.state,
                scene.origin.input.state(),
                scene.origin.geometry_epoch,
                view.ivars().geometry_epoch.get(),
                scene.origin.current(view),
                scene.current(view),
                view.ivars().depth.get(),
                self.active_serial.get(),
            );
        }
    }
    fn paint_inner(
        self: &Rc<Self>,
        view: &NativeMarkdownPagedTextView,
        scene: &SceneUse,
    ) -> Result<()> {
        let current = || scene.current(view);
        scene.scene.validate_ready(&current)?; // 原Source/Font/输出健康先于缓存命中；同nonce不能掩盖EIO。
        let geometry = self.geometry(view, scene);
        scene.scene.validate_ready(&current)?;
        let geometry = geometry?;
        let prior = self.published.borrow().clone(); // Rc只借固定metadata，结束槽借用后才进入Native/current。
        if let Some(prior) = prior
            && prior.key.same(&scene.key)
            && prior.geometry == geometry
        {
            let native = self
                .layers
                .validate(&self.layer, &prior.publication, &current);
            scene.scene.validate_ready(&current)?; // Native检查失权时仍让真实Source首错优先，不改读后继来源。
            native?;
            let same_geometry = self.geometry(view, scene);
            scene.scene.validate_ready(&current)?;
            let same_geometry = same_geometry? == geometry;
            let native = self
                .layers
                .validate(&self.layer, &prior.publication, &current);
            scene.scene.validate_ready(&current)?;
            native?;
            if !same_geometry || !current() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let count = self
                .reuses
                .get()
                .checked_add(1)
                .ok_or(NativeMarkdownPagedTextViewError::CounterExhausted)?;
            self.cancel_candidate(); // 即使producer返回旧已显示nonce，也先消费任何不相干的未发布候选。
            scene.scene.validate_ready(&current)?;
            self.reuses.set(count);
            return Ok(()); // 已发布的同一完整帧仍健康，不再分配第三帧，也不重复向CTFont提交glyph。
        }
        let existing = self
            .candidate
            .borrow()
            .as_ref()
            .map(|candidate| candidate.key.same(&scene.key) && candidate.geometry == geometry);
        if existing == Some(true) {
            return Ok(()); // 原候选已有唯一coalesced回调，重复updateLayer不能从首tile重新分配。
        }
        self.cancel_candidate();
        let serial = self
            .next_serial
            .get()
            .checked_add(1)
            .ok_or(NativeMarkdownPagedTextViewError::CounterExhausted)?;
        self.next_serial.set(serial);
        self.active_serial.set(Some(serial));
        let reusable = self
            .published
            .borrow()
            .as_ref()
            .and_then(|published| published.frame.borrow().as_ref().map(Rc::downgrade))
            .unwrap_or_default();
        let candidate =
            candidate::Candidate::new(serial, scene, geometry, Arc::clone(&self.pixels), reusable);
        match candidate {
            Ok(candidate) => self.advance(view, candidate),
            Err(error) => {
                self.cancel_candidate();
                Err(error)
            }
        }
    }

    /// 任何新原票或关闭只取消本surface候选；排队闭包仅含Weak，不保这些资源。
    pub(super) fn cancel_candidate(&self) {
        self.active_serial.set(None);
        self.scheduled.set(None);
        self.deferred.set(None);
        let candidate = self.candidate.borrow_mut().take();
        drop(candidate); // 借用先归还，再真实drop会话/context和已封存的有限tiles。
    }

    /// 单格准入后归还真实绘图读取；同步paint仍在栈上时拒绝，不以取消标志冒充其析构。
    pub(super) fn release_source_readers(&self) -> Result<()> {
        if self.painting.get() {
            return Err(NativeMarkdownPagedTextViewError::Busy);
        }
        self.cancel_candidate();
        Ok(()) // Published只持弱key和像素，可保留原帧，不保活草稿Source。
    }
    pub(super) fn cancel_if_not_key(&self, key: &NativeMarkdownPagedSceneKey) {
        let obsolete = self
            .candidate
            .borrow()
            .as_ref()
            .is_some_and(|candidate| !candidate.key.same(key));
        if obsolete {
            self.cancel_candidate();
        }
    }
    pub(super) fn pending(&self) -> bool {
        self.active_serial.get().is_some()
    }
    pub(super) fn steps(&self) -> usize {
        self.steps.get()
    }

    /// 只短借当前活帧；普通旧scene/几何过期是cache miss，真实Native所有权错误仍原样拒绝。
    fn reusable_frame(
        &self,
        view: &NativeMarkdownPagedTextView,
        expected: &std::rc::Weak<TileFrame>,
        geometry: Geometry,
        current: &impl Fn() -> bool,
    ) -> Result<Option<Rc<TileFrame>>> {
        let Some(frame) = expected.upgrade() else {
            return Ok(None); // 旧帧已经按原生命周期归还，不能升级历史缓存或重建旧图像。
        };
        let published = self.published.borrow().clone();
        let Some(published) = published else {
            return Ok(None);
        };
        let same_frame = published
            .frame
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, &frame));
        if !same_frame || published.geometry != geometry {
            return Ok(None); // resize/scale/旧帧更换不新增绘图前置失败，当前候选仍可完整重绘。
        }
        self.layer_current(view, current)?;
        self.layers
            .validate(&self.layer, &published.publication, current)?;
        let same = self
            .published
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, &published))
            && published
                .frame
                .borrow()
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, &frame));
        if !same || !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        } // Native/current重入后只能消费同一个原活帧，不给另一个publication补签。
        Ok(Some(frame))
    }

    fn advance(
        self: &Rc<Self>,
        view: &NativeMarkdownPagedTextView,
        candidate: candidate::Candidate,
    ) -> Result<()> {
        let serial = candidate.serial;
        let ticket = candidate.ticket.clone();
        let key = candidate.key.clone();
        let scene = Rc::clone(&candidate.scene);
        // Candidate会移入Pending/Complete或在错误时真正析构；本同步栈仅保原弱票与同一Scene，
        // 不另持Input/Doc，也不把较新Scene补签给旧绘图。错误分类必须发生在serial清理之前。
        let result = (|| {
            let step = self
                .steps
                .get()
                .checked_add(1)
                .ok_or(NativeMarkdownPagedTextViewError::CounterExhausted)?;
            self.steps.set(step); // 溢出也经过下方同一终态cleanup，不能剩active但已无candidate。
            match candidate.advance(self, view)? {
                candidate::Advance::Pending(candidate) => {
                    if self.active_serial.get() != Some(serial) {
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    }
                    let replaced = self.candidate.replace(Some(candidate));
                    drop(replaced);
                    self.schedule(view, serial)?;
                    Ok(()) // 尚未完整frame保持旧像素；不把Pending计为真实publish。
                }
                candidate::Advance::Complete(completed) => {
                    completed.validate(self, view)?;
                    let current = || completed.current(self, view);
                    self.layer_current(view, &current)?;
                    let mut publication_use = PublicationUse::new(self)?;
                    let prior = self.published.borrow().clone();
                    let retired = prior
                        .as_ref()
                        .and_then(|published| published.frame.borrow_mut().take());
                    drop(retired); // 原slot仍保旧可见图片；仅先归还frame cache，旧metadata/Ready到原提交点才换。
                    drop(prior); // 即使别处短持Published，它的帧槽也已空，不拖旧provider到transaction之后。
                    let publication = self.layers.publish(
                        &self.layer,
                        &completed.frame,
                        &publication_use.attempt,
                        completed.geometry.backing,
                        completed.geometry.scale,
                        &current,
                    );
                    completed.validate(self, view)?; // Source首错先于Native撤权；不可给较新scene补签。
                    let publication = publication?;
                    self.layer_current(view, &current)?;
                    completed.validate(self, view)?;
                    let count = self
                        .paints
                        .get()
                        .checked_add(1)
                        .ok_or(NativeMarkdownPagedTextViewError::CounterExhausted)?;
                    let published = Rc::new(Published {
                        key: completed.key.clone(),
                        geometry: completed.geometry,
                        publication,
                        frame: RefCell::new(Some(Rc::clone(&completed.frame))),
                    });
                    let old = self.published.replace(Some(published));
                    drop(old);
                    self.paints.set(count);
                    self.active_serial.set(None);
                    publication_use.commit(); // 已核完所有Source/几何事实；此后只交付提交观察，不再产生可回滚失败。
                    completed.scene.cpu_frame_published(count); // 只通知真实整帧提交事实；此后不新增可回滚Result或重领新scene。
                    Ok(())
                }
            }
        })();
        let failed = result.is_err();
        let result = match result {
            Err(NativeMarkdownPagedTextViewError::Revoked) => (|| {
                let current =
                    || self.active_serial.get() == Some(serial) && ticket.current(view, &key);
                match scene.validate_ready(&current) {
                    Ok(()) | Err(NativeMarkdownPagedTextViewError::Revoked) => {}
                    Err(cause) => return Err(cause),
                } // 沿原validate_ready保Source首错和实际字体/资源错误；不越过current给旧字体补权。
                if ticket.superseded(view)? {
                    Ok(()) // 已接纳的新客户/输入/几何只退休旧候选，不能污染新客户的永久输入失败槽。
                } else {
                    Err(NativeMarkdownPagedTextViewError::Revoked)
                } // 同原完整票的图层/Scene错误仍严格失败；不泛吞scene_current的bool拒绝。
            })(),
            other => other,
        };
        if failed && self.active_serial.get() == Some(serial) {
            self.cancel_candidate(); // 首错终态，不能下一轮从同失败session继续或重放。
        }
        result
    }

    fn geometry(&self, view: &NativeMarkdownPagedTextView, scene: &SceneUse) -> Result<Geometry> {
        self.geometry_for(view, &scene.key, &|| scene.current(view))
    }
    fn geometry_for(
        &self,
        view: &NativeMarkdownPagedTextView,
        key: &NativeMarkdownPagedSceneKey,
        current: &impl Fn() -> bool,
    ) -> Result<Geometry> {
        self.layer_current(view, current)?;
        let bounds = checked(current, || view.bounds())?;
        let backing = checked(current, || self.layer.bounds())?;
        let window =
            checked(current, || view.window())?.ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let scale = checked(current, || window.backingScaleFactor())?;
        if !finite_rect(bounds, true)
            || !finite_rect(backing, true)
            || bounds.size != backing.size
            || bounds.size.width != key.request.bounds_width
            || !scale.is_finite()
            || scale <= 0.0
            || !contains(bounds, key.request.viewport)
            || !finite_edges(bounds)
            || !(-scale * bounds.origin.x).is_finite()
            || !(-scale * bounds.origin.y).is_finite()
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        Ok(Geometry {
            bounds,
            backing,
            scale,
            width: dimension(bounds.size.width, scale)?,
            height: dimension(bounds.size.height, scale)?,
        })
    }

    pub(super) fn detach(&self, view: &NativeMarkdownPagedTextView) -> Result<()> {
        self.repaint_requested.set(false); // 关闭不复活较早的显示请求；标量本身没有异步业务强边。
        self.cancel_candidate(); // 先归还所有Pending session/context，不让真实业务数据躲在主队列任务里。
        let published = self.published.borrow_mut().take();
        drop(published);
        let actual = view.layer();
        let owned = actual
            .as_deref()
            .is_some_and(|layer| std::ptr::eq::<CALayer>(layer, &*self.layer));
        drop(actual);
        CATransaction::begin();
        let transaction = Transaction;
        CATransaction::setDisableActions(true);
        let intact = self.layers.detach(&self.layer); // backing已被替换也必须先清自身图片；外方parent保持原样。
        let delegate = self.layer.delegate();
        let our_delegate = delegate.as_deref().is_some_and(|delegate| {
            std::ptr::from_ref(delegate).cast::<()>() == std::ptr::from_ref(view).cast::<()>()
        });
        drop(delegate);
        if our_delegate {
            self.layer.setDelegate(None);
        } // 只断原AppKit→本view的关系，不清后来外方delegate。
        let actual = view.layer();
        let still_owned = actual
            .as_deref()
            .is_some_and(|layer| std::ptr::eq::<CALayer>(layer, &*self.layer));
        drop(actual);
        if still_owned {
            view.setWantsLayer(false);
            let actual = view.layer();
            let may_clear = actual
                .as_deref()
                .is_none_or(|layer| std::ptr::eq::<CALayer>(layer, &*self.layer));
            drop(actual);
            if may_clear {
                view.setLayer(None);
            } // 原setter可能已清nil；后继backing出现时不能再清它。
        }
        drop(transaction);
        let actual = view.layer();
        let final_layer = actual.is_some();
        drop(actual);
        let final_wants_layer = view.wantsLayer();
        if !owned || !intact || final_layer || final_wants_layer {
            return Err(NativeMarkdownPagedTextViewError::Revoked); // 仍完成原业务清理；不把外方活Weak谎称nil。
        }
        Ok(())
    }
}

fn checked<T>(current: &impl Fn() -> bool, call: impl FnOnce() -> T) -> Result<T> {
    if !current() {
        return Err(NativeMarkdownPagedTextViewError::Revoked);
    }
    let result = call();
    if !current() {
        return Err(NativeMarkdownPagedTextViewError::Revoked);
    }
    Ok(result)
}
fn dimension(points: f64, scale: f64) -> Result<usize> {
    let pixels = points * scale;
    // ruler等真实Native布局可产生分数point；覆盖它需要向外取完整设备像素，不能拒绝或缩放正文。
    // 分配仍按原TiledBitmap的实际ceil尺寸收费；usize上界用严格小于，避免f64将MAX舍入成2^64。
    if !points.is_finite()
        || points <= 0.0
        || !scale.is_finite()
        || scale <= 0.0
        || !pixels.is_finite()
        || pixels <= 0.0
        || pixels.ceil() >= usize::MAX as f64
    {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    Ok(pixels.ceil() as usize)
}
fn contains(outer: NSRect, inner: NSRect) -> bool {
    finite_rect(inner, true)
        && finite_edges(outer)
        && finite_edges(inner)
        && inner.origin.x >= outer.origin.x
        && inner.origin.y >= outer.origin.y
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height
}

fn finite_edges(rect: NSRect) -> bool {
    (rect.origin.x + rect.size.width).is_finite() && (rect.origin.y + rect.size.height).is_finite()
}
fn tile_transform(bounds: NSRect, scale: f64, tile: PixelTile) -> Result<CGAffineTransform> {
    let tx = -scale * bounds.origin.x - tile.x as f64;
    let ty = -scale * bounds.origin.y - tile.bottom as f64;
    if !tx.is_finite() || !ty.is_finite() {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    // 本视图坐标是Y-up；tile.y为provider顶行，真实bottom把同一source点映射回对应子context。
    Ok(CGAffineTransform {
        a: scale,
        b: 0.0,
        c: 0.0,
        d: scale,
        tx,
        ty,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSSize;

    #[test]
    fn nested_native_paint_cannot_steal_or_release_the_original_stack_guard() {
        let slot = Cell::new(false);
        let original = PaintUse::enter(&slot).unwrap();
        assert!(matches!(
            PaintUse::enter(&slot),
            Err(NativeMarkdownPagedTextViewError::Pending)
        ));
        assert!(slot.get());
        drop(original);
        assert!(!slot.get());
        drop(PaintUse::enter(&slot).unwrap());
        assert!(!slot.get());
    }

    #[test]
    fn finite_components_cannot_hide_overflowing_edges_or_backing_transform() {
        let overflowing = NSRect::new(NSPoint::new(f64::MAX, 0.0), NSSize::new(f64::MAX, 10.0));
        assert!(!contains(overflowing, overflowing));
        let source = NSRect::new(NSPoint::new(f64::MAX, 0.0), NSSize::new(1.0, 1.0));
        let tile = PixelTile {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bottom: 0,
        };
        assert!(matches!(
            tile_transform(source, 2.0, tile),
            Err(NativeMarkdownPagedTextViewError::InvalidGeometry)
        ));
        assert!(matches!(
            dimension(f64::MAX, 2.0),
            Err(NativeMarkdownPagedTextViewError::InvalidGeometry)
        ));
        // 原分数point不能被判无效，也不能通过饱和转换掩盖真正无法分配的设备尺寸。
        assert_eq!(dimension(613.69140625, 2.0).unwrap(), 1228);
        for (points, scale) in [(usize::MAX as f64, 1.0), (-1.0, -1.0), (1.0, 0.0)] {
            assert!(matches!(
                dimension(points, scale),
                Err(NativeMarkdownPagedTextViewError::InvalidGeometry)
            ));
        }
        // 同一正常source在部分尾tile中保留确切Y-up设备原点，不借截断或反向图来通过。
        let valid = NSRect::new(NSPoint::new(12.0, 7.0), NSSize::new(640.0, 320.0));
        let transform = tile_transform(
            valid,
            2.0,
            PixelTile {
                x: 512,
                y: 512,
                width: 256,
                height: 128,
                bottom: 0,
            },
        )
        .unwrap();
        assert_eq!(
            (transform.a, transform.d, transform.tx, transform.ty),
            (2.0, 2.0, -536.0, -14.0)
        );
        // 真实9→10行号后的最后设备列仍取原2倍像素比例；不得把1228列压成1227.3828125列。
        let fractional = NSRect::new(NSPoint::ZERO, NSSize::new(613.69140625, 240.0));
        let tail = tile_transform(
            fractional,
            2.0,
            PixelTile {
                x: 1024,
                y: 256,
                width: 204,
                height: 224,
                bottom: 0,
            },
        )
        .unwrap();
        assert_eq!((tail.a, tail.d, tail.tx, tail.ty), (2.0, 2.0, -1024.0, 0.0));
    }
}
