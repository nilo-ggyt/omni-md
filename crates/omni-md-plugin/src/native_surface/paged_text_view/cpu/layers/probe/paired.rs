//! 真实五层配对图的像素差分与安装中途撤权；只用固定合成位图，不接触文稿或产品交互。

use super::super::super::calibration::Calibrations;
use super::super::slot::{AssignmentPoint, Layout, Palette, Slot};
use super::*;
use crate::native_surface::viewport_pixels::MaskPairBlend;

#[derive(Clone, Copy, Debug)]
enum PaletteCase {
    CoverageRamp,
    GlyphColors,
}
impl PaletteCase {
    fn expected_blend(self) -> MaskPairBlend {
        match self {
            Self::CoverageRamp => MaskPairBlend::ForegroundThenForeground,
            Self::GlyphColors => MaskPairBlend::ForegroundThenBackground,
        }
    }
}

/// 左瓦片逐像素使用实际 sRGB 混色，右瓦片保留半透明彩色；两者必须同帧精确合成。
fn fixture(
    source: &mut Canvas,
    counters: Arc<NativeMarkdownViewportPixels>,
    case: PaletteCase,
) -> Result<TileFrame> {
    fixture_with_prior(source, counters, None, false, case)
}

fn fixture_with_prior(
    source: &mut Canvas,
    counters: Arc<NativeMarkdownViewportPixels>,
    prior: Option<&TileFrame>,
    change_pixel: bool,
    case: PaletteCase,
) -> Result<TileFrame> {
    let base = [19_u8, 21, 28];
    let ink = [219_u8, 224, 235];
    CGContext::set_rgb_fill_color(
        Some(&source.context),
        f64::from(base[0]) / 255.0,
        f64::from(base[1]) / 255.0,
        f64::from(base[2]) / 255.0,
        1.0,
    );
    CGContext::fill_rect(
        Some(&source.context),
        NSRect::new(NSPoint::ZERO, NSSize::new(512.0, HEIGHT as f64)),
    );
    for y in 0..HEIGHT {
        for x in 0..512 {
            // 同时改变行和列，错误的顶底翻转、mask 对调或 stride 都不能被恒定行隐藏。
            let alpha = ((x + y * 7) & 255) as f64 / 255.0;
            CGContext::set_rgb_fill_color(
                Some(&source.context),
                f64::from(ink[0]) / 255.0,
                f64::from(ink[1]) / 255.0,
                f64::from(ink[2]) / 255.0,
                alpha,
            );
            CGContext::fill_rect(
                Some(&source.context),
                NSRect::new(NSPoint::new(x as f64, y as f64), NSSize::new(1.0, 1.0)),
            );
        }
    }
    CGContext::flush(Some(&source.context));
    if matches!(case, PaletteCase::GlyphColors) {
        // 这些有限原色取自实际字形输出，含同前景双遮罩无法表示的161820。
        // 这里仅构造精确像素运输控制，真实字体/选区绘制由独立大窗场景验证。
        let colors = [
            [19, 21, 28, 255],
            [219, 224, 235, 255],
            [22, 24, 32, 255],
            [99, 102, 110, 255],
            [181, 185, 196, 255],
            [171, 176, 186, 255],
            [77, 79, 88, 255],
            [44, 46, 53, 255],
            [147, 151, 161, 255],
            [193, 197, 208, 255],
            [133, 137, 146, 255],
            [29, 32, 39, 255],
            [42, 44, 52, 255],
            [216, 221, 232, 255],
            [200, 205, 216, 255],
            [23, 25, 32, 255],
        ];
        for y in 0..HEIGHT {
            for x in 0..512 {
                // 原始二维颜色分布不做近似；翻转、交换mask或错误stride都必须被差分发现。
                let pixel = colors[(x + y * 7) % colors.len()];
                source.pixels[(y * WIDTH + x) * 4..][..4].copy_from_slice(&pixel);
            }
        }
    }
    if change_pixel {
        // 只把一个真实像素替换成色表内的另一实际颜色；端点、几何和其它瓦片均不变。
        let first: [u8; 4] = source.pixels[..4].try_into().unwrap();
        let other: [u8; 4] = source.pixels[4..512 * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .find(|pixel| **pixel != first)
            .copied()
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        source.pixels[..4].copy_from_slice(&other);
    }
    for y in 0..HEIGHT {
        for x in 512..WIDTH {
            source.pixels[(y * WIDTH + x) * 4..][..4].copy_from_slice(&[128, 0, 0, 128]);
        }
    }
    let mut bitmap = TiledBitmap::new(WIDTH, HEIGHT, counters.clone())?;
    let mut calibrations = Calibrations::default();
    while let Some(tile) = bitmap.next_tile() {
        let writable = bitmap.begin_tile(tile)?;
        let row = CGBitmapContextGetBytesPerRow(Some(writable.context()));
        let data = CGBitmapContextGetData(Some(writable.context()));
        if data.is_null() || row < tile.width * 4 {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        for y in 0..tile.height {
            // SAFETY: 唯一可写 context 覆盖原 tile 的 stride*height；仅写有效像素，不跨 Native 借用。
            let target = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * row), tile.width * 4)
            };
            let at = ((tile.y + y) * WIDTH + tile.x) * 4;
            target.copy_from_slice(&source.pixels[at..at + tile.width * 4]);
        }
        let prepared = bitmap.prepare_tile(writable)?;
        let pairs = match prepared.mask_request() {
            Some(request) => calibrations.pairs(&request, counters.clone(), &|| Ok(()))?,
            None => None, // 真透明瓦片不取得任何双遮罩提案，保留原 RGBA。
        };
        bitmap.finish_prepared(prepared, pairs.as_ref(), prior)?;
    }
    drop(calibrations); // 原候选结束前归还校准页，不能将纯表藏到原帧或闲置插件中。
    let frame = bitmap.finish()?;
    if frame.tiles.len() != 2
        || !matches!(
            frame.tiles[0].presentation,
            PixelPresentation::OpaquePairedMasks { blend, .. } if blend == case.expected_blend()
        )
        || frame.tiles[0].secondary_image.is_none()
        || frame.tiles[1].presentation != PixelPresentation::Image
        || frame.tiles[1].secondary_image.is_some()
    {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    Ok(frame)
}

