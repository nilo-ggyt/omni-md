//! 实际有限字形画布与大逻辑高度的独立机制控制；不声明整篇排版或完整编辑对等。

#[path = "paged_close/event.rs"]
mod paged_close_event;

use std::{
    cell::{Cell, RefCell},
    io::Write,
    rc::{Rc, Weak as RcWeak},
};

use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::{Retained, Weak, autoreleasepool},
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSClipView,
    NSGraphicsContext, NSScrollView, NSTextInputClient, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CFRunLoop, CFString, CGPoint, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextGetBitsPerPixel, CGBitmapContextGetBytesPerRow,
    CGBitmapContextGetData, CGBitmapContextGetHeight, CGBitmapContextGetWidth, CGColorSpace,
    CGContext, CGImageAlphaInfo, CGImageByteOrderInfo, kCGColorSpaceSRGB,
};
use objc2_core_text::CTFont;
use objc2_foundation::{
    MainThreadMarker, NSEdgeInsets, NSNotFound, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize,
};
use objc2_quartz_core::CATransaction;
use omni_markdown_core::{
    MarkdownDocumentBuffer, MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownShapedSpool, MarkdownShapedSpoolAuthority,
    MarkdownShapedSpoolBuilder, MarkdownShapingBudget, MarkdownShapingDirection,
    MarkdownShapingJob, MarkdownShapingOptions, MarkdownShapingStatus, MarkdownTextSelection,
};
use omni_md_plugin::native_surface::{
    paged_fonts::NativeMarkdownPagedFont,
    paged_glyph_paint::{NativeMarkdownPagedGlyphOrigin, NativeMarkdownPagedGlyphPaintRequest},
    paged_input::{NativeMarkdownPagedInput, NativeMarkdownPagedInputCommit},
    paged_text_view::{
        NativeMarkdownPagedCharacterRect, NativeMarkdownPagedEditCommand,
        NativeMarkdownPagedEditPlan, NativeMarkdownPagedNavigation,
        NativeMarkdownPagedRenderStrategy, NativeMarkdownPagedSceneKey,
        NativeMarkdownPagedSceneNonce, NativeMarkdownPagedScenePaintBudget,
        NativeMarkdownPagedScenePaintCompletion, NativeMarkdownPagedScenePaintOutcome,
        NativeMarkdownPagedScenePaintSession, NativeMarkdownPagedScenePaintStep,
        NativeMarkdownPagedScenePaintWork, NativeMarkdownPagedSceneRequest,
        NativeMarkdownPagedTextScene, NativeMarkdownPagedTextView,
        NativeMarkdownPagedTextViewDelegate, NativeMarkdownPagedTextViewError as Error,
        NativeMarkdownPagedVirtualViewport,
    },
    paged_viewport::{NativeMarkdownPagedViewport, NativeMarkdownPagedViewportGeometry},
};

#[path = "paged_viewport/metrics.rs"]
mod metrics;

const WIDTH: usize = 640;
const HEIGHT: usize = 320;
// 与用户提供的2274×1492设备像素截图对应；保持2倍实际backing，不降采样制造通过。
const LARGE_WIDTH: usize = 1137;
const LARGE_HEIGHT: usize = 746;
const EPOCH: u64 = 7;
const LINE: &str = "Omni finite viewport office AV";

define_class!(
    // SAFETY: 真实主线程NSClipView只多一个翻转标量，无正文、回调或第二业务owner。
    #[unsafe(super(NSClipView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = bool]
    struct ViewportProbeClip;

    // SAFETY: NSObject身份和全部滚动实现仍沿原NSClipView，测试仅控制公开坐标方向。
    unsafe impl NSObjectProtocol for ViewportProbeClip {}

    impl ViewportProbeClip {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool { *self.ivars() }
    }
);

impl ViewportProbeClip {
    /// 坐标方向在父初始化前固定；不在AppKit缓存转换后悄悄改变isFlipped返回值。
    fn new(frame: NSRect, flipped: bool, mtm: MainThreadMarker) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(flipped);
        // SAFETY: 不变的坐标方向已在父初始化前安装；有限frame不携正文或外部借用。
        unsafe { msg_send![super(allocated), initWithFrame: frame] }
    }
}

/// 同一个真实sealed输出供本有限绘图控制消费；不提供全段/软行坐标或假命中结果。
struct PaintPayload {
    spool: RefCell<Option<MarkdownShapedSpool>>,
    output: MarkdownShapedSpoolAuthority,
    source: MarkdownLayoutTextAuthority,
    font_authority: MarkdownLayoutFontAuthority,
    font: NativeMarkdownPagedFont,
    glyphs: usize,
    paints: Cell<usize>,
    graphics_within_bound: Cell<bool>,
    process_within_bound: Cell<bool>,
    frames_complete: Cell<bool>,
    strategy: NativeMarkdownPagedRenderStrategy,
    bitmap_paints: Cell<usize>,
    bitmap_ink: Cell<usize>,
}

struct Scene {
    key: NativeMarkdownPagedSceneKey,
    _nonce: NativeMarkdownPagedSceneNonce,
    geometry: NativeMarkdownPagedViewportGeometry,
    payload: Rc<PaintPayload>,
}

