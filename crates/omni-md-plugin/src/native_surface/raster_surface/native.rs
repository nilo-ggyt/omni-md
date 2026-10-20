//! 自有透明视图和公开viewport代理；标准factory/fragment及原输入图保持原身份。

use super::*;
use objc2_app_kit::{
    NSTextLayoutFragment, NSTextViewportLayoutController, NSTextViewportLayoutControllerDelegate,
};
use objc2_core_graphics::CGImage;
use objc2_quartz_core::{CALayer, CATransaction, kCAContentsFormatRGBA8Uint, kCAGravityResize};

/// 事务撤权后仍须配平 begin/commit；此析构只恢复 Native 栈，不再发布像素或修改图层。
struct LayerTransaction(bool);
impl Drop for LayerTransaction {
    fn drop(&mut self) {
        if self.0 {
            CATransaction::commit();
        }
    }
}

fn checked<T>(
    current: &(impl Fn() -> bool + ?Sized),
    work: impl FnOnce() -> T,
) -> Result<T, String> {
    if !current() {
        return Err("光栅原票在 Native 调用前撤权".into());
    }
    let result = work();
    if !current() {
        return Err("光栅原票在 Native 调用后撤权".into());
    }
    Ok(result)
}

define_class!(
    // SAFETY: 自有 NSView 只经公开 layer API 发布不可变 CGImage；不持业务载荷或输入回调。
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    pub(super) struct NativeMarkdownRasterImageView;

    impl NativeMarkdownRasterImageView {
        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool { true }
        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            // contents 已由同票发布；AppKit 更新时不重新读正文或生成 CGContext 显示列表。
        }
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            None // 透明图像不能截获点击、拖选、表格字段或输入法的原生交互。
        }
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { false }
        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool { false }
        #[unsafe(method_id(accessibilityChildren))]
        fn accessibility_children(&self) -> Retained<NSArray<AnyObject>> {
            NSArray::new() // 原 NSTextView 继续独占可访问文本与选择，图像不制造第二份 AX 内容。
        }
    }
    // SAFETY: NSObject 身份和主线程约束由 NSView 继承，类不改变对象 ABI。
    unsafe impl NSObjectProtocol for NativeMarkdownRasterImageView {}
);

