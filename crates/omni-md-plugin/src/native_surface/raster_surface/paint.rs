//! 同一真实 viewport 的标准 fragment 只向一份 RGBA8 位图绘制，随后直接交给 CALayer。

use super::authority::Authority;
use super::*;
use objc2::runtime::Bool;
use objc2_app_kit::{
    NSTextContentManager, NSTextElement, NSTextLayoutFragment,
    NSTextLayoutFragmentEnumerationOptions, NSTextLayoutFragmentState, NSTextLocation,
};
use objc2_core_foundation::CGAffineTransform;
use objc2_core_graphics::CGContext;
use objc2_foundation::NSComparisonResult;

struct Fragment {
    native: Retained<NSTextLayoutFragment>,
    frame: NSRect,
    rendering_bounds: NSRect,
    source: Retained<NSTextRange>,
    element: Retained<NSTextElement>,
    element_range: Retained<NSTextRange>,
    #[cfg(feature = "native-block-fragment-diagnostic")]
    diagnostic_outflow: bool,
}

struct Enumeration {
    frontier: Retained<ProtocolObject<dyn NSTextLocation>>,
    visited: usize,
    complete: bool,
    error: Option<String>,
    visible: Vec<Rc<Fragment>>,
    flow: Option<PrimaryFlow>,
    previous_frame_end: Option<f64>,
}

/// 连续源流与需画ink分别拥有权限；首尾共享同批实际fragment，不再由ink过滤结果猜边界。
struct PrimaryFlow {
    first: Rc<Fragment>,
    last: Rc<Fragment>,
}

impl PrimaryFlow {
    fn include(flow: &mut Option<Self>, fragment: &Rc<Fragment>) {
        match flow {
            Some(flow) => flow.last = Rc::clone(fragment), // 纯Rc共享原claim，不再次retain Native。
            None => {
                *flow = Some(Self {
                    first: Rc::clone(fragment),
                    last: Rc::clone(fragment),
                }); // 首个实际框即源流起点，即使其渲染范围为空也不能丢失。
            }
        }
    }
}

struct CollectedBatch {
    flow: Option<PrimaryFlow>,
    visible: Vec<Rc<Fragment>>,
}

impl CollectedBatch {
    fn empty() -> Self {
        Self {
            flow: None,
            visible: Vec::new(),
        } // 只供已证明的终端；普通非空正文仍须独立完整源覆盖。
    }
}

/// 单一文字容器沿Y向排段，项目段前/后间距均非负；ink可越界，布局框不能逆向覆盖。
/// text_attributes与preview_blocks只添加非负段距，这里不把墨迹外溢误当作负段距。
fn frame_follows_source(previous_end: Option<f64>, frame: NSRect) -> bool {
    let end = frame.origin.y + frame.size.height;
    geometry::finite(frame)
        && frame.size.height >= 0.0
        && end.is_finite()
        && previous_end.is_none_or(|previous| frame.origin.y + 0.000_001 >= previous)
}

/// 流边界来自完整已认证枚举的首末框，与它们是否有ink、是否进入待画集合无关。
fn complete_flow_bounds(first: NSRect, last: NSRect) -> Option<(f64, f64)> {
    if !frame_follows_source(None, first) || !frame_follows_source(None, last) {
        return None;
    }
    let bottom = last.origin.y + last.size.height;
    (bottom >= first.origin.y).then_some((first.origin.y, bottom))
}

#[derive(Clone, Copy)]
enum OutflowSourceSide {
    Before,
    After,
}

/// 只认证布局框与实际primary流的源顺序；绘制仍用完整rendering bounds，不裁负ink。
fn outflow_frame_connects(
    side: OutflowSourceSide,
    frame: NSRect,
    primary_top: f64,
    primary_bottom: f64,
) -> bool {
    if !frame_follows_source(None, frame)
        || !primary_top.is_finite()
        || !primary_bottom.is_finite()
        || primary_bottom < primary_top
    {
        return false;
    }
    match side {
        OutflowSourceSide::Before => frame.origin.y + frame.size.height <= primary_top + 0.000_001,
        OutflowSourceSide::After => frame.origin.y + 0.000_001 >= primary_bottom,
    }
}

/// 无 Native 字段；即使下层校验提前失败，也会恢复同步重入标志。
struct Painting<'a>(&'a Cell<bool>);
impl Drop for Painting<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

enum GraphicsRestore {
    Stack,
    Current(Option<Retained<NSGraphicsContext>>),
}
impl Drop for GraphicsRestore {
    fn drop(&mut self) {
        match self {
            Self::Stack => NSGraphicsContext::restoreGraphicsState_class(), // drawRect还原原设备context栈。
            Self::Current(previous) => NSGraphicsContext::setCurrentContext(previous.as_deref()), // layer还原原可空槽。
        }
    }
}

pub(super) fn paint(
    owner: &NativeMarkdownRasterSurface,
    view: &PieceBackedMarkdownTextView,
    permit: &NativeMarkdownRasterOriginPermit,
) -> Result<bool, String> {
    paint_entry(owner, view, permit, false)
}

pub(super) fn paint_layer(
    owner: &NativeMarkdownRasterSurface,
    view: &PieceBackedMarkdownTextView,
    permit: &NativeMarkdownRasterOriginPermit,
) -> Result<bool, String> {
    paint_entry(owner, view, permit, true)
}

fn paint_entry(
    owner: &NativeMarkdownRasterSurface,
    view: &PieceBackedMarkdownTextView,
    permit: &NativeMarkdownRasterOriginPermit,
    layer_entry: bool,
) -> Result<bool, String> {
    if !permit.current(owner, view) {
        return Ok(false);
    }
    if owner.painting.replace(true) {
        return Err("光栅绘制发生同步重入".into());
    }
    let _painting = Painting(&owner.painting);
    let _operation = OperationGuard::new(Arc::clone(&owner.counters));
    autoreleasepool(|_| paint_visible(owner, view, permit, layer_entry))
}