/// 等值重绘必须成对复用；只改一像素则两张图都不能借旧身份，即使其中一平面的字节恰好相同。
fn paired_reuse(case: PaletteCase) -> Result<()> {
    let counters = Arc::new(NativeMarkdownViewportPixels::default());
    let observer = counters.observer();
    autoreleasepool(|_| -> Result<()> {
        let mut source = Canvas::new()?;
        let original = fixture(&mut source, counters.clone(), case)?;
        let same = fixture_with_prior(&mut source, counters.clone(), Some(&original), false, case)?;
        let changed =
            fixture_with_prior(&mut source, counters.clone(), Some(&original), true, case)?;
        let left = &original.tiles[0];
        let equal = &same.tiles[0];
        let altered = &changed.tiles[0];
        let first_equal = std::ptr::eq::<CGImage>(&*left.image, &*equal.image);
        let second_equal = std::ptr::eq::<CGImage>(
            left.secondary_image
                .as_deref()
                .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?,
            equal
                .secondary_image
                .as_deref()
                .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?,
        );
        let first_changed = !std::ptr::eq::<CGImage>(&*left.image, &*altered.image);
        let second_changed = !std::ptr::eq::<CGImage>(
            left.secondary_image
                .as_deref()
                .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?,
            altered
                .secondary_image
                .as_deref()
                .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?,
        );
        let valid = same.reused_images == 2
            && changed.reused_images == 1
            && first_equal
            && second_equal
            && first_changed
            && second_changed;
        println!(
            "paged_paired_reuse case={case:?} actual_same_tile_reuses={} actual_changed_tile_reuses={} actual_both_equal={first_equal}/{second_equal} actual_both_changed={first_changed}/{second_changed} formal=false",
            same.reused_images, changed.reused_images
        );
        drop((original, same, changed, source));
        CATransaction::flush();
        if !valid || !observer.is_released() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        Ok(())
    })
}