impl NativeMarkdownRasterImageView {
    pub(super) fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        let view = Self::alloc(mtm).set_ivars(());
        // SAFETY: 空 ivars 已初始化，仅在主线程调用公开 NSView designated initializer。
        unsafe { msg_send![super(view), initWithFrame: frame] }
    }

    pub(super) fn configure(&self, scale: f64) -> Result<(), String> {
        self.setWantsLayer(true);
        let layer = self.layer().ok_or("光栅图层尚未创建")?;
        layer.setContentsScale(scale);
        // SAFETY: QuartzCore 导出常量具有静态生命周期；只配置本项目创建的唯一视口层。
        unsafe {
            layer.setContentsFormat(kCAContentsFormatRGBA8Uint);
            layer.setContentsGravity(kCAGravityResize);
        }
        Ok(())
    }

    pub(super) fn configure_current(
        &self,
        scale: f64,
        layer: &CALayer,
        current: &impl Fn() -> bool,
    ) -> Result<(), String> {
        if !checked(current, || self.layer())?
            .is_some_and(|actual| std::ptr::eq::<CALayer>(&*actual, layer))
        {
            return Err("光栅配置时 CALayer 身份改变".into());
        }
        checked(current, || layer.setContentsScale(scale))?;
        // SAFETY: QuartzCore 常量静态存活，且每一个 setter 都受同一原始操作票约束。
        checked(current, || unsafe {
            layer.setContentsFormat(kCAContentsFormatRGBA8Uint)
        })?;
        // SAFETY: 同上，常量仅用于本项目的固定 CALayer，不创建较新图层或重新领取权限。
        checked(current, || unsafe {
            layer.setContentsGravity(kCAGravityResize)
        })?;
        Ok(())
    }

    pub(super) fn publish(
        &self,
        image: &CGImage,
        layer: &CALayer,
        current: &impl Fn() -> bool,
    ) -> Result<(), String> {
        if !checked(current, || self.layer())?
            .is_some_and(|actual| std::ptr::eq::<CALayer>(&*actual, layer))
        {
            return Err("光栅发布的固定 CALayer 已被替换".into());
        }
        if !current() {
            return Err("光栅事务开始前原票已撤权".into());
        }
        CATransaction::begin();
        let mut transaction = LayerTransaction(true);
        if !current() {
            return Err("光栅事务开始后原票已撤权".into());
        }
        checked(current, || CATransaction::setDisableActions(true))?;
        // SAFETY: CALayer.contents 公开接受 CGImage；本同步调用持有有效 image，layer 自行
        // 保留其 CF 所有权。桥接只用于该 setter，不发送其他 NSObject 消息或建立像素副本。
        checked(current, || unsafe {
            let object = &*std::ptr::from_ref(image).cast::<AnyObject>();
            layer.setContents(Some(object));
        })?;
        if !current() {
            return Err("光栅像素设置后原票已撤权".into());
        }
        transaction.0 = false;
        CATransaction::commit();
        if !current() {
            return Err("光栅事务提交后原票已撤权".into());
        }
        if !checked(current, || self.layer())?
            .is_some_and(|actual| std::ptr::eq::<CALayer>(&*actual, layer))
        {
            return Err("光栅事务返回后的 CALayer 身份改变".into());
        }
        // SAFETY: 此项目图层只接收公开允许的 CGImage/None；只比较 getter 返回对象的身份，
        // 不向 CF 图像发送任意 NSObject 消息或把另一个图像冒认为本次发布值。
        let contents = checked(current, || unsafe { layer.contents() })?;
        let matches = contents.as_deref().is_some_and(|actual| {
            std::ptr::eq(
                std::ptr::from_ref(actual),
                std::ptr::from_ref(image).cast::<AnyObject>(),
            )
        });
        drop(contents); // 临时 Native claim 也可能重入；归还之后再核同一原票。
        if !matches || !current() {
            return Err("实际图层内容不是本次同票 CGImage".into());
        }
        Ok(())
    }

    pub(super) fn clear(&self) {
        if let Some(layer) = self.layer() {
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            // SAFETY: 只清除本层 contents；None 归还层持有的 CGImage，不触碰原正文或选择层。
            unsafe { layer.setContents(None) };
            CATransaction::commit();
        }
        self.removeFromSuperview(); // 原 table field 等较晚挂载的兄弟不重排、不移除。
    }
}

pub(super) struct ViewportProxyIvars {
    owner: std::rc::Weak<NativeMarkdownRasterSurface>,
    counters: Arc<ResourceCounters>,
    viewport: Weak<NSTextViewportLayoutController>,
    original: Weak<ProtocolObject<dyn NSTextViewportLayoutControllerDelegate>>,
    received_layout_calls: Cell<u64>,
    forward_configure: bool,
    forward_configure_calls: Cell<u64>,
}

