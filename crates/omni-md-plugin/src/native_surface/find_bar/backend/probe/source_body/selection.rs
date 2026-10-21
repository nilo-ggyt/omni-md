//! Exercise bounded selection/emoji pixels on the original Body. Check its actual
//! Scene, visible grapheme geometry, and final Native output separately.

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputOutcome;
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedScenePaintBudget, NativeMarkdownPagedScenePaintOutcome,
    NativeMarkdownPagedTextScene,
};
use objc2_app_kit::NSPasteboardTypeString;
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
    kCGColorSpaceSRGB,
};

const WIDTH: usize = 720;
const HEIGHT: usize = 348;

/// Actual RGBA belongs only to this fixed viewport. Declaration order releases
/// CGContext before its backing Vec; neither canvas nor document text enters product state.
pub(super) struct Canvas {
    pub(super) context: CFRetained<CGContext>,
    pub(super) pixels: Vec<u8>,
}
impl Canvas {
    /// Multiple-selection controls keep one pixel per point; emoji comparison uses
    /// the window's actual backing scale separately.
    fn new(viewport: NSRect) -> Result<Self, String> {
        Self::scaled(viewport, 1.0)
    }

    /// Accept only the synthetic window's bounded RGBA extent at up to 2x actual
    /// backing scale, rejecting nonfinite or oversized geometry before allocation.
    pub(super) fn scaled(viewport: NSRect, scale: f64) -> Result<Self, String> {
        if !viewport.origin.x.is_finite()
            || !viewport.origin.y.is_finite()
            || !(0.0 < viewport.size.width && viewport.size.width <= WIDTH as f64)
            || !(0.0 < viewport.size.height && viewport.size.height <= HEIGHT as f64)
            || !scale.is_finite()
            || !(0.0 < scale && scale <= 2.0)
        {
            return Err("原像素视口或实际 backing scale 超出固定合成画布".into());
        }
        // Actual cell heights may be fractional. Round outward like production
        // CPU surfaces while preserving scale, origin, and precise clip, rather
        // than snapping valid point geometry or clipping the final pixel row.
        let width = (viewport.size.width * scale).ceil() as usize;
        let height = (viewport.size.height * scale).ceil() as usize;
        let mut pixels = vec![0; width * height * 4];
        // SAFETY: The system's public static sRGB name is valid for this process.
        let space =
            CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).ok_or("缺少 sRGB")?;
        // SAFETY: The Vec covers stride*height RGBA8 bytes and is never resized;
        // field drop order releases the context before its borrowed pixel allocation.
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
        .ok_or("无法创建有限像素画布")?;
        // Scene, final image, and position masks share actual scale and Y-up
        // coordinates, with no guessed line indexes or manual pixel flipping.
        CGContext::scale_ctm(Some(&context), scale, scale);
        CGContext::translate_ctm(Some(&context), -viewport.origin.x, -viewport.origin.y);
        CGContext::clip_to_rect(Some(&context), viewport);
        Ok(Self { context, pixels })
    }
}

/// Quantize selection color through independent actual Core Graphics painting,
/// never a guessed integer conversion of floating-point sRGB components.
pub(super) fn selection_pixel() -> Result<[u8; 4], String> {
    let mut pixel = [0_u8; 4];
    // SAFETY: The same system-static sRGB name provides no color data from the Scene under test.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).ok_or("缺少 sRGB")?;
    // SAFETY: The four-byte stack pixel outlives the context; no Native claim
    // referencing this stack allocation escapes the function.
    let context = unsafe {
        CGBitmapContextCreate(
            pixel.as_mut_ptr().cast(),
            1,
            1,
            8,
            4,
            Some(&space),
            CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
        )
    }
    .ok_or("无法创建选择色对照")?;
    CGContext::set_rgb_fill_color(Some(&context), 0.7, 0.8, 1.0, 1.0); // Independently fixed Rig selection color.
    CGContext::fill_rect(
        Some(&context),
        NSRect::new(NSPoint::ZERO, NSSize::new(1.0, 1.0)),
    );
    CGContext::flush(Some(&context));
    drop(context); // Return all Native access to the stack pixel before returning its actual bytes.
    Ok(pixel)
}

