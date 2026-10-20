//! 显式探针才把原标准 fragment 的字形送入单个有界 CPU 视口，再发布原生图层。

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

/// 辅助画布只共享本 editor 的原子资源收据，不拥有正文、Native 图或另一份计数器。
#[derive(Clone)]
pub(super) struct NativeMarkdownRasterAuxiliaryResources {
    counters: Arc<ResourceCounters>,
}

/// 同步辅助绘制沿原收据登记在途操作；析构只归还标量，不执行 Native。
pub(super) struct NativeMarkdownRasterAuxiliaryOperation {
    _guard: OperationGuard,
}

impl NativeMarkdownRasterAuxiliaryResources {
    pub(super) fn is_open(&self) -> bool {
        !self.counters.closed.load(Ordering::Acquire)
    }

    pub(super) fn fail(&self) {
        // 辅助绘制的真实失败仍进入原共享资源收据；不保已无消费者的首帧通知回链。
        self.counters.fail();
    }

    pub(super) fn invalidate_geometry(&self) {
        self.counters.invalidate_geometry(); // 仍撤同一原票，不另设 gutter 代次或调度器。
    }

    pub(super) fn operation(&self) -> Result<NativeMarkdownRasterAuxiliaryOperation, String> {
        if !self.is_open() {
            return Err("光栅辅助绘制已经关闭".into());
        }
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
        let (width, height) = geometry::dimensions(bounds, scale)?;
        pixels::Bitmap::new(width, height, Arc::clone(&self.counters.pixels))
    }
}

// 这里只限制标准框架 fragment 的 Weak 元数据；自有对象和实际业务值仍共用原 ledger 预算。
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
        drop(identities); // 所有 Weak 控制块先在 lease 外槽归还，再记无 payload 的标量收据。
        if let Some(counters) = self.counters.take()
            && counters.closed.load(Ordering::Acquire)
        {
            counters.registry_detached.store(true, Ordering::Release);
        }
    }
}

/// 关闭槽后把真实 payload 留在同步栈；失效 Native 前必须结束 CacheLease。
fn take_fragment_registry_for_close(
    fragments: &CacheOwner<FragmentRegistry>,
) -> Result<FragmentRegistry, String> {
    match fragments.begin() {
        CacheAdmission::Ready(mut lease) => {
            // 连计数收据一起移出，空替身的 Drop 不能在旧 Weak 容量仍活时报告 registry_detached。
            let registry = std::mem::replace(lease.payload_mut(), FragmentRegistry::empty());
            fragments.close();
            drop(lease); // Closed 禁止恢复旧槽；之后所有 Native 调用都不持 lease 或状态借用。
            Ok(registry)
        }
        CacheAdmission::Loaned => {
            // 在途扫描仍独占旧 Weak；只能封闭并明确失败，由原栈实际归还，不能补建另一列表。
            fragments.close();
            Err("关闭时光栅 fragment 弱登记仍被同步操作借出".into())
        }
        CacheAdmission::Closed => {
            // 首次 shutdown 不应先失去 registry；重复关闭由上层纯标量入口直接结束。
            Err("关闭时光栅 fragment 弱登记已被提前封闭".into())
        }
    }
}

/// 资源收据只持原像素/操作的原子事实，不保活 Native 图、文档或旧诊断目录。
/// 关闭成功仍由实际 provider/context 归还决定，不以 view 析构或时间流逝代替。
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
        // 真实事件在首个 Native 操作前撤原票；耗尽无法表达更新，永久失败而非回绕或饱和冒充新票。
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
        // 函数性失败另由 sticky_failure 验收；释放必须等待真实 provider/context 最后归还。
        s.closed
            && s.native_detached
            && s.registry_detached
            && s.active_operations == 0
            && s.live_pixel_buffers == 0
            && s.live_pixel_bytes == 0
            && !s.pixel_release_failed
    }
}

/// 同步 Native 操作只递增标量；析构不调用 Native，也不把延迟释放改写为成功。
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

