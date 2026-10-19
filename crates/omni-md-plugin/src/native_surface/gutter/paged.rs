//! Paged源码行号只消费原Scene的有限几何；挂载、像素与关闭仍属于原Body。

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak as RcWeak},
    sync::Arc,
};

use objc2::{
    ClassType, DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::{Retained, Weak, autoreleasepool},
    runtime::AnyObject,
};
use objc2_app_kit::{
    NSAccessibility, NSClipView, NSEvent, NSRulerOrientation, NSRulerView, NSScrollView, NSView,
    NSViewLayerContentsRedrawPolicy,
};
use objc2_core_foundation::CGAffineTransform;
use objc2_core_graphics::{CGColor, CGContext};
use objc2_foundation::{
    MainThreadMarker, NSEdgeInsets, NSObjectProtocol, NSPoint, NSRect, NSSize, ns_string,
};
use objc2_quartz_core::{CALayer, CATransaction, kCAContentsFormatRGBA8Uint, kCAGravityResize};

use super::labels::LineNumberFont;
use crate::native_surface::{
    paged_body::{NativeMarkdownPagedBody, scene::Scene},
    paged_text_view::{
        NativeMarkdownPagedCloseReceipt, NativeMarkdownPagedTextView,
        NativeMarkdownPagedTextViewError as Error,
    },
    viewport_pixels::{Bitmap, NativeMarkdownViewportPixels},
};

type Result<T> = std::result::Result<T, Error>;

/// 原canonical逻辑行与同次已度量软行的交集；续行没有伪造的行号，仍保活动底色。
pub(in crate::native_surface) struct GutterRow {
    pub number: Option<usize>,
    pub frame: NSRect,
    pub active: bool,
}

pub(in crate::native_surface) struct PagedGutterIvars {
    owner: RefCell<RcWeak<NativeMarkdownPagedBody>>,
    view: RefCell<Option<Rc<Weak<NativeMarkdownPagedTextView>>>>,
    scroll: RefCell<Option<Rc<Weak<NSScrollView>>>>,
    clip: RefCell<Option<Rc<Weak<NSClipView>>>>,
    scene: RefCell<RcWeak<Scene>>,
    font: RefCell<Option<Rc<LineNumberFont>>>,
    layer: RefCell<Option<Rc<Retained<CALayer>>>>,
    // 和正文同Arc；原存活像素额度、真实munmap和关闭事件共同覆盖这张窄图。
    pixels: RefCell<Option<Arc<NativeMarkdownViewportPixels>>>,
    original_ruler_flags: (bool, bool),
    original_clip_automatic_insets: bool,
    original_clip_content_insets: NSEdgeInsets,
    painting: Cell<bool>,
    closed: Cell<bool>,
}

define_class!(
    // SAFETY: 主线程NSRulerView的全部ivars在super init前安装；回指和Scene均为弱引用。
    #[unsafe(super(NSRulerView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PagedGutterIvars]
    pub(in crate::native_surface) struct NativeMarkdownPagedGutter;

    // SAFETY: 继承公开主线程NSObject身份，不改变父类对象或协议的布局。
    unsafe impl NSObjectProtocol for NativeMarkdownPagedGutter {}

    impl NativeMarkdownPagedGutter {
        #[unsafe(method(isFlipped))]
        /// 与Paged正文及CPU context保持同一y-up方向；转换由真实NSView关系完成。
        fn is_flipped(&self) -> bool { false }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool { true }

        #[unsafe(method(acceptsFirstResponder))]
        /// 行号只是静态绘图，原CodeMirror数字区不接键盘焦点或输入命令。
        fn accepts_first_responder(&self) -> bool { false }

        #[unsafe(method(canBecomeKeyView))]
        /// 从构造期排除Tab焦点资格，不让装饰视图成为正文输入链中的候选。
        fn can_become_key_view(&self) -> bool { false }

        #[unsafe(method(updateLayer))]
        /// 原AppKit绘制事件只消费当前弱Scene；失败不回采新权限或启动另一调度链。
        fn update_layer(&self) {
            if self.ivars().closed.get() || self.ivars().painting.replace(true) {
                return; // 关闭或同栈重入不能重复创建候选像素。
            }
            // 同步displayIfNeeded也可能由外层长寿命池调用；本次getter/CA临时claim只活这一帧。
            let result = autoreleasepool(|_| self.paint());
            self.ivars().painting.set(false);
            let owner = self.ivars().owner.borrow().upgrade();
            if let Err(error) = result
                && !matches!(error, Error::Pending | Error::Revoked)
                && let Some(owner) = owner
            {
                owner.gutter_failed(error); // 真实资源/字体/Source错误沿原Body首错退出，不静默丢帧。
            }
        }

        #[unsafe(method(drawHashMarksAndLabelsInRect:))]
        /// 标签只能进入上方CPU图，禁止NSRuler父类再建立窗口字形显示列表。
        fn draw_hash_marks_and_labels(&self, _dirty: NSRect) {}

        #[unsafe(method(drawRect:))]
        /// layer-backed入口已独占显示；旧ruler绘制不能旁路CPU像素归属。
        fn draw_rect(&self, _dirty: NSRect) {}

        #[unsafe(method(mouseDown:))]
        /// 原lineNumbers()没有数字区事件处理；不擅自增加整行选择或折叠交互。
        fn mouse_down(&self, _event: &NSEvent) {}
    }
);