fn paint_visible(
    owner: &NativeMarkdownRasterSurface,
    view: &PieceBackedMarkdownTextView,
    permit: &NativeMarkdownRasterOriginPermit,
    layer_entry: bool,
) -> Result<bool, String> {
    let Some(auth) = Authority::capture(owner, view, permit)? else {
        return Ok(false);
    };
    // updateLayer没有窗口绘图context；只用同一原Authority的实际视口/scale，并直接创建
    // 下方唯一CPU画布。drawRect对照仍完整验证原设备context、clip与CTM，不拿假context过门。
    let original = if layer_entry {
        None
    } else {
        Some(
            auth.step(NSGraphicsContext::currentContext)?
                .ok_or("原正文绘图 context 缺失")?,
        )
    };
    let original_cg = match original.as_ref() {
        Some(original) => {
            let cg = auth.step(|| original.CGContext())?;
            let supplied_clip = auth.step(|| CGContext::clip_bounding_box(Some(&cg)))?;
            if !geometry::finite(supplied_clip)
                || !geometry::intersects(supplied_clip, auth.visible)
            {
                return Err("原正文没有有效视口绘图 clip".into());
            }
            let transform = auth.step(|| CGContext::ctm(Some(&cg)))?;
            if ![
                transform.a,
                transform.b,
                transform.c,
                transform.d,
                transform.tx,
                transform.ty,
            ]
            .into_iter()
            .all(f64::is_finite)
                || transform.a <= 0.0
                || transform.d == 0.0
                || transform.b.abs() > 0.000_001
                || transform.c.abs() > 0.000_001
                || (transform.a.abs() - transform.d.abs()).abs() > 0.000_001 * transform.a.abs()
            {
                return Err("原正文设备变换不支持精确光栅映射".into());
            }
            Some(cg)
        }
        None => None, // 真正layer入口不要求不存在的window clip，也不补造第二份位图。
    };
    let body_layer = if layer_entry {
        let lease = view
            .ivars()
            .raster_body_layer
            .borrow()
            .clone()
            .ok_or("真实layer入口缺少原正文backing租约")?;
        let layer = auth
            .step(|| lease.installed_layer())?
            .ok_or("原正文backing层已释放")?;
        verify_body_layer(&auth, &layer)?;
        Some(layer)
    } else {
        None
    };
    let mut collected = if auth.terminal_viewport {
        collect_terminal(&auth)? // 已证实EOF可没有字形；仍尽可能取得实际末片作布局流边界。
    } else {
        collect(&auth)?
    };
    let outflow = collect_outflow(&auth, collected.flow.as_ref(), &mut collected.visible)?;
    let requires_table_paint = if collected.visible.is_empty() && !auth.terminal_viewport {
        let Some(required) = auth.table_overlay_requirement()? else {
            return Ok(false); // 无当前占位结论时保留Pending，不把未知内容发布成空背景。
        };
        required
    } else {
        false // 实际glyph或已认证终端已覆盖正文；表格仍在同一画布正常绘制。
    };
    if requires_table_paint
        && !view
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| !cache.tables.is_empty())
    {
        return Ok(false); // 原流完整而占位尚无glyph/已发布表格，等原合并事件准备，不造空图或sticky。
    }
    let mut fragments = collected.visible;
    order_fragments(&auth, &mut fragments)?;
    #[cfg(feature = "native-block-fragment-diagnostic")]
    trace_deep_fragment_geometry(&auth, &fragments);
    let (width, height) = geometry::dimensions(auth.visible, auth.scale)?;
    let bitmap =
        auth.step(|| pixels::Bitmap::new(width, height, Arc::clone(&owner.counters.pixels)))??;
    let context = bitmap.context();
    // 标准 source 坐标为 Y-down；独立建立与图像上边一致的实际 backing-scale 变换。
    // 原设备 context 可能是正 Y，不能照抄其符号，也不能仅放宽符号检查造成倒字。
    auth.step(|| {
        CGContext::concat_ctm(
            Some(context),
            CGAffineTransform {
                a: auth.scale,
                b: 0.0,
                c: 0.0,
                d: -auth.scale,
                tx: -auth.scale * auth.visible.origin.x,
                ty: auth.scale * (auth.visible.origin.y + auth.visible.size.height),
            },
        )
    })?;
    // 每次 contents 替换的是整份视口图像；须重画该有界视口，不能把 dirty 外像素换成透明洞。
    // 该 CPU clip 从不扩大到正文高度，原 Native dirty clip 和后续装饰绘制保持原值。
    auth.step(|| CGContext::clip_to_rect(Some(context), auth.visible))?;
    if let Some(original_cg) = original_cg.as_ref() {
        let text_matrix = auth.step(|| CGContext::text_matrix(Some(original_cg)))?;
        auth.step(|| CGContext::set_text_matrix(Some(context), text_matrix))?;
        let text_position = auth.step(|| CGContext::text_position(Some(original_cg)))?;
        auth.step(|| {
            CGContext::set_text_position(Some(context), text_position.x, text_position.y)
        })?;
        let interpolation = auth.step(|| CGContext::interpolation_quality(Some(original_cg)))?;
        auth.step(|| CGContext::set_interpolation_quality(Some(context), interpolation))?;
    } // Layer入口使用新建真实RGBA8/sRGB画布自己的原始文字矩阵与插值属性，不借别处显示状态。
    // 临时 AppKit context 必须在 finish 前真实归还其 CGContext claim；外层 pool 不足以证明这一点。
    let content_painted = autoreleasepool(|_| -> Result<bool, String> {
        let raster =
            auth.step(|| NSGraphicsContext::graphicsContextWithCGContext_flipped(context, true))?;
        if let Some(original) = original.as_ref() {
            let antialias = auth.step(|| original.shouldAntialias())?;
            auth.step(|| raster.setShouldAntialias(antialias))?;
            let interpolation = auth.step(|| original.imageInterpolation())?;
            auth.step(|| raster.setImageInterpolation(interpolation))?;
            let operation = auth.step(|| original.compositingOperation())?;
            auth.step(|| raster.setCompositingOperation(operation))?;
            let intent = auth.step(|| original.colorRenderingIntent())?;
            auth.step(|| raster.setColorRenderingIntent(intent))?;
            let phase = auth.step(|| original.patternPhase())?;
            auth.step(|| raster.setPatternPhase(phase))?;
        }
        if !auth.current() {
            return Err("标准字形绘制前原视口失效".into());
        }
        let _restore = if layer_entry {
            // 无窗口context时直接保存原可空线程槽；退出时恢复None，不对空context发save消息。
            GraphicsRestore::Current(auth.step(NSGraphicsContext::currentContext)?)
        } else {
            NSGraphicsContext::saveGraphicsState_class();
            GraphicsRestore::Stack
        };
        if !auth.current() {
            return Err("保存原 GraphicsContext 后原票失效".into());
        }
        auth.step(|| NSGraphicsContext::setCurrentContext(Some(&raster)))?;
        // 同一有限画布先承接原生背景、选择与输入提示；原viewport proxy不建立窗口glyph子surface。
        // 随后逐片画标准glyph，所有实际发布仍须同一原始权限，避免第二份窗口显示列表。
        auth.step(|| view.draw_native_body_for_raster(auth.visible))?;
        for fragment in &fragments {
            if !fragment_current(&auth, fragment)? {
                return Err("原 fragment 在绘制前改变".into());
            }
            value_observation::capture_visible_lines(
                &owner.ledger,
                value_observation::VisibleLineObservation {
                    content: &auth.content,
                    manager: &auth.manager,
                },
                &fragment.native,
                fragment.frame,
                auth.visible,
                auth.origin,
                &|| auth.current(),
            )?;
            auth.step(|| {
                fragment.native.drawAtPoint_inContext(
                    NSPoint::new(
                        fragment.frame.origin.x + auth.origin.x,
                        fragment.frame.origin.y + auth.origin.y,
                    ),
                    context,
                )
            })?;
            if !fragment_current(&auth, fragment)? {
                return Err("原 fragment 在绘制后改变".into());
            }
        }
        // 表格文字和 Source 提示仍使用原已准备缓存；与正文共用有限 CPU 画布，避免窗口
        // 显示列表在后续提交时另外建立 glyph 资源。此处不解析、重排或创建第二份正文。
        let tables_painted = auth.step(|| view.draw_visible_markdown_tables(auth.visible))?;
        auth.step(|| view.draw_source_decorations(auth.visible))?;
        Ok(!requires_table_paint || tables_painted)
    })?;
    if !content_painted {
        return Ok(false); // 真实表格producer未完成原cache绘制；释放本次CPU画布但绝不发布空背景。
    }
    if let Some(layer) = body_layer.as_ref() {
        verify_body_layer(&auth, layer)?; // CPU同步原语不能把同一原正文layer悄换成后继backing。
    }
    let image = auth.step(|| bitmap.finish())??;
    if !auth.current() {
        return Err("光栅发布前原票失效".into());
    }
    auth.native
        .image()
        .publish(&image, &auth.layer, &|| auth.current())?;
    auth.bind_published_image(&image);
    if let Some(layer) = body_layer.as_ref() {
        verify_body_layer(&auth, layer)?;
    }
    if !auth.current() {
        return Err("光栅发布返回后原票失效".into());
    }
    let count = owner.counters.actual_paints.fetch_add(1, Ordering::AcqRel) + 1;
    if count <= 8 {
        eprintln!(
            "native_raster_surface event=paint_returned fragments={} outflow_added={} terminal_viewport={} width={} height={} actual_paints={} live_pixel_buffers={} live_pixel_bytes={} formal=false",
            fragments.len(),
            outflow,
            auth.terminal_viewport,
            width,
            height,
            count,
            owner
                .counters
                .pixels
                .live_pixel_buffers
                .load(Ordering::Acquire),
            owner
                .counters
                .pixels
                .live_pixel_bytes
                .load(Ordering::Acquire),
        );
    }
    Ok(true) // 真实同源字形或已证明终端的背景/选择已发布；不把无字形分支伪称字形绘制。
}

