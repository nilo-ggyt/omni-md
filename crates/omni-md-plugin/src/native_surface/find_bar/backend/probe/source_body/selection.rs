//! 原 Body 的有限选区与 Emoji 像素控制；真实 Scene、可见 GCB 和最终 Native 图分别核对。

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputOutcome;
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedScenePaintBudget, NativeMarkdownPagedScenePaintOutcome,
    NativeMarkdownPagedTextScene,
};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
    kCGColorSpaceSRGB,
};

const WIDTH: usize = 720;
const HEIGHT: usize = 348;

/// 仅供本固定 viewport 的实际 RGBA，context 先于像素 Vec 析构，不进入产品或保存正文。
pub(super) struct Canvas {
    pub(super) context: CFRetained<CGContext>,
    pub(super) pixels: Vec<u8>,
}
impl Canvas {
    /// 原多选控制沿用一个点对应一个像素；Emoji 对照另取窗口实际 backing scale。
    fn new(viewport: NSRect) -> Result<Self, String> {
        Self::scaled(viewport, 1.0)
    }

    /// 像素对照采用真实 backing scale；只接受本合成窗口至多两倍的有限 RGBA 画布。
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
        let width = viewport.size.width * scale;
        let height = viewport.size.height * scale;
        if (width - width.round()).abs() > 1e-6 || (height - height.round()).abs() > 1e-6 {
            return Err("原视口没有可精确对齐的实际 backing 像素尺寸".into());
        }
        let (width, height) = (width.round() as usize, height.round() as usize);
        let mut pixels = vec![0; width * height * 4];
        // SAFETY: 系统公开 sRGB 静态名在本进程有效。
        let space =
            CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).ok_or("缺少 sRGB")?;
        // SAFETY: Vec 覆盖固定 RGBA8 的 stride*height；不 resize，字段顺序先释放 context。
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
        // 原 Scene、最终图和位置遮罩共用 Y-up 坐标与实际 scale，不猜行号或手动翻转像素。
        CGContext::scale_ctm(Some(&context), scale, scale);
        CGContext::translate_ctm(Some(&context), -viewport.origin.x, -viewport.origin.y);
        CGContext::clip_to_rect(Some(&context), viewport);
        Ok(Self { context, pixels })
    }
}

/// 选择色由独立 CG 实绘量化，不能把 0.7*255 的猜测整数当作实际 sRGB 输出。
pub(super) fn selection_pixel() -> Result<[u8; 4], String> {
    let mut pixel = [0_u8; 4];
    // SAFETY: 同上，系统静态 sRGB 名不从被测 Scene 借色表。
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).ok_or("缺少 sRGB")?;
    // SAFETY: 四字节栈内单像素在 context 释放前有效；不让 Native claim 逃出此函数。
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
    CGContext::set_rgb_fill_color(Some(&context), 0.7, 0.8, 1.0, 1.0); // 固定 Rig 独立声明的选择色。
    CGContext::fill_rect(
        Some(&context),
        NSRect::new(NSPoint::ZERO, NSSize::new(1.0, 1.0)),
    );
    CGContext::flush(Some(&context));
    drop(context); // 归还 Native 对原栈内像素的全部访问权后才能返回实际字节。
    Ok(pixel)
}

/// 两项控制都消费原 Scene 的同一个有界绘制协议；只在真实完成票返回后读取像素。
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
        // 真实生产 session 每步继续原游标；绝不以一次无预算 paint 绕开原 bounded 路径。
        let step = session
            .step(NativeMarkdownPagedScenePaintBudget::default(), current)
            .map_err(|cause| cause.to_string())?;
        steps += 1;
        scene
            .validate_ready(current)
            .map_err(|cause| cause.to_string())?;
        if step.work.glyphs > 256
            || step.work.metadata_records > 32
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
                break; // 原消费者签完成后才能检查像素，Pending 不发布部分图像成功。
            }
        }
    }
    Ok(steps)
}

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
        // 几何来自同一真实 Scene 的首 GCB；独立三色 Native 遮罩只标位置，不复用其装饰代码。
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
        // 只统计 Native 遮罩内部的纯色像素，边缘抗锯齿不被当作正文选择或额外位置。
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