impl NativeMarkdownPagedGutter {
    /// 原Body已构造CPU正文；本ruler只借它的实际像素页账，不创建隐藏文本视图。
    pub(in crate::native_surface) fn new(
        owner: RcWeak<NativeMarkdownPagedBody>,
        scroll: &NSScrollView,
        view: &NativeMarkdownPagedTextView,
        lines: usize,
        mtm: MainThreadMarker,
    ) -> Result<Retained<Self>> {
        autoreleasepool(|_| {
            // 构造可能直接位于调用方会话池内；只让返回Retained和明确ivars越过此阶段。
            if construction_step(view, || scroll.verticalRulerView())?.is_some() {
                return Err(Error::InvalidGeometry); // 不夺取调用方已挂载的另一行号图。
            }
            let font = Rc::new(construction_step(view, LineNumberFont::new)??);
            let width = font.width(lines);
            let pixels = construction_step(view, || view.pixel_observer())?
                .expect("Source Body已装配CPU正文")
                .0;
            let original_ruler_flags = (
                construction_step(view, || scroll.hasVerticalRuler())?,
                construction_step(view, || scroll.rulersVisible())?,
            );
            let clip = construction_step(view, || scroll.contentView())?;
            let original_clip_automatic_insets =
                construction_step(view, || clip.automaticallyAdjustsContentInsets())?;
            let original_clip_content_insets = construction_step(view, || clip.contentInsets())?;
            let allocated = Self::alloc(mtm).set_ivars(PagedGutterIvars {
                owner: RefCell::new(owner),
                view: RefCell::new(Some(Rc::new(Weak::from(view)))),
                scroll: RefCell::new(Some(Rc::new(Weak::from(scroll)))),
                clip: RefCell::new(Some(Rc::new(Weak::from(&*clip)))),
                scene: RefCell::new(RcWeak::new()),
                font: RefCell::new(Some(font)),
                layer: RefCell::new(None),
                pixels: RefCell::new(Some(pixels)),
                original_ruler_flags,
                original_clip_automatic_insets,
                original_clip_content_insets,
                painting: Cell::new(false),
                closed: Cell::new(false),
            });
            // SAFETY: 主线程指定初始化器接收同一scroll，所有Rust载荷已经在上方固定。
            let ruler: Retained<Self> = construction_step(view, || unsafe {
                msg_send![super(allocated), initWithScrollView: Some(scroll), orientation: NSRulerOrientation::VerticalRuler]
            })?;
            construction_step(view, || ruler.setClipsToBounds(true))?;
            construction_step(view, || ruler.setReservedThicknessForMarkers(0.0))?;
            construction_step(view, || ruler.setReservedThicknessForAccessoryView(0.0))?;
            construction_step(view, || ruler.setRuleThickness(width))?;
            construction_step(view, || ruler.setClientView(Some(view)))?;
            construction_step(view, || {
                ruler.setAccessibilityLabel(Some(ns_string!("源码行号")))
            })?;
            // AppKit创建唯一backing；字体只在有限CPU context绘制，不调用NSRuler父类绘图。
            construction_step(view, || ruler.setWantsLayer(true))?;
            construction_step(view, || {
                ruler.setLayerContentsRedrawPolicy(
                    NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay,
                )
            })?;
            let layer = construction_step(view, || ruler.layer())?
                .ok_or_else(|| Error::Raster("无法建立源码行号图层".into()))?;
            let background = CGColor::new_srgb(19.0 / 255.0, 22.0 / 255.0, 28.0 / 255.0, 1.0);
            construction_step(view, || layer.setBackgroundColor(Some(&background)))?;
            *ruler.ivars().layer.borrow_mut() = Some(Rc::new(layer));
            Ok(ruler)
        })
    }

