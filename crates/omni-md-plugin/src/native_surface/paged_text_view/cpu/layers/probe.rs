//! 主线程显式slot负控：同帧遮罩与半透明彩色、真实renderer、后继关系及实际mmap释放。

use super::*;
use crate::native_surface::viewport_pixels::PixelPresentation;
use objc2::rc::autoreleasepool;
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGBitmapContextGetBytesPerRow,
    CGBitmapContextGetData, CGColor, CGColorSpace, CGContext, CGImageAlphaInfo,
    CGImageByteOrderInfo, kCGColorSpaceSRGB,
};

const WIDTH: usize = 1024;
const HEIGHT: usize = 32;

mod paired;
mod regions;
mod resize;

/// 固定小型对照图，不保存文稿；context先于原像素Vec归还。
struct Canvas {
    context: CFRetained<CGContext>,
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    row_bytes: usize,
}
impl Canvas {
    fn new() -> Result<Self> {
        Self::sized(WIDTH, HEIGHT)
    }
    fn sized(width: usize, height: usize) -> Result<Self> {
        if width == 0 || height == 0 || width > WIDTH || height > 512 {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        } // 仅供本固定小型图像控制，拒绝将辅助Canvas变为任意尺寸整帧分配入口。
        let row_bytes = width
            .checked_mul(4)
            .and_then(|row| row.checked_add(15))
            .map(|row| row & !15)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        let length = row_bytes
            .checked_mul(height)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        let mut pixels = vec![0; length];
        // SAFETY: 静态sRGB常量由系统提供；本控制不更改gamma或默认颜色空间。
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        // SAFETY: 固定Vec覆盖真实对齐row_bytes*height，context存活期不resize；
        // 字段顺序保证先释放context。原1024宽度stride不变，513宽度另有显式padding。
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width,
                height,
                8,
                row_bytes,
                Some(&space),
                CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
            )
        }
        .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        Ok(Self {
            context,
            pixels,
            width,
            height,
            row_bytes,
        })
    }
    fn blue(&self, bounds: NSRect) {
        CGContext::set_rgb_fill_color(Some(&self.context), 0.0, 0.0, 1.0, 1.0);
        CGContext::fill_rect(Some(&self.context), bounds);
    }
}

#[derive(Clone, Copy, Debug)]
enum Mutation {
    None,
    Mask,
    Color,
    Contents,
    Format,
    ReparentAttempt,
}

fn fixture(source: &mut Canvas, counters: Arc<NativeMarkdownViewportPixels>) -> Result<TileFrame> {
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let pixel = &mut source.pixels[(y * WIDTH + x) * 4..][..4];
            if x < 512 {
                let gray = ((x + y) & 255) as u8;
                pixel.copy_from_slice(&[gray, gray, gray, 255]);
            } else {
                pixel.copy_from_slice(&[128, 0, 0, 128]); // 真半透明彩色，不能被局部白底遮罩资格覆盖。
            }
        }
    }
    CGContext::flush(Some(&source.context));
    let mut bitmap = TiledBitmap::new(WIDTH, HEIGHT, counters)?;
    while let Some(tile) = bitmap.next_tile() {
        let writable = bitmap.begin_tile(tile)?;
        let row = CGBitmapContextGetBytesPerRow(Some(writable.context()));
        let data = CGBitmapContextGetData(Some(writable.context()));
        if data.is_null() || row < tile.width * 4 {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        for y in 0..tile.height {
            // SAFETY: 唯一WritableTile实际context覆盖有限stride*height；只写公开width像素，不跨借用或Native回调。
            let target = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * row), tile.width * 4)
            };
            let offset = ((tile.y + y) * WIDTH + tile.x) * 4;
            target.copy_from_slice(&source.pixels[offset..offset + tile.width * 4]);
        }
        bitmap.finish_tile(writable, None)?; // 真实最后context callback之后由生产封存器签发表示。
    }
    let frame = bitmap.finish()?;
    if frame.tiles.len() != 2
        || frame.tiles[0].presentation != PixelPresentation::OpaqueNeutralMask
        || frame.tiles[1].presentation != PixelPresentation::Image
    {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    Ok(frame)
}

