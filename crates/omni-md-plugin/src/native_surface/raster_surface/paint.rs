//! Draws the original viewport's standard fragments into one RGBA8 bitmap and publishes it to CALayer.

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
    // One synchronous claim per actual fragment captures the source/element and geometry used to draw.
    // Later checks must match all of them; a live framework object alone does not grant current authority.
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
    // The frontier tracks complete source coverage independently of visible ink. This temporary batch
    // is moved out of its RefCell before Native calls, so reentry cannot borrow half-mutated state.
    frontier: Retained<ProtocolObject<dyn NSTextLocation>>,
    visited: usize,
    complete: bool,
    error: Option<String>,
    visible: Vec<Rc<Fragment>>,
    flow: Option<PrimaryFlow>,
    previous_frame_end: Option<f64>,
}

/// Source continuity and visible ink have separate evidence; endpoints share the same actual batch claims.
struct PrimaryFlow {
    first: Rc<Fragment>,
    last: Rc<Fragment>,
}

impl PrimaryFlow {
    fn include(flow: &mut Option<Self>, fragment: &Rc<Fragment>) {
        match flow {
            Some(flow) => flow.last = Rc::clone(fragment), // Share the original Rust claim without another Native retain.
            None => {
                *flow = Some(Self {
                    first: Rc::clone(fragment),
                    last: Rc::clone(fragment),
                }); // Even a first frame with no ink remains the source-flow boundary.
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
        } // Only an authenticated terminal may use this; ordinary source still requires complete coverage.
    }
}

/// The single text container lays out paragraphs along Y with nonnegative project paragraph spacing.
/// Ink can overhang, but layout frames cannot overlap backwards; overhang is not negative paragraph spacing.
fn frame_follows_source(previous_end: Option<f64>, frame: NSRect) -> bool {
    let end = frame.origin.y + frame.size.height;
    geometry::finite(frame)
        && frame.size.height >= 0.0
        && end.is_finite()
        && previous_end.is_none_or(|previous| frame.origin.y + 0.000_001 >= previous)
}

/// Bounds use the authenticated source flow's first/last frames, including endpoints with no visible ink.
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

/// Checks layout-frame order against the actual primary flow; drawing keeps full, possibly negative ink bounds.
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

/// Restores the synchronous reentry flag on every exit without invoking Native from Drop.
struct Painting<'a>(&'a Cell<bool>);
impl Drop for Painting<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