    /// 先把owner槽交回调用方再挂载；同步Native通知能看到唯一已装配的ruler归属。
    pub(in crate::native_surface) fn mount(&self) -> Result<()> {
        autoreleasepool(|_| {
            // 原scroll装配触发的布局/getter临时值在返回前归还，不保留到业务关闭后的等待。
            let view = self.body_view().ok_or(Error::Revoked)?;
            let scroll = self.scroll_view().ok_or(Error::Revoked)?;
            let clip = self.clip_view().ok_or(Error::Revoked)?;
            if self.ivars().closed.get()
                || construction_step(&view, || scroll.verticalRulerView())?.is_some()
                || !std::ptr::eq::<NSClipView>(
                    &*construction_step(&view, || scroll.contentView())?,
                    &*clip,
                )
            {
                return Err(Error::Revoked);
            }
            construction_step(&view, || scroll.setVerticalRulerView(Some(self)))?;
            // 现代AppKit可把ruler覆盖在完整clip上；公开clip.contentInsets才记录实际预留。
            // 由原clip按真实ruler/header计算，viewport消费这些inset安放唯一正文，不猜ruler宽。
            // scroll自己的自动策略仅处理标题栏，属于调用方；此组件不再改它。
            construction_step(&view, || clip.setAutomaticallyAdjustsContentInsets(true))?;
            construction_step(&view, || scroll.setHasVerticalRuler(true))?;
            construction_step(&view, || scroll.setRulersVisible(true))?;
            construction_step(&view, || scroll.tile())?;
            Ok(())
        })
    }

    pub(in crate::native_surface) fn is_busy(&self) -> bool {
        self.ivars().painting.get()
    }

    /// 总行数只有位数跨界才改Native宽度；调用方在后继回合重新捕获真实正文几何。
    pub(in crate::native_surface) fn update_width(&self, lines: usize) -> Result<bool> {
        let font = self.ivars().font.borrow().clone().ok_or(Error::Revoked)?;
        let width = font.width(lines);
        let view = self.body_view().ok_or(Error::Revoked)?;
        if self.ivars().closed.get() {
            return Err(Error::Revoked);
        }
        if construction_step(&view, || self.ruleThickness())? == width {
            return Ok(false); // 同位数不重复布局或撤销正在绘制的正文Scene。
        }
        self.clear_snapshot();
        construction_step(&view, || self.setRuleThickness(width))?;
        let scroll = self.scroll_view().ok_or(Error::Revoked)?;
        // 位数改变后先完成真实scroll子视图布局；延迟tile会让后继正文捕获旧clip宽度并交付错误的一帧。
        construction_step(&view, || scroll.tile())?;
        Ok(true)
    }

    /// 只挂同次发布的弱Scene；行号不会延长原Reader、Source或文档的生命期。
    pub(in crate::native_surface) fn publish(&self, scene: &Rc<Scene>) -> Result<()> {
        scene.gutter_rows()?;
        if self.ivars().closed.get() {
            return Err(Error::Revoked);
        }
        *self.ivars().scene.borrow_mut() = Rc::downgrade(scene);
        self.native(scene, || self.setNeedsDisplay(true))
    }

