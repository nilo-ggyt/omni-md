//! Owns the transparent image view and public viewport proxy while preserving the original factory/input graph.

use super::*;
use objc2_app_kit::{
    NSTextLayoutFragment, NSTextViewportLayoutController, NSTextViewportLayoutControllerDelegate,
};
use objc2_core_graphics::CGImage;
use objc2_quartz_core::{CALayer, CATransaction, kCAContentsFormatRGBA8Uint, kCAGravityResize};

/// Balances begin/commit even after revocation; Drop unwinds Native transaction state without another write.
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
    // Every Native call can reenter; only the same original permit may authorize its result and successor.
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
    // SAFETY: This NSView publishes immutable CGImage contents through public layer APIs and owns no input state.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    pub(super) struct NativeMarkdownRasterImageView;

    impl NativeMarkdownRasterImageView {
        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool { true }
        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            // Contents were published under the original permit; AppKit updates do not read source
            // again or construct a CGContext display list from the body.
        }
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            None // Preserve body clicks, drag selection, table fields, and the original input-method client.
        }
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { false }
        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool { false }
        #[unsafe(method_id(accessibilityChildren))]
        fn accessibility_children(&self) -> Retained<NSArray<AnyObject>> {
            NSArray::new() // The original NSTextView owns accessible text/selection; publish no duplicate AX tree.
        }
    }
    // SAFETY: NSView supplies NSObject identity and main-thread constraints; this class preserves its ABI.
    unsafe impl NSObjectProtocol for NativeMarkdownRasterImageView {}
);

impl NativeMarkdownRasterImageView {
    pub(super) fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        let view = Self::alloc(mtm).set_ivars(());
        // SAFETY: Empty ivars are initialized before the public NSView designated initializer on main.
        unsafe { msg_send![super(view), initWithFrame: frame] }
    }

    pub(super) fn configure(&self, scale: f64) -> Result<(), String> {
        // Initial installation precedes callback publication. Later configuration uses the guarded
        // variant below so it cannot silently switch to a layer created by reentrant work.
        self.setWantsLayer(true);
        let layer = self.layer().ok_or("光栅图层尚未创建")?;
        layer.setContentsScale(scale);
        // SAFETY: QuartzCore exports static constants; configure only this project's viewport layer.
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
        // SAFETY: QuartzCore constants live statically; each setter consumes the same original permit.
        checked(current, || unsafe {
            layer.setContentsFormat(kCAContentsFormatRGBA8Uint)
        })?;
        // SAFETY: The static gravity constant applies only to the captured CALayer, without new authority.
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
        // Commit must balance begin even when a setter revokes authority and returns early.
        let mut transaction = LayerTransaction(true);
        if !current() {
            return Err("光栅事务开始后原票已撤权".into());
        }
        checked(current, || CATransaction::setDisableActions(true))?;
        // SAFETY: CALayer.contents publicly accepts CGImage. This stack owns a valid image and
        // the layer retains its CF ownership. Bridge only for this setter, without arbitrary
        // NSObject messages or a separately allocated pixel copy.
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
        // SAFETY: This layer receives only supported CGImage/None values. Compare getter identity;
        // do not send arbitrary NSObject messages to CF images or accept a successor image as ours.
        let contents = checked(current, || unsafe { layer.contents() })?;
        let matches = contents.as_deref().is_some_and(|actual| {
            std::ptr::eq(
                std::ptr::from_ref(actual),
                std::ptr::from_ref(image).cast::<AnyObject>(),
            )
        });
        // Returning the temporary Native claim may reenter; recheck the original permit afterwards.
        drop(contents);
        if !matches || !current() {
            return Err("实际图层内容不是本次同票 CGImage".into());
        }
        Ok(())
    }

    pub(super) fn clear(&self) {
        if let Some(layer) = self.layer() {
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            // SAFETY: None returns this layer's retained CGImage only, without touching source/selection layers.
            unsafe { layer.setContents(None) };
            CATransaction::commit();
        }
        // Remove only this view; later table fields and other siblings keep their existing ordering.
        self.removeFromSuperview();
    }
}

pub(super) struct ViewportProxyIvars {
    // Weak back-links avoid retaining the editor graph through the delegate. Scalar counters can
    // outlive teardown to report failures, but cannot grant late callbacks fresh business authority.
    owner: std::rc::Weak<NativeMarkdownRasterSurface>,
    counters: Arc<ResourceCounters>,
    viewport: Weak<NSTextViewportLayoutController>,
    original: Weak<ProtocolObject<dyn NSTextViewportLayoutControllerDelegate>>,
    received_layout_calls: Cell<u64>,
    forward_configure: bool,
    forward_configure_calls: Cell<u64>,
}