define_class!(
    // SAFETY: 保留macOS12已有的四项viewport协议；27新增公开通知另有availability和原responds检查。
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ViewportProxyIvars]
    pub(super) struct NativeMarkdownRasterViewportProxy;
    // SAFETY: 类保留NSObject身份，ivars仅Weak回链和独立原子收据，Native访问固定在主线程。
    unsafe impl NSObjectProtocol for NativeMarkdownRasterViewportProxy {}
    impl NativeMarkdownRasterViewportProxy {
        #[unsafe(method(textViewportLayoutControllerReceivedSetNeedsLayout:))]
        fn received_set_needs_layout(&self, viewport: &NSTextViewportLayoutController) {
            // 只消费系统交付的实际回调，不自行发事件、重排或定时查询；计数耗尽永久失败而不回绕。
            let Some(sequence) = self.ivars().received_layout_calls.get().checked_add(1) else {
                self.ivars().counters.fail(); return;
            };
            self.ivars().received_layout_calls.set(sequence);
            let available = objc2::available!(macos = 27.0);
            let mut responds_checked = false;
            let mut responds = false;
            let mut issued = false;
            let mut returned = false;
            let completed = if available {
                self.invoke("received_layout", viewport, |_owner, _view, _permit, original, current| {
                    responds = checked(current, || original.respondsToSelector(sel!(textViewportLayoutControllerReceivedSetNeedsLayout:)))?;
                    responds_checked = true;
                    if responds {
                        checked(current, || {
                            issued = true;
                            // SAFETY: 当前macOS27公开header声明void+一个viewport参数；runtime availability、
                            // 原delegate responds和同一OriginPermit均已核实，只调用实际响应对象一次。
                            unsafe { let _: () = msg_send![original, textViewportLayoutControllerReceivedSetNeedsLayout: viewport]; }
                            returned = true;
                        })?;
                    }
                    Ok(())
                }).is_some()
            } else {
                false // macOS14不调用尚未公开的原selector；既有四项协议路径保持原样。
            };
            if sequence <= 8 {
                eprintln!("native_raster_surface event=received_set_needs_layout sequence={sequence} available={available} responds_checked={responds_checked} original_responds={responds} issued={issued} returned={returned} completed={completed} formal=false");
            }
        }
    }
    // SAFETY: 每个参数/返回值匹配绑定的公开协议；默认配置只登记原fragment，显式因果对照
    // 才向原delegate转交同一实际配置，其他入口始终同票转原delegate。
    unsafe impl NSTextViewportLayoutControllerDelegate for NativeMarkdownRasterViewportProxy {
        #[unsafe(method(viewportBoundsForTextViewportLayoutController:))]
        fn viewport_bounds(&self, viewport: &NSTextViewportLayoutController) -> NSRect {
            self.invoke(
                "bounds",
                viewport,
                |_owner, _view, _permit, original, current| {
                    let bounds = checked(current, || {
                        original.viewportBoundsForTextViewportLayoutController(viewport)
                    })?;
                    if !geometry::finite(bounds) {
                        return Err("原viewport delegate返回非有限bounds".into());
                    }
                    Ok(bounds)
                },
            )
            .unwrap_or(NSRect::ZERO)
        }

        #[unsafe(method(textViewportLayoutController:configureRenderingSurfaceForTextLayoutFragment:))]
        fn configure_rendering_surface(
            &self,
            viewport: &NSTextViewportLayoutController,
            fragment: &NSTextLayoutFragment,
        ) {
            let sequence = if self.ivars().forward_configure {
                // 仅对真实交付回调计数；固定日志预算之外仍核耗尽，不回绕生成伪序号。
                let Some(sequence) = self.ivars().forward_configure_calls.get().checked_add(1) else {
                    self.ivars().counters.fail(); return;
                };
                self.ivars().forward_configure_calls.set(sequence);
                Some(sequence)
            } else {
                None // 构造时未显式启用对照，既有诊断路径不取得原生glyph surface。
            };
            let mut issued = false;
            let mut returned = false;
            let completed = self.invoke(
                "configure",
                viewport,
                |owner, _view, _permit, original, current| {
                    let actual = checked(current, || fragment.textLayoutManager())?;
                    let expected = checked(current, || owner.graph.manager.load())?;
                    if !actual
                        .as_deref()
                        .zip(expected.as_deref())
                        .is_some_and(|(a, b)| std::ptr::eq::<NSTextLayoutManager>(a, b))
                    {
                        return Err("viewport配置fragment不属于同一原manager".into());
                    }
                    // 标准fragment不进自有对象账本；实际业务值进入同一原ledger，仍必须真实nil。
                    owner.register_fragment(fragment, current)?;
                    value_observation::capture_paragraph(&owner.ledger, fragment, current)?;
                    if self.ivars().forward_configure {
                        // 原NSTextView的配置副作用可能同步调整几何；只调用公开协议required方法
                        // 一次，前后仍消费原OriginPermit，撤权后不补票或重新配置。
                        checked(current, || {
                            issued = true;
                            original.textViewportLayoutController_configureRenderingSurfaceForTextLayoutFragment(viewport, fragment);
                            returned = true;
                        })?;
                    }
                    owner
                        .counters
                        .configured_fragments
                        .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                            count.checked_add(1)
                        })
                        .map_err(|_| "viewport实际配置计数耗尽".to_owned())?;
                    Ok(())
                },
            ).is_some();
            if let Some(sequence) = sequence.filter(|sequence| *sequence <= 8) {
                eprintln!("native_raster_surface event=forward_configure sequence={sequence} issued={issued} returned={returned} completed={completed} formal=false");
            }
        }

        #[unsafe(method(textViewportLayoutControllerWillLayout:))]
        fn will_layout(&self, viewport: &NSTextViewportLayoutController) {
            // 真正viewport布局可只改fragment坐标而不触发view frame通知；先撤旧几何票，
            // 再由invoke取得本回调自己的原permit，不能让回调把自己新领的票立即作废。
            if let Some(owner) = self.ivars().owner.upgrade() {
                owner.invalidate_geometry();
            }
            self.invoke(
                "will",
                viewport,
                |_owner, _view, _permit, original, current| {
                    if checked(current, || {
                        original.respondsToSelector(sel!(textViewportLayoutControllerWillLayout:))
                    })? {
                        checked(current, || {
                            original.textViewportLayoutControllerWillLayout(viewport)
                        })?;
                    }
                    Ok(())
                },
            );
        }

        #[unsafe(method(textViewportLayoutControllerDidLayout:))]
        fn did_layout(&self, viewport: &NSTextViewportLayoutController) {
            // 原生布局已完成也是真实几何边界；废止布局中领取的旧票后，才转交原did回调。
            if let Some(owner) = self.ivars().owner.upgrade() {
                owner.invalidate_geometry();
            }
            self.invoke(
                "did",
                viewport,
                |_owner, _view, _permit, original, current| {
                    if checked(current, || {
                        original.respondsToSelector(sel!(textViewportLayoutControllerDidLayout:))
                    })? {
                        checked(current, || {
                            original.textViewportLayoutControllerDidLayout(viewport)
                        })?;
                    }
                    Ok(())
                },
            );
        }
    }
);

