//! Main-thread gated slot controls cover same-frame masks/translucent colors, actual rendering,
//! successor relationships, and real mapping release.

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

/// A fixed small comparison bitmap owns no document; its context drops before its backing pixel vector.
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
        } // Keep this Canvas limited to small image controls rather than an arbitrary full-frame allocation API.
        let row_bytes = width
            .checked_mul(4)
            .and_then(|row| row.checked_add(15))
            .map(|row| row & !15)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        let length = row_bytes
            .checked_mul(height)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        let mut pixels = vec![0; length];
        // SAFETY: The system supplies the static sRGB constant; this control changes neither gamma nor
        // default color space.
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        // SAFETY: The fixed vector covers aligned row_bytes*height and never resizes while its context
        // lives;
        // field order releases context first. Width 1024 retains its stride, and width 513 has explicit
        // padding.
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
                pixel.copy_from_slice(&[128, 0, 0, 128]); // Actual translucent color cannot be covered by local white-background mask qualification.
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
            // SAFETY: The unique WritableTile context covers bounded stride*height; write active width
            // only, without borrowed data across Native calls.
            let target = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * row), tile.width * 4)
            };
            let offset = ((tile.y + y) * WIDTH + tile.x) * 4;
            target.copy_from_slice(&source.pixels[offset..offset + tile.width * 4]);
        }
        bitmap.finish_tile(writable, None)?; // The production sealer certifies presentation only after the actual final-context callback.
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

/// Actually detach and report resources before returning controlled failures, without an early panic
/// bypassing cleanup.
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
                    pool.detach(&backing); // Publication failure returns the constructed candidate before exiting the control, preserving actual cleanup.
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
                        // SAFETY: An independent control owner actually replaces the mask; original
                        // cleanup must preserve that successor relationship.
                        unsafe { foreground.setMask(Some(&successor)) };
                    }
                    Mutation::Color => container.setBackgroundColor(Some(&foreign_color)),
                    Mutation::Contents => {
                        // SAFETY: The retained actual RGBA reference is independent; replacement
                        // contents are not the original mask provider.
                        unsafe {
                            mask.setContents(Some(
                                &*std::ptr::from_ref(&*reference).cast::<AnyObject>(),
                            ))
                        };
                    }
                    Mutation::Format => {
                        // SAFETY: A public static constant actually changes the original single-channel
                        // mask to RGBA; revoke based on that actual format.
                        unsafe {
                            mask.setContentsFormat(objc2_quartz_core::kCAContentsFormatRGBA8Uint)
                        };
                    }
                    Mutation::ReparentAttempt => {
                        successor.addSublayer(pool.mount()); // Transfer the complete mount only after actual publication.
                        if !pool.revoke_attempt(&attempt, &backing) {
                            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
                        } // The original serial still belongs to this actual attempt; return owned business images while preserving the external parent.
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
                    // SAFETY: Compare opaque identity only; successful original cleanup cannot skip
                    // successor-contents verification.
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
            drop(frame); // Return the control original frame claim first; the external owner retains mount but must retain no original business pixels.
            CATransaction::flush();
            let reparent_pixels =
                !matches!(mutation, Mutation::ReparentAttempt) || observer.is_released();
            println!(
                "paged_mask_slots mutation={mutation:?} foreign_parent_still_held={successor_preserved} original_pixels_released_before_foreign_owner_drop={reparent_pixels} formal=false"
            );
            // After verification, the external owner returns its own contents; this is not cleanup
            // performed by the old owner under test.
            // SAFETY: The control retains these actual roles; None returns only the successor
            // mask/contents it installed itself.
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