fn verify_body_layer(
    auth: &Authority<'_>,
    layer: &objc2_quartz_core::CALayer,
) -> Result<(), String> {
    if !auth.step(|| auth.view.wantsLayer())?
        || !auth.step(|| auth.view.wantsUpdateLayer())?
        || !auth
            .step(|| auth.view.layer())?
            .as_deref()
            .is_some_and(|actual| std::ptr::eq::<objc2_quartz_core::CALayer>(actual, layer))
    {
        return Err("真实layer入口的原正文backing关系改变".into());
    }
    Ok(())
}

/// 真空文档或EOF独立取实际末片；没有已布局字形时只发布真实背景，不创建占位fragment。
fn collect_terminal(auth: &Authority<'_>) -> Result<CollectedBatch, String> {
    let end = auth.step(|| auth.range.endLocation())?;
    let Some(native) = auth.step(|| auth.manager.textLayoutFragmentForLocation(&end))? else {
        return Ok(CollectedBatch::empty()); // 有界空背景仍需后续完整authority/publish；不宣称caret已实测。
    };
    if !auth
        .owner()
        .fragment_is_registered(&native, &|| auth.current())?
    {
        if auth.document_length == 0 {
            return Ok(CollectedBatch::empty()); // canonical无字符可遗漏；不使用尚未配置的Native片段伪造glyph。
        }
        return Err("终端fragment未经原viewport实际配置".into());
    }
    if auth.step(|| native.state())? != NSTextLayoutFragmentState::LayoutAvailable {
        return Ok(CollectedBatch::empty()); // 不读未就绪frame；有限registry扫描仍拒绝任何需显示但无完整布局的ink。
    }
    if !auth
        .step(|| native.textLayoutManager())?
        .is_some_and(|manager| std::ptr::eq::<NSTextLayoutManager>(&*manager, &*auth.manager))
    {
        return Err("终端fragment不属于原manager".into());
    }
    let source = auth.step(|| native.rangeInElement())?;
    let finish = auth.step(|| source.endLocation())?;
    if auth.step(|| finish.compare(&end))? != NSComparisonResult::Same
        || !actual_fragment(auth, &native, &source)?
    {
        return Ok(CollectedBatch::empty()); // 旧片或非EOF片不能冒充终端边界；实际外溢另由原有限扫描认证。
    }
    let (element, element_range) = capture_element(auth, &native, &source)?;
    let empty = auth.step(|| source.isEmpty())?;
    if auth.document_length == 0 && !empty {
        return Err("空canonical文档的终端fragment源范围并非空".into());
    }
    if empty {
        let paragraph = element
            .downcast_ref::<objc2_app_kit::NSTextParagraph>()
            .ok_or("空终端fragment没有实际paragraph证明")?;
        let value = auth.step(|| paragraph.attributedString())?;
        if auth.step(|| value.length())? != 0 {
            return Err("空终端fragment仍持非空paragraph值".into());
        }
        drop(value); // 实际空属性值临时claim归还后仍核原票，不把可变源强制清空。
    }
    let frame = auth.step(|| native.layoutFragmentFrame())?;
    let rendering_bounds = auth.step(|| native.renderingSurfaceBounds())?;
    if !frame_follows_source(None, frame)
        || frame.size.width < 0.0
        || !geometry::finite(rendering_bounds)
        || rendering_bounds.size.width < 0.0
        || rendering_bounds.size.height < 0.0
    {
        return Err("真实终端fragment几何无效".into());
    }
    let fragment = Rc::new(Fragment {
        native,
        frame,
        rendering_bounds,
        source,
        element,
        element_range,
        #[cfg(feature = "native-block-fragment-diagnostic")]
        diagnostic_outflow: false,
    });
    let visible = if frame.size.height > 0.0
        && geometry::intersects(
            geometry::ink(frame, rendering_bounds, auth.origin)?,
            auth.visible,
        ) {
        vec![Rc::clone(&fragment)] // 只共享同一实际末片；EOF无墨迹时仍保留独立源流边界。
    } else {
        Vec::new()
    };
    Ok(CollectedBatch {
        flow: Some(PrimaryFlow {
            first: Rc::clone(&fragment),
            last: fragment,
        }),
        visible,
    })
}

