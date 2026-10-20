//! 正式模式往返核同一分页客户的真实完整 CPU 帧和屏幕字形，不借旧 TextKit 定位请求签成功。

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputState;
use crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError;
use std::time::{Duration, Instant};

/// 只保原会话弱身份、完整输入事实和一个 scalar 的屏幕锚点；等待不会保活正文、Native 图或目录。
pub(super) struct ModeViewportExpectation {
    document: WeakMarkdownDocument,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    controller: Weak<NativeMarkdownController>,
    generation: u64,
    selection: MarkdownTextSelection,
    input: NativeMarkdownPagedInputState,
    probe: NSRange,
    screen_top: f64,
    viewport_top: f64,
    source: bool,
    owner: usize,
    view: usize,
    scroll: usize,
    clip: usize,
    started: Instant,
    frame: RefCell<Option<(NativeMarkdownPagedSceneKey, usize, Duration)>>,
    signal: RefCell<Option<Box<dyn FnOnce()>>>,
    failure: RefCell<Option<String>>,
}

/// 两种模式都从当前完整 CPU 帧查询首字；仅 Ready、同 generation 或一个非空像素缓存均不足以通过。
fn screen_frame(
    editor: &crate::native_surface::NativeMarkdownEditorSurface,
    probe: NSRange,
) -> Result<(NSRect, NSRect, NativeMarkdownPagedSceneKey), String> {
    let source = editor;
    let view = &source.text_view;
    let key = view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let published = view
        .probe_cpu_published_frame()
        .ok_or("正式模式没有真实完整 CPU 帧")?;
    if key.presentation_mode != editor.presentation_mode()
        || key.presentation_mode != source.body.presentation_mode()
        || !published.0.same(&key)
        || published.1 == 0
        || view.cpu_paints() != Some(published.1)
        || view.cpu_paint_pending() != Some(false)
    {
        return Err("正式模式当前 Ready 与实际 CPU 发布帧或展示模式不同".into());
    }
    let visible = view
        .verify_find_target_visible(&key, probe)
        .map_err(|cause| cause.to_string())?;
    let actual = view.screen_rect(probe).map_err(|cause| cause.to_string())?;
    if actual.actual_range.location > probe.location
        || actual
            .actual_range
            .location
            .checked_add(actual.actual_range.length)
            .is_none_or(|end| end < probe.location + probe.length)
        || (actual.rect.size.height - visible.first_rect().size.height).abs() > 0.5
    {
        return Err("正式模式首 scalar 的完整字形未进入实际 clip".into());
    }
    let window = view.window().ok_or("正式分页窗口已经释放")?;
    let viewport = window.convertRectToScreen(view.convertRect_toView(key.request.viewport, None));
    // 屏幕转换是真实 Native 边界；其后必须仍是同一次已发布帧，不能领取后继 key 补签。
    if !view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?
        .same(&key)
        || view
            .probe_cpu_published_frame()
            .is_none_or(|(actual, paints)| !actual.same(&key) || paints != published.1)
        || view.cpu_paint_pending() != Some(false)
    {
        return Err("正式模式屏幕字形查询被后继输入或几何撤销".into());
    }
    Ok((actual.rect, viewport, key))
}

impl NativeMarkdownPagedBodyPaintObserver for ModeViewportExpectation {
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, paints: usize) {
        if self.signal.borrow().is_none() {
            return; // 一次观察已经消费；后继帧不能覆盖这次原动作的结论。
        }
        let checked = (|| {
            let body = self.body.upgrade().ok_or("模式 CPU 帧原 Body 已释放")?;
            self.require_original(&body)?;
            let view = body.view().map_err(|cause| cause.to_string())?;
            let elapsed = self.started.elapsed();
            if key.presentation_mode != self.mode()
                || key.request.input != self.input
                || !std::rc::Weak::ptr_eq(&key.request.document, &self.document)
                || !view
                    .capture_ready_scene_key()
                    .map_err(|cause| cause.to_string())?
                    .same(key)
                || view.cpu_paint_pending() != Some(false)
                || view.cpu_paints() != Some(paints)
                || paints == 0
                || view
                    .probe_cpu_published_frame()
                    .is_none_or(|(actual, count)| !actual.same(key) || count != paints)
            {
                return Err("模式终点不是原输入在目标模式的真实当前完整 CPU 帧".into());
            }
            self.require_original(&body)?;
            if elapsed > Duration::from_secs(3) {
                return Err(format!(
                    "正式模式动作到完整 CPU 帧耗时 {:.3}ms，超过原三秒期限",
                    elapsed.as_secs_f64() * 1000.0
                ));
            }
            *self.frame.borrow_mut() = Some((key.clone(), paints, elapsed));
            Ok(())
        })();
        if let Err(cause) = checked {
            *self.failure.borrow_mut() = Some(cause);
        }
        // 结束原槽借用后再交付外部事件；该闭包不能在 RefMut 内重入原场景或释放 Native weak。
        let signal = self.signal.borrow_mut().take();
        if let Some(signal) = signal {
            signal();
        }
    }
}

