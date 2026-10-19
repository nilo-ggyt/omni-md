//! 一个真实有限glyph的Ready/Visible对照；不声称完整编辑、离屏排版或整产品验收。

use super::*;
use crate::native_surface::{
    paged_fonts::NativeMarkdownPagedFont,
    paged_glyph_paint::{NativeMarkdownPagedGlyphOrigin, NativeMarkdownPagedGlyphPaintRequest},
    paged_text_view::{
        NativeMarkdownPagedCharacterRect, NativeMarkdownPagedEditCommand,
        NativeMarkdownPagedEditPlan, NativeMarkdownPagedNavigation, NativeMarkdownPagedSceneNonce,
        NativeMarkdownPagedScenePaintBudget, NativeMarkdownPagedScenePaintCompletion,
        NativeMarkdownPagedScenePaintOutcome, NativeMarkdownPagedScenePaintSession,
        NativeMarkdownPagedScenePaintStep, NativeMarkdownPagedScenePaintWork,
    },
};
use objc2_core_foundation::{CFRetained, CFString, CGPoint, CGRect};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
    kCGColorSpaceSRGB,
};
use objc2_core_text::{CTFont, CTFontOrientation};
use omni_markdown_core::{
    MarkdownLayoutFontAuthority, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownShapedSpool, MarkdownShapedSpoolAuthority, MarkdownShapedSpoolBuilder,
    MarkdownShapingBudget, MarkdownShapingDirection, MarkdownShapingJob, MarkdownShapingOptions,
    MarkdownShapingStatus,
};
use std::ptr::NonNull;

struct Payload {
    spool: RefCell<Option<MarkdownShapedSpool>>,
    output: MarkdownShapedSpoolAuthority,
    source: MarkdownLayoutTextSource,
    source_authority: MarkdownLayoutTextAuthority,
    font_authority: MarkdownLayoutFontAuthority,
    font: NativeMarkdownPagedFont,
    baseline: CGPoint,
    rect: NSRect,
}
struct Scene {
    key: NativeMarkdownPagedSceneKey,
    _nonce: NativeMarkdownPagedSceneNonce,
    payload: Rc<Payload>,
}
impl NativeMarkdownPagedTextScene for Scene {
    fn key(&self) -> &NativeMarkdownPagedSceneKey {
        &self.key
    }
    fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<(), ViewError> {
        let check = || -> Result<(), ViewError> {
            let mut slot = self
                .payload
                .spool
                .try_borrow_mut()
                .map_err(|_| ViewError::Pending)?;
            let spool = slot.as_mut().ok_or(ViewError::Pending)?;
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
                        ) => ViewError::Document(cause),
                        other => ViewError::ShapedSpool(other),
                    })?;
            } // 空批仍实际核原输出和两个字体身份，不能以相同名字或bool续权。
            if !self.payload.font.accepts_shaping_size(spool.points()) {
                return Err(ViewError::InvalidGeometry);
            }
            Ok(())
        };
        let before = current();
        check()?; // Source实际首错优先于布尔撤权；借用在任何可重入current之前结束。
        let after = current();
        check()?;
        if !before || !after {
            return Err(ViewError::Revoked);
        }
        Ok(())
    }
    fn first_rect(&self, range: NSRange) -> Result<NativeMarkdownPagedCharacterRect, ViewError> {
        // 本oracle只建一个真实非空scalar；其它范围明确Pending，不能靠占位矩形通过。
        if range != NSRange::new(0, 1) {
            return Err(ViewError::Pending);
        }
        Ok(NativeMarkdownPagedCharacterRect {
            rect: self.payload.rect,
            actual_range: NSRange::new(0, 1),
        })
    }
    fn hit(&self, _: NSPoint) -> Result<usize, ViewError> {
        Err(ViewError::Pending)
    }
    fn mouse_selection(
        &self,
        _: NSPoint,
        _: usize,
        _: Option<usize>,
        _: bool,
    ) -> Result<MarkdownTextSelection, ViewError> {
        Err(ViewError::Pending)
    }
    fn navigate(
        &self,
        _: NativeMarkdownPagedNavigation,
        _: bool,
        _: &crate::native_surface::paged_text_view::NativeMarkdownPagedNavigationContext,
    ) -> Result<
        crate::native_surface::paged_text_view::NativeMarkdownPagedNavigationTarget,
        ViewError,
    > {
        Err(ViewError::Pending)
    }
    fn edit_plan(
        &self,
        _: NativeMarkdownPagedEditCommand,
    ) -> Result<NativeMarkdownPagedEditPlan, ViewError> {
        Err(ViewError::Pending)
    }
    fn begin_cpu_paint(
        self: Rc<Self>,
        context: CFRetained<CGContext>,
        current: &dyn Fn() -> bool,
    ) -> Result<Box<dyn NativeMarkdownPagedScenePaintSession>, ViewError> {
        self.validate_ready(current)?;
        Ok(Box::new(FiniteSession {
            scene: self,
            context,
        }))
    }
    fn paint(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<(), ViewError> {
        self.paint_native(context, current).map(drop)
    }
}