fn collect(auth: &Authority<'_>) -> Result<CollectedBatch, String> {
    let start = auth.step(|| auth.range.location())?;
    let end = auth.step(|| auth.range.endLocation())?;
    let fold_revision = auth.view.ivars().folds.revision(); // 原Authority已固定同一fold revision。
    let state = RefCell::new(Some(Enumeration {
        frontier: start.clone(),
        visited: 0,
        complete: false,
        error: None,
        visible: Vec::with_capacity(64),
        flow: None,
        previous_frame_end: None,
    }));
    let block = block2::RcBlock::new(|raw: NonNull<NSTextLayoutFragment>| -> Bool {
        // 先 take 整个临时批次，RefCell 借用结束后才调用 retain/getter/比较和析构。
        let Some(mut batch) = state.borrow_mut().take() else {
            return Bool::NO;
        };
        let result = (|| -> Result<(), String> {
            if batch.visited >= 256 {
                return Err("光栅 fragment 枚举超过预算".into());
            }
            batch.visited += 1;
            if !auth.current() {
                return Err("枚举期间原光栅视口改变".into());
            }
            // SAFETY: 同步公开枚举回调提供存活 fragment；临时 retain 仅持续本次画图。
            let native = unsafe { Retained::retain(raw.as_ptr()) }.ok_or("原 fragment 无法保留")?;
            if !auth
                .owner()
                .fragment_is_registered(&native, &|| auth.current())?
                || auth.step(|| native.state())? != NSTextLayoutFragmentState::LayoutAvailable
                || !auth
                    .step(|| native.textLayoutManager())?
                    .is_some_and(|m| std::ptr::eq::<NSTextLayoutManager>(&*m, &*auth.manager))
            {
                return Err("原标准fragment未被viewport实际配置或尚无有效布局".into());
            }
            let source = auth.step(|| native.rangeInElement())?;
            if !actual_fragment(auth, &native, &source)? {
                return Err("原 viewport 枚举返回了已被替换的 fragment".into());
            }
            let (element, element_range) = capture_element(auth, &native, &source)?;
            let location = auth.step(|| source.location())?;
            let finish = auth.step(|| source.endLocation())?;
            let start_order = auth.step(|| location.compare(&batch.frontier))?;
            let hidden_gap = start_order == NSComparisonResult::Descending
                && hidden_source_interval(auth, &batch.frontier, &location, fold_revision)?;
            let finish_order = if start_order == NSComparisonResult::Descending && !hidden_gap {
                None // 保留原短路次序；已发现空洞时不为了诊断追加原本不会发生的compare。
            } else {
                Some(auth.step(|| finish.compare(&batch.frontier))?)
            };
            if (start_order == NSComparisonResult::Descending && !hidden_gap)
                || finish_order != Some(NSComparisonResult::Descending)
            {
                #[cfg(feature = "native-block-fragment-diagnostic")]
                trace_fragment_frontier_failure(
                    auth,
                    [&start, &end, &location, &finish, &batch.frontier],
                    (
                        batch.visited,
                        batch.previous_frame_end,
                        start_order,
                        finish_order,
                    ),
                );
                return Err("原 viewport fragment 出现空洞或无进展".into());
            }
            let frame = auth.step(|| native.layoutFragmentFrame())?;
            let rendering_bounds = auth.step(|| native.renderingSurfaceBounds())?;
            if !geometry::finite(frame) || frame.size.width < 0.0 || frame.size.height < 0.0 {
                return Err("原 fragment 几何无效".into());
            }
            if frame.size.height == 0.0
                && !hidden_source_interval(auth, &location, &finish, fold_revision)?
            {
                return Err("零高fragment没有当前显式折叠证明".into());
            }
            if !frame_follows_source(batch.previous_frame_end, frame) {
                // 当前viewport自身也不能混用两次布局的坐标；失效整批，不遮掉冲突段落。
                return Err("原 viewport 的源顺序与实际布局框冲突".into());
            }
            batch.previous_frame_end = Some(frame.origin.y + frame.size.height);
            if !geometry::finite(rendering_bounds)
                || rendering_bounds.size.width < 0.0
                || rendering_bounds.size.height < 0.0
            {
                return Err("原 fragment renderingSurfaceBounds 无效".into());
            }
            batch.frontier = finish;
            let body = geometry::ink(frame, rendering_bounds, auth.origin)?;
            let fragment = Rc::new(Fragment {
                native,
                frame,
                rendering_bounds,
                source,
                element,
                element_range,
                #[cfg(feature = "native-block-fragment-diagnostic")]
                diagnostic_outflow: false,
            });
            PrimaryFlow::include(&mut batch.flow, &fragment);
            if frame.size.height > 0.0 && geometry::intersects(body, auth.visible) {
                batch.visible.push(fragment); // 需画集合只筛ink；完整源流首尾仍独立存在。
            }
            batch.complete =
                auth.step(|| batch.frontier.compare(&end))? != NSComparisonResult::Ascending;
            if !auth.current() {
                return Err("原 fragment 枚举返回时视口改变".into());
            }
            Ok(())
        })();
        if let Err(error) = result {
            batch.error = Some(error);
        }
        let again = batch.error.is_none() && !batch.complete;
        *state.borrow_mut() = Some(batch); // 只移动批次，不执行 Native 析构或 getter。
        Bool::from(again)
    });
    auth.step(|| {
        auth.manager
            .enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
                Some(&start),
                NSTextLayoutFragmentEnumerationOptions::empty(),
                &block,
            )
    })?;
    drop(block); // 公开枚举同步返回后结束闭包对批次槽的借用，再移出唯一临时 owner。
    let mut batch = state.into_inner().ok_or("光栅枚举回调未归还批次")?;
    if let Some(error) = batch.error {
        return Err(error);
    }
    if !batch.complete && hidden_source_interval(auth, &batch.frontier, &end, fold_revision)? {
        batch.complete = true; // 仅真实尾部全被当前fold覆盖时允许Native省略这些片段。
    }
    if !batch.complete || !auth.current() {
        return Err("光栅真实 viewport 未被当前可见 fragment 完整覆盖".into());
    }
    Ok(CollectedBatch {
        flow: batch.flow,
        visible: batch.visible,
    })
}