/// 所有异常都先实际detach并记录资源，再返回失败；不提前panic跳过清理。
pub(in crate::native_surface::paged_text_view) fn run(_mtm: MainThreadMarker) -> Result<()> {
    resize::run()?;
    for mutation in [
        Mutation::None,
        Mutation::Mask,
        Mutation::Color,
        Mutation::Contents,
        Mutation::Format,
        Mutation::ReparentAttempt,
    ] {
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let (passed, weaks) = autoreleasepool(|_| -> Result<_> {
            let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH as f64, HEIGHT as f64));
            let mut source = Canvas::new()?;
            let frame = fixture(&mut source, Arc::clone(&counters))?;
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
            let [container, foreground, mask] =
                pool.groups[0].slots.borrow()[0].diagnostic_layers();
            let result = (|| -> Result<(bool, bool)> {
                pool.validate(&backing, &publication, &|| true)?;
                pool.mount().renderInContext(&rendered.context);
                CGContext::flush(Some(&rendered.context));
                let exact = rendered.pixels == baseline.pixels;
                match mutation {
                    Mutation::None => {}
                    Mutation::Mask => {
                        // SAFETY: 独立外方owner真实替换mask，原清理不能清这条后继关系。
                        unsafe { foreground.setMask(Some(&successor)) };
                    }
                    Mutation::Color => container.setBackgroundColor(Some(&foreign_color)),
                    Mutation::Contents => {
                        // SAFETY: 独立已保活的真实RGBA参考图，后继contents不是原mask provider。
                        unsafe {
                            mask.setContents(Some(
                                &*std::ptr::from_ref(&*reference).cast::<AnyObject>(),
                            ))
                        };
                    }
                    Mutation::Format => {
                        // SAFETY: 公开静态常量把原单通道mask真实改回RGBA；必须据实际值撤权。
                        unsafe {
                            mask.setContentsFormat(objc2_quartz_core::kCAContentsFormatRGBA8Uint)
                        };
                    }
                    Mutation::ReparentAttempt => {
                        successor.addSublayer(pool.mount()); // 真实完整mount已经提交后才迁给外方。
                        if !pool.revoke_attempt(&attempt, &backing) {
                            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
                        } // 原serial仍同actual attempt，必须清自身业务图片而保外方parent。
                    }
                }
                let rejected = if matches!(mutation, Mutation::None) {
                    pool.validate(&backing, &publication, &|| true).is_ok()
                } else {
                    matches!(
                        pool.validate(&backing, &publication, &|| true),
                        Err(NativeMarkdownPagedTextViewError::Revoked)
                    )
                };
                println!(
                    "paged_mask_slots mutation={mutation:?} exact_rgba={exact} source_tiles=2 neutral_mask_tiles=1 transparent_color_tiles=1 actual_native_rejection={rejected} weak_roles={} formal=false",
                    weaks.len()
                );
                Ok((exact, rejected))
            })();
            let detached = pool.detach(&backing);
            let successor_preserved = match mutation {
                Mutation::Mask => foreground
                    .mask()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<CALayer>(actual, &*successor)),
                Mutation::Color => container
                    .backgroundColor()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<CGColor>(actual, &*foreign_color)),
                Mutation::Contents => {
                    // SAFETY: 只比较opaque身份，不能因原owner清理成功就略过后继contents核验。
                    unsafe { mask.contents() }.as_deref().is_some_and(|actual| {
                        std::ptr::eq(
                            std::ptr::from_ref(actual).cast::<()>(),
                            std::ptr::from_ref(&*reference).cast::<()>(),
                        )
                    })
                }
                Mutation::ReparentAttempt => pool
                    .mount()
                    .superlayer()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<CALayer>(actual, &*successor)),
                Mutation::None | Mutation::Format => true,
            };
            let expected_detach = if matches!(
                mutation,
                Mutation::Mask | Mutation::Color | Mutation::Contents | Mutation::ReparentAttempt
            ) {
                !detached
            } else {
                detached
            };
            drop(frame); // 先归还测试自身的原frame强边；外方仍持mount，必须已不再保原业务像素。
            CATransaction::flush();
            let reparent_pixels =
                !matches!(mutation, Mutation::ReparentAttempt) || observer.is_released();
            println!(
                "paged_mask_slots mutation={mutation:?} foreign_parent_still_held={successor_preserved} original_pixels_released_before_foreign_owner_drop={reparent_pixels} formal=false"
            );
            // 外方在核验后自行归还自己的内容；不是把这一步算入被测旧owner清理。
            // SAFETY: 本控制外方正持活这三个真实角色，None仅归还它自己安装的后继mask/contents。
            unsafe {
                foreground.setMask(None);
                mask.setContents(None);
                successor.setSublayers(None);
            }
            container.setBackgroundColor(None);
            let passed = result
                .as_ref()
                .is_ok_and(|(exact, rejected)| *exact && *rejected)
                && successor_preserved
                && reparent_pixels
                && expected_detach;
            println!(
                "paged_mask_slots mutation={mutation:?} actual_result={result:?} actual_detach_intact={detached} expected_detach={expected_detach} successor_preserved={successor_preserved} formal=false"
            );
            drop((
                pool,
                backing,
                container,
                foreground,
                mask,
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
        let native = weaks.iter().all(|weak| weak.load().is_none());
        let pixels = observer.is_released();
        println!(
            "paged_mask_slots mutation={mutation:?} native_roles={} actual_all_weak_nil={native} actual_pixels_released={pixels} pixels={:?} formal=false",
            weaks.len(),
            observer.snapshot()
        );
        if !passed || !native || !pixels {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
    }
    paired::run(_mtm)?;
    regions::run(_mtm)
}