#[derive(Clone, Copy, Debug)]
enum Mutation {
    None,
    SecondaryImage,
    SecondaryMask,
    SecondaryColor,
    Background,
    ReversedChildren,
    Hidden,
    Opacity,
    ZPosition,
    GroupHidden,
    GroupOpacity,
    GroupZPosition,
    MountHidden,
    MountOpacity,
    MountZPosition,
}

/// 所有负例均在完整真实发布后修改第二组或顺序，不能只测旧中性路径的第一张图。
fn visible_graphs(case: PaletteCase) -> Result<()> {
    for mutation in [
        Mutation::None,
        Mutation::SecondaryImage,
        Mutation::SecondaryMask,
        Mutation::SecondaryColor,
        Mutation::Background,
        Mutation::ReversedChildren,
        Mutation::Hidden,
        Mutation::Opacity,
        Mutation::ZPosition,
        Mutation::GroupHidden,
        Mutation::GroupOpacity,
        Mutation::GroupZPosition,
        Mutation::MountHidden,
        Mutation::MountOpacity,
        Mutation::MountZPosition,
    ] {
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let (passed, weaks) = autoreleasepool(|_| -> Result<_> {
            let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH as f64, HEIGHT as f64));
            let mut source = Canvas::new()?;
            let frame = fixture(&mut source, counters.clone(), case)?;
            let reference = CGBitmapContextCreateImage(Some(&source.context))
                .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
            let baseline = Canvas::new()?;
            baseline.blue(bounds);
            CGContext::draw_image(Some(&baseline.context), bounds, Some(&reference));
            CGContext::flush(Some(&baseline.context));
            let rendered = Canvas::new()?;
            rendered.blue(bounds);
            let backing = CALayer::new();
            backing.setFrame(bounds);
            backing.setBounds(bounds);
            let pool = LayerPool::new(&|| true)?;
            backing.addSublayer(pool.mount());
            let mut weaks = pool.weaks();
            weaks.push(layer_weak(&backing));
            let successor = CALayer::new();
            weaks.push(layer_weak(&successor));
            let foreign_color = CGColor::new_srgb(0.2, 0.3, 0.4, 1.0);
            let attempt = pool.attempt()?;
            let publication = match pool.publish(&backing, &frame, &attempt, bounds, 1.0, &|| true)
            {
                Ok(publication) => publication,
                Err(error) => {
                    pool.detach(&backing); // 发布失败也先归还已建候选，不能靠测试提前返回跳过真实清理。
                    return Err(error);
                }
            };
            weaks.extend(pool.weaks());
            let [container, first, mask, second, second_mask] =
                pool.groups[0].slots.borrow()[0].diagnostic_all_layers();
            let result = (|| -> Result<(bool, bool)> {
                pool.validate(&backing, &publication, &|| true)?;
                pool.mount().renderInContext(&rendered.context);
                CGContext::flush(Some(&rendered.context));
                let exact = rendered.pixels == baseline.pixels;
                match mutation {
                    Mutation::None => {}
                    Mutation::SecondaryImage => {
                        // SAFETY: 外方保活的完整 RGBA 图仅用于真实替换，不冒充配对 A8。
                        unsafe {
                            second_mask.setContents(Some(
                                &*std::ptr::from_ref(&*reference).cast::<AnyObject>(),
                            ))
                        };
                    }
                    Mutation::SecondaryMask => {
                        // SAFETY: 外方独立标准层替换第二 mask，原清理必须保持该后继关系。
                        unsafe { second.setMask(Some(&successor)) };
                    }
                    Mutation::SecondaryColor => second.setBackgroundColor(Some(&foreign_color)),
                    Mutation::Background => container.setBackgroundColor(Some(&foreign_color)),
                    Mutation::ReversedChildren => {
                        let reversed = NSArray::from_slice(&[&*second, &*first]);
                        // SAFETY: 只改变两个真实自有角色的排列，验证顺序本身是合成权限的一部分。
                        unsafe { container.setSublayers(Some(&reversed)) };
                    }
                    Mutation::Hidden => second.setHidden(true),
                    Mutation::Opacity => second.setOpacity(0.0),
                    Mutation::ZPosition => second.setZPosition(-1.0),
                    Mutation::GroupHidden => pool.groups[0].layer.setHidden(true),
                    Mutation::GroupOpacity => pool.groups[0].layer.setOpacity(0.5),
                    Mutation::GroupZPosition => pool.groups[0].layer.setZPosition(1.0),
                    Mutation::MountHidden => pool.mount().setHidden(true),
                    Mutation::MountOpacity => pool.mount().setOpacity(0.5),
                    Mutation::MountZPosition => pool.mount().setZPosition(1.0),
                }
                let rejected = if matches!(mutation, Mutation::None) {
                    pool.validate(&backing, &publication, &|| true).is_ok()
                } else {
                    matches!(
                        pool.validate(&backing, &publication, &|| true),
                        Err(NativeMarkdownPagedTextViewError::Revoked)
                    )
                };
                Ok((exact, rejected))
            })();
            let detached = pool.detach(&backing);
            let preserved = match mutation {
                Mutation::SecondaryImage => {
                    // SAFETY: 只核原外方 opaque 身份，不访问已撤权图像的内容。
                    unsafe { second_mask.contents() }
                        .as_deref()
                        .is_some_and(|actual| {
                            std::ptr::from_ref(actual).cast::<()>()
                                == std::ptr::from_ref(&*reference).cast::<()>()
                        })
                }
                Mutation::SecondaryMask => second
                    .mask()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<CALayer>(actual, &*successor)),
                Mutation::SecondaryColor => second
                    .backgroundColor()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<CGColor>(actual, &*foreign_color)),
                Mutation::Background => container
                    .backgroundColor()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<CGColor>(actual, &*foreign_color)),
                Mutation::Hidden => second.isHidden(),
                Mutation::Opacity => second.opacity() == 0.0,
                Mutation::ZPosition => second.zPosition() == -1.0,
                Mutation::GroupHidden => pool.groups[0].layer.isHidden(),
                Mutation::GroupOpacity => pool.groups[0].layer.opacity() == 0.5,
                Mutation::GroupZPosition => pool.groups[0].layer.zPosition() == 1.0,
                Mutation::MountHidden => pool.mount().isHidden(),
                Mutation::MountOpacity => pool.mount().opacity() == 0.5,
                Mutation::MountZPosition => pool.mount().zPosition() == 1.0,
                Mutation::None | Mutation::ReversedChildren => true,
            };
            let detach_expected = matches!(mutation, Mutation::None | Mutation::ReversedChildren);
            println!(
                "paged_paired_slots case={case:?} mutation={mutation:?} actual_result={result:?} actual_detach={detached} expected_detach={detach_expected} successor_preserved={preserved} actual_roles={} formal=false",
                weaks.len()
            );
            // 外方控制自行归还替换关系，才能在其后核全部原生 Weak；这不计入被测关闭动作。
            // SAFETY: 这些五层和外方 successor 只存在于本合成控制，业务清理已完成。
            unsafe {
                second.setMask(None);
                second_mask.setContents(None);
                successor.setSublayers(None);
            }
            second.setBackgroundColor(None);
            container.setBackgroundColor(None);
            second.setHidden(false);
            second.setOpacity(1.0);
            second.setZPosition(0.0);
            // 仅控制自身在被测关闭结束后收回外方污染；关闭结果已记录，不能靠此修正通过票。
            for layer in [&*pool.groups[0].layer, pool.mount()] {
                layer.setHidden(false);
                layer.setOpacity(1.0);
                layer.setZPosition(0.0);
            }
            drop(frame);
            CATransaction::flush();
            let pixels_released = observer.is_released();
            let passed = result.is_ok_and(|(exact, rejected)| exact && rejected)
                && preserved
                && detached == detach_expected
                && pixels_released;
            drop((
                pool,
                backing,
                container,
                first,
                mask,
                second,
                second_mask,
                successor,
                foreign_color,
                reference,
                source,
                baseline,
                rendered,
            ));
            CATransaction::flush();
            Ok((passed, weaks))
        })?;
        let native = weaks.iter().all(|value| value.load().is_none());
        println!(
            "paged_paired_slots case={case:?} mutation={mutation:?} exact_and_negative_checks={passed} actual_all_weak_nil={native} actual_pixels={:?} formal=false",
            observer.snapshot()
        );
        if !passed || !native || !observer.is_released() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
    }
    Ok(())
}