/// 原provider精确往返端点后，只用canonical显式fold解释省略区间，不读字符或补做布局。
fn hidden_source_interval(
    auth: &Authority<'_>,
    from: &ProtocolObject<dyn NSTextLocation>,
    to: &ProtocolObject<dyn NSTextLocation>,
    fold_revision: u64,
) -> Result<bool, String> {
    let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*auth.content);
    let document = auth.step(|| provider.documentRange())?;
    let origin = auth.step(|| document.location())?;
    let mut offsets = [0_usize; 2];
    for (slot, location) in offsets.iter_mut().zip([from, to]) {
        let offset = auth.step(|| {
            auth.content
                .offsetFromLocation_toLocation(&origin, location)
        })?;
        *slot = usize::try_from(offset).map_err(|_| "折叠空洞的原Native端点不能映射到canonical")?;
        let reconstructed = auth
            .step(|| {
                auth.content
                    .locationFromLocation_withOffset(&origin, offset)
            })?
            .ok_or("折叠空洞的原Native端点无法重建")?;
        if auth.step(|| {
            auth.content
                .offsetFromLocation_toLocation(location, &reconstructed)
        })? != 0
        {
            return Err("折叠空洞的原Native端点往返不一致".into());
        }
    }
    Ok(auth.view.ivars().folds.hidden_range_is_covered(
        &(offsets[0]..offsets[1]),
        auth.generation(),
        fold_revision,
        &|| auth.current(),
    ))
}

fn fragment_current(auth: &Authority<'_>, fragment: &Fragment) -> Result<bool, String> {
    let element = auth.step(|| fragment.native.textElement())?;
    let Some(element) = element else {
        return Ok(false);
    };
    let element_range = auth.step(|| element.elementRange())?;
    let Some(element_range) = element_range else {
        return Ok(false);
    };
    let source = auth.step(|| fragment.native.rangeInElement())?;
    Ok(auth.current()
        && std::ptr::eq::<NSTextElement>(&*element, &*fragment.element)
        && auth
            .step(|| element.textContentManager())?
            .is_some_and(|c| std::ptr::eq::<NSTextContentManager>(&*c, &*auth.content))
        && auth.step(|| element_range.isEqualToTextRange(&fragment.element_range))?
        && auth.step(|| fragment.native.state())? == NSTextLayoutFragmentState::LayoutAvailable
        && auth.step(|| fragment.native.layoutFragmentFrame())? == fragment.frame
        && auth.step(|| fragment.native.renderingSurfaceBounds())? == fragment.rendering_bounds
        && auth.step(|| source.isEqualToTextRange(&fragment.source))?
        && actual_fragment(auth, &fragment.native, &source)?
        && auth.current())
}

fn capture_element(
    auth: &Authority<'_>,
    native: &NSTextLayoutFragment,
    source: &NSTextRange,
) -> Result<(Retained<NSTextElement>, Retained<NSTextRange>), String> {
    let element = auth
        .step(|| native.textElement())?
        .ok_or("光栅 fragment 的原 element 缺失")?;
    if !auth
        .step(|| element.textContentManager())?
        .is_some_and(|c| std::ptr::eq::<NSTextContentManager>(&*c, &*auth.content))
    {
        return Err("光栅 fragment 的 element 不属于原 content manager".into());
    }
    let element_range = auth
        .step(|| element.elementRange())?
        .ok_or("光栅原 element 没有 source 范围")?;
    if !auth.step(|| element_range.containsRange(source))? {
        return Err("光栅 fragment source 越出原 element".into());
    }
    Ok((element, element_range))
}