    /// 原Scene失权即撤销标签像素；保留的空ruler不会签下一代行号或显示旧数字。
    pub(in crate::native_surface) fn clear_snapshot(&self) {
        *self.ivars().scene.borrow_mut() = RcWeak::new();
        let layer = self.ivars().layer.borrow().clone();
        if let Some(layer) = layer {
            // SAFETY: 只清本自有图层的原contents，provider沿原像素归还回调执行真实munmap。
            unsafe { layer.setContents(None) };
        }
    }

    /// Weak加载可能进入原生引用计数；先结束槽借用，关闭回调才可取走相同槽。
    fn body_view(&self) -> Option<Retained<NativeMarkdownPagedTextView>> {
        let view = self.ivars().view.borrow().clone();
        view.and_then(|view| view.load())
    }

    /// 仅在挂载或拆图时借原scroll身份；关闭后连Weak控制块也不留在系统空壳中。
    fn scroll_view(&self) -> Option<Retained<NSScrollView>> {
        let scroll = self.ivars().scroll.borrow().clone();
        scroll.and_then(|scroll| scroll.load())
    }

    /// 只恢复本次装配的原clip；弱身份避免关闭时误改调用方已经替换的contentView。
    fn clip_view(&self) -> Option<Retained<NSClipView>> {
        let clip = self.ivars().clip.borrow().clone();
        clip.and_then(|clip| clip.load())
    }