impl Scene {
    fn paint_native(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<NativeMarkdownPagedScenePaintCompletion, ViewError> {
        if !current() {
            return Err(ViewError::Revoked);
        }
        // 原sealed spool移入同步栈后才进CoreText；重入无法借到可变payload，也不会新建输出。
        let mut spool = self
            .payload
            .spool
            .borrow_mut()
            .take()
            .ok_or(ViewError::Pending)?;
        CGContext::save_g_state(Some(context));
        CGContext::set_rgb_fill_color(Some(context), 0.0, 0.0, 0.0, 1.0);
        let result = NativeMarkdownPagedScenePaintCompletion::paint_finite_spool(
            &mut spool,
            &self.payload.output,
            NativeMarkdownPagedGlyphPaintRequest {
                source: &self.payload.source_authority,
                font_authority: &self.payload.font_authority,
                native_font: &self.payload.font,
                glyph_range: 0..1,
                origin: NativeMarkdownPagedGlyphOrigin {
                    baseline: self.payload.baseline,
                    pen_x_26_6: 0,
                    pen_y_26_6: 0,
                },
            },
            &self.key,
            context,
            current,
        );
        CGContext::restore_g_state(Some(context)); // 失败也配平变换，部分pixels不能签为成功。
        let previous = self.payload.spool.borrow_mut().replace(spool);
        if previous.is_some() {
            return Err(ViewError::InvalidGeometry);
        }
        let (completion, receipt) = result?;
        if receipt.source_utf16_range != (0..1) || receipt.missing_glyphs != 0 || !current() {
            return Err(ViewError::Revoked);
        }
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
    ) -> Result<NativeMarkdownPagedScenePaintStep, ViewError> {
        self.scene.validate_ready(current)?;
        let count = 1;
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
    controller: Weak<NativeMarkdownController>,
    scene: RefCell<Option<Rc<Scene>>>,
}
impl NativeMarkdownPagedTextViewDelegate for Delegate {
    fn current(&self, epoch: u64) -> bool {
        epoch == 7
            && self.controller.load().is_some_and(|controller| {
                native_markdown_controller_has_foreground_authority(&controller)
            })
    }
    fn virtual_viewport(
        &self,
        epoch: u64,
    ) -> Result<NativeMarkdownPagedVirtualViewport, ViewError> {
        if !self.current(epoch) {
            return Err(ViewError::Revoked);
        }
        Ok(NativeMarkdownPagedVirtualViewport {
            document_origin: NSPoint::ZERO,
            scroll_epoch: 1,
        })
    }
    fn ready_scene(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Rc<dyn NativeMarkdownPagedTextScene>, ViewError> {
        let scene = self.scene.borrow().clone().ok_or(ViewError::Pending)?;
        if !self.current(7) || !scene.key.request.same(request) {
            return Err(ViewError::Pending);
        }
        Ok(scene)
    }
    fn scene_current(&self, key: &NativeMarkdownPagedSceneKey) -> bool {
        self.current(7)
            && self.scene.borrow().as_ref().is_some_and(|scene| {
                scene.key.same(key) && scene.payload.source.byte_offset_for_utf16(0).is_ok()
            })
    }
    fn committed(&self, _: &NativeMarkdownPagedInputCommit) -> Result<(), ViewError> {
        Err(ViewError::Pending) // 本控制不重排输入后继状态，不能伪造新的Ready。
    }
    fn reveal_committed_selection(
        &self,
        _: &NativeMarkdownPagedInputCommit,
    ) -> Result<(), ViewError> {
        // 本有限组件没有正文Producer/reveal续体，不能把选择提交伪装成实际可见。
        Err(ViewError::Pending)
    }
    fn copy_selection(&self, _: &NativeMarkdownPagedSceneRequest) -> Result<(), ViewError> {
        Err(ViewError::Pending)
    }
}

struct Canvas {
    context: CFRetained<CGContext>,
    pixels: Vec<u8>,
}
impl Canvas {
    fn new() -> Result<Self, String> {
        let mut pixels = vec![255u8; 720 * 300 * 4];
        // SAFETY: 框架静态sRGB常量在当前进程有效。
        let space =
            CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).ok_or("缺少sRGB")?;
        // SAFETY: 固定RGBA8完整allocation；context字段先析构，不会比pixels活得更久。
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                720,
                300,
                8,
                720 * 4,
                Some(&space),
                CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
            )
        }
        .ok_or("无法创建有限glyph画布")?;
        Ok(Self { context, pixels })
    }
    fn ink(&mut self) -> usize {
        CGContext::flush(Some(&self.context));
        self.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0].min(pixel[1]).min(pixel[2]) < 250)
            .count()
    }
    fn ink_in(&mut self, rect: NSRect) -> usize {
        CGContext::flush(Some(&self.context));
        // 默认NSView和本context均是y-up；RGBA数组从顶行起，转换后只数首glyph矩形内的真实pixels。
        self.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter(|(index, pixel)| {
                let x = (index % 720) as f64 + 0.5;
                let y = 300.0 - ((index / 720) as f64 + 0.5);
                x >= rect.origin.x
                    && x < rect.origin.x + rect.size.width
                    && y >= rect.origin.y
                    && y < rect.origin.y + rect.size.height
                    && pixel[0].min(pixel[1]).min(pixel[2]) < 250
            })
            .count()
    }
}