/// viewportRange连续性不能证明range外的负ink；补扫原viewport实际配置登记的标准片段。
fn collect_outflow(
    auth: &Authority<'_>,
    flow: Option<&PrimaryFlow>,
    fragments: &mut Vec<Rc<Fragment>>,
) -> Result<usize, String> {
    let primary = if let Some(flow) = flow {
        let (first, last) = (&flow.first, &flow.last);
        if !fragment_current(auth, first)? || !fragment_current(auth, last)? {
            return Err("完整primary源流边界在外溢观察前已变化".into());
        }
        let (top, bottom) =
            complete_flow_bounds(first.frame, last.frame).ok_or("完整primary首末流边界无效")?;
        Some((
            auth.step(|| first.source.location())?,
            auth.step(|| last.source.endLocation())?,
            top,
            bottom,
        ))
    } else if auth.terminal_viewport {
        None // 唯有已认证的终端可无字形；扫描仍须证明没有遗漏任何实际需显示的ink。
    } else {
        return Err("外溢字形缺少实际primary流边界".into());
    };
    let CacheAdmission::Ready(mut lease) = auth.owner().fragments.begin() else {
        return Err("光栅真实 fragment 集合正被借出或已经关闭".into());
    };
    let original_count = fragments.len();
    let entries = &mut lease.payload_mut().identities;
    if entries.len() > MAX_FRAGMENT_WEAK_METADATA {
        return Err("光栅 Weak 集合超过原身份预算".into());
    }
    for weak in entries.iter() {
        // 集合唯一 owner 已移出 RefCell；临时 Native claim 只活到本次扫描/画图，不增加布局所有权。
        let Some(own) = auth.step(|| weak.load())? else {
            continue;
        };
        let native: &NSTextLayoutFragment = &own;
        let manager = auth.step(|| native.textLayoutManager())?;
        let Some(manager) = manager else {
            continue;
        }; // 已脱离任何 manager 的旧缓存没有当前绘制权。
        if !std::ptr::eq::<NSTextLayoutManager>(&*manager, &*auth.manager) {
            return Err("已登记 fragment 被转移到其他 manager".into());
        }
        let state = auth.step(|| native.state())?;
        if state <= NSTextLayoutFragmentState::EstimatedUsageBounds {
            continue; // 尚无实际渲染度量的片段不能绘制；原 viewport 必需片仍由连续性门禁拒绝缺失。
        }
        if auth.terminal_viewport && auth.document_length == 0 {
            let source = auth.step(|| native.rangeInElement())?;
            let end = auth.step(|| source.endLocation())?;
            let viewport_end = auth.step(|| auth.range.endLocation())?;
            if auth.step(|| source.isEmpty())?
                && auth.step(|| end.compare(&viewport_end))? == NSComparisonResult::Same
                && actual_fragment(auth, native, &source)?
            {
                continue; // canonical与Native均证实无字符，不把空ink度量当成尚缺的字形。
            }
        }
        let frame = auth.step(|| native.layoutFragmentFrame())?;
        let rendering_bounds = auth.step(|| native.renderingSurfaceBounds())?;
        if !geometry::finite(frame)
            || !geometry::finite(rendering_bounds)
            || frame.size.width < 0.0
            || frame.size.height < 0.0
            || rendering_bounds.size.width < 0.0
            || rendering_bounds.size.height < 0.0
        {
            return Err("已登记 fragment 的实际 ink 几何无效".into());
        }
        if frame.size.height == 0.0 {
            let source = auth.step(|| native.rangeInElement())?;
            let start = auth.step(|| source.location())?;
            let end = auth.step(|| source.endLocation())?;
            if auth.terminal_viewport && auth.step(|| source.isEmpty())? {
                let viewport_end = auth.step(|| auth.range.endLocation())?;
                if auth.step(|| end.compare(&viewport_end))? == NSComparisonResult::Same
                    && actual_fragment(auth, native, &source)?
                {
                    continue; // 真实EOF空片无需glyph；其存在不能制造另一个字形或覆盖其它源。
                }
            }
            if !hidden_source_interval(auth, &start, &end, auth.view.ivars().folds.revision())? {
                return Err("零高外溢缓存没有当前显式折叠证明".into());
            }
            continue; // 真实隐藏片只保原Weak生命周期，不绘制其旧ink或额外扩大primary流。
        }
        let ink = geometry::ink(frame, rendering_bounds, auth.origin)?;
        if !geometry::intersects(ink, auth.visible) {
            continue;
        }
        if state != NSTextLayoutFragmentState::LayoutAvailable {
            return Err("真实外溢 ink 尚无完整字形布局".into());
        }
        if fragments
            .iter()
            .any(|known| std::ptr::eq::<NSTextLayoutFragment>(&*known.native, native))
        {
            continue;
        }
        let source = auth.step(|| native.rangeInElement())?;
        if !actual_fragment(auth, native, &source)? {
            continue;
        } // 存活旧缓存不能与实际当前片重复叠绘。
        let start = auth.step(|| source.location())?;
        let end = auth.step(|| source.endLocation())?;
        let Some((primary_start, primary_end, primary_top, primary_bottom)) = primary.as_ref()
        else {
            return Err("终端视口存在需绘制ink但缺少实际末片流边界".into());
        };
        let side = if auth.step(|| end.compare(primary_start))? != NSComparisonResult::Descending {
            OutflowSourceSide::Before
        } else if auth.step(|| start.compare(primary_end))? != NSComparisonResult::Ascending {
            OutflowSourceSide::After
        } else {
            // primary源范围已连续覆盖，此处额外身份不能获得重复绘制权；Weak仍留在原登记中。
            continue;
        };
        if !outflow_frame_connects(side, frame, *primary_top, *primary_bottom) {
            // TextKit可缓存旧state3框；与本批实际流矛盾的旧框没有当前绘制权，不能叠到新段上。
            // 不失效Native、不删除Weak或业务值，也不裁掉合法框的负方向ink。
            continue;
        }
        let (element, element_range) = capture_element(auth, native, &source)?;
        fragments.push(Rc::new(Fragment {
            native: own,
            frame,
            rendering_bounds,
            source,
            element,
            element_range,
            #[cfg(feature = "native-block-fragment-diagnostic")]
            diagnostic_outflow: true,
        }));
        if !auth.current() {
            return Err("补齐真实外溢片段时原状态改变".into());
        }
    }
    drop(lease); // 先归还 Weak 元数据唯一 owner，析构/恢复后的原票仍须有效。
    if !auth.current() {
        return Err("外溢片段扫描归还后原票失效".into());
    }
    if let Some(flow) = flow
        && (!fragment_current(auth, &flow.first)? || !fragment_current(auth, &flow.last)?)
    {
        return Err("外溢片段扫描后完整primary边界改变".into());
    }
    Ok(fragments.len() - original_count)
}