impl ModeViewportExpectation {
    fn mode(&self) -> MarkdownPresentationMode {
        if self.source {
            MarkdownPresentationMode::Source
        } else {
            MarkdownPresentationMode::InlinePreview
        }
    }

    /// 同一个 Body 固定持有原 Input 和视图；完整 InputState 保留主方向、多选、marked、fold 与 serial。
    /// Native controller weak 的加载与前台查询不持 Doc 或回调槽借用，之后再核一遍真实输入。
    fn require_original(&self, body: &Rc<NativeMarkdownPagedBody>) -> Result<(), String> {
        if !std::rc::Weak::ptr_eq(&self.body, &Rc::downgrade(body))
            || body.presentation_mode() != self.mode()
        {
            return Err("模式终点替换了原 Body 或尚未安装目标模式".into());
        }
        let controller = self.controller.load().ok_or("模式原 controller 已释放")?;
        if std::ptr::from_ref(&*controller).addr() != self.owner
            || !native_markdown_controller_has_foreground_authority(&controller)
        {
            return Err("模式动作不再拥有原前台 controller".into());
        }
        drop(controller);
        let view = body.view().map_err(|cause| cause.to_string())?;
        let input = view.input_state().map_err(|cause| cause.to_string())?;
        let document = self.document.upgrade().ok_or("模式原文稿已释放")?;
        let document = document.borrow();
        if std::ptr::from_ref(&**view).addr() != self.view
            || body.presentation_mode() != self.mode()
            || input != self.input
            || document.generation() != self.generation
            || document.selection() != self.selection
        {
            return Err("模式终点改变了原文稿、Input、主选择或正文客户".into());
        }
        Ok(())
    }