impl NativeMarkdownRasterViewportProxy {
    pub(super) fn new(
        owner: &Rc<NativeMarkdownRasterSurface>,
        viewport: &NSTextViewportLayoutController,
        original: &ProtocolObject<dyn NSTextViewportLayoutControllerDelegate>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        // 只在该session代理构造时读取一次精确开关；不会随帧或环境后续变化切换渲染路径。
        let forward_configure = std::env::var_os("OMNI_NATIVE_RASTER_FORWARD_CONFIGURE")
            .is_some_and(|value| value == "1");
        let proxy = Self::alloc(mtm).set_ivars(ViewportProxyIvars {
            owner: Rc::downgrade(owner),
            counters: Arc::clone(&owner.counters),
            viewport: Weak::from(viewport),
            original: Weak::from(original),
            received_layout_calls: Cell::new(0),
            forward_configure,
            forward_configure_calls: Cell::new(0),
        });
        // SAFETY: 所有Weak/原子ivars已初始化，主线程调用NSObject公开初始化器。
        unsafe { msg_send![super(proxy), init] }
    }

    fn callback_current(
        &self,
        owner: &NativeMarkdownRasterSurface,
        view: &PieceBackedMarkdownTextView,
        permit: &NativeMarkdownRasterOriginPermit,
        viewport: &NSTextViewportLayoutController,
    ) -> bool {
        if !permit.current(owner, view) {
            return false;
        }
        let result = (|| -> Result<bool, String> {
            let actual = permit.step(owner, view, || self.ivars().viewport.load())?;
            if !actual.as_deref().is_some_and(|actual| {
                std::ptr::eq::<NSTextViewportLayoutController>(actual, viewport)
            }) {
                return Ok(false);
            }
            let manager = permit.step(owner, view, || viewport.textLayoutManager())?;
            let original = permit.step(owner, view, || owner.graph.manager.load())?;
            if !manager
                .as_deref()
                .zip(original.as_deref())
                .is_some_and(|(a, b)| std::ptr::eq::<NSTextLayoutManager>(a, b))
            {
                return Ok(false);
            }
            let delegate = permit.step(owner, view, || viewport.delegate())?;
            Ok(delegate.as_deref().is_some_and(|delegate| {
                std::ptr::eq(
                        std::ptr::from_ref::<
                            ProtocolObject<dyn NSTextViewportLayoutControllerDelegate>,
                        >(delegate)
                        .cast::<AnyObject>(),
                        std::ptr::from_ref(self).cast::<AnyObject>(),
                    )
            }))
        })();
        // 临时Native claim归还也可能撤票，整个检查结束后必须仍为原请求。
        result.unwrap_or(false) && permit.current(owner, view)
    }