fn actual_fragment(
    auth: &Authority<'_>,
    native: &NSTextLayoutFragment,
    source: &NSTextRange,
) -> Result<bool, String> {
    let location = auth.step(|| source.location())?;
    let actual = auth.step(|| auth.manager.textLayoutFragmentForLocation(&location))?;
    Ok(actual.is_some_and(|actual| std::ptr::eq::<NSTextLayoutFragment>(&*actual, native)))
}

fn order_fragments(auth: &Authority<'_>, fragments: &mut [Rc<Fragment>]) -> Result<(), String> {
    let failed = Cell::new(false);
    // Weak 集合按真实创建顺序登记，外溢片未必晚于主视口片；按原 source 顺序恢复标准叠绘关系。
    fragments.sort_by(|left, right| {
        if failed.get() {
            return std::cmp::Ordering::Equal;
        }
        let result = (|| {
            let a = auth.step(|| left.source.location())?;
            let b = auth.step(|| right.source.location())?;
            auth.step(|| a.compare(&b))
        })();
        match result {
            Ok(NSComparisonResult::Ascending) => std::cmp::Ordering::Less,
            Ok(NSComparisonResult::Same) => std::cmp::Ordering::Equal,
            Ok(NSComparisonResult::Descending) => std::cmp::Ordering::Greater,
            Err(_) => {
                failed.set(true);
                std::cmp::Ordering::Equal
            }
        }
    });
    if failed.get() || !auth.current() {
        return Err("按原 source 排列外溢字形时原票改变".into());
    }
    Ok(())
}

/// 只诊断深视口的原批次；固定预算外不查询Native，也不改变任何布局或绘制判据。
#[cfg(feature = "native-block-fragment-diagnostic")]
fn trace_deep_fragment_geometry(auth: &Authority<'_>, fragments: &[Rc<Fragment>]) {
    native_markdown_trace! {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DEEP_PAINTS: AtomicUsize = AtomicUsize::new(0);
        if auth.visible.origin.y <= 1_000.0 {
            return; // 首屏不消耗深位置预算，便于保留真实Find后的首组几何。
        }
        let Ok(previous) = DEEP_PAINTS.try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            (count < 8).then(|| count + 1)
        }) else {
            return; // 计数停在8，不因后续绘制增长或回绕恢复观察权限。
        };
        let ordinal = previous + 1;
        let result = (|| -> Result<(), String> {
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*auth.content);
            let document = auth.step(|| provider.documentRange())?;
            let document_start = auth.step(|| document.location())?;
            let offsets = |range: &NSTextRange| -> Result<(isize, isize), String> {
                let start = auth.step(|| range.location())?;
                let end = auth.step(|| range.endLocation())?;
                let from = auth.step(|| auth.content.offsetFromLocation_toLocation(&document_start, &start))?;
                let to = auth.step(|| auth.content.offsetFromLocation_toLocation(&document_start, &end))?;
                Ok((from, to)) // 只翻译原Native位置，不读字符、创建正文或修正负坐标。
            };
            let (viewport_start, viewport_end) = offsets(&auth.range)?;
            let epoch = auth.owner().counters.viewport_epoch.load(Ordering::Acquire);
            if !auth.current() {
                return Err("深视口诊断翻译原范围时撤权".into());
            }
            eprintln!("native_raster_fragment_geometry event=batch ordinal={ordinal} viewport_source_start={viewport_start} viewport_source_end={viewport_end} viewport_epoch={epoch} visible={:?} origin={:?} fragments={} reported={} formal=false", auth.visible, auth.origin, fragments.len(), fragments.len().min(12));
            for (index, fragment) in fragments.iter().take(12).enumerate() {
                let (source_start, source_end) = offsets(&fragment.source)?;
                let ink = geometry::ink(fragment.frame, fragment.rendering_bounds, auth.origin)?;
                if !fragment_current(auth, fragment)? {
                    return Err("深视口诊断的原fragment几何已改变".into());
                }
                // 只输出同一source排序批次的已捕获几何；原票和实际frame仍由原函数逐原语复核。
                let source = if fragment.diagnostic_outflow { "outflow" } else { "primary" };
                eprintln!("native_raster_fragment_geometry event=fragment ordinal={ordinal} index={index} source_start={source_start} source_end={source_end} membership={source} captured_frame={:?} captured_ink={ink:?} formal=false", fragment.frame);
            }
            Ok(())
        })();
        if let Err(error) = result {
            // 诊断失败不签绘制成功，也不另设失败门；原paint继续按既有原票与几何谓词判定。
            eprintln!("native_raster_fragment_geometry event=unavailable ordinal={ordinal} reason={error:?} formal=false");
        }
    }
}