    /// 捕获产品本次实际交接的源字形，再独立测其旧屏幕位置；不把离屏选择冒充可见锚点。
    /// 这里只验证该真实字形的前后几何，不声称独立验证产品挑选锚点的策略。
    pub(super) fn capture(
        surface: &NativeMarkdownWindowSurface,
        source: bool,
    ) -> Result<Self, String> {
        let editor = surface.editor.as_ref().ok_or("模式视口验收缺少原 editor")?;
        let paged = editor;
        let (anchor, original_key) = paged
            .body
            .capture_mode_viewport()
            .map_err(|cause| cause.to_string())?;
        let document = editor.document().borrow();
        let selection = document.selection();
        let generation = document.generation();
        if !Rc::ptr_eq(
            &anchor.document.upgrade().ok_or("模式交接原文稿已释放")?,
            editor.document(),
        ) || anchor.generation != generation
            || anchor.length != document.utf16_len()
            || anchor.selection != selection
        {
            return Err("模式交接字形不是原文稿或原选择的实际捕获".into());
        }
        // 原选择与交接锚各只核一个完整scalar；前者可以离屏，但不能修改选择或借替代输入补权。
        // 不采用产品给出的screen_y，后面仍由原视图的独立screen_frame取得实际屏幕几何。
        let mut probes = [NSRange::new(0, 0); 2];
        for (probe, offset) in probes.iter_mut().zip([
            selection.anchor_utf16.min(selection.head_utf16),
            anchor.probe.start,
        ]) {
            let width = if offset == document.utf16_len() {
                0
            } else {
                let first = document
                    .utf16_code_unit_at(offset)
                    .map_err(|cause| cause.to_string())?;
                if (0xd800..=0xdbff).contains(&first) {
                    2
                } else {
                    1
                }
            };
            let end = offset.checked_add(width).ok_or("模式scalar范围溢出")?;
            document
                .byte_offset_for_utf16(offset)
                .map_err(|cause| cause.to_string())?;
            document
                .byte_offset_for_utf16(end)
                .map_err(|cause| cause.to_string())?;
            *probe = NSRange::new(offset, width);
        }
        let [selection_probe, probe] = probes;
        if anchor.probe != (probe.location..probe.location + probe.length) {
            return Err("模式交接锚点没有完整覆盖一个实际scalar".into());
        }
        drop(document); // 其后实际 Native getter 可以交付通知；不能持文稿借用跨屏幕转换。
        // 原Find选中项若确实可见，仍必须保留该项首scalar的专属位置断言；只有真实离屏
        // Pending才允许交接另一个已显示字形，Source/权限/I/O错误不能被解释为离屏。
        match paged
            .text_view
            .verify_find_target_visible(&original_key, selection_probe)
        {
            Ok(_) if probe != selection_probe => {
                return Err("模式交接没有保留原可见选择的首scalar".into());
            }
            Ok(_) | Err(NativeMarkdownPagedTextViewError::Pending) => {}
            Err(cause) => return Err(cause.to_string()),
        }
        let (frame, viewport, key) = screen_frame(editor, probe)?;
        let input = paged
            .text_view
            .input_state()
            .map_err(|cause| cause.to_string())?;
        let clip = paged.scroll_view.contentView();
        let controller = Weak::from_retained(&surface.controller);
        if ![
            frame.origin.y,
            frame.size.height,
            frame.origin.y + frame.size.height,
        ]
        .into_iter()
        .all(f64::is_finite)
            || frame.size.height <= 0.0
            || input != key.request.input
            || !key.same(&original_key)
            || input.generation != generation
            || input.selection != selection
            || !paged
                .text_view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?
                .same(&key)
        {
            return Err("原模式字形不是同一完整输入中的有限正高屏幕位置".into());
        }
        Ok(Self {
            document: Rc::downgrade(editor.document()),
            body: Rc::downgrade(&paged.body),
            controller,
            generation,
            selection,
            input,
            probe,
            screen_top: frame.origin.y + frame.size.height,
            viewport_top: viewport.origin.y + viewport.size.height,
            source,
            owner: std::ptr::from_ref(&*surface.controller).addr(),
            view: std::ptr::from_ref(&**paged.text_view).addr(),
            scroll: std::ptr::from_ref(&*paged.scroll_view).addr(),
            clip: std::ptr::from_ref(&*clip).addr(),
            started: Instant::now(),
            frame: RefCell::new(None),
            signal: RefCell::new(None),
            failure: RefCell::new(None),
        })
    }

    /// 原 selector 排队完成后只观察同 Body 的 CPU 发布；不再创建或伪用 TextKit ModeViewportRequest。
    pub(super) fn observe_preview_terminal(
        self: &Rc<Self>,
        controller: &Retained<NativeMarkdownController>,
        observer: impl FnOnce() + 'static,
    ) -> Result<(), String> {
        if std::ptr::from_ref(&**controller).addr() != self.owner || self.signal.borrow().is_some()
        {
            return Err("模式完整帧观察不属于原 controller 或已在等待".into());
        }
        *self.signal.borrow_mut() = Some(Box::new(observer));
        let captured = MainThreadBound::new(Rc::downgrade(self), controller.mtm());
        controller.enqueue_owned(move || {
            let mtm = MainThreadMarker::new().expect("模式 CPU 帧登记只在原主队列执行");
            let Some(expected) = captured.into_inner(mtm).upgrade() else {
                return;
            };
            let registered = (|| {
                let body = expected.body.upgrade().ok_or("模式观察原 Body 已释放")?;
                expected.require_original(&body)?;
                let view = body.view().map_err(|cause| cause.to_string())?;
                if let Some((key, paints)) = view.probe_cpu_published_frame()
                    && key.presentation_mode == expected.mode()
                    && key.request.input == expected.input
                    && paints != 0
                    && view.cpu_paint_pending() == Some(false)
                    && view
                        .capture_ready_scene_key()
                        .is_ok_and(|ready| ready.same(&key))
                {
                    drop(view);
                    drop(body); // 外部终点回调前归还同步 Native 客户与 Body claim，跨事件只留下原弱观察。
                    expected.frame_published(&key, paints);
                    return Ok(()); // 只接已经真实完成的目标模式帧；旧 Source 同 generation 不能冒充。
                }
                let erased: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = expected.clone();
                body.observe_next_cpu_frame(Rc::downgrade(&erased))
                    .map_err(|cause| cause.to_string())
            })();
            if let Err(cause) = registered {
                *expected.failure.borrow_mut() = Some(cause);
                let signal = expected.signal.borrow_mut().take();
                if let Some(signal) = signal {
                    signal();
                }
            }
        });
        Ok(())
    }

