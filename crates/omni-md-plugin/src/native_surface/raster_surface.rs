//! An explicit probe draws the original TextKit fragments into one bounded CPU viewport.
//! Its Native layer and release receipts do not certify the separate paged-body implementation.

use super::cache_owner::{CacheAdmission, CacheOwner, CachePayload};
use super::*;
use objc2_app_kit::{
    NSClipView, NSTextLayoutFragment, NSViewBoundsDidChangeNotification,
    NSViewFrameDidChangeNotification, NSWindowOrderingMode,
};
use objc2_foundation::NSNotificationCenter;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

mod authority;
mod body_layer;
mod geometry;
mod native;
mod paint;
mod pixels;
mod value_observation;
pub(super) use authority::NativeMarkdownRasterOriginPermit;
pub(in crate::native_surface) use body_layer::NativeMarkdownRasterBodyLayerLease;
use native::{NativeMarkdownRasterImageView, NativeMarkdownRasterViewportProxy};
pub(super) use pixels::Bitmap as NativeMarkdownRasterAuxiliaryBitmap;

/// Auxiliary canvases share this editor's atomic receipts, without owning its document or Native graph.
#[derive(Clone)]
pub(super) struct NativeMarkdownRasterAuxiliaryResources {
    counters: Arc<ResourceCounters>,
}

/// Tracks synchronous auxiliary work in the same receipt; dropping it never invokes Native code.
pub(super) struct NativeMarkdownRasterAuxiliaryOperation {
    _guard: OperationGuard,
}

impl NativeMarkdownRasterAuxiliaryResources {
    pub(super) fn is_open(&self) -> bool {
        !self.counters.closed.load(Ordering::Acquire)
    }

    pub(super) fn fail(&self) {
        // Report an actual auxiliary failure through the original shared receipt. No obsolete
        // first-frame notification link is retained solely to announce that failure.
        self.counters.fail();
    }

    pub(super) fn invalidate_geometry(&self) {
        // Revoke the original geometry permit; auxiliary drawing has no independent epoch or scheduler.
        self.counters.invalidate_geometry();
    }

    pub(super) fn operation(&self) -> Result<NativeMarkdownRasterAuxiliaryOperation, String> {
        if !self.is_open() {
            return Err("光栅辅助绘制已经关闭".into());
        }
        // This guard covers only the caller's synchronous work and cannot authorize later callbacks.
        Ok(NativeMarkdownRasterAuxiliaryOperation {
            _guard: OperationGuard::new(Arc::clone(&self.counters)),
        })
    }

    pub(super) fn bitmap(
        &self,
        bounds: NSRect,
        scale: f64,
    ) -> Result<NativeMarkdownRasterAuxiliaryBitmap, String> {
        if !self.is_open() {
            return Err("光栅辅助画布已经关闭".into());
        }
        // Allocate at the real backing scale, within the viewport budget, using the same pixel owner.
        let (width, height) = geometry::dimensions(bounds, scale)?;
        pixels::Bitmap::new(width, height, Arc::clone(&self.counters.pixels))
    }
}

// This bounds weak framework-fragment metadata. Owned objects and business values use the original ledger.
const MAX_FRAGMENT_WEAK_METADATA: usize = 1_024;

struct FragmentRegistry {
    identities: Vec<Weak<NSTextLayoutFragment>>,
    counters: Option<Arc<ResourceCounters>>,
}
impl CachePayload for FragmentRegistry {
    fn empty() -> Self {
        Self {
            identities: Vec::new(),
            counters: None,
        }
    }
}
impl Drop for FragmentRegistry {
    fn drop(&mut self) {
        let identities = std::mem::take(&mut self.identities);
        // Return weak control blocks outside the cache slot before reporting payload detachment.
        drop(identities);
        if let Some(counters) = self.counters.take()
            && counters.closed.load(Ordering::Acquire)
        {
            counters.registry_detached.store(true, Ordering::Release);
        }
    }
}