impl NativeMarkdownPagedTextScene for Scene {
    fn key(&self) -> &NativeMarkdownPagedSceneKey {
        &self.key
    }
    fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<(), Error> {
        let check = || -> Result<(), Error> {
            let mut slot = self
                .payload
                .spool
                .try_borrow_mut()
                .map_err(|_| Error::Pending)?;
            let spool = slot.as_mut().ok_or(Error::Pending)?;
            let count = spool.glyph_count();
            let actual_font = self.payload.font.core_font().authority();
            for font in [&self.payload.font_authority, &actual_font] {
                spool
                    .read_batch(
                        &self.payload.output,
                        &self.key.source,
                        font,
                        count..count,
                        &mut [],
                    )
                    .map_err(|cause| match cause {
                        omni_markdown_core::MarkdownShapedSpoolError::Source(cause)
                        | omni_markdown_core::MarkdownShapedSpoolError::Shaping(
                            omni_markdown_core::MarkdownShapingError::Source(cause),
                        ) => Error::Document(cause),
                        other => Error::ShapedSpool(other),
                    })?;
            } // 空批仍实际核原输出和两个字体身份，不能以相同名字或bool续权。
            if !self.payload.font.accepts_shaping_size(spool.points()) {
                return Err(Error::InvalidGeometry);
            }
            Ok(())
        };
        let before = current();
        check()?; // Source实际首错优先于布尔撤权；借用在任何可重入current之前结束。
        let after = current();
        check()?;
        if !before || !after {
            return Err(Error::Revoked);
        }
        Ok(())
    }
    fn first_rect(&self, _range: NSRange) -> Result<NativeMarkdownPagedCharacterRect, Error> {
        Err(Error::Pending)
    }
    fn hit(&self, _local: NSPoint) -> Result<usize, Error> {
        Err(Error::Pending)
    }
    fn mouse_selection(
        &self,
        _local: NSPoint,
        _clicks: usize,
        _anchor: Option<usize>,
        _extend: bool,
    ) -> Result<
        (
            MarkdownTextSelection,
            omni_md_plugin::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity,
        ),
        Error,
    > {
        Err(Error::Pending)
    }
    fn navigate(
        &self,
        _action: NativeMarkdownPagedNavigation,
        _extend: bool,
        _context: &omni_md_plugin::native_surface::paged_text_view::NativeMarkdownPagedNavigationContext,
    ) -> Result<
        omni_md_plugin::native_surface::paged_text_view::NativeMarkdownPagedNavigationTarget,
        Error,
    > {
        Err(Error::Pending)
    }
    fn edit_plan(
        &self,
        _action: NativeMarkdownPagedEditCommand,
    ) -> Result<NativeMarkdownPagedEditPlan, Error> {
        Err(Error::Pending)
    }
    fn begin_cpu_paint(
        self: Rc<Self>,
        context: CFRetained<CGContext>,
        current: &dyn Fn() -> bool,
    ) -> Result<Box<dyn NativeMarkdownPagedScenePaintSession>, Error> {
        self.validate_ready(current)?;
        Ok(Box::new(FiniteSession {
            scene: self,
            context,
        }))
    }
    fn paint(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<(), Error> {
        self.paint_native(context, current).map(drop)
    }
}

impl Scene {
    fn paint_native(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedScenePaintCompletion, Error> {
        let valid = || current() && self.geometry.is_current();
        if !valid() {
            return Err(Error::Revoked);
        }
        // Native绘图前先把原spool移到同步栈，不能让RefCell借用跨CTFont回调。
        // 重入只会看到Pending，不会领取另一个文件或让外层票通过新输出补签。
        let mut spool = self
            .payload
            .spool
            .borrow_mut()
            .take()
            .ok_or(Error::Pending)?;
        // 这里只验证本地有限画布的真实ink；相同首行在不同视口中作为合成ink探针，
        // 不能解释为整篇文稿在这些Y坐标的布局或选择已经完成。
        CGContext::save_g_state(Some(context));
        CGContext::set_rgb_fill_color(Some(context), 1.0, 1.0, 1.0, 1.0);
        CGContext::fill_rect(Some(context), self.key.request.viewport);
        CGContext::set_rgb_fill_color(Some(context), 0.0, 0.0, 0.0, 1.0);
        let result = NativeMarkdownPagedScenePaintCompletion::paint_finite_spool(
            &mut spool,
            &self.payload.output,
            NativeMarkdownPagedGlyphPaintRequest {
                source: &self.payload.source,
                font_authority: &self.payload.font_authority,
                native_font: &self.payload.font,
                glyph_range: 0..self.payload.glyphs,
                origin: NativeMarkdownPagedGlyphOrigin {
                    baseline: CGPoint { x: 16.0, y: 48.0 },
                    pen_x_26_6: 0,
                    pen_y_26_6: 0,
                },
            },
            &self.key,
            context,
            &valid,
        );
        assert!(self.payload.spool.borrow_mut().replace(spool).is_none());
        CGContext::restore_g_state(Some(context)); // 错误也配平本层状态，不将部分候选pixels签成成功。
        let (completion, receipt) = result.map_err(|error| {
            eprintln!("paged_viewport painter_error={error:?}");
            error
        })?;
        if receipt.glyph_range != (0..self.payload.glyphs) || receipt.missing_glyphs != 0 {
            return Err(Error::InvalidGeometry); // 真实输出不完整时由原view记录失败，不能在ObjC绘图回调内panic。
        }
        if !valid() {
            return Err(Error::Revoked);
        }
        if self.payload.strategy == NativeMarkdownPagedRenderStrategy::CpuViewport {
            // 此模式的实际scene只收到生产CPU surface的完整有限RGBA8 context；不读layer.contents。
            // 指针只在本同步回调内读取，检查公开几何和行跨度后立即归还，不缓存或复制像素。
            let data = CGBitmapContextGetData(Some(context));
            let width = CGBitmapContextGetWidth(Some(context));
            let height = CGBitmapContextGetHeight(Some(context));
            let row = CGBitmapContextGetBytesPerRow(Some(context));
            if data.is_null()
                || CGBitmapContextGetBitsPerPixel(Some(context)) != 32
                || !(1..=LARGE_WIDTH * 4).contains(&width)
                || !(1..=LARGE_HEIGHT * 4).contains(&height)
                || width.checked_mul(4).is_none_or(|active| active > row)
            {
                return Err(Error::InvalidGeometry); // 未证明真实内存格式时不能生成raw slice，也不能把无墨迹当成功。
            }
            let bytes = row.checked_mul(height).ok_or(Error::InvalidGeometry)?;
            if bytes > LARGE_WIDTH * LARGE_HEIGHT * 4 * 16 {
                return Err(Error::InvalidGeometry);
            }
            if !valid() {
                return Err(Error::Revoked);
            }
            CGContext::flush(Some(context));
            if !valid() {
                return Err(Error::Revoked);
            }
            let active = width.checked_mul(4).ok_or(Error::InvalidGeometry)?;
            let mut ink = 0usize;
            for row_index in 0..height {
                let offset = row_index.checked_mul(row).ok_or(Error::InvalidGeometry)?;
                // SAFETY: 原生产Bitmap在本回调全程持有真实stride和tile可见矩形；
                // 上方核active<=row与完整有限跨度，这里只读本行width个RGBA，绝不读取行padding。
                let pixels =
                    unsafe { std::slice::from_raw_parts(data.cast::<u8>().add(offset), active) };
                let row_ink = pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|pixel| pixel[0].min(pixel[1]).min(pixel[2]) < 250)
                    .count();
                ink = ink.checked_add(row_ink).ok_or(Error::InvalidGeometry)?;
            }
            // 同一个scene可能分多tile交付；空白tile是合法的，只有本stage全部真实绘制都无墨迹才失败。
            self.payload.bitmap_ink.set(
                self.payload
                    .bitmap_ink
                    .get()
                    .checked_add(ink)
                    .ok_or(Error::InvalidGeometry)?,
            );
            self.payload
                .bitmap_paints
                .set(self.payload.bitmap_paints.get() + 1);
        }
        if !valid() {
            return Err(Error::Revoked);
        }
        self.payload.paints.set(self.payload.paints.get() + 1);
        Ok(completion)
    }
}
/// 只消费原合成spool完整≤256glyph；既不调用任意scene.paint，也不建立长文回退。
struct FiniteSession {
    scene: Rc<Scene>,
    context: CFRetained<CGContext>,
}
impl NativeMarkdownPagedScenePaintSession for FiniteSession {
    fn step(
        self: Box<Self>,
        budget: NativeMarkdownPagedScenePaintBudget,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedScenePaintStep, Error> {
        self.scene.validate_ready(current)?;
        let count = self.scene.payload.glyphs;
        if count > budget.glyphs.min(256) || budget.metadata_records == 0 {
            return Ok(NativeMarkdownPagedScenePaintStep {
                work: NativeMarkdownPagedScenePaintWork::default(),
                outcome: NativeMarkdownPagedScenePaintOutcome::Pending(self),
            });
        } // 没有本轮真实预算时保原游标，不偷偷同步draw或重新分配。
        let completion = self.scene.paint_native(&self.context, current)?;
        let work = completion.work();
        Ok(NativeMarkdownPagedScenePaintStep {
            work,
            outcome: NativeMarkdownPagedScenePaintOutcome::Complete(completion),
        })
    }
}

struct Delegate {
    active: Cell<bool>,
    viewport: RefCell<RcWeak<NativeMarkdownPagedViewport>>,
    scene: RefCell<Option<Rc<Scene>>>,
}
impl NativeMarkdownPagedTextViewDelegate for Delegate {
    fn current(&self, epoch: u64) -> bool {
        self.active.get() && epoch == EPOCH
    }
    fn virtual_viewport(
        &self,
        epoch: u64,
        _bounds_width: f64,
    ) -> Result<NativeMarkdownPagedVirtualViewport, Error> {
        if !self.current(epoch) {
            return Err(Error::Revoked);
        }
        let owner = self.viewport.borrow().upgrade().ok_or(Error::Pending)?;
        let geometry = owner.geometry().map_err(|_| Error::Pending)?;
        Ok(NativeMarkdownPagedVirtualViewport {
            document_origin: geometry.body_frame.origin,
            layout_origin_x: geometry.layout_origin_x,
            layout_width: geometry.layout_width,
            scroll_epoch: geometry.scroll_epoch,
        })
    }
    fn ready_scene(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Rc<dyn NativeMarkdownPagedTextScene>, Error> {
        let scene = self.scene.borrow().clone().ok_or(Error::Pending)?;
        if !self.current(EPOCH) || !scene.geometry.is_current() || !scene.key.request.same(request)
        {
            return Err(Error::Pending);
        }
        Ok(scene)
    }
    fn scene_current(&self, key: &NativeMarkdownPagedSceneKey) -> bool {
        self.active.get()
            && self
                .scene
                .borrow()
                .as_ref()
                .is_some_and(|scene| scene.key.same(key) && scene.geometry.is_current())
    }
    fn committed(&self, _commit: &NativeMarkdownPagedInputCommit) -> Result<(), Error> {
        Err(Error::Pending)
    }
    fn reveal_committed_selection(&self, _: &NativeMarkdownPagedInputCommit) -> Result<(), Error> {
        // 本有限组件没有正文Producer/reveal续体，不能把选择提交伪装成实际可见。
        Err(Error::Pending)
    }
    fn copy_selection(&self, _request: &NativeMarkdownPagedSceneRequest) -> Result<(), Error> {
        Err(Error::Pending)
    }
}

struct Canvas {
    context: CFRetained<CGContext>,
    pixels: Vec<u8>,
}
impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        assert!(width <= LARGE_WIDTH && height <= LARGE_HEIGHT); // 只接受本测试预声明的实际viewport上界。
        let mut pixels = vec![255; width * height * 4];
        // SAFETY: 框架固定sRGB常量自进程启动可用；不读取设备名或用户图片。
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap();
        // SAFETY: 完整有限RGBA8 allocation在context存活期不resize；字段顺序保证context先Drop。
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * 4,
                Some(&space),
                CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
            )
        }
        .unwrap();
        Self { context, pixels }
    }
    fn ink(&self) -> usize {
        CGContext::flush(Some(&self.context));
        self.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0].min(pixel[1]).min(pixel[2]) < 250)
            .count()
    }
}