    /// 同一 Doc/Input/Body/scroll/clip、目标模式完整帧及独立 screen 字形同时成立，才消费本次模式期望。
    pub(super) fn verify(&self, surface: &NativeMarkdownWindowSurface) -> Result<(), String> {
        if let Some(cause) = self.failure.borrow().as_ref() {
            return Err(cause.clone());
        }
        let editor = surface
            .editor
            .as_ref()
            .ok_or("模式视口验收缺少目标 editor")?;
        let paged = editor;
        self.require_original(&paged.body)?;
        let clip = paged.scroll_view.contentView();
        if std::ptr::from_ref(&*surface.controller).addr() != self.owner
            || !std::rc::Weak::ptr_eq(&self.document, &Rc::downgrade(editor.document()))
            || std::ptr::from_ref(&*paged.scroll_view).addr() != self.scroll
            || std::ptr::from_ref(&*clip).addr() != self.clip
            || editor.presentation_mode() != self.mode()
        {
            return Err("模式交接替换了原窗口、文稿、滚动容器或视口".into());
        }
        let (frame, viewport, key) = screen_frame(editor, self.probe)?;
        let observed = self.frame.borrow().clone();
        let elapsed = if let Some((published, paints, elapsed)) = observed {
            // Memory 的 Source 与 Preview 都登记同一真实 CPU 终点；SourceSession 的 Source
            // 仍由原 SourceFrame 先核完整帧，不能因为 generation 相同接受目标模式之外的旧帧。
            if !published.same(&key) || paged.text_view.cpu_paints() != Some(paints) {
                return Err("模式原完整 CPU 帧已被后继输入或几何撤销".into());
            }
            elapsed
        } else if !self.source {
            return Err("Preview 没有原动作的真实完整 CPU 帧终点".into());
        } else {
            // 返回 Source 的父 SourceFrame 已核真实发布；这里再以动作至验证的上界守同一三秒期限。
            self.started.elapsed()
        };
        if elapsed > Duration::from_secs(3) {
            return Err("正式模式动作未在原三秒内完成完整 CPU 帧验证".into());
        }
        let before_y = self.viewport_top - self.screen_top;
        let after_y = viewport.origin.y + viewport.size.height - frame.origin.y - frame.size.height;
        let delta = after_y - before_y;
        let glyph_clamped =
            delta.abs() > 0.5 && self.actual_preview_glyph_clamps(surface, frame, viewport)?;
        let clamped = delta.abs() > 0.5 && actual_boundary_clamps(editor, delta)?;
        self.require_original(&paged.body)?;
        if !paged
            .text_view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?
            .same(&key)
            || paged.text_view.cpu_paint_pending() != Some(false)
            || paged
                .text_view
                .probe_cpu_published_frame()
                .is_none_or(|(actual, _)| !actual.same(&key))
        {
            return Err("模式实际 clip 约束查询撤销了原完整 CPU 帧".into());
        }
        eprintln!(
            "native_source_mode_viewport source={} scalar={}..{} before_y={before_y} after_y={after_y} glyph_screen={frame:?} viewport_screen={viewport:?} screen_delta={delta} actual_boundary_clamped={clamped} actual_full_glyph_clamped={glyph_clamped} actual_cpu_mode={:?} actual_cpu_paints={:?} elapsed_ms={:.3} same_body_input_scroll_clip=true selection_unchanged=true generation_unchanged=true",
            self.source,
            self.probe.location,
            self.probe.location + self.probe.length,
            key.presentation_mode,
            paged.text_view.cpu_paints(),
            elapsed.as_secs_f64() * 1000.0,
        );
        if !delta.is_finite() || delta.abs() > 0.5 && !clamped && !glyph_clamped {
            return Err(format!(
                "正式模式往返没有保持原 scalar 字形屏幕位置，偏移{delta}px"
            ));
        }
        Ok(())
    }