/// Closes admission while moving the actual registry onto the shutdown stack, outside CacheLease.
fn take_fragment_registry_for_close(
    fragments: &CacheOwner<FragmentRegistry>,
) -> Result<FragmentRegistry, String> {
    match fragments.begin() {
        CacheAdmission::Ready(mut lease) => {
            // Move the counters with the real payload. Dropping the empty replacement must not
            // report detachment while the old weak-vector allocation remains on this stack.
            let registry = std::mem::replace(lease.payload_mut(), FragmentRegistry::empty());
            fragments.close();
            // Closed admission prevents restoration; subsequent Native invalidation holds no slot borrow.
            drop(lease);
            Ok(registry)
        }
        CacheAdmission::Loaned => {
            // An active scan still owns the registry. Revoke future admission and fail rather than
            // stealing its allocation or constructing another registry to claim cleanup.
            fragments.close();
            Err("关闭时光栅 fragment 弱登记仍被同步操作借出".into())
        }
        CacheAdmission::Closed => {
            // The first shutdown must still own the registry. The scalar shutdown gate handles repeats.
            Err("关闭时光栅 fragment 弱登记已被提前封闭".into())
        }
    }
}

/// Holds atomic facts about the original pixels and operations, without retaining Native or document data.
/// Actual provider/context release determines completion; view destruction or elapsed time cannot replace it.
#[derive(Clone)]
pub(super) struct NativeMarkdownRasterReleaseReceipt {
    resources: Arc<ResourceCounters>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct NativeMarkdownRasterResourceSnapshot {
    pub(super) closed: bool,
    pub(super) native_detached: bool,
    pub(super) registry_detached: bool,
    pub(super) active_operations: usize,
    pub(super) live_pixel_buffers: usize,
    pub(super) live_pixel_bytes: usize,
    pub(super) pixel_release_failed: bool,
    pub(super) sticky_failure: bool,
    pub(super) actual_paints: usize,
    pub(super) configured_fragments: usize,
}

#[derive(Default)]
struct ResourceCounters {
    closed: AtomicBool,
    native_detached: AtomicBool,
    registry_detached: AtomicBool,
    active_operations: AtomicUsize,
    pixels: Arc<super::viewport_pixels::NativeMarkdownViewportPixels>,
    sticky_failure: AtomicBool,
    actual_paints: AtomicUsize,
    configured_fragments: AtomicUsize,
    viewport_epoch: AtomicU64,
    viewport_epoch_exhausted: AtomicBool,
    geometry_mismatch_logs: AtomicUsize,
    proxy_failure_logs: AtomicUsize,
}

impl ResourceCounters {
    fn fail(&self) {
        self.sticky_failure.store(true, Ordering::Release);
    }

    fn invalidate_geometry(&self) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        // Revoke the old permit before any Native operation in the event. Exhaustion permanently
        // fails admission: wrapping or saturating would let a later event reuse an old identity.
        if self
            .viewport_epoch
            .try_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1)
            })
            .is_err()
        {
            self.viewport_epoch_exhausted.store(true, Ordering::Release);
            self.fail();
        }
    }
}

impl NativeMarkdownRasterReleaseReceipt {
    pub(super) fn snapshot(&self) -> NativeMarkdownRasterResourceSnapshot {
        let c = &self.resources;
        NativeMarkdownRasterResourceSnapshot {
            closed: c.closed.load(Ordering::Acquire),
            native_detached: c.native_detached.load(Ordering::Acquire),
            registry_detached: c.registry_detached.load(Ordering::Acquire),
            active_operations: c.active_operations.load(Ordering::Acquire),
            live_pixel_buffers: c.pixels.live_pixel_buffers.load(Ordering::Acquire),
            live_pixel_bytes: c.pixels.live_pixel_bytes.load(Ordering::Acquire),
            pixel_release_failed: c.pixels.pixel_release_failed.load(Ordering::Acquire),
            sticky_failure: c.sticky_failure.load(Ordering::Acquire),
            actual_paints: c.actual_paints.load(Ordering::Acquire),
            configured_fragments: c.configured_fragments.load(Ordering::Acquire),
        }
    }