/// Both controls consume the original Scene's bounded paint protocol. Read pixels
/// only after its actual completion receipt returns.
pub(super) fn paint_scene(
    scene: Rc<dyn NativeMarkdownPagedTextScene>,
    key: &NativeMarkdownPagedSceneKey,
    actual: &Canvas,
    current: &dyn Fn() -> bool,
) -> Result<usize, String> {
    let started = Instant::now();
    let mut session = scene
        .clone()
        .begin_cpu_paint(actual.context.clone(), current)
        .map_err(|cause| cause.to_string())?;
    let mut steps = 0;
    loop {
        // One metadata record in the first step must permit yielding after actual
        // window acquisition; subsequent steps continue the same original paint cursor.
        let budget = if steps == 0 {
            NativeMarkdownPagedScenePaintBudget {
                glyphs: 1,
                metadata_records: 1,
            }
        } else {
            NativeMarkdownPagedScenePaintBudget::default()
        };
        // Continue the actual production session cursor; an unbudgeted paint call
        // cannot substitute for this bounded path.
        let step = session
            .step(budget, current)
            .map_err(|cause| cause.to_string())?;
        steps += 1;
        scene
            .validate_ready(current)
            .map_err(|cause| cause.to_string())?;
        if step.work.glyphs > budget.glyphs
            || step.work.metadata_records > budget.metadata_records
            || step.work.batches > 256
            || started.elapsed() > Duration::from_secs(3)
        {
            return Err("真实有限绘图超过原单步预算或三秒期限".into());
        }
        match step.outcome {
            NativeMarkdownPagedScenePaintOutcome::Pending(next) => session = next,
            NativeMarkdownPagedScenePaintOutcome::Complete(completion) => {
                if !completion.permits(key, &actual.context) || completion.work().glyphs == 0 {
                    return Err("有限像素缺少原 context/Scene 的真实非空完成票".into());
                }
                break; // Only actual completion permits pixel inspection; Pending cannot publish partial-image success.
            }
        }
    }
    Ok(steps)
}