enum GraphicsRestore {
    // drawRect has an existing graphics stack; updateLayer can begin with an actually empty context slot.
    // Preserve those different restoration obligations instead of inventing a context for the layer route.
    Stack,
    Current(Option<Retained<NSGraphicsContext>>),
}
impl Drop for GraphicsRestore {
    fn drop(&mut self) {
        match self {
            Self::Stack => NSGraphicsContext::restoreGraphicsState_class(), // Restore drawRect's original device stack.
            Self::Current(previous) => NSGraphicsContext::setCurrentContext(previous.as_deref()), // Restore the actual optional slot.
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
    // Both guards unwind on every error. The pool bounds autoreleased Native claims created by this paint.
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
    // updateLayer has no window graphics context; use the same Authority's actual viewport/scale
    // for the one CPU canvas below. drawRect still authenticates its real device context, clip,
    // and CTM; a fabricated context cannot satisfy that entry's contract.
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
            // This mapping supports finite axis-aligned uniform scale with either Y direction.
            // Rotation, skew, zero scale, or unequal axes cannot produce an exact viewport bitmap here.
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
        None => None, // The real layer entry requires no nonexistent window clip and creates no extra bitmap.
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
        collect_terminal(&auth)? // Authenticated EOF may have no glyphs; retain its actual final flow boundary if available.
    } else {
        collect(&auth)?
    };
    let outflow = collect_outflow(&auth, collected.flow.as_ref(), &mut collected.visible)?;
    let requires_table_paint = if collected.visible.is_empty() && !auth.terminal_viewport {
        let Some(required) = auth.table_overlay_requirement()? else {
            return Ok(false); // Unknown placeholder requirements stay pending instead of publishing empty content.
        };
        required
    } else {
        false // Actual glyphs or an authenticated terminal cover source; tables still draw into this same canvas.
    };
    if requires_table_paint
        && !view
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| !cache.tables.is_empty())
    {
        // Source flow is complete, but the original table producer has not prepared its visible cache.
        // Wait for that lifecycle event without publishing an empty image or creating a sticky failure.
        return Ok(false);
    }
    let mut fragments = collected.visible;
    order_fragments(&auth, &mut fragments)?;
    #[cfg(feature = "native-block-fragment-diagnostic")]
    trace_deep_fragment_geometry(&auth, &fragments);
    let (width, height) = geometry::dimensions(auth.visible, auth.scale)?;
    let bitmap =
        auth.step(|| pixels::Bitmap::new(width, height, Arc::clone(&owner.counters.pixels)))??;
    let context = bitmap.context();
    // Standard source coordinates point down Y. Build the actual backing-scale transform relative
    // to this image's top edge; copying a device context's Y sign could draw text upside down.
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
    // Replacing contents replaces the whole viewport image, so redraw that bounded viewport rather
    // than leaving transparent holes outside the dirty rect. This CPU clip never expands to body
    // height; the original Native dirty clip and later decoration drawing retain their own values.
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
    } // The layer route uses its new RGBA8/sRGB context's own defaults, without unrelated display state.
    // Return the temporary AppKit context's actual CGContext claim before finish. An outer pool
    // alone would not guarantee that this owner has ended before the bitmap is converted to an image.
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
            // Save the actual optional thread slot; restore None rather than sending save to an absent context.
            GraphicsRestore::Current(auth.step(NSGraphicsContext::currentContext)?)
        } else {
            NSGraphicsContext::saveGraphicsState_class();
            GraphicsRestore::Stack
        };
        if !auth.current() {
            return Err("保存原 GraphicsContext 后原票失效".into());
        }
        auth.step(|| NSGraphicsContext::setCurrentContext(Some(&raster)))?;
        // Draw the original background, selection, and input hints into this bounded canvas first.
        // The default proxy route does not configure window glyph surfaces. Draw each standard
        // fragment under the same authority, without an additional window display list here.
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
        // Tables and Source hints use their already prepared caches on the same bounded canvas.
        // Do not parse, reflow, or create another document here, or let a later window display list
        // allocate a separate set of glyph resources for these decorations.
        let tables_painted = auth.step(|| view.draw_visible_markdown_tables(auth.visible))?;
        auth.step(|| view.draw_source_decorations(auth.visible))?;
        Ok(!requires_table_paint || tables_painted)
    })?;
    if !content_painted {
        return Ok(false); // The original table cache did not paint; drop this canvas without publishing blank contents.
    }
    if let Some(layer) = body_layer.as_ref() {
        verify_body_layer(&auth, layer)?; // Synchronous drawing cannot silently replace the original body backing.
    }
    let image = auth.step(|| bitmap.finish())??;
    // finish transfers the bounded pixel owner to the CGImage's provider. CALayer then retains that
    // same image; only the provider's actual final release can return the underlying mapping.
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
    Ok(true) // Publishes original glyphs or authenticated terminal background/selection, not invented glyph coverage.
}