/// hook 严格位于真实 setter 效果之后、槽登记之前；外方持槽容器仍不得保住原像素。
fn interrupted_installations(case: PaletteCase) -> Result<()> {
    for point in [
        AssignmentPoint::Image,
        AssignmentPoint::SecondaryImage,
        AssignmentPoint::Background,
        AssignmentPoint::ForegroundColor,
        AssignmentPoint::SecondColor,
        AssignmentPoint::Mask,
        AssignmentPoint::SecondMask,
        AssignmentPoint::Children,
    ] {
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let (passed, weaks) = autoreleasepool(|_| -> Result<_> {
            let mut source = Canvas::new()?;
            let frame = fixture(&mut source, counters.clone(), case)?;
            let image = &frame.tiles[0];
            let slot = Slot::new(image.presentation, &|| true)?;
            let palette = Palette::new(&|| true)?;
            let group = CALayer::new();
            let mut weaks = Vec::new();
            slot.add_weaks(&mut weaks);
            weaks.push(layer_weak(&group));
            let container = slot.container().clone(); // 外方真实持有原层至像素归还验收之后。
            let fired = Cell::new(false);
            let installed = slot.install_with_probe(
                &image.image,
                image.secondary_image.as_ref(),
                Layout {
                    rect: NSRect::new(NSPoint::ZERO, NSSize::new(512.0, HEIGHT as f64)),
                    scale: 1.0,
                    presentation: image.presentation,
                },
                &palette,
                &|| true,
                &|observed| {
                    if observed == point && !fired.replace(true) {
                        let _ = slot.clear(&group, &palette);
                    }
                },
            );
            let rejected = matches!(installed, Err(NativeMarkdownPagedTextViewError::Revoked));
            let cleared = slot.clear(&group, &palette);
            drop(frame);
            CATransaction::flush();
            let pixels_released_with_foreign_container = observer.is_released();
            println!(
                "paged_paired_assignment case={case:?} point={point:?} effect_before_record_hook={} actual_rejected={rejected} final_clear={cleared} original_pixels_released_with_container_alive={pixels_released_with_foreign_container} actual_pixels={:?} formal=false",
                fired.get(),
                observer.snapshot()
            );
            let passed =
                fired.get() && rejected && cleared && pixels_released_with_foreign_container;
            drop((container, slot, group, palette, source));
            CATransaction::flush();
            Ok((passed, weaks))
        })?;
        let native = weaks.iter().all(|value| value.load().is_none());
        if !passed || !native || !observer.is_released() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
    }
    Ok(())
}

pub(super) fn run(_mtm: MainThreadMarker) -> Result<()> {
    for case in [PaletteCase::CoverageRamp, PaletteCase::GlyphColors] {
        // 两种实际合成顺序各走相同复用、外方修改与setter间隙撤权，不借另一模式的通过结果。
        paired_reuse(case)?;
        visible_graphs(case)?;
        interrupted_installations(case)?;
    }
    Ok(())
}