/// Compare actual synchronous/1-glyph-1-metadata stepped painting pixel for pixel,
/// and require the completion receipt to return its actual context claim. Use
/// product Scene interfaces and real pixel resources, not private Grid/cursor/publication state.
pub(super) fn exact_scene_rgba(
    scene: Rc<dyn NativeMarkdownPagedTextScene>,
    key: &NativeMarkdownPagedSceneKey,
    scale: f64,
    current: &dyn Fn() -> bool,
) -> Result<(), String> {
    use crate::native_surface::viewport_pixels::{NativeMarkdownViewportPixels, TiledBitmap};
    let expected = Canvas::scaled(key.request.viewport, scale)?;
    let actual = Canvas::scaled(key.request.viewport, scale)?;
    // The synchronous path keeps its original background/selection loops rather
    // than the stepped decoration cursor; equal system CG colors/scale provide
    // an independent pixel comparison for the new stepped path.
    scene
        .paint(&expected.context, current)
        .map_err(|cause| cause.to_string())?;
    let started = Instant::now();
    let mut session = scene
        .clone()
        .begin_cpu_paint(actual.context.clone(), current)
        .map_err(|cause| cause.to_string())?;
    let mut steps = 0;
    loop {
        let budget = NativeMarkdownPagedScenePaintBudget {
            glyphs: 1,
            metadata_records: 1,
        };
        let step = session
            .step(budget, current)
            .map_err(|cause| cause.to_string())?;
        steps += 1;
        scene
            .validate_ready(current)
            .map_err(|cause| cause.to_string())?;
        // Budgets require actual bounded main-thread work, not merely a matching
        // final image after one cell synchronously processes all its parts.
        if step.work.glyphs > budget.glyphs
            || step.work.metadata_records > budget.metadata_records
            || started.elapsed() > Duration::from_secs(3)
        {
            return Err("真实Grid分步绘制超出1/1预算或原三秒帧期限".into());
        }
        match step.outcome {
            NativeMarkdownPagedScenePaintOutcome::Pending(next) => session = next,
            NativeMarkdownPagedScenePaintOutcome::Complete(completion) => {
                if !completion.permits(key, &actual.context) || completion.work().glyphs == 0 {
                    return Err("真实Grid分步完成票丢失原Scene/context或实际字符绘制".into());
                }
                drop(completion); // Return actual cursor/completion context claims before reading pixels.
                break;
            }
        }
    }
    CGContext::flush(Some(&expected.context));
    CGContext::flush(Some(&actual.context));
    scene
        .validate_ready(current)
        .map_err(|cause| cause.to_string())?;
    if let Some((pixel, (expected, actual))) = expected
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(actual.pixels.as_chunks::<4>().0)
        .enumerate()
        .find(|(_, (expected, actual))| expected != actual)
    {
        return Err(format!(
            "真实Grid同步/1项预算异步RGBA不相同：pixel={pixel}, expected={expected:?}, actual={actual:?}"
        ));
    }

    // Use this same Scene with an actual product tile to verify context release.
    // Canvas field drop order alone cannot prove it. prepare_tile seals the real
    // mmap only after its Native context-release callback, rejecting surviving external claims.
    let resources = std::sync::Arc::new(NativeMarkdownViewportPixels::default());
    let observer = resources.observer();
    let bitmap = TiledBitmap::new(
        // Cover the same outward-rounded pixels for fractional viewports;
        // context-release verification must not omit the edge row.
        (key.request.viewport.size.width * scale).ceil() as usize,
        (key.request.viewport.size.height * scale).ceil() as usize,
        resources,
    )
    .map_err(|cause| cause.to_string())?;
    let tile = bitmap.next_tile().ok_or("真实Grid像素候选没有首tile")?;
    let writable = bitmap.begin_tile(tile).map_err(|cause| cause.to_string())?;
    // Use the product tile's same Y-up coordinates/actual scale. This context
    // belongs only to this function's actual unpublished candidate.
    CGContext::scale_ctm(Some(writable.context()), scale, scale);
    CGContext::translate_ctm(
        Some(writable.context()),
        -key.request.viewport.origin.x - tile.x as f64 / scale,
        -key.request.viewport.origin.y - tile.bottom as f64 / scale,
    );
    let mut session = scene
        .clone()
        .begin_cpu_paint(writable.context_claim(), current)
        .map_err(|cause| cause.to_string())?;
    loop {
        let step = session
            .step(NativeMarkdownPagedScenePaintBudget::default(), current)
            .map_err(|cause| cause.to_string())?;
        if started.elapsed() > Duration::from_secs(3) {
            return Err("真实Grid完成context归还超过原帧期限".into());
        }
        match step.outcome {
            NativeMarkdownPagedScenePaintOutcome::Pending(next) => session = next,
            NativeMarkdownPagedScenePaintOutcome::Complete(completion) => {
                if !completion.permits(key, writable.context()) {
                    return Err("真实Grid完成票属于另一个tile context".into());
                }
                drop(completion);
                break;
            }
        }
    }
    let prepared = bitmap
        .prepare_tile(writable)
        .map_err(|cause| cause.to_string())?;
    drop(prepared); // This resource control publishes no image; the production owner returns the actual sealed mmap.
    drop(bitmap);
    scene
        .validate_ready(current)
        .map_err(|cause| cause.to_string())?;
    if !observer.is_released() {
        return Err(format!(
            "真实Grid完成context后像素资源未归还：{:?}",
            observer.snapshot()
        ));
    }
    println!(
        "native_body_table_paint exact_sync_async_rgba=true metadata_budget=1 glyph_budget=1 actual_steps={steps} actual_context_sealed=true pixel_resources_released=true mid_paint_cancel_tested=false whole_product=false formal=false"
    );
    Ok(())
}