    fn require(&self, scene: &Rc<Scene>) -> Result<()> {
        scene.gutter_rows()?; // Source/Font/Reader首错先于普通Native撤权，不重捕后继Scene。
        if self.ivars().closed.get()
            || !RcWeak::ptr_eq(&self.ivars().scene.borrow(), &Rc::downgrade(scene))
        {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// Native重入前后仍核同一Scene；返回值不在失权后作为新布局的输入。
    fn native<T>(&self, scene: &Rc<Scene>, work: impl FnOnce() -> T) -> Result<T> {
        self.require(scene)?;
        let value = work();
        self.require(scene)?;
        Ok(value)
    }

    fn paint(&self) -> Result<()> {
        let scene = self
            .ivars()
            .scene
            .borrow()
            .upgrade()
            .ok_or(Error::Pending)?;
        self.require(&scene)?;
        let body = self
            .native(&scene, || self.body_view())?
            .ok_or(Error::Revoked)?;
        let font = self.ivars().font.borrow().clone().ok_or(Error::Revoked)?;
        let layer = self.ivars().layer.borrow().clone().ok_or(Error::Revoked)?;
        let pixels = self.ivars().pixels.borrow().clone().ok_or(Error::Revoked)?;
        let bounds = self.native(&scene, || self.bounds())?;
        let window = self
            .native(&scene, || self.window())?
            .ok_or(Error::Pending)?;
        let scale = self.native(&scene, || window.backingScaleFactor())?;
        let width = bounds.size.width * scale;
        let height = bounds.size.height * scale;
        if !scale.is_finite()
            || scale <= 0.0
            || !width.is_finite()
            || !height.is_finite()
            || width <= 0.0
            || height <= 0.0
            || width > usize::MAX as f64
            || height > usize::MAX as f64
            || !bounds.origin.x.is_finite()
            || !bounds.origin.y.is_finite()
        {
            return Err(Error::InvalidGeometry);
        }
        // 只分配实际ruler视口尺寸；原Bitmap复用真实页预算，绝不采用文档逻辑高度。
        let bitmap = Bitmap::new(width.ceil() as usize, height.ceil() as usize, pixels)
            .map_err(Error::Raster)?;
        let context = bitmap.context();
        CGContext::concat_ctm(
            Some(context),
            CGAffineTransform {
                a: scale,
                b: 0.0,
                c: 0.0,
                d: scale,
                tx: -bounds.origin.x * scale,
                ty: -bounds.origin.y * scale,
            },
        );
        CGContext::clip_to_rect(Some(context), bounds);
        CGContext::set_rgb_fill_color(Some(context), 19.0 / 255.0, 22.0 / 255.0, 28.0 / 255.0, 1.0);
        CGContext::fill_rect(Some(context), bounds);
        for row in scene.gutter_rows()? {
            let frame =
                self.native(&scene, || self.convertRect_fromView(row.frame, Some(&body)))?;
            let bottom = bounds.origin.y.max(frame.origin.y);
            let top =
                (bounds.origin.y + bounds.size.height).min(frame.origin.y + frame.size.height);
            if top <= bottom {
                continue; // 真实首soft行已出屏时不把它的数字挪到视口顶；续行仍沿原几何独立裁切。
            }
            if row.active {
                // 相邻真实软行可以在分数device-pixel边界相接；分别抗锯齿会把两边各自
                // 混入未活动底色，留下并不存在的横缝。仅背景矩形用完整像素覆盖，字形状态随后恢复。
                CGContext::save_g_state(Some(context));
                CGContext::set_should_antialias(Some(context), false);
                CGContext::set_rgb_fill_color(
                    Some(context),
                    27.0 / 255.0,
                    32.0 / 255.0,
                    42.0 / 255.0,
                    1.0,
                );
                CGContext::fill_rect(
                    Some(context),
                    NSRect::new(
                        NSPoint::new(bounds.origin.x, bottom),
                        NSSize::new(bounds.size.width, top - bottom),
                    ),
                );
                CGContext::restore_g_state(Some(context));
            }
            if let Some(number) = row.number {
                CGContext::set_rgb_fill_color(
                    Some(context),
                    81.0 / 255.0,
                    91.0 / 255.0,
                    109.0 / 255.0,
                    1.0,
                );
                self.native(&scene, || {
                    font.draw(
                        context,
                        number,
                        bounds.origin.x + bounds.size.width - 1.0 - 3.0,
                        frame,
                    )
                })?;
            } // 数字只来自原逻辑行首；后续wrapped行不建立重复标签或折叠标记。
        }
        CGContext::set_rgb_fill_color(Some(context), 37.0 / 255.0, 42.0 / 255.0, 51.0 / 255.0, 1.0);
        CGContext::fill_rect(
            Some(context),
            NSRect::new(
                NSPoint::new(bounds.origin.x + bounds.size.width - 1.0, bounds.origin.y),
                NSSize::new(1.0, bounds.size.height),
            ),
        );
        self.require(&scene)?;
        let image = bitmap.finish().map_err(Error::Raster)?; // 写context已真实结束，才能将只读provider交给图层。
        if self.native(&scene, || self.bounds())? != bounds
            || self.native(&scene, || window.backingScaleFactor())? != scale
            || !self
                .native(&scene, || self.layer())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq(actual, &**layer))
            || !self
                .native(&scene, || self.clientView())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq::<NSView>(actual, body.as_super()))
            || !self
                .native(&scene, || self.window())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq(actual, &*window))
        {
            return Err(Error::Revoked);
        }
        let transaction = self.native(&scene, || {
            CATransaction::begin();
            GutterTransaction // 后置原票撤销也由临时值Drop配平，不能遗留已经开始的事务。
        })?;
        self.native(&scene, || CATransaction::setDisableActions(true))?;
        // SAFETY: 两个字符串为QuartzCore公开的进程期常量；只设原同一自有backing。
        self.native(&scene, || unsafe {
            layer.setContentsFormat(kCAContentsFormatRGBA8Uint)
        })?;
        // SAFETY: 同上，完整图已按实际scale绘制，无局部图或降采样替补。
        self.native(&scene, || unsafe {
            layer.setContentsGravity(kCAGravityResize)
        })?;
        self.native(&scene, || layer.setContentsScale(scale))?;
        let mut publication = GutterPublication {
            layer: &layer,
            image: std::ptr::from_ref(&*image).cast::<AnyObject>(),
            accepted: false,
        };
        // SAFETY: CALayer.contents接受真实CF CGImage；这里只桥接同一个对象，不向CF发送NSObject消息。
        self.native(&scene, || unsafe {
            layer.setContents(Some(&*std::ptr::from_ref(&*image).cast::<AnyObject>()));
        })?;
        drop(transaction);
        if self.native(&scene, || self.bounds())? != bounds
            || self.native(&scene, || window.backingScaleFactor())? != scale
            || !self
                .native(&scene, || self.layer())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq(actual, &**layer))
            || !self
                .native(&scene, || self.clientView())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq::<NSView>(actual, body.as_super()))
            || !self
                .native(&scene, || self.window())?
                .as_deref()
                .is_some_and(|actual| std::ptr::eq(actual, &*window))
        {
            return Err(Error::Revoked); // setter已执行仍须核真实原几何；失权只清本次实际图，不回滚后继内容。
        }
        // SAFETY: 只读本图层的公开contents关系，不对返回的CF图像发送NSObject消息。
        let actual = self.native(&scene, || unsafe { layer.contents() })?;
        if !actual
            .as_deref()
            .is_some_and(|actual| std::ptr::eq(actual, publication.image))
        {
            return Err(Error::Revoked);
        }
        publication.accepted = true;
        Ok(())
    }

    /// 同原NSRuler合同，实际像素必须归还；系统若缓存空控件，收据另核每个自有载荷已取走。
    pub(in crate::native_surface) fn close(
        &self,
        receipt: &NativeMarkdownPagedCloseReceipt,
    ) -> Result<()> {
        if self.ivars().closed.replace(true) {
            return Ok(());
        }
        let pixels = self
            .ivars()
            .pixels
            .borrow_mut()
            .take()
            .expect("已装配的Source行号持有原正文像素页账");
        receipt.track_pixels(&pixels);
        drop(pixels); // 收据保留真实页归还观察者；缓存的ruler不能继续持有项目的共享Arc。
        let layer = self.ivars().layer.borrow().clone();
        receipt.track_gutter(PagedGutterRelease {
            ruler: Weak::from(self),
            layer: layer.as_ref().map(|layer| Weak::from(&***layer)),
        });
        let owner = self.ivars().owner.replace(RcWeak::new());
        drop(owner); // 即使Body强引用已为零，旧Weak控制块也必须离开可被框架缓存的对象。
        autoreleasepool(|_| {
            // 本关闭阶段的公开getter、CA临时claim与contents返回值不能留到调用方跨wait的池。
            // 原收据固定实际像素与原层身份；以下仍履行真实拆图并保留第一清理错误。
            let original_window = self.window();
            let this_view: &NSView = self;
            // SAFETY: 原主线程关闭栈持活ruler；公开key-view getter仅返回此刻的真实相邻视图。
            let (previous_key, next_key) = unsafe { (self.previousKeyView(), self.nextKeyView()) };
            if let Some(previous) = previous_key.as_deref()
                && !std::ptr::eq(previous, this_view)
            {
                // SAFETY: previous由上方公开getter持活；只读取真实incoming关系，不遍历焦点图。
                let incoming = unsafe { previous.nextKeyView() };
                if incoming
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq(actual, this_view))
                {
                    let successor = next_key
                        .as_deref()
                        .filter(|actual| !std::ptr::eq(*actual, this_view));
                    // SAFETY: 仅仍指向本次关闭视图的原前驱被接回真实后继；不改外方已经替换的链。
                    unsafe { previous.setNextKeyView(successor) };
                }
            }
            // SAFETY: 本ruler已封闭，清自己的公开outgoing关系；不销毁或访问相邻视图载荷。
            unsafe { self.setNextKeyView(None) };
            self.clear_snapshot();
            let font = self.ivars().font.borrow_mut().take();
            drop(font); // 十个glyph标量与本项目NSFont强边先结束，系统字体没有正文或业务回指。
            self.setClientView(None);
            let mut result = Ok(());
            if let Some(scroll) = self.scroll_view() {
                if scroll
                    .verticalRulerView()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<NSRulerView>(actual, self.as_super()))
                {
                    scroll.setVerticalRulerView(None);
                    scroll.setHasVerticalRuler(self.ivars().original_ruler_flags.0);
                    scroll.setRulersVisible(self.ivars().original_ruler_flags.1);
                    if let Some(clip) = self.clip_view()
                        && std::ptr::eq::<NSClipView>(&*scroll.contentView(), &*clip)
                    {
                        clip.setAutomaticallyAdjustsContentInsets(
                            self.ivars().original_clip_automatic_insets,
                        );
                        if !self.ivars().original_clip_automatic_insets {
                            // 调用方原先手工设置的值会被挂载期自动策略覆盖；先关闭自动，再还具体值。
                            // 原策略为true时仍由AppKit按已移出的ruler重算，不能写回旧自动结果。
                            clip.setContentInsets(self.ivars().original_clip_content_insets);
                        }
                        if !std::ptr::eq::<NSClipView>(&*scroll.contentView(), &*clip) {
                            result = Err(Error::Revoked); // setter同步重入也不能把已被替换的原clip还原算成成功。
                        }
                    } else {
                        result = Err(Error::Revoked); // 后继contentView不属于这次原clip策略的恢复范围。
                    }
                } else {
                    result = Err(Error::Revoked); // 外方替换了挂载关系，不能为了清理删除后继ruler。
                }
            }
            // scroll的ruler属性和NSView子关系是两条独立强边；关闭同时撤掉真实自有子视图，
            // 再清NSRuler的弱scroll关联，不能以属性变nil冒充它已离开原生父图。
            self.removeFromSuperview();
            self.setScrollView(None);
            if let Some(window) = &original_window {
                window.recalculateKeyViewLoop(); // 这里只标记待重算；真实移出后的焦点环仍由原窗口更新。
                if let Some(view) = self.body_view()
                    && view
                        .window()
                        .as_deref()
                        .is_some_and(|actual| std::ptr::eq(actual, &**window))
                {
                    // AppKit在窗口内视图的下一次key-view查询才重算；仅查询原正文一次，不移动焦点。
                    // SAFETY: 原window和正文均存活且关系已核同一窗口；重算由AppKit按当前窗口树建立有效key-view。
                    drop(unsafe { view.nextKeyView() });
                }
            }
            self.setNeedsDisplay(false); // 取消本自有视图待交付的原生脏标记，不再为关闭后的空壳绘图。
            if let Some(layer) = &layer {
                let actual = self.layer();
                if actual
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<CALayer>(actual, &***layer))
                {
                    self.setWantsLayer(false);
                    // 禁层可能已经同步清了backing；只撤仍是原层或已经为空的关系，不夺取外方替换层。
                    let after_disable = self.layer();
                    if after_disable
                        .as_deref()
                        .is_none_or(|actual| std::ptr::eq::<CALayer>(actual, &***layer))
                    {
                        self.setLayer(None);
                    } else {
                        result = Err(Error::Revoked);
                    }
                } else {
                    result = Err(Error::Revoked); // 不清外方替换的backing；原自有层仍在下面解除自己的载荷。
                }
                layer.removeFromSuperlayer(); // 原自有层不继续作为其他层的子项保留这次绘制。
                layer.setDelegate(None); // NSView给backing的绘制回调关系也必须实际撤掉。
                layer.setBackgroundColor(None);
                // SAFETY: 只清本组件原层的图像/遮罩/子层；传入None不引入外方对象或CA所有权。
                unsafe {
                    layer.setContents(None);
                    layer.setMask(None);
                    layer.setSublayers(None);
                }
            }
            drop(self.ivars().layer.borrow_mut().take());
            // 所有Native getter已结束，取走Native Weak自身的分配；空控件不保留旧视图图的身份槽。
            let view = self.ivars().view.borrow_mut().take();
            let scroll = self.ivars().scroll.borrow_mut().take();
            let clip = self.ivars().clip.borrow_mut().take();
            drop((view, scroll, clip));
            drop(layer);
            CATransaction::flush(); // 自有backing/挂载已真实detach后提交CA归还，再结束这个局部Native池。
            result
        })
    }

    /// 逐字段证明框架可能缓存的仅为空壳；Weak控制块、页账Arc和字体也不能留在ivars中。
    fn payload_is_empty(&self) -> bool {
        let payload_empty = self.ivars().closed.get()
            && !self.ivars().painting.get()
            && RcWeak::ptr_eq(&self.ivars().owner.borrow(), &RcWeak::new())
            && self.ivars().view.borrow().is_none()
            && self.ivars().scroll.borrow().is_none()
            && self.ivars().clip.borrow().is_none()
            && RcWeak::ptr_eq(&self.ivars().scene.borrow(), &RcWeak::new())
            && self.ivars().font.borrow().is_none()
            && self.ivars().layer.borrow().is_none()
            && self.ivars().pixels.borrow().is_none();
        if !payload_empty {
            return false; // 死Weak或零像素计数不能替代项目实际取走相同的所有权槽。
        }
        // Rust借用已全部结束；公开关系必须真实detach，不能只凭closed标志授权成功。
        // SAFETY: 收据的同一主线程Weak加载持活本对象，getter不访问已经取走的项目载荷。
        let detached = unsafe { self.superview().is_none() && self.nextKeyView().is_none() };
        detached
            && self.window().is_none()
            && self.clientView().is_none()
            && self.scrollView().is_none()
            && self.layer().is_none()
            && !self.wantsLayer()
    }
}