fn deliver_once() {
    // SAFETY: 固定公开default mode；一次有限交付，绝不轮询直到成功或用延时替代收据。
    unsafe {
        CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 0.05, false);
    }
}

fn expected_pending(error: Option<Error>, payload: &PaintPayload) {
    if let Some(error) = error {
        let expected = matches!(error, Error::Pending | Error::Revoked);
        if !expected {
            payload.frames_complete.set(false);
        }
        println!(
            "paged_viewport prior_scene_status={error:?} expected_pending={expected} old_pixels_not_replayed=true formal=false"
        );
    } // 非预期失败保持sticky，仍走真实关闭后统一拒收；不能提前panic丢失资源收据。
}

fn paint_stage(
    stage: &str,
    height: f64,
    view: &NativeMarkdownPagedTextView,
    owner: &Rc<NativeMarkdownPagedViewport>,
    delegate: &Rc<Delegate>,
    payload: &Rc<PaintPayload>,
) -> NativeMarkdownPagedViewportGeometry {
    assert!(owner.take_failure().is_none(), "原clip通知发生真实失败");
    let geometry = owner.geometry().unwrap();
    assert!(geometry.is_current());
    let request = view.capture_scene_request().unwrap();
    assert_eq!(request.frame_in_parent, geometry.body_frame);
    assert_eq!(view.frame(), geometry.body_frame);
    assert_eq!(
        request.virtual_viewport.document_origin,
        geometry.body_frame.origin
    );
    assert_eq!(
        view.bounds(),
        NSRect::new(NSPoint::ZERO, geometry.body_frame.size)
    );
    assert_eq!(view.visibleRect().size, geometry.body_frame.size);
    assert!(
        view.frame().size.width <= LARGE_WIDTH as f64
            && view.frame().size.height <= LARGE_HEIGHT as f64
    );
    expected_pending(view.take_failure(), payload); // 初始挂载/原票撤销期间的Pending不当作glyph Ready。
    let nonce = NativeMarkdownPagedSceneNonce::new();
    let scene = Rc::new(Scene {
        key: NativeMarkdownPagedSceneKey {
            presentation_mode: omni_markdown_core::MarkdownPresentationMode::Source,
            request,
            source: payload.source.clone(),
            nonce: nonce.authority(),
        },
        _nonce: nonce,
        geometry: geometry.clone(),
        payload: Rc::clone(payload),
    });
    payload.bitmap_ink.set(0); // 清除上一stage的真实采样，不能拿旧viewport墨迹给新scene补签。
    let obsolete = delegate.scene.replace(Some(scene));
    drop(obsolete);
    view.scene_ready().unwrap();
    metrics::sample("scene_published", height);
    let before = payload.paints.get();
    let bitmap_before = payload.bitmap_paints.get();
    let cpu_before = view.cpu_paints();
    let canvas = match payload.strategy {
        NativeMarkdownPagedRenderStrategy::Direct => {
            let width = view.bounds().size.width as usize;
            let actual_height = view.bounds().size.height as usize;
            let canvas = Canvas::new(width, actual_height);
            let graphics =
                NSGraphicsContext::graphicsContextWithCGContext_flipped(&canvas.context, false);
            metrics::sample("before_explicit_draw", height);
            view.displayRectIgnoringOpacity_inContext(view.bounds(), &graphics);
            metrics::sample("after_explicit_draw", height);
            Some(canvas)
        }
        NativeMarkdownPagedRenderStrategy::CpuViewport => {
            // 只让原view公开display入口消费已经发布的同票scene；不直接调用updateLayer或另画一份。
            metrics::sample("before_view_display", height);
            view.displayIfNeeded();
            metrics::sample("after_view_display", height);
            None
        }
    };
    let layer = view.layer().expect("空shim下已有实际body layer");
    assert_eq!(layer.bounds().size, view.bounds().size);
    assert!(layer.bounds().size.height < height);
    println!(
        "paged_viewport stage={stage} body_frame={:?} body_bounds={:?} actual_layer_bounds={:?} scale={} update_layer={} strategy={:?} formal=false",
        view.frame(),
        view.bounds(),
        layer.bounds(),
        layer.contentsScale(),
        view.wantsUpdateLayer(),
        payload.strategy
    );
    deliver_once(); // 系统可额外绘图，完整累计paint次数，不宣称只有一次系统绘制。
    if let Some(error) = view.take_failure() {
        payload.frames_complete.set(false);
        println!(
            "paged_viewport stage={stage} actual_frame_failed={error:?} complete_frame=false cleanup_still_required=true formal=false"
        );
        let snapshot = metrics::sample("failed_frame_before_cleanup", height);
        payload
            .process_within_bound
            .set(payload.process_within_bound.get() && snapshot.process_within_bound);
        payload.graphics_within_bound.set(
            payload.graphics_within_bound.get() && (0..100_000_000).contains(&snapshot.graphics),
        );
        return geometry; // 原viewport几何仍可用于撤票检查，不能把失败候选称作已经显示的frame。
    }
    assert!(
        payload.paints.get() > before,
        "没有实际sealed spool绘制返回"
    );
    let ink = match canvas {
        Some(canvas) => canvas.ink(),
        None => {
            assert!(
                payload.bitmap_paints.get() > bitmap_before,
                "没有实际CPU bitmap字形墨迹收据"
            );
            assert!(
                view.cpu_paints()
                    .zip(cpu_before)
                    .is_some_and(|(after, before)| after > before),
                "没有真实同票CGImage发布"
            );
            payload.bitmap_ink.get()
        }
    };
    assert!(ink > 0, "有限viewport必须出现真实字体墨迹");
    assert!(
        geometry.is_current(),
        "系统交付撤销原几何，不能沿旧scene签成功"
    );
    assert!(owner.take_failure().is_none());
    expected_pending(view.take_failure(), payload);
    let snapshot = metrics::sample(stage, height);
    // 内存失败保持sticky，但仍执行后续几何与真实关闭，最终统一拒收，不能因早panic丢释放证据。
    if !(0..100_000_000).contains(&snapshot.graphics) {
        payload.graphics_within_bound.set(false);
    }
    if !snapshot.process_within_bound {
        payload.process_within_bound.set(false);
    }
    println!(
        "paged_viewport stage={stage} virtual_height={height} frame={:?} ink={ink} actual_paints={} geometry_current=true softline_geometry_ready=false whole_product=false formal=false",
        view.frame(),
        payload.paints.get()
    );
    geometry
}