/// Independently mark both selected graphemes and their unselected gap, then
/// compare actual selection/background pixels under the original Scene authority.
fn paint(rig: &Rig, key: &NativeMarkdownPagedSceneKey) -> Result<(), String> {
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    if !scene.key().same(key) {
        return Err("多选绘制未取得同一原 Ready Scene".into());
    }
    let current = || NativeMarkdownPagedTextViewDelegate::scene_current(&*rig.body, key);
    let actual = Canvas::new(key.request.viewport)?;
    let mask = Canvas::new(key.request.viewport)?;
    let steps = paint_scene(scene.clone(), key, &actual, &current)?;
    for (position, color) in [
        (fixture::LOCAL_FIRST, [1.0, 0.0, 0.0]),
        (fixture::LOCAL_SECOND, [0.0, 1.0, 0.0]),
        (20, [0.0, 0.0, 1.0]),
    ] {
        // The same actual Scene supplies first-grapheme geometry. Independent
        // three-color Native masks mark positions without sharing decoration code.
        let range = NSRange::new(position, 1);
        let first = scene.first_rect(range).map_err(|cause| cause.to_string())?;
        if first.actual_range != range
            || first.rect.size.width <= 0.0
            || first.rect.size.height <= 0.0
        {
            return Err("固定多选/空隙位置缺少真实可见 GCB 矩形".into());
        }
        CGContext::set_rgb_fill_color(Some(&mask.context), color[0], color[1], color[2], 1.0);
        CGContext::fill_rect(Some(&mask.context), first.rect);
    }
    CGContext::flush(Some(&actual.context));
    CGContext::flush(Some(&mask.context));
    let selected = selection_pixel()?;
    let mut locations = [0_usize; 3];
    let mut selections = [0_usize; 3];
    let mut backgrounds = [0_usize; 3];
    for (pixel, marker) in actual
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(mask.pixels.as_chunks::<4>().0)
    {
        // Count solid interior mask pixels only, excluding antialiased edges from
        // selection/background and position counts.
        let location = match *marker {
            [255, 0, 0, 255] => 0,
            [0, 255, 0, 255] => 1,
            [0, 0, 255, 255] => 2,
            _ => continue,
        };
        locations[location] += 1;
        selections[location] += usize::from(*pixel == selected);
        backgrounds[location] += usize::from(*pixel == [255; 4]);
    }
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    if locations.contains(&0)
        || selections[0] == 0
        || selections[1] == 0
        || selections[2] != 0
        || backgrounds[2] == 0
    {
        return Err(format!(
            "真实多选图像未分别高亮或误包络间隙：mask={locations:?} selection={selections:?} background={backgrounds:?}"
        ));
    }
    println!(
        "native_body_find stage=select_all_pixels actual_ranges=2 bounded_paint_steps={steps} native_mask_pixels={locations:?} actual_selection_pixels={selections:?} actual_background_pixels={backgrounds:?} original_source_and_scene=true formal=false whole_product=false"
    );
    Ok(())
}