    /// 目标真实字形变高时，只允许完整显示所必需的最近位置；原位置可显示时仍严格守半像素容差。
    fn actual_preview_glyph_clamps(
        &self,
        surface: &NativeMarkdownWindowSurface,
        frame: NSRect,
        viewport: NSRect,
    ) -> Result<bool, String> {
        if self.source {
            return Ok(false);
        }
        let editor = surface.editor.as_ref().expect("原模式验证已取得 editor");
        let preview = editor;
        self.require_original(&preview.body)?;
        let before_y = self.viewport_top - self.screen_top;
        let after_y = viewport.origin.y + viewport.size.height - frame.origin.y - frame.size.height;
        let last_top = viewport.size.height - frame.size.height;
        if last_top < 0.0 {
            return Ok(false);
        }
        let projected_y = before_y.clamp(0.0, last_top);
        if (projected_y - before_y).abs() <= 0.5 {
            return Ok(false);
        }
        let view = &preview.text_view;
        let key = view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let current = || {
            self.require_original(&preview.body).is_ok()
                && key.presentation_mode == self.mode()
                && view
                    .capture_ready_scene_key()
                    .is_ok_and(|actual| actual.same(&key))
        };
        // 只查询真实 NSClip 的约束，不滚动、不改高度、不把临时 prefix 底边当作完整文尾。
        let boundaries = (|| -> Result<_, NativeMarkdownStableTableFailure> {
            let clip =
                native_markdown_table_intent_step(&current, || preview.scroll_view.contentView())?;
            let document =
                native_markdown_table_intent_step(&current, || preview.scroll_view.documentView())?
                    .ok_or(NativeMarkdownStableTableFailure::MissingGeometry)?;
            // Paged 的真实 documentView 是空高度容器，有限正文是其子视图；
            // 核两条实际挂载关系，不能沿用旧 TextKit 将正文自身当 documentView 的前提。
            // SAFETY: 原弱 Body 已升级并持活主线程 view；这里只读取公开 parent，不改变 Native 图。
            let parent =
                native_markdown_table_intent_step(&current, || unsafe { view.superview() })?;
            let enclosing =
                native_markdown_table_intent_step(&current, || view.enclosingScrollView())?;
            if !parent
                .as_deref()
                .is_some_and(|actual| std::ptr::eq(actual, &*document))
                || !enclosing
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq(actual, &*preview.scroll_view))
            {
                return Err(NativeMarkdownStableTableFailure::InvalidSourceAssociation);
            }
            let document_frame = native_markdown_table_intent_step(&current, || document.frame())?;
            let bounds = native_markdown_table_intent_step(&current, || clip.bounds())?;
            let convert = |point| {
                let window = native_markdown_table_intent_step(&current, || {
                    surface.panel.convertPointFromScreen(point)
                })?;
                native_markdown_table_intent_step(&current, || {
                    clip.convertPoint_fromView(window, None)
                })
            };
            let top = viewport.origin.y + viewport.size.height;
            let actual = convert(NSPoint::new(
                frame.origin.x,
                frame.origin.y + frame.size.height,
            ))?;
            let desired = convert(NSPoint::new(frame.origin.x, top - before_y))?;
            let nearest = convert(NSPoint::new(frame.origin.x, top - projected_y))?;
            let mut proposed = bounds;
            proposed.origin.y += actual.y - desired.y;
            let mut projected = bounds;
            projected.origin.y += actual.y - nearest.y;
            let constrained =
                native_markdown_table_intent_step(&current, || clip.constrainBoundsRect(proposed))?;
            let projected_constrained = native_markdown_table_intent_step(&current, || {
                clip.constrainBoundsRect(projected)
            })?;
            let final_bounds = native_markdown_table_intent_step(&current, || clip.bounds())?;
            Ok((
                document_frame,
                bounds,
                final_bounds,
                proposed,
                constrained,
                projected,
                projected_constrained,
            ))
        })()
        .map_err(|cause| format!("完整字形最小投影的实际 clip 查询失败：{cause:?}"))?;
        let (
            document_frame,
            bounds,
            final_bounds,
            proposed,
            constrained,
            projected,
            projected_constrained,
        ) = boundaries;
        let document_unclamped = (constrained.origin.y - proposed.origin.y).abs() <= 0.5
            && (constrained.origin.x - proposed.origin.x).abs() <= 0.5
            && constrained.size == proposed.size;
        let nearest_is_current = (projected_constrained.origin.y - bounds.origin.y).abs() <= 0.5
            && (projected_constrained.origin.x - bounds.origin.x).abs() <= 0.5
            && projected_constrained.size == bounds.size
            && final_bounds == bounds;
        let residual = after_y - projected_y;
        let confirmed = current()
            && document_unclamped
            && nearest_is_current
            && residual.is_finite()
            && residual.abs() <= 0.5;
        eprintln!(
            "native_source_mode_glyph_boundary before_y={before_y} actual_y={after_y} glyph_height={} viewport_height={} legal_top=0..{last_top} projected_top={projected_y} necessary_delta={} residual={residual} document_frame={document_frame:?} proposed={proposed:?} constrained={constrained:?} projected={projected:?} projected_constrained={projected_constrained:?} current={bounds:?} document_unclamped={document_unclamped} nearest_is_current={nearest_is_current} confirmed={confirmed}",
            frame.size.height,
            viewport.size.height,
            projected_y - before_y,
        );
        Ok(confirmed)
    }
}