/// 同原Scene的公开重绘必须核健康并复用完整像素，不产生候选或重复glyph提交。
fn same_scene_repaint(view: &NativeMarkdownPagedTextView, payload: &PaintPayload, stage: &str) {
    if payload.strategy != NativeMarkdownPagedRenderStrategy::CpuViewport {
        return;
    }
    let observer = view.pixel_observer().unwrap();
    let pixels = observer.snapshot();
    let paints = view.cpu_paints().unwrap();
    let reuses = view.cpu_reuses().unwrap();
    let glyphs = payload.bitmap_paints.get();
    view.setNeedsDisplay(true);
    view.displayIfNeeded();
    deliver_once();
    let failure = view.take_failure();
    let after = observer.snapshot();
    let reused = failure.is_none()
        && view.cpu_paints() == Some(paints)
        && view.cpu_reuses().is_some_and(|value| value > reuses)
        && payload.bitmap_paints.get() == glyphs
        && after.peak_reserved_pixel_bytes == pixels.peak_reserved_pixel_bytes
        && after.reserved_pixel_bytes <= pixels.reserved_pixel_bytes;
    if !reused {
        payload.frames_complete.set(false);
    }
    println!(
        "paged_viewport stage={stage} same_scene_real_repaint=true validated_complete_reuse={reused} no_candidate_growth={} no_extra_glyphs={} failure={failure:?} formal=false",
        after.peak_reserved_pixel_bytes == pixels.peak_reserved_pixel_bytes,
        payload.bitmap_paints.get() == glyphs
    );
}