define_class!(
    // SAFETY: Keep the four existing viewport methods; the newer notification has runtime availability/responds checks.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ViewportProxyIvars]
    pub(super) struct NativeMarkdownRasterViewportProxy;
    // SAFETY: NSObject identity is preserved; ivars hold weak links/atomics and Native access stays on main.
    unsafe impl NSObjectProtocol for NativeMarkdownRasterViewportProxy {}
    impl NativeMarkdownRasterViewportProxy {
        #[unsafe(method(textViewportLayoutControllerReceivedSetNeedsLayout:))]
        fn received_set_needs_layout(&self, viewport: &NSTextViewportLayoutController) {
            // Consume actual system callbacks only, without creating events, reflow, or polling.
            // Sequence exhaustion fails permanently instead of wrapping to an earlier diagnostic identity.
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
                            // SAFETY: The macOS 27 header declares void plus one viewport argument.
                            // Availability, the original delegate's selector response, and the same
                            // OriginPermit are checked before this single call to that actual object.
                            unsafe { let _: () = msg_send![original, textViewportLayoutControllerReceivedSetNeedsLayout: viewport]; }
                            returned = true;
                        })?;
                    }
                    Ok(())
                }).is_some()
            } else {
                false // Earlier systems do not call the unavailable selector; existing protocol methods still apply.
            };
            if sequence <= 8 {
                eprintln!("native_raster_surface event=received_set_needs_layout sequence={sequence} available={available} responds_checked={responds_checked} original_responds={responds} issued={issued} returned={returned} completed={completed} formal=false");
            }
        }
    }
    // SAFETY: Signatures match the public protocol. Default configuration registers original fragments;
    // only the explicit comparison forwards that same configure call. Other methods forward under one permit.
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
                // Count only delivered callbacks. Check exhaustion even beyond the fixed log budget;
                // wrapping cannot manufacture another valid sequence.
                let Some(sequence) = self.ivars().forward_configure_calls.get().checked_add(1) else {
                    self.ivars().counters.fail(); return;
                };
                self.ivars().forward_configure_calls.set(sequence);
                Some(sequence)
            } else {
                None // Without the construction-time switch, this diagnostic does not request a Native glyph surface.
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
                    // Framework fragments use the weak registry; their actual business values enter
                    // the original owned ledger and must still reach actual nil during resource acceptance.
                    owner.register_fragment(fragment, current)?;
                    value_observation::capture_paragraph(&owner.ledger, fragment, current)?;
                    if self.ivars().forward_configure {
                        // The original NSTextView may synchronously change geometry during configuration.
                        // Call the public required method once under the same permit; revocation cannot
                        // renew it or repeat configuration against a successor state.
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
            // Viewport layout can change fragment coordinates without a frame notification.
            // Revoke older geometry before invoke captures this callback's permit, so this event
            // does not immediately invalidate the authority it has just acquired.
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
            // Completed Native layout is another actual geometry boundary: revoke permits captured
            // during layout before forwarding the original did-layout callback.
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
        // Read the exact comparison switch once for this proxy. Later frames/environment changes
        // cannot switch rendering paths inside an existing session.
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
        // SAFETY: All weak/atomic ivars are initialized before NSObject's public initializer on main.
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
            // Certify the callback's exact viewport, original manager, and still-installed proxy.
            // Equal dimensions or matching document scalars do not prove this delegate relationship.
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
        // Temporary Native claim destruction can revoke the permit; recheck after the whole local scope.
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
            return None; // Close sealed authority; a late callback cannot restart the old delegate/layout.
        }
        let Some(owner) = self.ivars().owner.upgrade() else {
            // Losing the owner while the proxy is active is structural failure. Do not switch
            // graphs or fall back to the original GPU configuration path.
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
        // Keep original payloads live through this synchronous callback. The permit is captured
        // only after these original graph upgrades, and no borrow crosses the forwarded Native call.
        let permit = match NativeMarkdownRasterOriginPermit::begin(&owner, &view) {
            Ok(Some(permit)) => permit,
            Ok(None) => {
                self.trace_failure(kind, "original_permit_unavailable");
                return None; // Unready construction or legitimate revocation cancels without a successor permit.
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
            return None; // Legitimate revocation is not renewed; a later real Native event owns its own operation.
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
        // Emit at most eight callback/reason diagnostics without document contents. Logging neither
        // certifies completion nor arranges another event or retry.
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
        // Load the delegate to restore before the final installed-proxy check; do not upgrade another
        // weak object after that check and accidentally overwrite a relation changed by reentry.
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
        // Setter return alone does not prove restoration; require the exact original live delegate.
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