    pub(super) fn is_released(&self) -> bool {
        let s = self.snapshot();
        // Functional failure is reported separately. Release still requires the last real
        // provider/context owner, native owner, registry, and synchronous operation to return.
        s.closed
            && s.native_detached
            && s.registry_detached
            && s.active_operations == 0
            && s.live_pixel_buffers == 0
            && s.live_pixel_bytes == 0
            && !s.pixel_release_failed
    }
}

/// Counts a synchronous Native operation; Drop changes only the count and never fabricates release.
struct OperationGuard(Arc<ResourceCounters>);
impl OperationGuard {
    fn new(counters: Arc<ResourceCounters>) -> Self {
        counters.active_operations.fetch_add(1, Ordering::AcqRel);
        Self(counters)
    }
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.0.active_operations.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Product back-links are weak; Rc allows synchronous access after the state borrow has ended.
struct WeakGraph {
    view: Weak<PieceBackedMarkdownTextView>,
    storage: Weak<PieceBackedMarkdownTextStorage>,
    manager: Weak<NSTextLayoutManager>,
    clip: Weak<NSClipView>,
    document: WeakMarkdownDocument,
}

struct NativeOwners {
    image: Option<Retained<NativeMarkdownRasterImageView>>,
    proxy: Option<Retained<NativeMarkdownRasterViewportProxy>>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    observed: [ObservedView; 2],
    counters: Arc<ResourceCounters>,
}

struct ObservedView {
    view: Weak<NSView>,
    previous_posts_bounds: bool,
    previous_posts_frame: bool,
}

impl NativeOwners {
    fn image(&self) -> &NativeMarkdownRasterImageView {
        self.image.as_deref().expect("live native owner has image")
    }
}

impl Drop for NativeOwners {
    fn drop(&mut self) {
        // Restore only the delegate relation this proxy installed; never overwrite a successor.
        // Native restoration and temporary claim destruction occur outside the state-slot borrow.
        let proxy = self.proxy.take();
        if let Some(proxy) = proxy.as_ref()
            && proxy.restore_original_delegate().is_err()
        {
            self.counters.fail();
        }
        drop(proxy);
        // After detaching the proxy, remove our notifications, restore their original flags,
        // and return the image retained by our layer.
        let observers = std::mem::take(&mut self.observers);
        for observer in observers {
            // SAFETY: Each token came from this owner and the default center; this owner drops on main.
            unsafe {
                NSNotificationCenter::defaultCenter().removeObserver(<ProtocolObject<
                    dyn NSObjectProtocol,
                > as AsRef<AnyObject>>::as_ref(
                    &*observer
                ))
            };
            drop(observer);
        }
        for observed in &self.observed {
            if let Some(view) = observed.view.load() {
                view.setPostsBoundsChangedNotifications(observed.previous_posts_bounds);
                view.setPostsFrameChangedNotifications(observed.previous_posts_frame);
            }
        }
        let image = self.image.take();
        if let Some(image) = image.as_ref() {
            image.clear();
        }
        drop(image);
        // This reports returned project relationships and claims. The weak ledger still checks
        // actual Native deallocation independently, including framework-delayed releases.
        self.counters.native_detached.store(true, Ordering::Release);
    }
}

pub(super) struct NativeMarkdownRasterSurface {
    closed: Cell<bool>,
    counters: Arc<ResourceCounters>,
    ledger: Rc<NativeMarkdownOwnedLedger>,
    graph: Rc<WeakGraph>,
    native: RefCell<Option<Rc<NativeOwners>>>,
    painting: Cell<bool>,
    fragments: CacheOwner<FragmentRegistry>,
    fragment_epoch: Cell<u64>,
}

impl NativeMarkdownRasterSurface {
    /// Shares the same editor's receipts with auxiliary viewports without exposing Native owners.
    pub(super) fn auxiliary_resources(self: &Rc<Self>) -> NativeMarkdownRasterAuxiliaryResources {
        NativeMarkdownRasterAuxiliaryResources {
            counters: Arc::clone(&self.counters),
        }
    }