/// Copy both actual matches under original Source authority to this function's
/// unique private pasteboard, preserving selection and never reading another Source.
fn copy_selected_source(rig: &Rig, key: &NativeMarkdownPagedSceneKey) -> Result<(), String> {
    let input = rig
        .body
        .probe_find_input_weak()
        .upgrade()
        .ok_or("原多选输入已经释放")?;
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let geometry = rig.view.find_geometry_epoch().ok_or("原多选几何已经撤销")?;
    let current = |epoch| {
        epoch == EPOCH
            && rig
                .view
                .find_input_is_current(&input, epoch, &before, Some(geometry))
            && NativeMarkdownPagedTextViewDelegate::scene_current(&*rig.body, key)
            && native_markdown_controller_has_foreground_authority(&rig.controller)
    };
    let permit = input
        .capture(EPOCH, &current)
        .map_err(|cause| cause.to_string())?;
    let ranges = input
        .selected_ranges(&permit, &current)
        .map_err(|cause| cause.to_string())?;
    if ranges
        != [
            fixture::LOCAL_FIRST..fixture::LOCAL_FIRST + 2,
            fixture::LOCAL_SECOND..fixture::LOCAL_SECOND + 2,
        ]
        || before != key.request.input
    {
        return Err("原SelectAll没有保持两个确切局部命中范围".into());
    }
    let document_state = || {
        let document = rig.document.upgrade().ok_or("原Find文档已释放")?;
        let document = document.try_borrow().map_err(|_| "原正文仍在完成编辑")?;
        Ok::<_, String>((
            document.generation(),
            document.byte_len(),
            document.utf16_len(),
            document.selection(),
            document.is_dirty(),
            document.can_undo(),
            document.can_redo(),
        ))
    }; // Project bounded scalars only; return each document Rc/Ref before Native calls, without a full-document hash/String.
    let canonical = document_state()?;
    // The parent can recover this exact owned name if this process aborts; the
    // normal result path below still releases it before selection restoration.
    let pasteboard = private_pasteboard()?;
    let result = autoreleasepool(|_| {
        rig.body
            .copy_source_selection_to(&key.request, &pasteboard)
            .map_err(|cause| cause.to_string())?;
        // SAFETY: The process-lifetime system constant is the same standard plain-
        // text type used by production publication.
        let string_type = unsafe { NSPasteboardTypeString };
        let data = pasteboard
            .dataForType(string_type)
            .ok_or("私有剪贴板没有实际多选输出")?;
        let expected = "局部\n局部";
        if data.length() != expected.len() {
            return Err("原多选复制长度不符，可能误复制了范围包络或整篇正文".into());
        }
        // SAFETY: This synchronous stack retains the complete NSData; inspect the
        // fixed 13 bytes without sending any message that mutates that data.
        if unsafe { data.as_bytes_unchecked() } != expected.as_bytes() {
            return Err("原多选复制未精确保留两个局部命中和中间单个换行".into());
        }
        let value = pasteboard
            .stringForType(string_type)
            .ok_or("多选UTF-8输出未被系统识别为文本")?;
        if value.length() != expected.encode_utf16().count() || value.to_string() != expected {
            return Err("系统多选文本值与固定Unicode期待不符".into());
        }
        let after = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        if !after.same(key)
            || input.state().map_err(|cause| cause.to_string())? != before
            || input
                .selected_ranges(&permit, &current)
                .map_err(|cause| cause.to_string())?
                != ranges
            || document_state()? != canonical
            || rig.backing.failed()
        {
            return Err("多选复制改变了原Source、完整选择、canonical或历史".into());
        }
        println!(
            "native_body_find stage=select_all_copy actual_ranges=2 actual_utf8_bytes={} exact_private_value=true canonical_selection_history_unchanged=true original_source_and_scene=true formal=false whole_product=false",
            data.length(),
        );
        Ok(())
    });
    // Release only this unique owned pasteboard name on success or ordinary
    // failure, before continuing original selection restoration.
    // SAFETY: This function created the board on the current main thread;
    // public releaseGlobally takes no arguments and returns void.
    unsafe {
        let _: () = objc2::msg_send![&*pasteboard, releaseGlobally];
    }
    result
}