    fn invoke<T>(
        &self,
        kind: &'static str,
        viewport: &NSTextViewportLayoutController,
        work: impl FnOnce(
            &NativeMarkdownRasterSurface,
            &PieceBackedMarkdownTextView,
            &NativeMarkdownRasterOriginPermit,
            &ProtocolObject<dyn NSTextViewportLayoutControllerDelegate>,
            &dyn Fn() -> bool,
        ) -> Result<T, String>,
    ) -> Option<T> {
        if self.ivars().counters.closed.load(Ordering::Acquire) {
            return None; // 原关闭已封业务权；迟到viewport回调不重启原delegate或旧布局。
        }
        let Some(owner) = self.ivars().owner.upgrade() else {
            // 活动代理失去原owner属于结构失败，不能转入另一个图或原GPU配置路径。
            self.ivars().counters.fail();
            return None;
        };
        let _operation = OperationGuard::new(Arc::clone(&owner.counters));
        let Some(view) = owner.graph.view.load() else {
            owner.counters.fail();
            return None;
        };
        let _view_payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        let Some(storage) = owner.graph.storage.load() else {
            owner.counters.fail();
            return None;
        };
        let _storage_payload = PayloadUseGuard::enter(&storage.ivars().payload_use_depth);
        let permit = match NativeMarkdownRasterOriginPermit::begin(&owner, &view) {
            Ok(Some(permit)) => permit,
            Ok(None) => {
                self.trace_failure(kind, "original_permit_unavailable");
                return None; // 构造未就绪或原票合法撤销，只取消，不领取后继业务票。
            }
            Err(error) => {
                self.trace_failure(kind, &error);
                owner.counters.fail();
                return None;
            }
        };
        let current = || self.callback_current(&owner, &view, &permit, viewport);
        let result: Result<T, String> = (|| {
            if !current() {
                return Err("viewport代理原关系失效".into());
            }
            let original = checked(&current, || self.ivars().original.load())?
                .ok_or("viewport原delegate已释放")?;
            let result = work(&owner, &view, &permit, &original, &current)?;
            drop(original);
            if !current() {
                return Err("viewport代理返回时原关系失效".into());
            }
            Ok(result)
        })();
        if !permit.current(&owner, &view) {
            return None; // 正常同步撤权不被重签；后继真实Native事件自行发起新的独立操作。
        }
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.trace_failure(kind, &error);
                owner.counters.fail();
                None
            }
        }
    }

    fn trace_failure(&self, kind: &'static str, reason: &str) {
        // 总共最多八条固定回调/无正文原因；这不是完成收据，也不安排后继事件或重试。
        if self
            .ivars()
            .counters
            .proxy_failure_logs
            .fetch_add(1, Ordering::AcqRel)
            < 8
        {
            eprintln!(
                "native_raster_surface event=proxy_unavailable callback={kind} reason={reason} formal=false"
            );
        }
    }

    pub(super) fn restore_original_delegate(&self) -> Result<(), String> {
        let viewport = self
            .ivars()
            .viewport
            .load()
            .ok_or("撤proxy时原viewport已释放")?;
        // 先取得要恢复的原claim，随后最后一次核仍挂本proxy；不能在这个核验后再Weak升级别的对象。
        let original = self.ivars().original.load();
        let actual = viewport.delegate();
        let matches = actual.as_deref().is_some_and(|delegate| {
            std::ptr::eq(
                std::ptr::from_ref::<ProtocolObject<dyn NSTextViewportLayoutControllerDelegate>>(
                    delegate,
                )
                .cast::<AnyObject>(),
                std::ptr::from_ref(self).cast::<AnyObject>(),
            )
        });
        if !matches {
            return Err("撤proxy时原delegate关系已被替换".into());
        }
        viewport.setDelegate(original.as_deref());
        let restored = viewport.delegate();
        if !restored
            .as_deref()
            .zip(original.as_deref())
            .is_some_and(|(a, b)| {
                std::ptr::eq::<ProtocolObject<dyn NSTextViewportLayoutControllerDelegate>>(a, b)
            })
        {
            return Err("撤proxy后未恢复同一原delegate".into());
        }
        Ok(())
    }
}
