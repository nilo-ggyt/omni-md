//! 将滚动文档的逻辑高度与正文backing分开；只有实际可见尺寸的子视图可以绘制。
//!
//! 此模块不读取文字、不排版、不分配像素，也不改变快捷键或插件生命周期。虚拟文档
//! 仅提供原NSScrollView的滚动范围，正文沿原clip通知移动；布局owner仍须发布同源scene。

use std::{
    cell::{Cell, RefCell},
    ptr::NonNull,
    rc::{Rc, Weak as RcWeak},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use block2::RcBlock;
use dispatch2::MainThreadBound;
use objc2::{
    MainThreadOnly, define_class, msg_send,
    rc::{Retained, Weak},
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{
    NSClipView, NSScrollView, NSView, NSViewBoundsDidChangeNotification,
    NSViewFrameDidChangeNotification,
};
use objc2_foundation::{
    MainThreadMarker, NSIntersectionRect, NSNotification, NSNotificationCenter, NSObjectProtocol,
    NSPoint, NSRect, NSSize,
};

use super::paged_text_view::{NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError};

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownPagedViewportError {
    #[error("原滚动正文已经封闭或被替换")]
    Closed,
    #[error("原滚动正文仍在同步更新")]
    Busy,
    #[error("原滚动正文几何已经改变")]
    Revoked,
    #[error("原滚动范围不是有限正数或大于可精确表示范围")]
    InvalidGeometry,
    #[error("原滚动几何计数已经耗尽")]
    CounterExhausted,
    #[error("原滚动视图已经挂有其他正文")]
    AlreadyMounted,
    #[error("原正文几何撤权失败：{0}")]
    Body(#[from] NativeMarkdownPagedTextViewError),
}
type Result<T> = std::result::Result<T, NativeMarkdownPagedViewportError>;

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    /// 无像素、文字、回调和业务owner；高度只供NSScrollView的公开几何计算使用。
    pub struct NativeMarkdownPagedScrollDocument;

    // SAFETY: NSObject身份沿主线程NSView实现，新增类没有改变父类对象布局。
    unsafe impl NSObjectProtocol for NativeMarkdownPagedScrollDocument {}

    impl NativeMarkdownPagedScrollDocument {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool { true }
        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool { true }
        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            // 逻辑滚动文档没有contents；不调用drawRect或创建按整篇高度增长的CG backing。
        }
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { false }
        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool { false }
    }
);

impl NativeMarkdownPagedScrollDocument {
    fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(());
        // SAFETY: 纯空ivars已初始化，frame由调用者核为有限正数，只在AppKit主线程构造。
        unsafe { msg_send![super(allocated), initWithFrame: frame] }
    }
}

/// 只含真实几何和原发布身份；不会保活视图、正文或旧scene。
#[derive(Clone)]
pub struct NativeMarkdownPagedViewportGeometry {
    owner: RcWeak<NativeMarkdownPagedViewport>,
    nonce: Rc<()>,
    /// 原clip公开的曝露矩形；不扣兄弟ruler/header覆盖，保留给Native滚动关系核对。
    pub document_visible: NSRect,
    /// 扣实际clip inset后转入原document的有限正文；Source原点和绘图尺寸均消费此值。
    pub body_frame: NSRect,
    pub content_height: f64,
    /// 来自原同步更新的checked_add计数；不是由frame、时间或后继scene推算出的序号。
    pub scroll_epoch: u64,
}
impl NativeMarkdownPagedViewportGeometry {
    /// 原发布后发生滚动、宽度、内容高或关闭时返回false；不现场修复或重捕新票。
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.owner.upgrade().is_some_and(|owner| {
            let same = || {
                !owner.closed.get()
                    && !owner.poisoned.get()
                    && !owner.off_main_notice.load(Ordering::Acquire)
                    && owner
                        .geometry
                        .borrow()
                        .as_ref()
                        .is_some_and(|actual| Rc::ptr_eq(&actual.nonce, &self.nonce))
            };
            // same只核纯metadata；Weak.load和可重入session委托都在借用结束后执行。
            same()
                && owner
                    .body
                    .load()
                    .is_some_and(|body| body.session_is_current())
                && same()
        })
    }
}