    /// Executes auxiliary work with the original permit, without adopting newer source, mode, or fonts.
    pub(super) fn auxiliary_step<T>(
        &self,
        view: &PieceBackedMarkdownTextView,
        permit: &NativeMarkdownRasterOriginPermit,
        work: impl FnOnce() -> T,
    ) -> Result<T, String> {
        permit.step(self, view, work)
    }

    pub(super) fn requested() -> bool {
        // This diagnostic route is opt-in; an arbitrary nonempty environment value does not enable it.
        std::env::var_os("OMNI_NATIVE_RASTER_SURFACE").is_some_and(|value| value == "1")
    }

    pub(super) fn install(
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        manager: &NSTextLayoutManager,
        scroll: &NSScrollView,
        ledger: &Rc<NativeMarkdownOwnedLedger>,
        mtm: MainThreadMarker,
    ) -> Result<Rc<Self>, String> {
        if !Self::requested() {
            return Err("未显式启用光栅探针".into());
        }
        // Preserve the existing fragment factory and body identity rather than replacing another owner.
        if manager.delegate().is_some() {
            return Err("原布局已有 fragment factory，光栅探针不替换它".into());
        }
        if scroll.documentView().is_some_and(|document| {
            let original: &NSView = view;
            !std::ptr::eq::<NSView>(&*document, original)
        }) {
            return Err("前置光栅安装的 scroll 已拥有其他正文".into());
        }
        let viewport = manager.textViewportLayoutController();
        let original_delegate = viewport.delegate().ok_or("原 viewport delegate 缺失")?;
        let clip = scroll.contentView();
        let document = storage.document().ok_or("光栅安装时原文档缺失")?;
        let graph = Rc::new(WeakGraph {
            view: Weak::from(view),
            storage: Weak::from(storage),
            manager: Weak::from(manager),
            clip: Weak::from_retained(&clip),
            document: Rc::downgrade(&document),
        });
        // Keep only the original document's weak identity; installation never reads its text.
        drop(document);
        let counters = Arc::new(ResourceCounters::default());
        let owner = Rc::new(Self {
            closed: Cell::new(false),
            counters: Arc::clone(&counters),
            ledger: Rc::clone(ledger),
            graph: Rc::clone(&graph),
            native: RefCell::new(None),
            painting: Cell::new(false),
            fragments: CacheOwner::new(FragmentRegistry {
                identities: Vec::new(),
                counters: Some(Arc::clone(&counters)),
            }),
            fragment_epoch: Cell::new(0),
        });
        let body_layer = if view.uses_raster_layer_updates() {
            Some(Rc::new(NativeMarkdownRasterBodyLayerLease::install(
                view, ledger, &owner,
            )?))
        } else {
            None // The default drawRect comparison does not enable body backing or add a restoration lease.
        };
        let image = NativeMarkdownRasterImageView::new(view.visibleRect(), mtm);
        let proxy =
            NativeMarkdownRasterViewportProxy::new(&owner, &viewport, &original_delegate, mtm);
        image.configure(view.window().map_or(1.0, |w| w.backingScaleFactor()))?;
        let body: &NSView = view;
        let clip_view: &NSView = &clip;
        let observed = [body, clip_view].map(|view| ObservedView {
            view: Weak::from(view),
            previous_posts_bounds: view.postsBoundsChangedNotifications(),
            previous_posts_frame: view.postsFrameChangedNotifications(),
        });
        // Once constructed, this local owner rolls back every later failure through Drop,
        // including observers already registered before a ledger admission fails.
        let mut native = NativeOwners {
            image: Some(image),
            proxy: Some(proxy),
            observers: Vec::with_capacity(4),
            observed,
            counters: Arc::clone(&counters),
        };
        ledger.include("editor.raster.image", native.image())?;
        if let Some(layer) = native.image().layer() {
            ledger.include("editor.raster.layer", &layer)?;
        }
        ledger.include(
            "editor.raster.viewport-proxy",
            native.proxy.as_ref().expect("proxy constructed"),
        )?;
        let weak_view = MainThreadBound::new(Weak::from(view), mtm);
        let notify_counters = Arc::clone(&counters);
        let callback = block2::RcBlock::new(move |_notification: NonNull<NSNotification>| {
            // Revoke first: scrolling during even the final getter must invalidate the original epoch.
            notify_counters.invalidate_geometry();
            let Some(mtm) = MainThreadMarker::new() else {
                notify_counters.fail();
                return;
            };
            if notify_counters.closed.load(Ordering::Acquire) {
                return;
            }
            let reentry = super::viewport_reentry_diagnostic::geometry_callback(
                super::viewport_reentry_diagnostic::GeometryKind::Notice,
                None,
            );
            let view = {
                let _phase = super::viewport_reentry_diagnostic::enter(
                    super::viewport_reentry_diagnostic::Phase::NoticeLoad,
                );
                weak_view.get(mtm).load()
            };
            if let Some(view) = view {
                reentry.record(view.ivars().table_layout_update_scheduled.get());
                // A real clip notification requests the existing NSView lifecycle. It neither moves
                // the image nor forces layout or schedules polling from inside the notification.
                {
                    let _phase = super::viewport_reentry_diagnostic::enter(
                        super::viewport_reentry_diagnostic::Phase::NoticeLayout,
                    );
                    view.setNeedsLayout(true);
                }
                view.invalidate_visible_body();
            }
        });
        for observed in [body, clip_view] {
            // Bounds notifications do not cover every frame-size change, so observe both kinds.
            // SAFETY: AppKit exports these static names; observed objects are the original body/clip.
            for name in unsafe {
                [
                    NSViewBoundsDidChangeNotification,
                    NSViewFrameDidChangeNotification,
                ]
            } {
                // SAFETY: A nil queue uses the posting thread. The block checks main-thread access
                // before loading its MainThreadBound weak reference and otherwise holds only atomics.
                let token = unsafe {
                    NSNotificationCenter::defaultCenter()
                        .addObserverForName_object_queue_usingBlock(
                            Some(name),
                            Some(observed),
                            None,
                            &callback,
                        )
                };
                native.observers.push(token);
                // Give the token to the local owner first, so a ledger error still removes the
                // center's registration rather than abandoning a retained notification block.
                let token = &**native.observers.last().expect("geometry observer owned");
                ledger.include(
                    "editor.raster.geometry-observer",
                    <ProtocolObject<dyn NSObjectProtocol> as AsRef<AnyObject>>::as_ref(token),
                )?;
            }
            observed.setPostsBoundsChangedNotifications(true);
            observed.setPostsFrameChangedNotifications(true);
        }
        view.addSubview_positioned_relativeTo(native.image(), NSWindowOrderingMode::Above, None);
        viewport.setDelegate(Some(ProtocolObject::from_ref(
            &**native.proxy.as_ref().expect("proxy created"),
        )));
        // Preserve the standard factory/fragments. The public viewport proxy handles later surface
        // configuration without invalidating the document, ensuring layout, or rebuilding text elements.
        *owner.native.borrow_mut() = Some(Rc::new(native));
        *view.ivars().raster_body_layer.borrow_mut() = body_layer;
        eprintln!("native_raster_surface event=installed formal=false");
        Ok(owner)
    }