fn verify_body_layer(
    auth: &Authority<'_>,
    layer: &objc2_quartz_core::CALayer,
) -> Result<(), String> {
    // updateLayer must still own the exact backing installed before attachment, including both public flags.
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

/// Uses the real final fragment for canonical emptiness/EOF, without creating a placeholder for absent glyphs.
fn collect_terminal(auth: &Authority<'_>) -> Result<CollectedBatch, String> {
    let end = auth.step(|| auth.range.endLocation())?;
    let Some(native) = auth.step(|| auth.manager.textLayoutFragmentForLocation(&end))? else {
        return Ok(CollectedBatch::empty()); // Empty background still needs full authority/publication; this is no caret test.
    };
    if !auth
        .owner()
        .fragment_is_registered(&native, &|| auth.current())?
    {
        if auth.document_length == 0 {
            return Ok(CollectedBatch::empty()); // Canonical emptiness needs no unconfigured fragment to invent glyphs.
        }
        return Err("终端fragment未经原viewport实际配置".into());
    }
    if auth.step(|| native.state())? != NSTextLayoutFragmentState::LayoutAvailable {
        return Ok(CollectedBatch::empty()); // Do not read unready frames; bounded registry scanning still rejects unlaid-out visible ink.
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
        return Ok(CollectedBatch::empty()); // A stale/non-EOF fragment cannot become the terminal; outflow needs its own scan.
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
        drop(value); // Return the actual empty attributed value; never clear mutable source to satisfy this condition.
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
        vec![Rc::clone(&fragment)] // Share this actual final fragment; no ink still leaves an independent flow boundary.
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
    let fold_revision = auth.view.ivars().folds.revision(); // The Authority already fixed this fold revision.
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
        // Move the whole batch out before retain/getter/comparison/destruction can reenter Native.
        // A recursive callback cannot borrow partially updated source-frontier state.
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
            // SAFETY: Public synchronous enumeration supplies a live fragment; the retain lasts only this paint.
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
            // Accept an omitted source interval only when the original canonical fold explicitly
            // covers it. Otherwise every actual fragment must advance the continuous source frontier.
            let hidden_gap = start_order == NSComparisonResult::Descending
                && hidden_source_interval(auth, &batch.frontier, &location, fold_revision)?;
            let finish_order = if start_order == NSComparisonResult::Descending && !hidden_gap {
                None // Preserve short-circuit order; diagnostics must not add a compare after an already known gap.
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
                // A viewport cannot mix coordinates from different layouts. Reject the entire batch
                // instead of hiding the paragraph whose frame conflicts with the current source flow.
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
                batch.visible.push(fragment); // Filter only visible ink; keep complete source-flow endpoints separately.
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
        *state.borrow_mut() = Some(batch); // Moving the batch performs no Native getter or destruction in this borrow.
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
    // Enumeration returned synchronously; drop its closure before extracting the unique temporary owner.
    drop(block);
    let mut batch = state.into_inner().ok_or("光栅枚举回调未归还批次")?;
    if let Some(error) = batch.error {
        return Err(error);
    }
    if !batch.complete && hidden_source_interval(auth, &batch.frontier, &end, fold_revision)? {
        batch.complete = true; // Native may omit the tail only when the original fold covers that entire source interval.
    }
    if !batch.complete || !auth.current() {
        return Err("光栅真实 viewport 未被当前可见 fragment 完整覆盖".into());
    }
    Ok(CollectedBatch {
        flow: batch.flow,
        visible: batch.visible,
    })
}

/// Round-trips original provider endpoints, then checks canonical folds without reading text or adding layout.
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
    // A cached framework fragment can stay alive after replacement. Revalidate its exact element,
    // source, manager, state, and both geometries before and after drawing that captured fragment.
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
    // The source range must belong to an actual element in the same content manager. A matching
    // fragment address alone cannot authorize a source range moved into another Native graph.
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

/// Viewport-range continuity cannot cover negative ink outside it; scan the original configured registry too.
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
        None // Only an authenticated terminal may have no glyphs; the scan must still exclude omitted visible ink.
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
        // The unique registry payload is outside RefCell. Temporary Native claims last only through
        // this scan/paint and do not become another persistent layout owner.
        let Some(own) = auth.step(|| weak.load())? else {
            continue;
        };
        let native: &NSTextLayoutFragment = &own;
        let manager = auth.step(|| native.textLayoutManager())?;
        let Some(manager) = manager else {
            continue;
        }; // An old cache detached from every manager has no current drawing authority.
        if !std::ptr::eq::<NSTextLayoutManager>(&*manager, &*auth.manager) {
            return Err("已登记 fragment 被转移到其他 manager".into());
        }
        let state = auth.step(|| native.state())?;
        if state <= NSTextLayoutFragmentState::EstimatedUsageBounds {
            continue; // Unmeasured fragments cannot draw; primary continuity still rejects any required missing fragment.
        }
        if auth.terminal_viewport && auth.document_length == 0 {
            let source = auth.step(|| native.rangeInElement())?;
            let end = auth.step(|| source.endLocation())?;
            let viewport_end = auth.step(|| auth.range.endLocation())?;
            if auth.step(|| source.isEmpty())?
                && auth.step(|| end.compare(&viewport_end))? == NSComparisonResult::Same
                && actual_fragment(auth, native, &source)?
            {
                continue; // Canonical and Native emptiness agree; empty ink metrics do not imply missing glyphs.
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
                    continue; // A real empty EOF fragment needs no glyph and cannot cover another source interval.
                }
            }
            if !hidden_source_interval(auth, &start, &end, auth.view.ivars().folds.revision())? {
                return Err("零高外溢缓存没有当前显式折叠证明".into());
            }
            continue; // Keep a hidden fragment's original weak lifecycle without drawing stale ink or expanding primary flow.
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
        } // A live stale cache cannot draw over the actual current fragment.
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
            // Primary source is already continuously covered. Another identity inside that range
            // receives no duplicate drawing rights; its weak entry remains in the original registry.
            continue;
        };
        if !outflow_frame_connects(side, frame, *primary_top, *primary_bottom) {
            // TextKit can cache old LayoutAvailable frames. A frame inconsistent with this actual
            // source flow cannot draw over relocated paragraphs. Do not invalidate Native, delete
            // its weak/business observations, or clip valid negative ink to hide that inconsistency.
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
    // Return the unique weak-metadata payload first; destruction/restoration must leave the permit current.
    drop(lease);
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
    // Query the original manager at this source location; a registered but replaced identity is stale.
    let location = auth.step(|| source.location())?;
    let actual = auth.step(|| auth.manager.textLayoutFragmentForLocation(&location))?;
    Ok(actual.is_some_and(|actual| std::ptr::eq::<NSTextLayoutFragment>(&*actual, native)))
}

fn order_fragments(auth: &Authority<'_>, fragments: &mut [Rc<Fragment>]) -> Result<(), String> {
    let failed = Cell::new(false);
    // Registry order is creation order, so outflow need not follow viewport fragments. Restore source
    // order for standard compositing. Native comparison failure cancels the batch, not just that pair.
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

/// Diagnoses only the original deep-viewport batch, within a fixed budget and without changing paint decisions.
#[cfg(feature = "native-block-fragment-diagnostic")]
fn trace_deep_fragment_geometry(auth: &Authority<'_>, fragments: &[Rc<Fragment>]) {
    native_markdown_trace! {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DEEP_PAINTS: AtomicUsize = AtomicUsize::new(0);
        if auth.visible.origin.y <= 1_000.0 {
            return; // Preserve the deep-position budget for the first actual Find/reveal geometry.
        }
        let Ok(previous) = DEEP_PAINTS.try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            (count < 8).then(|| count + 1)
        }) else {
            return; // Stop at eight; later paints cannot wrap this counter into renewed observation rights.
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
                Ok((from, to)) // Translate Native positions only, without text materialization or coordinate correction.
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
                // Log captured geometry from this same source-ordered batch. The original function
                // still validates authority and actual frame identity around each Native primitive.
                let source = if fragment.diagnostic_outflow { "outflow" } else { "primary" };
                eprintln!("native_raster_fragment_geometry event=fragment ordinal={ordinal} index={index} source_start={source_start} source_end={source_end} membership={source} captured_frame={:?} captured_ink={ink:?} formal=false", fragment.frame);
            }
            Ok(())
        })();
        if let Err(error) = result {
            // Diagnostics certify no paint result and add no failure gate. The existing paint
            // path still decides using its original permit and geometry predicates.
            eprintln!("native_raster_fragment_geometry event=unavailable ordinal={ordinal} reason={error:?} formal=false");
        }
    }
}

/// Translates five original positions at bounded coverage failures, without layout, text reads, or error replacement.
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
            return; // A fixed observation budget prevents unbounded repeated-failure logs and Native queries.
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
        // The collector already confirmed LayoutAvailable. This failure precedes frame lookup:
        // report that absence explicitly rather than inventing geometry from an earlier fragment.
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
        // Endpoints absent from the ink set still define the complete 100..214 source-flow bounds.
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
        // A zero-height frame can terminate flow; the actual canonical fold separately proves it is hidden.
        assert_eq!(
            complete_flow_bounds(frame(100.0, 38.0), frame(138.0, 0.0)),
            Some((100.0, 138.0)),
        );
    }

    #[test]
    fn cached_preceding_source_cannot_reenter_a_relocated_primary_flow() {
        // These observed stale frames remained LayoutAvailable in the same manager, with earlier
        // source positions, yet their geometry intruded into the relocated primary flow.
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
        // Both the preceding frame's downward ink and the following frame's negative ink enter
        // the viewport; valid admission must not force ink to stay inside the layout-frame bounds.
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