/// 故意非法读原spool使它真实poison并close；同key缓存仍须返回原typed错误，不能画旧字形冒成功。
fn poisoned_spool_refuses_reuse(view: &NativeMarkdownPagedTextView, payload: &PaintPayload) {
    if payload.strategy != NativeMarkdownPagedRenderStrategy::CpuViewport
        || !payload.frames_complete.get()
    {
        return;
    }
    let paints = view.cpu_paints();
    let reuses = view.cpu_reuses();
    let pixels = view.pixel_observer().unwrap().snapshot();
    let observer = {
        let mut slot = payload.spool.borrow_mut();
        let spool = slot.as_mut().unwrap();
        let observer = spool.resource_observer();
        let end = spool.glyph_count().checked_add(1).unwrap();
        assert!(matches!(
            spool.read_batch(
                &payload.output,
                &payload.source,
                &payload.font_authority,
                end..end,
                &mut []
            ),
            Err(omni_markdown_core::MarkdownShapedSpoolError::InvalidRequest)
        ));
        assert_eq!(
            spool.status(),
            omni_markdown_core::MarkdownShapedSpoolStatus::Poisoned
        );
        observer
    }; // 原spool借用已结束；后继Native绘图沿原健康门观察实际失效，未伪造Source EIO。
    assert!(
        !observer.snapshot().file_open,
        "原spool非法读取必须已真实归还FD"
    );
    view.setNeedsDisplay(true);
    view.displayIfNeeded();
    deliver_once();
    let failure = view.take_failure();
    let typed = matches!(
        failure,
        Some(Error::ShapedSpool(
            omni_markdown_core::MarkdownShapedSpoolError::Poisoned
        ))
    );
    let after = view.pixel_observer().unwrap().snapshot();
    let rejected = typed
        && view.cpu_paints() == paints
        && view.cpu_reuses() == reuses
        && after.peak_reserved_pixel_bytes == pixels.peak_reserved_pixel_bytes;
    if !rejected {
        payload.frames_complete.set(false);
    }
    println!(
        "paged_viewport real_spool_poison=true original_fd_closed={} same_key_rejected={rejected} actual_failure={failure:?} source_eio_not_injected=true formal=false",
        !observer.snapshot().file_open
    );
}

fn weak(value: &AnyObject) -> Weak<AnyObject> {
    Weak::from(value)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    #[cfg(feature = "native-markdown-runtime-probe")]
    if first.as_deref() == Some("mask-slots") {
        assert!(args.next().is_none(), "遮罩负控不接受其它工作负载参数");
        let mtm = MainThreadMarker::new().expect("遮罩图必须在真实主线程");
        NSApplication::sharedApplication(mtm)
            .setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        metrics::sample("before_mask_slots", 0.0);
        let result = NativeMarkdownPagedTextView::probe_cpu_mask_slots(mtm);
        let memory = metrics::sample("after_mask_slots", 0.0);
        result.expect("真实遮罩混合图、后继关系或资源门失败");
        assert!(
            memory.process_within_bound,
            "遮罩独立组件实际峰值未通过100MB"
        );
        return;
    }
    #[cfg(feature = "native-markdown-runtime-probe")]
    if first.as_deref() == Some("installation") {
        assert!(args.next().is_none(), "安装负控不接受其它工作负载参数");
        let mtm = MainThreadMarker::new().expect("安装负控必须在真实主线程");
        NSApplication::sharedApplication(mtm)
            .setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        metrics::sample("before_installation_controls", 0.0);
        let result = NativeMarkdownPagedTextView::probe_cpu_installation_rollback(mtm);
        let memory = metrics::sample("after_installation_controls", 0.0);
        result.expect("实际未发布安装的撤权或资源负控失败");
        assert!(
            memory.process_within_bound,
            "失败构造独立进程的实际峰值仍须低于100MB"
        );
        return; // 独立测完整lifetime峰；不先暖起正常默认/截图窗口的框架或字体资源。
    }
    let height = match first.as_deref() {
        None | Some("6728") => 6728.0,
        Some("1000000") => 1_000_000.0,
        Some("520") => 520.0,
        _ => panic!("仅接受520、6728或1000000三种固定机制场景"),
    };
    let mut strategy = NativeMarkdownPagedRenderStrategy::Direct;
    let flush_transaction = match args.next().as_deref() {
        None => false,
        Some("flush") => true,
        Some(mode @ ("direct" | "cpu")) => {
            if mode == "cpu" {
                strategy = NativeMarkdownPagedRenderStrategy::CpuViewport;
            }
            match args.next().as_deref() {
                None => false,
                Some("flush") => true,
                _ => panic!("绘图策略后只接受公开CATransaction flush单变量"),
            }
        }
        _ => panic!("只接受direct/cpu绘图策略或公开flush单变量"),
    };
    assert!(args.next().is_none());
    let (width, viewport_height) = match std::env::var("OMNI_NATIVE_VIEWPORT_SIZE") {
        Err(std::env::VarError::NotPresent) => (WIDTH, HEIGHT),
        Ok(value) if value == "screenshot" => (LARGE_WIDTH, LARGE_HEIGHT),
        // 环境变量仅选择本合成回归的已声明几何；错误值不能悄悄退回小窗口。
        other => panic!("未知的原生视口验收尺寸：{other:?}"),
    };
    let mtm = MainThreadMarker::new().expect("视口机制控制必须在真实主线程");
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    // 每种坐标方向都从初始化开始保持不变；前一真实图通过全部释放门后才建立下一图，
    // 不把动态篡改isFlipped造成的转换缓存失配误判成生产视口失败。
    for flipped in [true, false] {
        run_case(
            height,
            strategy,
            flush_transaction,
            width,
            viewport_height,
            flipped,
            mtm,
        );
    }
}