/// Compare the fixed emoji's original Scene RGBA with actual published CALayer
/// output after complete-frame delivery. This is no independent shaping oracle;
/// only the fixture's two-emoji or emoji/Chinese ranges are admitted, never blank pixels.
pub(super) fn emoji_pixels(rig: &Rig, stage: &str, range: NSRange) -> Result<(), String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if key.request.input.primary_range != range
        || !matches!(range.length, 3 | 4)
        || rig.copy::<2>(range.location)? != [0xd83d, 0xde00]
    {
        return Err("原 Emoji 对照没有保持实际 Find 选择与固定首字符".into());
    }
    let emoji_count = if range.length == 4 {
        if rig.copy::<2>(range.location + 2)? != [0xd83d, 0xde00] {
            return Err("双 Emoji 对照的实际第二字符不是原夹具的😀".into());
        }
        2
    } else {
        if rig.copy::<1>(range.location + 2)? != [0x4e2d] {
            return Err("混合 Emoji 对照的实际后继字符不是原夹具的中".into());
        }
        1
    }; // Read at most four original selected UTF-16 units, never a paragraph or new Source.
    let paints = rig
        .view
        .cpu_paints()
        .ok_or("原 Emoji 没有 CPU 绘图 owner")?;
    let current = || {
        NativeMarkdownPagedTextViewDelegate::scene_current(&*rig.body, &key)
            && rig.view.cpu_paints() == Some(paints)
            && rig.view.cpu_paint_pending() == Some(false)
            && rig
                .view
                .capture_ready_scene_key()
                .is_ok_and(|actual| actual.same(&key))
    };
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    if !scene.key().same(&key) || !current() {
        return Err("原 Emoji 尚未发布同一完整 CPU 帧".into());
    }
    let scale = rig.panel.backingScaleFactor();
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    let actual = Canvas::scaled(key.request.viewport, scale)?;
    let rendered = Canvas::scaled(key.request.viewport, scale)?;
    let mask = Canvas::scaled(key.request.viewport, scale)?;
    let steps = paint_scene(scene.clone(), &key, &actual, &current)?;
    for index in 0..emoji_count {
        let scalar = NSRange::new(range.location + index * 2, 2);
        let first = scene
            .first_rect(scalar)
            .map_err(|cause| cause.to_string())?;
        if first.actual_range != scalar
            || first.rect.size.width <= 0.0
            || first.rect.size.height <= 0.0
        {
            return Err("原 Emoji 缺少同一可见 GCB 的真实非空框".into());
        }
        // Actual glyph geometry supplies the rectangle; red/green mark location
        // independently of painted colors and any guessed baseline.
        CGContext::set_rgb_fill_color(
            Some(&mask.context),
            if index == 0 { 1.0 } else { 0.0 },
            if index == 0 { 0.0 } else { 1.0 },
            0.0,
            1.0,
        );
        CGContext::fill_rect(Some(&mask.context), first.rect);
        scene
            .validate_ready(&current)
            .map_err(|cause| cause.to_string())?;
    }

    // Inspect the actual published backing tree, which must have one CPU mount.
    // A separately constructed layer pool cannot stand in for product output.
    let bounds = rig.view.bounds();
    let backing = rig.view.layer().ok_or("原 Emoji 视图没有实际 backing")?;
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    // SAFETY: This stack retains the current key's live main-thread backing;
    // observe only its actual public CALayer child relationships.
    let children = unsafe { backing.sublayers() }.ok_or("原 Emoji 没有实际挂载图")?;
    if children.len() != 1 {
        return Err("原 Emoji 的实际 backing 不再包含唯一 CPU 挂载点".into());
    }
    let mount = children.objectAtIndex(0);
    let local = NSRect::new(NSPoint::ZERO, bounds.size);
    if mount.frame() != bounds || mount.bounds() != local || !current() {
        return Err("原 Emoji 的实际挂载图与原有限视口坐标不符".into());
    }
    // Mount coordinates begin at zero. Restore the actual view-bounds origin,
    // then render with the same backing scale.
    CGContext::translate_ctm(Some(&rendered.context), bounds.origin.x, bounds.origin.y);
    mount.renderInContext(&rendered.context);
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    CGContext::flush(Some(&actual.context));
    CGContext::flush(Some(&rendered.context));
    CGContext::flush(Some(&mask.context));
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;

    let mut locations = [0_usize; 2];
    let mut colored = [0_usize; 2];
    let mut rendered_colored = [0_usize; 2];
    let mut mismatch = None;
    for (pixel_index, ((expected, output), marker)) in actual
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rendered.pixels.as_chunks::<4>().0)
        .zip(mask.pixels.as_chunks::<4>().0)
        .enumerate()
    {
        let index = match *marker {
            [255, 0, 0, 255] => 0,
            [0, 255, 0, 255] => 1,
            _ => continue, // Compare actual visible emoji rectangles only, excluding offscreen text and other controls.
        };
        locations[index] += 1;
        // The fixed emoji must have actual warm pixels. White, blue selection,
        // and antialiased black text cannot satisfy both red/green exceeding blue.
        let warm = |pixel: &[u8; 4]| pixel[3] != 0 && pixel[0] > pixel[2] && pixel[1] > pixel[2];
        colored[index] += usize::from(warm(expected));
        rendered_colored[index] += usize::from(warm(output));
        if expected != output && mismatch.is_none() {
            mismatch = Some((pixel_index, *expected, *output)); // Retain the first exact mismatch without color/tolerance approximation.
        }
    }
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    if locations[..emoji_count].contains(&0)
        || colored[..emoji_count].contains(&0)
        || rendered_colored[..emoji_count].contains(&0)
        || mismatch.is_some()
    {
        return Err(format!(
            "原 Emoji Scene 与实际 CALayer 颜色不符：range={range:?} mask={locations:?} scene_warm={colored:?} layer_warm={rendered_colored:?} first_rgba_mismatch={mismatch:?}"
        ));
    }
    println!(
        "native_body_find stage={stage} actual_emoji_range={range:?} actual_backing_scale={scale} bounded_paint_steps={steps} actual_visible_emoji_pixels={locations:?} actual_scene_warm_pixels={colored:?} actual_layer_warm_pixels={rendered_colored:?} exact_visible_rgba=true original_scene_and_published_layers=true independent_shaping_oracle=false whole_product=false formal=false"
    );
    Ok(()) // Return all canvases, Native claims, and Scene synchronously, retaining no cross-frame copy.
}