pub(super) fn run_ready_control(
    controller: &NativeMarkdownController,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    let (
        released,
        backing,
        source_observer,
        font_bytes,
        spool_observer,
        weak_view,
        weak_doc,
        weak_input,
    ) = autoreleasepool(|_| -> Result<_, String> {
        let owned = Rc::new(NativeMarkdownOwnedLedger::default());
        let mut file = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
        file.write_all(b"x").map_err(|error| error.to_string())?;
        let document = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(file.path(), 4096).map_err(|error| error.to_string())?,
        ));
        document
            .borrow_mut()
            .set_selection(MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 1,
            })
            .map_err(|error| error.to_string())?;
        let backing = document
            .borrow()
            .original_backing_observer()
            .ok_or("缺少真实mmap")?;
        let source = MarkdownLayoutTextSource::capture(&document.borrow());
        let source_observer = source.resource_observer();
        // SAFETY: 本地CFString在同步构造期有效；空matrix要求标准32pt字体，不传入外部指针。
        let native =
            unsafe { CTFont::with_name(&CFString::from_str("Helvetica"), 32.0, std::ptr::null()) };
        let font = NativeMarkdownPagedFont::capture(&native, 512 * 1024 * 1024, mtm, &|| true)
            .map_err(|error| error.to_string())?;
        let font_bytes = font.bytes_observer();
        let sa = source.authority();
        let fa = font.core_font().authority();
        let mut job = MarkdownShapingJob::new(
            source.clone(),
            &sa,
            font.core_font().clone(),
            &fa,
            0..1,
            MarkdownShapingOptions {
                direction: MarkdownShapingDirection::LeftToRight,
                script: *b"Latn",
                language: None,
                points: 32.0,
                beginning_of_text: true,
                end_of_text: true,
                features: Vec::new(),
            },
        )
        .map_err(|error| error.to_string())?;
        // 预声明仅一个ASCII scalar；按Core有界预算消费，同一任务才可进入shape和seal。
        while job.status() == MarkdownShapingStatus::Ingesting {
            job.step(
                &sa,
                &fa,
                MarkdownShapingBudget {
                    bytes: 1,
                    scalars: 1,
                },
            )
            .map_err(|error| error.to_string())?;
        }
        if job.shape(&sa, &fa).map_err(|error| error.to_string())? != MarkdownShapingStatus::Ready
            || job
                .profile()
                .map_err(|error| error.to_string())?
                .glyph_count
                != 1
        {
            return Err("有限glyph未真正Ready".into());
        }
        let glyph = job
            .glyph(&sa, &fa, 0)
            .map_err(|error| error.to_string())?
            .ok_or("没有真实glyph")?;
        let mut glyph_id = u16::try_from(glyph.glyph_id).map_err(|_| "glyph不能由原CTFont表示")?;
        if glyph_id == 0 || glyph.source_utf16_cluster != 0 {
            return Err("真实glyph缺失或Source不符".into());
        }
        // SAFETY: glyph来自同一个捕获font，单个u16指针完整存活；只请求一个真实设计包围盒。
        let bbox = unsafe {
            font.native_font().bounding_rects_for_glyphs(
                CTFontOrientation::Horizontal,
                NonNull::from(&mut glyph_id),
                std::ptr::null_mut::<CGRect>(),
                1,
            )
        };
        if bbox.size.width <= 0.0 || bbox.size.height <= 0.0 {
            return Err("真实glyph没有非空包围盒".into());
        }
        let mut builder = MarkdownShapedSpoolBuilder::new(&mut job, &sa, &fa)
            .map_err(|error| error.to_string())?;
        if builder
            .step(&mut job, 1)
            .map_err(|error| error.to_string())?
            .glyphs
            != 1
        {
            return Err("真实sealed输出不完整".into());
        }
        let spool = builder.seal(&mut job).map_err(|error| error.to_string())?;
        let spool_observer = spool.resource_observer();
        let output = spool.authority();
        drop(job);
        let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document))
            .map_err(|error| error.to_string())?;
        let delegate = Rc::new(Delegate {
            controller: Weak::new(controller),
            scene: RefCell::new(None),
        });
        let erased: Rc<dyn NativeMarkdownPagedTextViewDelegate> = delegate.clone();
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(720.0, 300.0));
        let view =
            NativeMarkdownPagedTextView::new(input.clone(), Rc::downgrade(&erased), 7, frame, mtm)
                .map_err(|error| error.to_string())?;
        if view.isFlipped() {
            return Err("本有限glyph控制必须使用实际默认y-up正文坐标".into());
        }
        let parent = NSView::initWithFrame(NSView::alloc(mtm), frame);
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        parent.addSubview(&scroll);
        scroll.setDocumentView(Some(&view));
        let bar = NativeMarkdownFindBar::new(controller, &owned, Rc::downgrade(&document), mtm)?;
        bar.show(&scroll, true, controller)?;
        bar.query.setStringValue(ns_string!("x"));
        let request = view
            .capture_scene_request()
            .map_err(|error| error.to_string())?;
        let baseline = CGPoint { x: 16.0, y: 48.0 };
        let x_offset = f64::from(glyph.x_offset) / 64.0;
        let y_offset = f64::from(glyph.y_offset) / 64.0;
        // 字形与first_rect共用实际y-up坐标、baseline、offset和CTFont包围盒，无占位行高或人为翻转。
        let rect = NSRect::new(
            NSPoint::new(
                baseline.x + x_offset + bbox.origin.x,
                baseline.y + y_offset + bbox.origin.y,
            ),
            bbox.size,
        );
        let payload = Rc::new(Payload {
            spool: RefCell::new(Some(spool)),
            output,
            source,
            source_authority: sa,
            font_authority: fa,
            font,
            baseline,
            rect,
        });
        let nonce = NativeMarkdownPagedSceneNonce::new();
        let scene = Rc::new(Scene {
            key: NativeMarkdownPagedSceneKey {
                request,
                source: payload.source_authority.clone(),
                nonce: nonce.authority(),
            },
            _nonce: nonce,
            payload: payload.clone(),
        });
        let mut canvas = Canvas::new()?;
        scene
            .paint(&canvas.context, &|| delegate.current(7))
            .map_err(|error| error.to_string())?;
        let ink = canvas.ink();
        let first_ink = canvas.ink_in(payload.rect);
        if ink == 0 || first_ink == 0 {
            return Err("真实Ready glyph绘图没有落在首位置包围盒内的墨迹".into());
        }
        *delegate.scene.borrow_mut() = Some(scene.clone());
        let key = view
            .capture_ready_scene_key()
            .map_err(|error| error.to_string())?;
        let proof = view
            .verify_find_target_visible(&key, NSRange::new(0, 1))
            .map_err(|error| error.to_string())?;
        let port = PendingPort::default();
        let execute = || {
            execute_find_backend(
                &bar,
                NativeMarkdownFindBackend::Paged(NativeMarkdownPagedFindBackend {
                    view: &view,
                    input: &input,
                    epoch: 7,
                    controller,
                    intent: controller.find_input_intent(),
                    reveal: &port,
                }),
                MarkdownFindAction::Next,
            )
        };
        if !matches!(execute(), Ok(NativeMarkdownFindExecution::Complete))
            || !bar.probe_found_feedback()
            || port.0.borrow().is_some()
        {
            return Err("真实Ready同票Find未完成原可见fast path".into());
        }
        // 同Source/generation也不能借新nonce挽救旧proof；只验证拒绝，不改实际glyph几何。
        let nonce = NativeMarkdownPagedSceneNonce::new();
        *delegate.scene.borrow_mut() = Some(Rc::new(Scene {
            key: NativeMarkdownPagedSceneKey {
                request: key.request.clone(),
                source: payload.source_authority.clone(),
                nonce: nonce.authority(),
            },
            _nonce: nonce,
            payload: payload.clone(),
        }));
        if !matches!(
            view.verify_find_target_visible(proof.key(), proof.range()),
            Err(ViewError::Revoked)
        ) {
            return Err("后继Ready错误地重签旧Find可见proof".into());
        }
        // 移除现成Scene后同一真实Find只能Pending；不得凭刚才的成功或仍存活的旧Rc缓存完成。
        delegate.scene.borrow_mut().take();
        let pending = match execute() {
            Ok(NativeMarkdownFindExecution::Pending(request)) => request,
            _ => return Err("移除Ready后Find没有保持Pending".into()),
        };
        if pending.completed_visible() || bar.probe_found_feedback() {
            return Err("Pending复用了旧可见成功".into());
        }
        port.cancel_pending();
        drop(pending);
        let weak_view = Weak::new(&*view);
        let weak_doc = Rc::downgrade(&document);
        let weak_input = Rc::downgrade(&input);
        owned.include("paged-find-ready.view", &view)?;
        owned.include("paged-find-ready.scroll", &scroll)?;
        owned.include("paged-find-ready.parent", &parent)?;
        let mut released = owned.snapshot();
        released.seal();
        bar.finish_accepted();
        scroll.setDocumentView(None);
        let close = view.begin_close().map_err(|error| error.to_string())?;
        wait_closed_view(&close)?;
        drop(port);
        drop(bar);
        drop(scroll);
        drop(parent);
        drop(view);
        drop(erased);
        drop(delegate);
        drop(scene);
        drop(payload);
        drop(input);
        drop(document);
        println!(
            "native_paged_find_ready actual_ink={ink} first_rect_ink={first_ink} actual_ctfont_rect=true same_ready_visible=true nonce_rejected=true pending_after_ready=true no_body_textkit=true formal=false"
        );
        Ok((
            released,
            backing,
            source_observer,
            font_bytes,
            spool_observer,
            weak_view,
            weak_doc,
            weak_input,
        ))
    })?;
    let spool = spool_observer.snapshot();
    if weak_view.load().is_some()
        || weak_doc.upgrade().is_some()
        || weak_input.upgrade().is_some()
        || !source_observer.payload_released()
        || !backing.unmap_succeeded()
        || backing.mapped_bytes() != 0
        || backing.failed()
        || !font_bytes.unmap_succeeded()
        || font_bytes.mapped_bytes() != 0
        || font_bytes.failed()
        || spool.file_open
        || !spool.close_succeeded
        || spool.cleanup_failed
        || !spool.source_payload_released
        || !spool.font_payload_released
    {
        return Err("Ready Find控制原Native/Source/Font/spool未实际释放".into());
    }
    released.verify_released()?;
    Ok(())
}