/// 所有产品回链均为弱引用；Rc 只让同步操作在结束状态借用后安全加载它们。
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
        // 先撤本proxy，只复原它安装前的同一原delegate；不覆盖后来被替换的关系。
        // 字段已移出状态槽，Native恢复和临时claim析构均不跨RefCell借用。
        let proxy = self.proxy.take();
        if let Some(proxy) = proxy.as_ref()
            && proxy.restore_original_delegate().is_err()
        {
            self.counters.fail();
        }
        drop(proxy);
        // 代理关系已撤，再撤真实通知、恢复原通知开关并清除图像内容。
        let observers = std::mem::take(&mut self.observers);
        for observer in observers {
            // SAFETY: 只撤本对象从默认中心取得的 token，且整个 Native owner 仅能在主线程析构。
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
        // 这里只证明项目关系和强 claim 已归还；真实 Native dealloc 仍由统一 weak ledger 核验。
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
    /// 只给同一 editor 的附属视口共享既有收据，不把私有计数器或 Native owner 暴露出去。
    pub(super) fn auxiliary_resources(self: &Rc<Self>) -> NativeMarkdownRasterAuxiliaryResources {
        NativeMarkdownRasterAuxiliaryResources {
            counters: Arc::clone(&self.counters),
        }
    }

    /// 附属视图沿本次原 OriginPermit 执行原语；不补领较新正文、模式或字体权限。
    pub(super) fn auxiliary_step<T>(
        &self,
        view: &PieceBackedMarkdownTextView,
        permit: &NativeMarkdownRasterOriginPermit,
        work: impl FnOnce() -> T,
    ) -> Result<T, String> {
        permit.step(self, view, work)
    }

    pub(super) fn requested() -> bool {
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
        drop(document); // 从此没有第二份正文强 owner，安装过程也不读取正文字符串。
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
            None // 默认drawRect对照既不显式开启正文backing，也不增加恢复租约。
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
        // 安装临时 owner 后任何后续失败都走同一 Drop 撤销，不留下半挂接观察者或图层。
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
            notify_counters.invalidate_geometry(); // 先撤票；最后一个 getter 内发生滚动也不能逃过原 epoch。
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
                // 真实 clip 通知只请求原 NSView 生命周期；不移动图像、不强制布局或排轮询。
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
            // Bounds 通知不覆盖 setFrameSize 引起的变化，必须分别订阅原 Frame 与 Bounds。
            // SAFETY: 两个名称由 AppKit 静态导出；对象为原正文/clip，不观察自有 image 的设置。
            for name in unsafe {
                [
                    NSViewBoundsDidChangeNotification,
                    NSViewFrameDidChangeNotification,
                ]
            } {
                // SAFETY: 空 queue 使用原通知线程；闭包首核主线程，且仅持主线程封装 Weak 与原子收据。
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
                // 先交给本地 owner，ledger 失败返回也能真实 removeObserver，不遗留中心持有的 token。
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
        // 标准factory和标准fragment均保留；只由公开viewport代理接管后续rendering surface配置。
        // 不失效全文、不ensure，也不重新构造完整text element。
        *owner.native.borrow_mut() = Some(Rc::new(native));
        *view.ivars().raster_body_layer.borrow_mut() = body_layer;
        eprintln!("native_raster_surface event=installed formal=false");
        Ok(owner)
    }

    fn native_owner(&self) -> Result<Rc<NativeOwners>, String> {
        // Rc clone 本身不 retain Native；结束 RefCell 借用后才允许任何可重入 getter/setter。
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
        // lease 已将整个 payload 移出 RefCell；Weak load/析构不跨槽借用。只淘汰真实 nil，
        // 活身份不可因换视口或预算用尽而丢失，重复的同一存活对象不再次累计。
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
            drop(original); // 临时 claim 归还也可撤权；检查后禁止后继 Weak load。
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
        self.counters.invalidate_geometry(); // Root 的公开 Native 几何回调只调用此纯标量入口。
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
        } // 合法撤权只取消，不重签或制造完成。
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
            self.counters.fail(); // 入口合同错误仍进入原真实失败收据。
            return Err("正文未显式启用真实layer绘制入口".into());
        }
        let result = paint::paint_layer(self, view, permit);
        if !permit.current(self, view) {
            return Ok(false); // 与原drawRect同样只取消原操作，绝不从后继状态补领发布票据。
        }
        if result.is_err() {
            self.counters.fail();
        }
        result
    }

    pub(super) fn fail_layer_update_sequence(&self) {
        self.counters.fail(); // 只记不可回绕序号耗尽，不读取Native或制造后继显示任务。
    }

    pub(super) fn release_receipt(&self) -> NativeMarkdownRasterReleaseReceipt {
        NativeMarkdownRasterReleaseReceipt {
            resources: Arc::clone(&self.counters),
        }
    }

    pub(super) fn shutdown(&self) -> Result<(), String> {
        if self.closed.replace(true) {
            // Native 失效/最后 release 可同步重入；原关闭栈独占清理，重复调用不重开 registry。
            // 返回本身不是释放收据，统一 barrier 仍核真实 Native weak 与原子资源状态。
            return Ok(());
        }
        self.counters.closed.store(true, Ordering::Release);
        let registry = take_fragment_registry_for_close(&self.fragments);
        let active = self.counters.active_operations.load(Ordering::Acquire);
        // 封闭标量先拒绝绘制和代理配置，再撤原Native proxy和观察者；不持RefCell借用。
        let obsolete = self.native.borrow_mut().take();
        drop(obsolete); // 同步绘图临时 Rc 若还存在，真实清理须等待它归还，不能谎称已经释放。
        let result = if active != 0 {
            // 已有同步 Native 操作不能被关闭栈抢占；不得在其仍使用 fragment 时失效布局。
            Err("关闭时仍存在光栅同步操作".into())
        } else {
            match registry.as_ref() {
                Ok(registry) => self.invalidate_closed_fragments(registry),
                Err(error) => Err(error.clone()),
            }
        };
        drop(registry); // 最后真实归还 Weak 数组容量后，FragmentRegistry::drop 才报告 detached。
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
                // 只核原身份和存活标量；每个 Native 原语前后结束 Weak 字段借用，不读正文或 line array。
                self.closed.get()
                    && self.counters.closed.load(Ordering::Acquire)
                    && self.graph.document.strong_count() != 0
                    && storage.ivars().document.try_borrow().is_ok_and(|document| {
                        std::rc::Weak::ptr_eq(&document, &self.graph.document)
                    })
            };
        if !source_current() {
            // 旧 NSString 的来源已撤，公开失效也不能在不合法的旧范围上继续执行。
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
                continue; // 真实 nil 已完成归还；不能重建对象或为它补一份布局。
            };
            // SAFETY: 原片段仍在主线程同步强 claim 内；None 撤其异步队列，不引入跨线程操作。
            unsafe { fragment.setLayoutQueue(None) };
            if !source_current() {
                return Err("撤旧 fragment 队列后原文档已撤销".into());
            }
            fragment.invalidateLayout(); // 仅公开撤销已有布局，不 ensure、不读取 state3 专属行数据。
            if !source_current() {
                return Err("失效旧 fragment 布局后原文档已撤销".into());
            }
            drop(fragment); // 标准壳不作自有资源豁免；原ledger中真实业务值仍必须逐个Weak归零。
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
    fn shutdown_registry_receipt_waits_for_real_extracted_payload_drop() {
        let counters = Arc::new(ResourceCounters::default());
        let registry = CacheOwner::new(FragmentRegistry {
            identities: Vec::with_capacity(8),
            counters: Some(Arc::clone(&counters)),
        });
        counters.closed.store(true, Ordering::Release);
        let detached = take_fragment_registry_for_close(&registry).expect("可关闭的独占 registry");
        // 槽和 lease 已结束，但待逐个失效的旧 Weak 容量仍在同步栈，不能提前报告实际归还。
        assert!(registry.is_released());
        assert!(!counters.registry_detached.load(Ordering::Acquire));
        assert!(detached.identities.capacity() >= 8);
        drop(detached);
        assert!(counters.registry_detached.load(Ordering::Acquire));
    }

    #[test]
    fn shutdown_cannot_steal_a_live_registry_loan_or_report_early_release() {
        let counters = Arc::new(ResourceCounters::default());
        let registry = CacheOwner::new(FragmentRegistry {
            identities: Vec::new(),
            counters: Some(Arc::clone(&counters)),
        });
        let CacheAdmission::Ready(lease) = registry.begin() else {
            panic!("初始 registry 应可借出");
        };
        counters.closed.store(true, Ordering::Release);
        assert!(take_fragment_registry_for_close(&registry).is_err());
        // 关闭只能撤销后续准入；原栈未归还时必须继续显示 payload 存活，且不能补建第二份。
        assert!(matches!(registry.begin(), CacheAdmission::Closed));
        assert!(!registry.is_released());
        assert!(!counters.registry_detached.load(Ordering::Acquire));
        drop(lease);
        assert!(registry.is_released());
        assert!(counters.registry_detached.load(Ordering::Acquire));
    }

    #[test]
    fn geometry_events_revoke_old_epoch_and_cannot_wrap_into_a_valid_receipt() {
        let counters = ResourceCounters::default();
        let original = counters.viewport_epoch.load(Ordering::Acquire);
        counters.invalidate_geometry();
        assert_ne!(counters.viewport_epoch.load(Ordering::Acquire), original);
        // 最后一个可表达事件仍有效；下一次必须永久失败，不能饱和到同一个可复用的 MAX 票。
        counters
            .viewport_epoch
            .store(u64::MAX - 1, Ordering::Release);
        counters.invalidate_geometry();
        assert_eq!(counters.viewport_epoch.load(Ordering::Acquire), u64::MAX);
        assert!(!counters.viewport_epoch_exhausted.load(Ordering::Acquire));
        counters.invalidate_geometry();
        assert_eq!(counters.viewport_epoch.load(Ordering::Acquire), u64::MAX);
        assert!(counters.viewport_epoch_exhausted.load(Ordering::Acquire));
        assert!(counters.sticky_failure.load(Ordering::Acquire));
    }
}