struct NotificationLease {
    clip: Weak<NSClipView>,
    bounds_before: bool,
    frame_before: bool,
    tokens: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

/// 唯一构造owner只强持空滚动文档与两个observer；正文、clip和scroll均由原Native图拥有。
pub struct NativeMarkdownPagedViewport {
    scroll: Weak<NSScrollView>,
    body: Weak<NativeMarkdownPagedTextView>,
    document: RefCell<Option<Rc<Retained<NativeMarkdownPagedScrollDocument>>>>,
    notifications: RefCell<Option<Rc<NotificationLease>>>,
    geometry: RefCell<Option<NativeMarkdownPagedViewportGeometry>>,
    height: Cell<f64>,
    epoch: Cell<u64>,
    busy: Cell<bool>,
    closed: Cell<bool>,
    poisoned: Cell<bool>,
    failure: RefCell<Option<NativeMarkdownPagedViewportError>>,
    off_main_notice: Arc<AtomicBool>,
    _main_thread: MainThreadMarker,
}

struct Updating<'a>(&'a Cell<bool>);
impl Drop for Updating<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

fn finite(rect: NSRect) -> bool {
    [
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
        rect.origin.x + rect.size.width,
        rect.origin.y + rect.size.height,
    ]
    .into_iter()
    .all(|n| n.is_finite() && n.abs() < (1u64 << 46) as f64)
        && rect.size.width > 0.0
        && rect.size.height > 0.0
}

fn step<T>(current: &(impl Fn() -> bool + ?Sized), operation: impl FnOnce() -> T) -> Result<T> {
    if !current() {
        return Err(NativeMarkdownPagedViewportError::Revoked);
    }
    let value = operation();
    if !current() {
        return Err(NativeMarkdownPagedViewportError::Revoked);
    }
    Ok(value)
}