/// 只观察这次真实ruler/backing；不持正文、字体、像素Arc或Native强引用，不引入第二资源账本。
pub(in crate::native_surface) struct PagedGutterRelease {
    ruler: Weak<NativeMarkdownPagedGutter>,
    layer: Option<Weak<CALayer>>,
}

impl PagedGutterRelease {
    /// AppKit可以缓存已清空的标准控件；项目载荷和原图层关系仍按实际对象逐项核验。
    pub(in crate::native_surface) fn verify_released(&self) -> std::result::Result<(), String> {
        autoreleasepool(|_| {
            if self
                .ruler
                .load()
                .is_some_and(|ruler| !ruler.payload_is_empty())
            {
                return Err("源码行号仍持有业务载荷或原生挂载关系".into());
            }
            if let Some(layer) = self.layer.as_ref().and_then(Weak::load) {
                // SAFETY: 收据只读原自有层；CF contents只查是否为空，不向其发送NSObject消息。
                let contents_empty = unsafe { layer.contents().is_none() };
                // SAFETY: 同一主线程原层仍存活，仅查询公开子层关系；数组只查长度，不改未知对象。
                let children_empty =
                    unsafe { layer.sublayers() }.is_none_or(|children| children.is_empty());
                if !contents_empty
                    || !children_empty
                    || layer.mask().is_some()
                    || layer.backgroundColor().is_some()
                    || layer.delegate().is_some()
                    || layer.superlayer().is_some()
                {
                    return Err("源码行号原图层仍持有像素、回调或父子关系".into());
                }
            } // 实际层已析构或全部载荷/关系均为空，才符合原标准控件空壳合同。
            Ok(())
        })
    }
}