pub(super) fn run(rig: &mut Rig) -> Result<(), String> {
    cases::local_first(rig)?;
    let original = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    rig.bar.query.setStringValue(ns_string!("局部"));
    let result = (|| {
        let frame = observe(&rig.body)?;
        if !matches!(
            rig.action(MarkdownFindAction::SelectAll),
            Ok(NativeMarkdownFindExecution::Complete)
        ) {
            return Err("原局部查询的 SelectAll 未同步完成真实多选".into());
        }
        wait_frame("select_all_frame", &rig.body, frame)?; // Preserve the original three-second action-to-complete-frame gate.
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        if key.request.input.selection_range_count != 2 || key.source != original.source {
            return Err("SelectAll 未保存两个原范围或创建了第二 Source".into());
        }
        paint(rig, &key)?;
        copy_selected_source(rig, &key)?; // Read both original ranges under the same complete frame without disrupting later restoration.
        rig.remember_receipt()
    })();
    let restored = (|| {
        // Multi-selection preserves Core's original memento. Next from 2..4 would
        // select the second match rather than start at document beginning. Use the
        // actual Input's explicit-target contract only to establish the next case's
        // start, without claiming this verifies undirected multi-selection navigation.
        let frame = observe(&rig.body)?;
        {
            let input = rig
                .body
                .probe_find_input_weak()
                .upgrade()
                .ok_or("原多选输入已经释放")?;
            let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
            let geometry = rig.view.find_geometry_epoch().ok_or("原多选几何已经撤销")?;
            let current = |epoch| {
                epoch == EPOCH
                    && rig
                        .view
                        .find_input_is_current(&input, epoch, &before, Some(geometry))
                    && native_markdown_controller_has_foreground_authority(&rig.controller)
            };
            let permit = input
                .capture(EPOCH, &current)
                .map_err(|cause| cause.to_string())?;
            let outcome = input
                .set_selection(&permit, MarkdownTextSelection::caret(0), &current)
                .map_err(|cause| cause.to_string())?;
            let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome else {
                return Err("真实多选恢复文首没有发布唯一选择收据".into());
            };
            if commit.state.generation != before.generation
                || commit.state.selection_range_count != 1
                || commit.state.directed_selection() != Some(MarkdownTextSelection::caret(0))
            {
                return Err("多选恢复错误地编辑正文或未得到真实文首单选".into());
            }
            rig.view
                .apply_find_commit(&input, &commit)
                .map_err(|cause| cause.to_string())?;
        } // Return temporary Input Rc, permit, and commit payload before waiting for the actual Native frame.
        wait_frame("select_all_restore_caret", &rig.body, frame)?;
        let restored = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        if restored.source != original.source {
            return Err("多选测试恢复错误地建立第二 Source".into());
        }
        cases::local_first(rig)
    })(); // Restore original single selection on success or failure before the following transaction cases.
    result?;
    restored
}