impl NativeMarkdownPagedViewport {
    /// 调用方已用真实布局提供当前/下一页范围；沿原滚动容器的公开分页重叠配置滚动。
    ///
    /// # Errors
    /// 原挂载/输入失效、非法原生几何或clip未交付实际目标时失败；调用方不得重放。
    pub fn scroll_page(self: &Rc<Self>, down: bool, current: &dyn Fn() -> bool) -> Result<NSPoint> {
        if self.busy.get() {
            return Err(NativeMarkdownPagedViewportError::Busy);
        }
        let geometry = self.geometry()?;
        let scroll = step(current, || self.scroll.load())?
            .ok_or(NativeMarkdownPagedViewportError::Closed)?;
        let document = self
            .document
            .borrow()
            .as_ref()
            .cloned()
            .ok_or(NativeMarkdownPagedViewportError::Closed)?;
        if !step(current, || scroll.documentView())?
            .is_some_and(|actual| std::ptr::eq::<NSView>(&*actual, &***document))
        {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        let clip = step(current, || scroll.contentView())?;
        let before = step(current, || clip.documentVisibleRect())?;
        if before != geometry.document_visible || !geometry.is_current() {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        let overlap = step(current, || scroll.verticalPageScroll())?;
        let bounds = step(current, || clip.bounds())?;
        if !finite(bounds) || !overlap.is_finite() || overlap < 0.0 {
            return Err(NativeMarkdownPagedViewportError::InvalidGeometry);
        }
        // AppKit的pageScroll表示分页后保留的上下文高度，不是移动距离。使用原值而不改
        // 系统配置；原生文本动作的私有页距不属于这个空滚动文档的实现契约。
        let distance = (geometry.body_frame.size.height - overlap).max(0.0);
        let old = step(current, || {
            clip.convertPoint_fromView(geometry.body_frame.origin, Some(&***document))
        })?;
        let target = step(current, || {
            clip.convertPoint_fromView(
                NSPoint::new(
                    geometry.body_frame.origin.x,
                    geometry.body_frame.origin.y + if down { distance } else { -distance },
                ),
                Some(&***document),
            )
        })?;
        let mut proposed = bounds;
        proposed.origin.y += target.y - old.y;
        let constrained = step(current, || clip.constrainBoundsRect(proposed))?;
        if !finite(constrained) {
            return Err(NativeMarkdownPagedViewportError::InvalidGeometry);
        }
        if constrained.origin != bounds.origin {
            // 真实BOF/EOF由原clip按已安装的实际排版高度约束；同步通知可撤掉旧geometry，
            // 所以这里只核稳定输入票，不要求滚动前的nonce在原生调用后仍然有效。
            step(current, || clip.scrollToPoint(constrained.origin))?;
            step(current, || scroll.reflectScrolledClipView(&clip))?;
            self.synchronize()?;
        } // 边界空操作也仅消费这一条动作，不补发、不移动选择、不制造额外页面。
        let visible = step(current, || clip.documentVisibleRect())?;
        let actual = self.geometry()?;
        if actual.document_visible != visible || !actual.is_current() || !current() {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        Ok(actual.body_frame.origin) // 同步通知已发布实际正文原点，不能把兄弟覆盖区冒充可见Source。
    }

    /// 挂载只建立空高度文档与视口正文；调用方须持有并验证实际View/Context/层的构造账本。
    ///
    /// # Errors
    /// 已挂载、非法几何、原图被替换或同步Native回调撤权时拒绝，候选沿同一关闭边归还。
    pub fn attach(
        scroll: &NSScrollView,
        body: &NativeMarkdownPagedTextView,
        content_height: f64,
        mtm: MainThreadMarker,
    ) -> Result<Rc<Self>> {
        let current = || body.session_is_current();
        if step(&current, || scroll.documentView())?.is_some() {
            return Err(NativeMarkdownPagedViewportError::AlreadyMounted);
        }
        // SAFETY: 调用方持活主线程body；只读取公开parent关系，不能把别的图中的正文搬来。
        if step(&current, || unsafe { body.superview() })?.is_some() {
            return Err(NativeMarkdownPagedViewportError::AlreadyMounted);
        }
        let clip = step(&current, || scroll.contentView())?;
        let visible = step(&current, || clip.bounds())?;
        let height = content_height.max(visible.size.height);
        if !finite(visible)
            || !content_height.is_finite()
            || content_height <= 0.0
            || height >= (1u64 << 46) as f64
        {
            return Err(NativeMarkdownPagedViewportError::InvalidGeometry);
        }
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(visible.size.width, height));
        let document = step(&current, || {
            NativeMarkdownPagedScrollDocument::new(frame, mtm)
        })?;
        // 只有空高度容器开层；wantsUpdateLayer不生成全尺寸contents，正文由实际有限子层绘制。
        step(&current, || document.setWantsLayer(true))?;
        let owner = Rc::new(Self {
            scroll: Weak::from(scroll),
            body: Weak::from(body),
            document: RefCell::new(Some(Rc::new(document))),
            notifications: RefCell::new(None),
            geometry: RefCell::new(None),
            height: Cell::new(content_height),
            epoch: Cell::new(0),
            busy: Cell::new(false),
            closed: Cell::new(false),
            poisoned: Cell::new(false),
            failure: RefCell::new(None),
            off_main_notice: Arc::new(AtomicBool::new(false)),
            _main_thread: mtm,
        });
        let candidate = owner
            .document
            .borrow()
            .as_ref()
            .cloned()
            .ok_or(NativeMarkdownPagedViewportError::Closed)?;
        if step(&current, || scroll.documentView())?.is_some() {
            return Err(NativeMarkdownPagedViewportError::AlreadyMounted);
        }
        // 现代NSView默认可把visibleRect扩到父clip；有限正文必须实际裁在自身bounds内，
        // 否则兄弟ruler/header的覆盖区仍会作为可见正文或分数像素尾部交给绘制。
        step(&current, || body.setClipsToBounds(true))?;
        step(&current, || candidate.addSubview(body))?;
        step(&current, || scroll.setDocumentView(Some(&**candidate)))?;
        // 安装失败也不改变其他后继documentView；Drop只撤仍属于本候选的原关系。
        owner.observe(&clip)?;
        owner.synchronize()?;
        Ok(owner)
    }

    fn observe(self: &Rc<Self>, clip: &NSClipView) -> Result<()> {
        let center = NSNotificationCenter::defaultCenter();
        let mut lease = NotificationLease {
            clip: Weak::from(clip),
            bounds_before: clip.postsBoundsChangedNotifications(),
            frame_before: clip.postsFrameChangedNotifications(),
            tokens: Vec::with_capacity(2),
        };
        // SAFETY: 两个名称均为AppKit导出的进程静态NSString，只借用它们登记原clip的公开通知。
        let names = unsafe {
            [
                NSViewBoundsDidChangeNotification,
                NSViewFrameDidChangeNotification,
            ]
        };
        for name in names {
            let weak = MainThreadBound::new(Rc::downgrade(self), self._main_thread);
            let off_main = Arc::clone(&self.off_main_notice);
            let callback = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                let Some(mtm) = MainThreadMarker::new() else {
                    off_main.store(true, Ordering::Release);
                    return; // 外部代码可在其他线程伪发同名通知；不触碰Rc、Native或主线程状态。
                };
                if let Some(owner) = weak.get(mtm).upgrade() {
                    // 通知不携文字或事件；只立即撤旧geometry并核对本次真实clip，不保存待重放输入。
                    if let Err(error) = owner.synchronize() {
                        let mut slot = owner.failure.borrow_mut();
                        if slot.is_none() {
                            *slot = Some(error);
                        }
                    }
                }
            });
            // SAFETY: None queue沿发布线程同步调用；block的RcWeak由MainThreadBound保护，
            // 首先验证真实主线程，外来线程只能设置原子永久失败位，不能读取Native或Rc。
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    Some(clip),
                    None,
                    &callback,
                )
            };
            lease.tokens.push(token);
        }
        self.notifications.replace(Some(Rc::new(lease)));
        clip.setPostsBoundsChangedNotifications(true);
        clip.setPostsFrameChangedNotifications(true);
        Ok(())
    }

    /// 内容高度只能来自原实际布局owner；改变高度先撤票，再沿同一同步几何路径更新。
    ///
    /// # Errors
    /// 非有限、非正数、关闭或已被Native后继替换时拒绝；不会截短正文高度制造内存通过。
    pub fn set_content_height(self: &Rc<Self>, height: f64) -> Result<()> {
        if !height.is_finite() || height <= 0.0 || height >= (1u64 << 46) as f64 {
            return Err(NativeMarkdownPagedViewportError::InvalidGeometry);
        }
        if self.closed.get() {
            return Err(NativeMarkdownPagedViewportError::Closed);
        }
        if self.poisoned.get() {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        if self.busy.get() || self.epoch.get() == u64::MAX {
            // 同步重入和计数耗尽仍走原撤票出口，不能先执行新增的Native预滚动。
            self.height.set(height);
            return self.synchronize();
        }
        let geometry = self.geometry()?;
        let body = self
            .body
            .load()
            .ok_or(NativeMarkdownPagedViewportError::Closed)?;
        // 本次高度来自原布局输入；几何通知可以合法换票，新的正文/选择却不能借此续接旧锚点。
        let input_before = body.input_state()?;
        let current = || {
            !self.closed.get()
                && !self.poisoned.get()
                && !self.off_main_notice.load(Ordering::Acquire)
                && body.input_state().is_ok_and(|state| state == input_before)
        };
        let result = (|| {
            let scroll = step(&current, || self.scroll.load())?
                .ok_or(NativeMarkdownPagedViewportError::Closed)?;
            let clip = step(&current, || scroll.contentView())?;
            let bounds = step(&current, || clip.bounds())?;
            if !finite(bounds)
                || step(&current, || clip.documentVisibleRect())? != geometry.document_visible
                || !geometry.is_current()
            {
                return Err(NativeMarkdownPagedViewportError::InvalidGeometry);
            }
            let document = self
                .document
                .borrow()
                .as_ref()
                .cloned()
                .ok_or(NativeMarkdownPagedViewportError::Closed)?;
            if !step(&current, || scroll.documentView())?
                .is_some_and(|actual| std::ptr::eq::<NSView>(&*actual, &***document))
            {
                return Err(NativeMarkdownPagedViewportError::Revoked);
            }
            let frame = step(&current, || document.frame())?;
            if !finite(frame) {
                return Err(NativeMarkdownPagedViewportError::InvalidGeometry);
            }
            // 真实正文比clip少掉覆盖区；目标仍是原Source顶端，越过新尾端时才向上夹回。
            let maximum_y = (height - geometry.body_frame.size.height).max(0.0);
            let y = geometry.body_frame.origin.y.clamp(0.0, maximum_y);
            let reversed =
                step(&current, || clip.isFlipped())? != step(&current, || document.isFlipped())?;
            let frame_delta = if reversed {
                // 相反方向的父/子坐标以文档高端相接；改frame高度自身已移动Source原点。
                // 旧转换给出的目标位移必须同时计入这份真实frame差，不能把缩高算两次。
                height.max(bounds.size.height) - frame.size.height
            } else {
                0.0 // 同方向转换不依赖文档高端，只有原Source目标自身的位移。
            };
            let old = step(&current, || {
                clip.convertPoint_fromView(geometry.body_frame.origin, Some(&***document))
            })?;
            let target = step(&current, || {
                clip.convertPoint_fromView(
                    NSPoint::new(geometry.body_frame.origin.x, y),
                    Some(&***document),
                )
            })?;
            let mut proposed = bounds;
            proposed.origin.y += target.y - old.y + frame_delta;
            if proposed.origin != bounds.origin {
                // 先在旧文档中预滚动可用的部分，避免缩高时AppKit自行夹回越界origin而嵌套撤票。
                // 此约束只知道旧frame，不能把其结果当成新高度下锚点已经完成。
                let constrained = step(&current, || clip.constrainBoundsRect(proposed))?;
                if !finite(constrained) {
                    return Err(NativeMarkdownPagedViewportError::InvalidGeometry);
                }
                step(&current, || clip.scrollToPoint(constrained.origin))?;
                step(&current, || scroll.reflectScrolledClipView(&clip))?;
                if step(&current, || clip.bounds())? != constrained {
                    return Err(NativeMarkdownPagedViewportError::Revoked);
                }
            }
            // 原同步路径先安装真实新frame；所有旧Scene票随真实通知撤销，不沿旧票发布新正文。
            self.height.set(height);
            self.synchronize()?;
            let actual = self.geometry()?;
            if actual.body_frame.origin.y != y {
                // 非翻转clip在BOF增长时，目标可能超出旧frame允许的bounds；现在只消费真实
                // 新frame做一次原reveal，保同输入权限，不轮询或重放已失效的旧Scene请求。
                self.reveal_document_rect(
                    NSRect::new(
                        NSPoint::new(geometry.body_frame.origin.x, y),
                        NSSize::new(1.0, geometry.body_frame.size.height.min(height)),
                    ),
                    &current,
                )?;
            }
            if !current() || self.geometry()?.body_frame.origin.y != y {
                return Err(NativeMarkdownPagedViewportError::Revoked);
            }
            Ok(())
        })();
        // current中的布尔撤权不能吞掉真实Source首错；无论哪一Native步骤先返回，末尾仍取
        // 同一输入的typed健康结果。后继输入只使原高度操作失权，绝不替它重捕权限。
        let input_after = body.input_state()?;
        if input_after != input_before {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        result
    }

    /// 只滚到调用方已实际度量的正文矩形；此方法不定位Source、不读取文本或扩张内容高度。
    /// 返回true表示真实滚动改变了几何，调用方须结束旧请求并在下一续体领取新Scene票。
    pub fn reveal_document_rect(
        self: &Rc<Self>,
        rect: NSRect,
        current: &impl Fn() -> bool,
    ) -> Result<bool> {
        if !finite(rect)
            || rect.origin.y < 0.0
            || rect.origin.y + rect.size.height > self.height.get()
        {
            return Err(NativeMarkdownPagedViewportError::InvalidGeometry);
        }
        if self.busy.get() {
            return Err(NativeMarkdownPagedViewportError::Busy);
        }
        let geometry = self.geometry()?;
        let scroll = step(current, || self.scroll.load())?
            .ok_or(NativeMarkdownPagedViewportError::Closed)?;
        let document = self
            .document
            .borrow()
            .as_ref()
            .cloned()
            .ok_or(NativeMarkdownPagedViewportError::Closed)?;
        // 原Native关系在借用外核验；外方重挂scroll不能借本方法滚动另一个文档。
        if !step(current, || scroll.documentView())?
            .is_some_and(|actual| std::ptr::eq::<NSView>(&*actual, &***document))
        {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        let clip = step(current, || scroll.contentView())?;
        let exposed = step(current, || clip.documentVisibleRect())?;
        if exposed != geometry.document_visible || !geometry.is_current() {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        let visible = geometry.body_frame;
        let bottom = rect.origin.y + rect.size.height;
        let y = if rect.origin.y < visible.origin.y || rect.size.height > visible.size.height {
            rect.origin.y // 超高行也只把其真实首部放进可见区，不按字形数虚构缩放。
        } else if bottom > visible.origin.y + visible.size.height {
            bottom - visible.size.height
        } else {
            return Ok(false); // 原实际矩形已可见，不发冗余scroll或额外几何代次。
        };
        let y = y.clamp(0.0, (self.height.get() - visible.size.height).max(0.0));
        if y == visible.origin.y {
            return Ok(false);
        }
        // Source目标转换到实际clip再滚动；不把documentVisibleRect的原点误当成clip.bounds原点。
        // 原生约束负责真实边界；current只核稳定目标身份，这次滚动必然撤销旧geometry。
        let bounds = step(current, || clip.bounds())?;
        let old = step(current, || {
            clip.convertPoint_fromView(visible.origin, Some(&***document))
        })?;
        let target = step(current, || {
            clip.convertPoint_fromView(NSPoint::new(visible.origin.x, y), Some(&***document))
        })?;
        let mut proposed = bounds;
        proposed.origin.y += target.y - old.y;
        let constrained = step(current, || clip.constrainBoundsRect(proposed))?;
        if !finite(constrained) {
            return Err(NativeMarkdownPagedViewportError::InvalidGeometry);
        }
        step(current, || clip.scrollToPoint(constrained.origin))?;
        step(current, || scroll.reflectScrolledClipView(&clip))?;
        self.synchronize()?;
        if !current() {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        let actual = self.geometry()?;
        if actual.body_frame.origin.y != y {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        Ok(true)
    }

    /// 消费真实clip几何；正文frame只覆盖可见band，绝不沿完整逻辑高度扩张。
    ///
    /// # Errors
    /// 同步重入会撤销旧票并返回Busy，不能继续发布混合几何；原调用的末端也必须拒绝。
    pub fn synchronize(self: &Rc<Self>) -> Result<()> {
        if self.closed.get() {
            return Err(NativeMarkdownPagedViewportError::Closed);
        }
        if self.poisoned.get() {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        if self.off_main_notice.load(Ordering::Acquire) {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        let Some(epoch) = self.epoch.get().checked_add(1) else {
            self.geometry.borrow_mut().take();
            self.poisoned.set(true);
            return Err(NativeMarkdownPagedViewportError::CounterExhausted);
        }; // 耗尽时旧票必须失效；不能因无法计数而继续消费变化前的viewport。
        self.epoch.set(epoch);
        self.geometry.borrow_mut().take(); // 第一个可重入getter之前，所有后继消费先失去旧发布票。
        let body = self
            .body
            .load()
            .ok_or(NativeMarkdownPagedViewportError::Closed)?;
        body.invalidate_geometry()?;
        if self.busy.replace(true) {
            return Err(NativeMarkdownPagedViewportError::Busy);
        }
        let _updating = Updating(&self.busy);
        let pure_current = || {
            !self.closed.get()
                && !self.poisoned.get()
                && !self.off_main_notice.load(Ordering::Acquire)
                && self.epoch.get() == epoch
        };
        let current = || pure_current() && body.session_is_current() && pure_current();
        let checked = |value: bool| {
            if value && current() {
                Ok(())
            } else {
                Err(NativeMarkdownPagedViewportError::Revoked)
            }
        };
        let scroll = step(&current, || self.scroll.load())?
            .ok_or(NativeMarkdownPagedViewportError::Closed)?;
        let document = self
            .document
            .borrow()
            .as_ref()
            .cloned()
            .ok_or(NativeMarkdownPagedViewportError::Closed)?;
        checked(
            step(&current, || scroll.documentView())?
                .is_some_and(|actual| std::ptr::eq::<NSView>(&*actual, &***document)),
        )?;
        let clip = step(&current, || scroll.contentView())?;
        let before = step(&current, || clip.bounds())?;
        checked(finite(before))?;
        let document_frame = NSRect::new(
            NSPoint::ZERO,
            NSSize::new(before.size.width, self.height.get().max(before.size.height)),
        );
        if step(&current, || document.frame())? != document_frame {
            step(&current, || document.setFrame(document_frame))?;
        } // 精确相等时不发冗余setFrame，以免一条clip通知制造无关layout递归。
        let visible = step(&current, || clip.documentVisibleRect())?;
        checked(finite(visible))?;
        let bounds = step(&current, || clip.bounds())?;
        let insets = step(&current, || clip.contentInsets())?;
        let flipped = step(&current, || clip.isFlipped())?;
        checked(
            finite(bounds)
                && [insets.top, insets.left, insets.bottom, insets.right]
                    .into_iter()
                    .all(f64::is_finite),
        )?;
        // documentVisibleRect不会排除兄弟ruler/header。只用原clip实际inset求有效区；
        // top/bottom是视觉边，先按clip真实翻转选其最小Y侧，再由公开转换处理document翻转。
        let content = NSRect::new(
            NSPoint::new(
                bounds.origin.x + insets.left,
                bounds.origin.y + if flipped { insets.top } else { insets.bottom },
            ),
            NSSize::new(
                bounds.size.width - insets.left - insets.right,
                bounds.size.height - insets.top - insets.bottom,
            ),
        );
        checked(finite(content))?;
        let in_document = step(&current, || {
            document.convertRect_fromView(content, Some(&clip))
        })?;
        let body_frame = NSIntersectionRect(in_document, visible);
        checked(finite(body_frame))?;
        if step(&current, || body.frame())? != body_frame {
            step(&current, || body.setFrame(body_frame))?;
        } // 只移动或缩放有限正文；源的绝对Y由frame_in_parent和本receipt交给真实scene owner。
        let final_visible = step(&current, || clip.documentVisibleRect())?;
        checked(
            final_visible == visible
                && step(&current, || clip.bounds())? == bounds
                && step(&current, || clip.contentInsets())? == insets
                && step(&current, || clip.isFlipped())? == flipped
                && step(&current, || {
                    document.convertRect_fromView(content, Some(&clip))
                })? == in_document
                && step(&current, || body.frame())? == body_frame,
        )?;
        let body_bounds = step(&current, || body.bounds())?;
        checked(body_bounds == NSRect::new(NSPoint::ZERO, body_frame.size))?;
        // 本视图不承载整文坐标或隐式缩放；非零bounds/大逻辑画布必须被拒绝，不能发布伪小frame。
        // 发布前再核原Native关系；嵌套通知或后继挂载不能把原候选geometry补签成功。
        checked(
            step(&current, || scroll.documentView())?
                .is_some_and(|actual| std::ptr::eq::<NSView>(&*actual, &***document)),
        )?;
        let geometry = NativeMarkdownPagedViewportGeometry {
            owner: Rc::downgrade(self),
            nonce: Rc::new(()),
            document_visible: visible,
            body_frame,
            content_height: self.height.get(),
            scroll_epoch: epoch,
        };
        self.geometry.replace(Some(geometry));
        Ok(())
    }

    /// 只借当前已经核对的geometry；调用方不能用此getter推进布局或覆盖先前失败。
    pub fn geometry(&self) -> Result<NativeMarkdownPagedViewportGeometry> {
        if self.closed.get() {
            return Err(NativeMarkdownPagedViewportError::Closed);
        }
        if self.off_main_notice.load(Ordering::Acquire) {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        if self.poisoned.get() {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        let geometry = self
            .geometry
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedViewportError::Revoked)?;
        if !geometry.is_current() {
            return Err(NativeMarkdownPagedViewportError::Revoked);
        }
        Ok(geometry)
    }

    /// 供构造账本登记实际空容器；临时强claim必须在严格关闭核Weak之前归还。
    pub fn document_view(&self) -> Result<Retained<NativeMarkdownPagedScrollDocument>> {
        let claim = self
            .document
            .borrow()
            .as_ref()
            .cloned()
            .ok_or(NativeMarkdownPagedViewportError::Closed)?;
        Ok(Retained::clone(&claim))
    }

    pub fn take_failure(&self) -> Option<NativeMarkdownPagedViewportError> {
        self.failure.borrow_mut().take()
    }

    /// 原composition root在接受关闭前核对；同步几何栈未归还时不能提前拆图。
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy.get()
    }

    /// 构造账本只取得两个真实token的Weak，不持有observer、回调或容器载荷。
    ///
    /// # Errors
    /// 尚未观察、已经关闭或构造中撤销时拒绝；零个token不能冒充两个角色已释放。
    pub fn observer_weaks(&self) -> Result<[Weak<AnyObject>; 2]> {
        let lease = self
            .notifications
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedViewportError::Closed)?;
        if self.closed.get() || lease.tokens.len() != 2 {
            return Err(NativeMarkdownPagedViewportError::Closed);
        }
        // Rc clone已经结束槽借用；objc弱登记即使发生Native回调，也不能重入同一RefCell借用。
        let weaks = std::array::from_fn(|index| {
            Weak::from(
                <ProtocolObject<dyn NSObjectProtocol> as AsRef<AnyObject>>::as_ref(
                    &*lease.tokens[index],
                ),
            )
        });
        if self.closed.get() {
            return Err(NativeMarkdownPagedViewportError::Closed);
        }
        Ok(weaks)
    }

    /// 先封闭observer与geometry，再拆本候选的实际关系；不替代正文自己的close/Weak门。
    ///
    /// # Errors
    /// 正在更新时拒绝，交由原同步栈归还；不偷偷轮询、隐藏窗口或接受未完成释放。
    pub fn close(&self) -> Result<()> {
        if self.closed.get() {
            return Ok(());
        }
        if self.busy.get() {
            return Err(NativeMarkdownPagedViewportError::Busy);
        }
        self.closed.set(true);
        self.geometry.borrow_mut().take();
        let lease = self.notifications.borrow_mut().take();
        let document = self.document.borrow_mut().take();
        if let Some(lease) = lease {
            let center = NSNotificationCenter::defaultCenter();
            for token in &lease.tokens {
                // SAFETY: token来自本owner实际addObserver返回值，只撤本次监听，不按类型清全局通知。
                unsafe {
                    center.removeObserver(<ProtocolObject<dyn NSObjectProtocol> as AsRef<
                        AnyObject,
                    >>::as_ref(&**token));
                }
            }
            if let Some(clip) = lease.clip.load() {
                clip.setPostsBoundsChangedNotifications(lease.bounds_before);
                clip.setPostsFrameChangedNotifications(lease.frame_before);
            }
        } // 归还token及旧flags之后才拆图，迟到的本owner回调只能看到closed。
        if let Some(document) = document {
            if let Some(scroll) = self.scroll.load()
                && scroll
                    .documentView()
                    .is_some_and(|actual| std::ptr::eq::<NSView>(&*actual, &***document))
            {
                scroll.setDocumentView(None);
            }
            if let Some(body) = self.body.load()
                // SAFETY: 本地强claim贯穿公开getter；只核原parent，不假设Native图仍有本owner。
                && unsafe { body.superview() }
                    .is_some_and(|actual| std::ptr::eq::<NSView>(&*actual, &***document))
            {
                body.removeFromSuperview();
            } // 只断本次实际parent强边，不覆盖后继视图树中的正文位置。
        }
        Ok(())
    }
}

impl Drop for NativeMarkdownPagedViewport {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!("native_paged_viewport event=cleanup_failed error={error:?}");
        } // Drop是失败补偿；正式验收仍要显式close返回成功和逐角色实际Weak nil。
    }
}