/// 仅真实文首或已完整度量的文尾可夹回；短prefix的临时底边不能作为位置正确的证明。
fn actual_boundary_clamps(
    editor: &crate::native_surface::NativeMarkdownEditorSurface,
    delta: f64,
) -> Result<bool, String> {
    let source = editor;
    let view = &source.text_view;
    let key = view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let origin = key.request.virtual_viewport.document_origin;
    let desired_y = origin.y + delta;
    let progress = source.body.progress().map_err(|cause| cause.to_string())?;
    // 文首固定为原Source的0；文尾则必须是真正完成的Source，不接受刚覆盖目标行的短前缀。
    if desired_y >= 0.0 && !progress.complete {
        return Ok(false);
    }
    let scroll = &source.scroll_view;
    let clip = scroll.contentView();
    let document = scroll.documentView().ok_or("模式边界缺少原滚动文档")?;
    let bounds = clip.bounds();
    let old = clip.convertPoint_fromView(origin, Some(&document));
    let target = clip.convertPoint_fromView(NSPoint::new(origin.x, desired_y), Some(&document));
    let mut proposed = bounds;
    proposed.origin.y += target.y - old.y;
    // 只查询系统原约束，不滚动、不扩高，也不改变产品状态以使测试通过。
    let constrained = clip.constrainBoundsRect(proposed);
    if !view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?
        .same(&key)
    {
        return Err("模式边界查询期间原布局已经变化".into());
    }
    let clamped = [proposed.origin.y, constrained.origin.y, bounds.origin.y]
        .into_iter()
        .all(f64::is_finite)
        && (proposed.origin.y - constrained.origin.y).abs() > 0.5
        && (constrained.origin.y - bounds.origin.y).abs() <= 0.5
        && (constrained.origin.x - bounds.origin.x).abs() <= 0.5
        && constrained.size == bounds.size;
    eprintln!(
        "native_source_mode_boundary desired_source_y={desired_y} actual_source_y={} complete={} proposed={proposed:?} constrained={constrained:?} current={bounds:?} confirmed={clamped}",
        origin.y, progress.complete
    );
    Ok(clamped)
}