/// 任意失败出口也配平原CATransaction；提交本身不授予失权候选成功资格。
struct GutterTransaction;
impl Drop for GutterTransaction {
    fn drop(&mut self) {
        CATransaction::commit();
    }
}

/// setter后撤权也有真实像素后果；只撤仍等于本次image的关系，不覆盖后继已发布内容。
struct GutterPublication<'a> {
    layer: &'a CALayer,
    image: *const AnyObject,
    accepted: bool,
}
impl Drop for GutterPublication<'_> {
    fn drop(&mut self) {
        if self.accepted {
            return; // 完整原票和真实contents均通过后，唯一图像归属由图层继续持有。
        }
        // SAFETY: 只比较原生公开contents身份，CF对象只作地址比较，不被发送对象消息。
        let actual = unsafe { self.layer.contents() };
        if actual
            .as_deref()
            .is_some_and(|actual| std::ptr::eq(actual, self.image))
        {
            // SAFETY: 关系仍属于本次失败setter，只清自己的像素，不删除外方或后继内容。
            unsafe { self.layer.setContents(None) };
        }
    }
}

/// 构造/宽度事务尚没有Scene；只借原CPU正文的现有会话权限，Native关闭后不能继续挂回旧图。
fn construction_step<T>(view: &NativeMarkdownPagedTextView, work: impl FnOnce() -> T) -> Result<T> {
    if !view.session_is_current() {
        return Err(Error::Revoked);
    }
    let value = work();
    if !view.session_is_current() {
        return Err(Error::Revoked);
    }
    Ok(value)
}