/// 首个覆盖失败只翻译五个原位置，不ensure、读字符或改写原错误；上限外不做Native查询。
#[cfg(feature = "native-block-fragment-diagnostic")]
fn trace_fragment_frontier_failure(
    auth: &Authority<'_>,
    positions: [&ProtocolObject<dyn NSTextLocation>; 5],
    facts: (
        usize,
        Option<f64>,
        NSComparisonResult,
        Option<NSComparisonResult>,
    ),
) {
    let (visited, previous_frame_end, start_order, finish_order) = facts;
    native_markdown_trace! {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static FAILURES: AtomicUsize = AtomicUsize::new(0);
        let Ok(ordinal) = FAILURES.try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            (count < 16).then(|| count + 1)
        }) else {
            return; // 固定一次性观察预算，不无限记录持续失败或补做查询。
        };
        let offsets = (|| -> Result<[isize; 5], String> {
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*auth.content);
            let document = auth.step(|| provider.documentRange())?;
            let origin = auth.step(|| document.location())?;
            let mut result = [0; 5];
            for (slot, position) in result.iter_mut().zip(positions) {
                *slot = auth.step(|| auth.content.offsetFromLocation_toLocation(&origin, position))?;
            }
            Ok(result)
        })();
        let kind = if start_order == NSComparisonResult::Descending { "hole" } else { "no_progress" };
        // state3已由原collector的准入查询证实；frame尚未读取必须明确None，不能猜造旧值。
        eprintln!("native_raster_fragment_frontier_failure ordinal={} kind={kind} visited={visited} offsets_viewport_start_end_source_start_end_frontier={offsets:?} start_order={start_order:?} finish_order={finish_order:?} state_layout_available=true frame_queried=false previous_frame_end={previous_frame_end:?} visible={:?} origin={:?} formal=false", ordinal + 1, auth.visible, auth.origin);
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;

    fn frame(y: f64, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(5.0, y), NSSize::new(780.0, height))
    }

    #[test]
    fn zero_ink_endpoints_still_bound_the_complete_source_flow() {
        let first = frame(100.0, 38.0);
        let last = frame(176.0, 38.0);
        let viewport = frame(100.0, 114.0);
        let no_ink = NSRect::new(NSPoint::ZERO, NSSize::ZERO);
        assert!(!geometry::intersects(
            geometry::ink(first, no_ink, NSPoint::ZERO).unwrap(),
            viewport,
        ));
        assert!(!geometry::intersects(
            geometry::ink(last, no_ink, NSPoint::ZERO).unwrap(),
            viewport,
        ));
        // 即使两端均未进入ink集合，外溢准入仍必须使用100..214的实际连续流。
        let (top, bottom) = complete_flow_bounds(first, last).unwrap();
        assert_eq!((top, bottom), (100.0, 214.0));
        assert!(!outflow_frame_connects(
            OutflowSourceSide::Before,
            frame(120.0, 18.0),
            top,
            bottom,
        ));
        assert!(outflow_frame_connects(
            OutflowSourceSide::After,
            frame(214.0, 18.0),
            top,
            bottom,
        ));
    }

    #[test]
    fn complete_flow_rejects_invalid_endpoints_without_repacking_native_frames() {
        assert!(complete_flow_bounds(frame(100.0, 38.0), frame(0.0, 17.0)).is_none());
        assert!(complete_flow_bounds(frame(f64::NAN, 38.0), frame(176.0, 38.0)).is_none());
        assert!(complete_flow_bounds(frame(100.0, -1.0), frame(176.0, 38.0)).is_none());
        assert!(complete_flow_bounds(frame(100.0, 38.0), frame(f64::MAX, f64::MAX)).is_none());
        // 显式隐藏的零高框仍可作为流末端；是否隐藏继续由原canonical fold证明负责。
        assert_eq!(
            complete_flow_bounds(frame(100.0, 38.0), frame(138.0, 0.0)),
            Some((100.0, 138.0)),
        );
    }

    #[test]
    fn cached_preceding_source_cannot_reenter_a_relocated_primary_flow() {
        // 实测旧片段仍state3且同manager，源位置在11763之前，但布局框向后侵入新primary。
        for stale in [
            frame(6443.0, 17.0),
            frame(6460.0, 51.0),
            frame(6511.0, 17.0),
        ] {
            assert!(!outflow_frame_connects(
                OutflowSourceSide::Before,
                stale,
                6456.0,
                7017.0
            ));
        }
        assert!(outflow_frame_connects(
            OutflowSourceSide::Before,
            frame(6439.0, 17.0),
            6456.0,
            7017.0
        ));
        assert!(!frame_follows_source(Some(6528.0), frame(6456.0, 17.0)));
        assert!(frame_follows_source(Some(6473.0), frame(6473.0, 17.0)));
    }

    #[test]
    fn valid_before_and_after_frames_keep_their_complete_overhanging_ink() {
        let viewport = NSRect::new(NSPoint::new(0.0, 100.0), NSSize::new(800.0, 50.0));
        let before = frame(80.0, 20.0);
        let after = frame(150.0, 20.0);
        assert!(outflow_frame_connects(
            OutflowSourceSide::Before,
            before,
            100.0,
            150.0
        ));
        assert!(outflow_frame_connects(
            OutflowSourceSide::After,
            after,
            100.0,
            150.0
        ));
        // 前段向下和后段负方向墨迹都真实进入视口；准入不要求ink也服从布局框边界。
        let before_ink = NSRect::new(NSPoint::new(-3.0, 0.0), NSSize::new(790.0, 30.0));
        let after_ink = NSRect::new(NSPoint::new(-3.0, -10.0), NSSize::new(790.0, 30.0));
        assert!(geometry::intersects(
            geometry::ink(before, before_ink, NSPoint::ZERO).unwrap(),
            viewport
        ));
        assert!(geometry::intersects(
            geometry::ink(after, after_ink, NSPoint::ZERO).unwrap(),
            viewport
        ));
        assert!(!outflow_frame_connects(
            OutflowSourceSide::After,
            frame(149.0, 20.0),
            100.0,
            150.0
        ));
    }
}