    fn native_owner(&self) -> Result<Rc<NativeOwners>, String> {
        // Clone only the Rust owner here. End the RefCell borrow before any reentrant Native getter/setter.
        self.native
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| "光栅 Native owner 已归还".into())
    }

    fn register_fragment(
        &self,
        fragment: &NSTextLayoutFragment,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<(), String> {
        if !current() {
            return Err("原 fragment 在弱登记前失效".into());
        }
        let CacheAdmission::Ready(mut lease) = self.fragments.begin() else {
            return Err("光栅 fragment 弱登记发生重入或已关闭".into());
        };
        let payload = lease.payload_mut();
        let mut duplicate = false;
        let mut failed = false;
        // The lease moves the payload out of RefCell before weak loads/destruction. Evict only
        // actual nil entries: changing viewports or exhausting the budget cannot discard live
        // identities. Repeated registration of the same live fragment consumes no extra entry.
        payload.identities.retain(|weak| {
            if failed || !current() {
                failed = true;
                return true;
            }
            let original = weak.load();
            if !current() {
                failed = true;
                drop(original);
                return true;
            }
            let keep = original.is_some();
            if let Some(original) = original.as_ref() {
                duplicate |= std::ptr::eq::<NSTextLayoutFragment>(&**original, fragment);
            }
            // Returning a temporary claim can reenter; revoke further weak loads if it changes authority.
            drop(original);
            if !current() {
                failed = true;
                return true;
            }
            keep
        });
        if failed || !current() {
            return Err("原 fragment 在登记扫描中失效".into());
        }
        if duplicate {
            return Ok(());
        }
        // Refuse excess metadata instead of losing lifecycle coverage for an existing live fragment.
        if payload.identities.len() >= MAX_FRAGMENT_WEAK_METADATA {
            return Err("光栅标准 fragment Weak 元数据超过固定预算".into());
        }
        let weak = Weak::from(fragment);
        if !current() {
            return Err("原 fragment 在 Weak 初始化后失效".into());
        }
        payload.identities.push(weak);
        let next = self
            .fragment_epoch
            .get()
            .checked_add(1)
            .ok_or("光栅 fragment 登记代次耗尽")?;
        self.fragment_epoch.set(next);
        Ok(())
    }

    fn fragment_is_registered(
        &self,
        fragment: &NSTextLayoutFragment,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<bool, String> {
        if !current() {
            return Err("查询标准fragment登记前原票已撤销".into());
        }
        let CacheAdmission::Ready(mut lease) = self.fragments.begin() else {
            return Err("标准fragment登记正被借出或已关闭".into());
        };
        let mut found = false;
        // Each weak load and release may reenter Native. The original permit must survive both,
        // and the lease must be returned before reporting the identity to a later Native caller.
        for weak in &lease.payload_mut().identities {
            if !current() {
                return Err("查询标准fragment登记期间原票已撤销".into());
            }
            let native = weak.load();
            if !current() {
                return Err("升级标准fragment登记后原票已撤销".into());
            }
            found |= native
                .as_deref()
                .is_some_and(|actual| std::ptr::eq::<NSTextLayoutFragment>(actual, fragment));
            drop(native);
            if !current() {
                return Err("归还标准fragment登记临时claim后原票已撤销".into());
            }
            if found {
                break;
            }
        }
        drop(lease);
        if !current() {
            return Err("归还标准fragment登记元数据后原票已撤销".into());
        }
        Ok(found)
    }

    pub(super) fn begin(
        self: &Rc<Self>,
        view: &PieceBackedMarkdownTextView,
    ) -> Result<Option<NativeMarkdownRasterOriginPermit>, String> {
        let result = NativeMarkdownRasterOriginPermit::begin(self, view);
        if result.is_err() {
            self.counters.fail();
        }
        result
    }

    pub(super) fn invalidate_geometry(&self) {
        // Public geometry callbacks revoke through this scalar-only entry before doing Native work.
        self.counters.invalidate_geometry();
    }

    pub(super) fn settle_layout(
        &self,
        view: &PieceBackedMarkdownTextView,
        permit: &NativeMarkdownRasterOriginPermit,
    ) -> Result<bool, String> {
        let result = (|| {
            if !permit.current(self, view) {
                return Ok(false);
            }
            let _operation = OperationGuard::new(Arc::clone(&self.counters));
            let Some(window) = permit.step(self, view, || view.window())? else {
                return Ok(false);
            };
            if !permit.step(self, view, || window.isVisible())? {
                return Ok(false);
            }
            let visible = permit.step(self, view, || view.visibleRect())?;
            let origin = permit.step(self, view, || view.textContainerOrigin())?;
            let scroll = permit
                .step(self, view, || view.enclosingScrollView())?
                .ok_or("光栅原 scroll 缺失")?;
            let clip_size = permit.step(self, view, || scroll.contentSize())?;
            if !native_markdown_paint_viewport_is_valid(visible, origin, clip_size) {
                return Err("光栅布局没有有效真实视口".into());
            }
            let scale = permit.step(self, view, || window.backingScaleFactor())?;
            geometry::dimensions(visible, scale)?;
            let native = self.native_owner()?;
            let layer = permit
                .step(self, view, || native.image().layer())?
                .ok_or("光栅布局的原 CALayer 缺失")?;
            // Retain the exact window/layer locally and recheck the captured visible rect, origin,
            // and backing scale across every configuration call; do not configure successor geometry.
            let current = || {
                permit.current(self, view)
                    && permit
                        .step(self, view, || view.visibleRect())
                        .is_ok_and(|rect| rect == visible)
                    && permit
                        .step(self, view, || view.textContainerOrigin())
                        .is_ok_and(|point| point == origin)
                    && permit
                        .step(self, view, || view.window())
                        .is_ok_and(|actual| {
                            actual.is_some_and(|actual| {
                                std::ptr::eq::<objc2_app_kit::NSWindow>(&*actual, &*window)
                            })
                        })
                    && permit
                        .step(self, view, || window.backingScaleFactor())
                        .is_ok_and(|actual| actual == scale)
                    && permit
                        .step(self, view, || native.image().layer())
                        .is_ok_and(|actual| {
                            actual.is_some_and(|actual| {
                                std::ptr::eq::<objc2_quartz_core::CALayer>(&*actual, &*layer)
                            })
                        })
            };
            if !current() {
                return Err("光栅布局原几何已经改变".into());
            }
            permit.step(self, view, || native.image().setFrame(visible))?;
            if !current() {
                return Err("光栅布局设置 frame 后原几何改变".into());
            }
            native.image().configure_current(scale, &layer, &current)?;
            if !current() || permit.step(self, view, || native.image().frame())? != visible {
                return Err("光栅布局完成后原几何失效".into());
            }
            Ok(true)
        })();
        if !permit.current(self, view) {
            return Ok(false);
        } // Legitimate revocation cancels this operation; it neither renews the permit nor reports completion.
        if result.is_err() {
            self.counters.fail();
        }
        result
    }

    pub(super) fn paint(
        &self,
        view: &PieceBackedMarkdownTextView,
        permit: &NativeMarkdownRasterOriginPermit,
    ) -> Result<bool, String> {
        let result = paint::paint(self, view, permit);
        if !permit.current(self, view) {
            return Ok(false);
        }
        if result.is_err() {
            self.counters.fail();
        }
        result
    }

    pub(super) fn paint_layer(
        &self,
        view: &PieceBackedMarkdownTextView,
        permit: &NativeMarkdownRasterOriginPermit,
    ) -> Result<bool, String> {
        if !view.uses_raster_layer_updates() {
            self.counters.fail(); // An invalid entry contract is a functional failure in the same receipt.
            return Err("正文未显式启用真实layer绘制入口".into());
        }
        let result = paint::paint_layer(self, view, permit);
        if !permit.current(self, view) {
            return Ok(false); // Like drawRect, cancellation cannot acquire publishing rights from newer state.
        }
        if result.is_err() {
            self.counters.fail();
        }
        result
    }

    pub(super) fn fail_layer_update_sequence(&self) {
        // Sequence exhaustion is terminal; do not read Native or manufacture another display request.
        self.counters.fail();
    }

    pub(super) fn release_receipt(&self) -> NativeMarkdownRasterReleaseReceipt {
        NativeMarkdownRasterReleaseReceipt {
            resources: Arc::clone(&self.counters),
        }
    }

    pub(super) fn shutdown(&self) -> Result<(), String> {
        if self.closed.replace(true) {
            // Native invalidation/final release may reenter. The original shutdown stack alone
            // cleans up; this repeat cannot reopen the registry. Returning is not a release receipt:
            // the barrier still checks actual Native weak references and original atomic resources.
            return Ok(());
        }
        self.counters.closed.store(true, Ordering::Release);
        let registry = take_fragment_registry_for_close(&self.fragments);
        let active = self.counters.active_operations.load(Ordering::Acquire);
        // Seal drawing/proxy admission before detaching Native owners, outside the RefCell borrow.
        let obsolete = self.native.borrow_mut().take();
        // An active synchronous Rc may delay destruction; only its eventual return proves detachment.
        drop(obsolete);
        let result = if active != 0 {
            // Shutdown cannot preempt synchronous Native work or invalidate a fragment it still uses.
            Err("关闭时仍存在光栅同步操作".into())
        } else {
            match registry.as_ref() {
                Ok(registry) => self.invalidate_closed_fragments(registry),
                Err(error) => Err(error.clone()),
            }
        };
        // FragmentRegistry reports detachment only after returning the actual weak-array allocation.
        drop(registry);
        if result.is_err() {
            self.counters.fail();
        }
        result
    }

    fn invalidate_closed_fragments(&self, registry: &FragmentRegistry) -> Result<(), String> {
        let _operation = OperationGuard::new(Arc::clone(&self.counters));
        let view = self
            .graph
            .view
            .load()
            .ok_or("失效旧 fragment 时原正文视图已释放")?;
        let _view_payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        let storage = self
            .graph
            .storage
            .load()
            .ok_or("失效旧 fragment 时原文本存储已释放")?;
        let _storage_payload = PayloadUseGuard::enter(&storage.ivars().payload_use_depth);
        let source_current =
            || {
                // Check original identity/liveness only. Weak-field borrows end before Native;
                // shutdown never reads document text or a fragment's complete line array.
                self.closed.get()
                    && self.counters.closed.load(Ordering::Acquire)
                    && self.graph.document.strong_count() != 0
                    && storage.ivars().document.try_borrow().is_ok_and(|document| {
                        std::rc::Weak::ptr_eq(&document, &self.graph.document)
                    })
            };
        if !source_current() {
            // Once the old NSString's source is revoked, even public invalidation cannot use its range.
            return Err("失效旧 fragment 前原文档身份已撤销".into());
        }
        for weak in &registry.identities {
            if !source_current() {
                return Err("失效旧 fragment 扫描前原文档已撤销".into());
            }
            let fragment = weak.load();
            if !source_current() {
                return Err("加载旧 fragment 后原文档已撤销".into());
            }
            let Some(fragment) = fragment else {
                continue; // Actual nil is already returned; do not reconstruct the fragment or its layout.
            };
            // SAFETY: The original fragment has a synchronous main-thread claim; None detaches its queue.
            unsafe { fragment.setLayoutQueue(None) };
            if !source_current() {
                return Err("撤旧 fragment 队列后原文档已撤销".into());
            }
            // Revoke existing layout publicly without ensuring layout or reading LayoutAvailable-only lines.
            fragment.invalidateLayout();
            if !source_current() {
                return Err("失效旧 fragment 布局后原文档已撤销".into());
            }
            // Framework shells do not exempt business values from the original ledger's actual-nil checks.
            drop(fragment);
            if !source_current() {
                return Err("归还旧 fragment 临时 claim 后原文档已撤销".into());
            }
        }
        Ok(())
    }
}

impl Drop for NativeMarkdownRasterSurface {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_events_revoke_the_original_epoch() {
        let counters = ResourceCounters::default();
        let original = counters.viewport_epoch.load(Ordering::Acquire);
        // Use the production event entry to revoke the original geometry epoch.
        counters.invalidate_geometry();
        assert_ne!(counters.viewport_epoch.load(Ordering::Acquire), original);
    }
}