/// 原工作负载、绘图及释放断言逐方向完整执行；两次调用不共享Doc、Input、窗口或Source。
fn run_case(
    height: f64,
    strategy: NativeMarkdownPagedRenderStrategy,
    flush_transaction: bool,
    width: usize,
    viewport_height: usize,
    flipped: bool,
    mtm: MainThreadMarker,
) {
    println!("paged_viewport fixture_clip_flipped={flipped} immutable_from_init=true formal=false");
    metrics::sample("before_graph", height);
    let (
        native,
        mapping,
        source_observer,
        font_bytes,
        spool_observer,
        document_weak,
        input_weak,
        viewport_weak,
        graphics_within_bound,
        process_within_bound,
        frames_complete,
        pixels,
    ) = autoreleasepool(|_| {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        // 源文件显著大于4KiB；固定小片段流式写入夹具，未创建全源String/UTF16数组。
        for _ in 0..4096 {
            file.write_all(LINE.as_bytes()).unwrap();
            file.write_all(b"\n").unwrap();
        }
        let document = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(file.path(), 1024 * 1024).unwrap(),
        ));
        let mapping = document.borrow().original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document.borrow());
        // 固定阶段只读本进程分类，分开字体/窗口/实际绘图的增量，不把时序相关当作因果。
        metrics::sample("source_captured", height);
        let source_observer = source.resource_observer();
        let name = CFString::from_str("Helvetica");
        // SAFETY: 固定有效名字和32pt，None矩阵没有借用外部存储。
        let ct = unsafe { CTFont::with_name(&name, 32.0, std::ptr::null()) };
        let font = NativeMarkdownPagedFont::capture(&ct, 512 * 1024 * 1024, mtm, &|| true).unwrap();
        let font_bytes = font.bytes_observer();
        metrics::sample("font_captured", height);
        let sa = source.authority();
        let fa = font.core_font().authority();
        let mut job = MarkdownShapingJob::new(
            source.clone(),
            &sa,
            font.core_font().clone(),
            &fa,
            0..LINE.len(),
            MarkdownShapingOptions {
                direction: MarkdownShapingDirection::LeftToRight,
                script: *b"Latn",
                language: None,
                points: 32.0,
                beginning_of_text: true,
                end_of_text: false,
                features: Vec::new(),
            },
        )
        .unwrap();
        while job.status() == MarkdownShapingStatus::Ingesting {
            job.step(
                &sa,
                &fa,
                MarkdownShapingBudget {
                    bytes: 256,
                    scalars: 64,
                },
            )
            .unwrap();
        }
        assert_eq!(job.shape(&sa, &fa).unwrap(), MarkdownShapingStatus::Ready);
        let profile = job.profile().unwrap();
        assert!((1..=256).contains(&profile.glyph_count));
        assert!(profile.source_borrow_max_bytes <= 4096 && profile.shaper_access_certified);
        let job_resources = job.resource_observer();
        let mut builder = MarkdownShapedSpoolBuilder::new(&mut job, &sa, &fa).unwrap();
        assert_eq!(
            builder.step(&mut job, 256).unwrap().glyphs,
            profile.glyph_count
        );
        let spool = builder.seal(&mut job).unwrap();
        let spool_observer = spool.resource_observer();
        let output = spool.authority();
        drop(job);
        // seal正当地保留原Source/Font；这里只要求可变shaper自己的FD/映射已经真实归还。
        let shaper_closed = job_resources.snapshot();
        assert_eq!(
            (shaper_closed.files_open, shaper_closed.mappings_open),
            (0, 0)
        );
        assert!(!shaper_closed.cleanup_failed);
        println!(
            "paged_viewport source_bytes={} shaped_source_end={} source_borrow_max_bytes={} real_sealed_spool=true glyphs={} actual_document_layout=false formal=false",
            source.byte_len(),
            LINE.len(),
            profile.source_borrow_max_bytes,
            profile.glyph_count
        );
        let payload = Rc::new(PaintPayload {
            spool: RefCell::new(Some(spool)),
            output,
            source: sa,
            font_authority: fa,
            font,
            glyphs: profile.glyph_count,
            paints: Cell::new(0),
            graphics_within_bound: Cell::new(true),
            process_within_bound: Cell::new(true),
            frames_complete: Cell::new(true),
            strategy,
            bitmap_paints: Cell::new(0),
            bitmap_ink: Cell::new(0),
        });
        metrics::sample("spool_sealed", height);
        let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document)).unwrap();
        metrics::sample("input_created", height);
        let input_weak = Rc::downgrade(&input);
        let document_weak = Rc::downgrade(&document);
        let delegate = Rc::new(Delegate {
            active: Cell::new(true),
            viewport: RefCell::new(RcWeak::new()),
            scene: RefCell::new(None),
        });
        let erased: Rc<dyn NativeMarkdownPagedTextViewDelegate> = delegate.clone();
        let frame = NSRect::new(
            NSPoint::ZERO,
            NSSize::new(width as f64, viewport_height as f64),
        );
        let body = NativeMarkdownPagedTextView::new_with_render_strategy(
            input.clone(),
            Rc::downgrade(&erased),
            EPOCH,
            frame,
            strategy,
            mtm,
        )
        .unwrap();
        let pixels = body.pixel_observer();
        assert_eq!(
            pixels.is_some(),
            strategy == NativeMarkdownPagedRenderStrategy::CpuViewport
        );
        metrics::sample("body_created", height);
        let context = body.inputContext().unwrap();
        metrics::sample("context_obtained", height);
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll.setHasVerticalScroller(false); // 机制控制不添加任何用户交互；clip公开API执行三次真实滚动。
        let controlled_clip = ViewportProbeClip::new(frame, flipped, mtm);
        scroll.setContentView(&controlled_clip); // 原fixture只换真实clip的方向控制，正文、Input与Source均不复制。
        // SAFETY: 当前主线程创建唯一有限borderless合成窗口，backing使用公开buffered，无用户数据。
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: Retained显式持有窗口直到close之后；禁止框架再额外release同一owning引用。
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        metrics::sample("window_created", height);
        window.setContentView(Some(&scroll));
        let viewport = NativeMarkdownPagedViewport::attach(&scroll, &body, height, mtm).unwrap();
        metrics::sample("viewport_attached", height);
        let viewport_weak = Rc::downgrade(&viewport);
        *delegate.viewport.borrow_mut() = Rc::downgrade(&viewport);
        let shim = viewport.document_view().unwrap();
        let observers = viewport.observer_weaks().unwrap();
        let clip = scroll.contentView();
        window.orderFrontRegardless();
        metrics::sample("window_ordered", height);
        window.displayIfNeeded();
        metrics::sample("window_displayed", height);
        deliver_once();
        metrics::sample("window_delivered", height);
        let initial = paint_stage("top", height, &body, &viewport, &delegate, &payload);
        same_scene_repaint(&body, &payload, "top_repaint");
        if std::env::var_os("OMNI_NATIVE_VIEWPORT_INSPECT").is_some() {
            use std::io::Read;
            // 仅离线诊断按明确stdin事件暂停一次，供同UID vmmap读取实际分配；不轮询Native或等待Weak变空。
            println!("paged_viewport inspection_ready=true");
            std::io::stdin().read_exact(&mut [0u8; 1]).unwrap();
        }
        let body_layer = body.layer().unwrap();
        let shim_layer = shim.layer().unwrap();
        let mut native = vec![
            ("body", weak(&body)),
            ("context", weak(&context)),
            ("shim", weak(&shim)),
            ("body_layer", weak(&body_layer)),
            ("shim_layer", weak(&shim_layer)),
            ("scroll", weak(&scroll)),
            ("clip", weak(&clip)),
            ("window", weak(&window)),
        ];
        native.extend(observers.into_iter().enumerate().map(|(i, w)| {
            (
                if i == 0 {
                    "bounds_observer"
                } else {
                    "frame_observer"
                },
                w,
            )
        }));
        // 惰性瓦片会在每个实际场景创建后继角色；每次合并真实 LayerPool 的 Weak，保留前序收据。
        // Direct 场景没有 CPU owner，其余 Native 角色仍由同一关闭列表逐对象核真实析构。
        let mut register_cpu_layers = || {
            if strategy == NativeMarkdownPagedRenderStrategy::CpuViewport {
                let layers = body
                    .cpu_layer_weaks()
                    .expect("已实际绘图的 CPU 视口必须保留原资源拥有者");
                native.extend(layers.into_iter().map(|layer| ("cpu_owned_layer", layer)));
            }
        };
        register_cpu_layers();
        drop(body_layer);
        drop(shim_layer);
        drop(context);
        drop(shim);
        let mut old = initial;
        for (stage, y) in [
            ("middle", height * 0.5),
            ("tail", height - clip.bounds().size.height),
        ] {
            clip.scrollToPoint(NSPoint::new(0.0, y));
            scroll.reflectScrolledClipView(&clip);
            assert!(!old.is_current(), "真实clip滚动没有撤旧票");
            old = paint_stage(stage, height, &body, &viewport, &delegate, &payload);
            register_cpu_layers();
        }
        // 实际改变clip大小，依赖原frame通知撤票；不调用synchronize补签或伪造新geometry。
        window.setContentSize(NSSize::new(480.0, 240.0));
        scroll.setFrameSize(NSSize::new(480.0, 240.0));
        scroll.layoutSubtreeIfNeeded();
        assert!(!old.is_current(), "真实resize没有撤销旧几何票");
        let resized = paint_stage("resized", height, &body, &viewport, &delegate, &payload);
        register_cpu_layers();
        same_scene_repaint(&body, &payload, "resized_repaint");
        assert!(resized.body_frame.size.width < width as f64);
        autoreleasepool(|_| {
            // 原默认阶段全部保留；不对称覆盖区只经公开API改变，clip方向仍是构造时的同一值。
            // 原owner同步消费真实Native转换，不补跑绘图executor，也没有第二Source/Input。
            scroll.setAutomaticallyAdjustsContentInsets(false);
            clip.setAutomaticallyAdjustsContentInsets(false);
            let insets = NSEdgeInsets {
                top: 13.0,
                left: 17.0,
                bottom: 7.0,
                right: 11.0,
            };
            clip.setContentInsets(insets);
            scroll.tile();
            viewport.synchronize().unwrap();
            let stage = if flipped {
                "insets_flipped"
            } else {
                "insets_unflipped"
            };
            let geometry = paint_stage(stage, height, &body, &viewport, &delegate, &payload);
            register_cpu_layers();
            let document_view = viewport.document_view().unwrap();
            let actual_clip_frame =
                clip.convertRect_fromView(geometry.body_frame, Some(&document_view));
            let bounds = clip.bounds();
            assert_eq!(clip.isFlipped(), flipped);
            assert_eq!(geometry.document_visible, clip.documentVisibleRect());
            assert_ne!(geometry.body_frame, geometry.document_visible);
            assert_eq!(actual_clip_frame.origin.x, bounds.origin.x + 17.0);
            assert_eq!(actual_clip_frame.size.width, bounds.size.width - 28.0);
            assert_eq!(
                actual_clip_frame.origin.y,
                bounds.origin.y + if flipped { 13.0 } else { 7.0 }
            );
            assert_eq!(actual_clip_frame.size.height, bounds.size.height - 20.0);
            assert!(geometry.body_frame.origin.y >= 0.0);

            // 分页按实际正文高度减原page overlap；返回实际Source原点，不能返回raw clip起点。
            let distance = (geometry.body_frame.size.height - scroll.verticalPageScroll()).max(0.0);
            let expected_down = (geometry.body_frame.origin.y + distance)
                .min(height - geometry.body_frame.size.height);
            let down = viewport
                .scroll_page(true, &|| delegate.active.get())
                .unwrap();
            assert_eq!(down.y, expected_down);
            assert_eq!(down, viewport.geometry().unwrap().body_frame.origin);
            let expected_up = (expected_down - distance).max(0.0);
            let up = viewport
                .scroll_page(false, &|| delegate.active.get())
                .unwrap();
            assert_eq!(up.y, expected_up);
            assert_eq!(up, viewport.geometry().unwrap().body_frame.origin);

            // BOF/EOF reveal都提交原document的真实矩形；左右inset不混入文本padding。
            let rect = |y| {
                NSRect::new(
                    NSPoint::new(geometry.body_frame.origin.x, y),
                    NSSize::new(1.0, 1.0),
                )
            };
            viewport
                .reveal_document_rect(rect(0.0), &|| delegate.active.get())
                .unwrap();
            assert_eq!(viewport.geometry().unwrap().body_frame.origin.y, 0.0);
            viewport
                .reveal_document_rect(rect(height - 1.0), &|| delegate.active.get())
                .unwrap();
            let tail = viewport.geometry().unwrap();
            assert_eq!(
                tail.body_frame.origin.y + tail.body_frame.size.height,
                height
            );
            assert!(clip.bounds().origin.y != 0.0, "滚动必须实际移动clip bounds");

            // 位于真实尾端再缩短高度，预滚动必须以有效正文底边锚定，随后恢复原工作负载高度。
            viewport.set_content_height(height - 17.0).unwrap();
            let shorter = viewport.geometry().unwrap();
            assert_eq!(
                shorter.body_frame.origin.y + shorter.body_frame.size.height,
                height - 17.0
            );
            viewport.set_content_height(height).unwrap();
            assert_eq!(
                viewport.geometry().unwrap().body_frame.origin.y,
                shorter.body_frame.origin.y,
                "恢复高度仍须保留同一Source顶端，不能因父坐标翻转自动换到另一处正文"
            );
            if !flipped {
                // 非翻转clip在BOF增长时，原目标超出旧frame的约束上界；必须在真实新frame
                // 上完成一次有权reveal，Source0与真实完整帧都不能靠旧bounds结果冒认。
                viewport
                    .reveal_document_rect(rect(0.0), &|| delegate.active.get())
                    .unwrap();
                assert_eq!(viewport.geometry().unwrap().body_frame.origin.y, 0.0);
                viewport.set_content_height(height + 17.0).unwrap();
                assert_eq!(viewport.geometry().unwrap().body_frame.origin.y, 0.0);
                paint_stage(
                    "insets_unflipped_bof_growth",
                    height + 17.0,
                    &body,
                    &viewport,
                    &delegate,
                    &payload,
                );
                register_cpu_layers();
                viewport.set_content_height(height).unwrap();
                assert_eq!(viewport.geometry().unwrap().body_frame.origin.y, 0.0);
            }
            let stage = if flipped {
                "insets_flipped_after_scroll_reveal_height"
            } else {
                "insets_unflipped_after_scroll_reveal_height"
            };
            paint_stage(stage, height, &body, &viewport, &delegate, &payload);
            register_cpu_layers();
        });
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*body);
        let mut actual = NSRange::new(9, 9);
        // SAFETY: 栈输出完整可写；没有caret producer的scene必须真实返回Pending/NotFound。
        let rect = unsafe {
            client.firstRectForCharacterRange_actualRange(NSRange::new(0, 0), &mut actual)
        };
        assert_eq!(rect, NSRect::ZERO);
        assert_eq!(actual.location, NSNotFound as usize);
        let failure = body.take_failure();
        if !matches!(failure, Some(Error::Pending)) {
            payload.frames_complete.set(false);
            println!(
                "paged_viewport first_rect_actual_failure={failure:?} expected_pending=false formal=false"
            );
        }
        poisoned_spool_refuses_reuse(&body, &payload);
        register_cpu_layers();
        assert!(!viewport.is_busy() && !body.is_busy());
        viewport.close().unwrap();
        assert!(!resized.is_current());
        let close_started = std::time::Instant::now();
        let close = body.begin_close().unwrap();
        assert!(Rc::ptr_eq(&close, &body.begin_close().unwrap()));
        paged_close_event::wait(&close);
        close.verify_released().unwrap();
        let close_elapsed = close_started.elapsed();
        let close_pixels = pixels.as_ref().map(|observer| observer.snapshot());
        let native_flush_in_close = strategy == NativeMarkdownPagedRenderStrategy::CpuViewport;
        println!(
            "paged_viewport actual_body_close_elapsed_us={} actual_close_pixels={close_pixels:?} native_flush_in_close={native_flush_in_close} polling=false formal=false",
            close_elapsed.as_micros()
        );
        delegate.active.set(false);
        let discarded = delegate.scene.borrow_mut().take();
        drop(discarded);
        window.setContentView(None);
        window.orderOut(None);
        window.close();
        drop(viewport);
        drop(body);
        drop(clip);
        drop(controlled_clip);
        drop(scroll);
        drop(window);
        drop(input);
        drop(delegate);
        drop(erased);
        let sealed = payload.spool.borrow_mut().take().unwrap();
        sealed.close().unwrap();
        let graphics_within_bound = payload.graphics_within_bound.get();
        let process_within_bound = payload.process_within_bound.get();
        let frames_complete = payload.frames_complete.get();
        println!("paged_viewport all_actual_frames_complete={frames_complete} formal=false");
        drop(payload);
        drop(source);
        drop(document);
        if flush_transaction {
            // 仅本次独立对照提交尚未提交的公开隐式事务；不等待动画、轮询Weak或隐藏释放失败。
            CATransaction::flush();
        }
        println!("paged_viewport cleanup_flush={flush_transaction} delay_added=false formal=false");
        (
            native,
            mapping,
            source_observer,
            font_bytes,
            spool_observer,
            document_weak,
            input_weak,
            viewport_weak,
            graphics_within_bound,
            process_within_bound,
            frames_complete,
            pixels,
        )
    });
    // 逐角色先记录同一关闭边界的事实，再统一拒收；首个失败不能遮住其它对象和内核资源。
    // Weak.load只在本语句持有临时Retained，不跨下一个角色或输出额外强owner。
    let mut native_weak_nil = true;
    for (index, (role, object)) in native.into_iter().enumerate() {
        let nil = object.load().is_none();
        println!(
            "paged_viewport release_role={role} role_index={index} actual_weak_nil={nil} formal=false"
        );
        native_weak_nil &= nil;
    }
    let rust_weak_nil = document_weak.upgrade().is_none()
        && input_weak.upgrade().is_none()
        && viewport_weak.upgrade().is_none();
    let source_released = source_observer.payload_released();
    let source_unmap =
        mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed();
    let font_unmap =
        font_bytes.unmap_succeeded() && font_bytes.mapped_bytes() == 0 && !font_bytes.failed();
    let spool = spool_observer.snapshot();
    let spool_closed = !spool.file_open
        && spool.close_succeeded
        && !spool.cleanup_failed
        && spool.source_payload_released
        && spool.font_payload_released;
    let pixels_released = pixels
        .as_ref()
        .is_none_or(|observer| observer.is_released());
    if let Some(observer) = &pixels {
        println!(
            "paged_viewport cpu_pixel_release={:?} actual_released={pixels_released} formal=false",
            observer.snapshot()
        );
    }
    let released = metrics::sample("released", height);
    let process_within_bound = process_within_bound && released.process_within_bound;
    println!(
        "paged_viewport release rust_weak_nil={rust_weak_nil} source_released={source_released} source_unmap={source_unmap} font_unmap={font_unmap} sealed_closed={spool_closed} graphics_within_bound={graphics_within_bound} process_within_bound={process_within_bound} all_frames_complete={frames_complete} formal=false"
    );
    assert!(native_weak_nil, "实际Native角色仍未全部释放");
    assert!(
        rust_weak_nil
            && source_released
            && source_unmap
            && font_unmap
            && spool_closed
            && pixels_released
            && frames_complete
    );
    println!(
        "paged_viewport virtual_height={height} actual_ink=true actual_scroll_resize=true old_geometry_revoked=true native_weak_nil=true source_unmap=true sealed_fd_closed=true graphics_within_bound={graphics_within_bound} process_within_bound={process_within_bound} full_editing_parity=false whole_product=false formal=false"
    );
    assert!(graphics_within_bound, "隔离视口机制仍申请百MB graphics");
    assert!(
        process_within_bound,
        "隔离控制真实footprint或RSS峰值超100MB"
    );
}