/// 同一原Source票复制真实两个命中；只写本函数的唯一私有板，不变更选择或借另一Source读正文。
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
    }; // 只投影有限标量；每次原Doc Rc和Ref都在进入Native之前归还，不创建全文hash或String。
    let canonical = document_state()?;
    let pasteboard = NSPasteboard::pasteboardWithUniqueName();
    let result = autoreleasepool(|_| {
        rig.body
            .copy_source_selection_to(&key.request, &pasteboard)
            .map_err(|cause| cause.to_string())?;
        // SAFETY: 系统常量具有进程寿命，与实际生产发布使用完全相同的标准纯文本类型。
        let string_type = unsafe { NSPasteboardTypeString };
        let data = pasteboard
            .dataForType(string_type)
            .ok_or("私有剪贴板没有实际多选输出")?;
        let expected = "局部\n局部";
        if data.length() != expected.len() {
            return Err("原多选复制长度不符，可能误复制了范围包络或整篇正文".into());
        }
        // SAFETY: 本同步栈Retained持有完整NSData，只读固定13字节；期间没有任何修改data的消息。
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
    // 只有唯一自有命名板才允许归还全局名称；成功/普通错误均先归还，保持后续原选择恢复流程。
    // SAFETY: 本板由本函数在当前主线程创建，releaseGlobally公开方法无参数且返回void。
    unsafe {
        let _: () = objc2::msg_send![&*pasteboard, releaseGlobally];
    }
    result
}

/// 完整帧后比较固定 Emoji 的原 Scene RGBA 与当前实际 CALayer 输出；不独立认证整形。
/// range 只接受本导航夹具中的“😀😀”或“😀中”，避免用无 Emoji 的空白区域证明保色。
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
    }; // 每次只读取原选择的至多四个 UTF-16 units，不取得长段落或新 Source。
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
        // 框来自真实 glyph 几何；只用红/绿位置标记，不复制绘制颜色或猜 baseline。
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

    // 读取当前实际发布树；原 backing 必须只有 CPU mount，不能另建 LayerPool 代替产品输出。
    let bounds = rig.view.bounds();
    let backing = rig.view.layer().ok_or("原 Emoji 视图没有实际 backing")?;
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    // SAFETY: 当前 key 对应的活主线程 backing 被本同步栈持有，只读其实际标准 CALayer 子关系。
    let children = unsafe { backing.sublayers() }.ok_or("原 Emoji 没有实际挂载图")?;
    if children.len() != 1 {
        return Err("原 Emoji 的实际 backing 不再包含唯一 CPU 挂载点".into());
    }
    let mount = children.objectAtIndex(0);
    let local = NSRect::new(NSPoint::ZERO, bounds.size);
    if mount.frame() != bounds || mount.bounds() != local || !current() {
        return Err("原 Emoji 的实际挂载图与原有限视口坐标不符".into());
    }
    // mount 自身坐标从零开始；恢复真实 view bounds 原点后沿同一 backing scale 渲染。
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
            _ => continue, // 只比较真实可见 Emoji 框，离屏正文和其他控件不进入该有限控制。
        };
        locations[index] += 1;
        // 固定😀必须含实际暖色像素；白底、蓝色选区及其黑字抗锯齿均不能满足红绿大于蓝。
        let warm = |pixel: &[u8; 4]| pixel[3] != 0 && pixel[0] > pixel[2] && pixel[1] > pixel[2];
        colored[index] += usize::from(warm(expected));
        rendered_colored[index] += usize::from(warm(output));
        if expected != output && mismatch.is_none() {
            mismatch = Some((pixel_index, *expected, *output)); // 保留首差异，不对像素容差或颜色做近似。
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
    Ok(()) // 所有 Canvas、临时 Native claim 与 Scene 到此同步归还，不保存跨帧副本。
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
        wait_frame("select_all_frame", &rig.body, frame)?; // 原三秒动作到完整帧门保持不变。
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        if key.request.input.selection_range_count != 2 || key.source != original.source {
            return Err("SelectAll 未保存两个原范围或创建了第二 Source".into());
        }
        paint(rig, &key)?;
        copy_selected_source(rig, &key)?; // 同一完整帧下只读复制原两范围，不扰动后面的原单选恢复。
        rig.remember_receipt()
    })();
    let restored = (|| {
        // 多选保留 Core 原 memento；直接 Next 会从原 2..4 跳到第二项，不能假定它从全文开头搜索。
        // 这里只用原 Input 的显式目标合同设置下一测试起点；不声称修复未定向多选的文首导航。
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
        } // 原 Input 的临时 Rc、permit 和提交载荷均在等待 Native 帧前归还。
        wait_frame("select_all_restore_caret", &rig.body, frame)?;
        let restored = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        if restored.source != original.source {
            return Err("多选测试恢复错误地建立第二 Source".into());
        }
        cases::local_first(rig)
    })(); // 检查完成或失败都先归回原单选，不改后继完整事务的起点。
    result?;
    restored
}
