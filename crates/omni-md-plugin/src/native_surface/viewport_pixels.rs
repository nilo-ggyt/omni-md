//! Each bounded pixel allocation transfers from its drawing context to a
//! read-only image provider without copying an entire viewport bitmap.

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{
    CGBitmapContextCreateWithData, CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGContext,
    CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, kCGColorSpaceSRGB,
};
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

mod release;

/// Cross-thread release callbacks update fixed resource facts only. This owner
/// retains neither Native graphs, document text, nor main-thread closures.
#[derive(Default)]
pub(in crate::native_surface) struct NativeMarkdownViewportPixels {
    release: Mutex<release::ReleaseState>,
    pub(in crate::native_surface) live_pixel_buffers: AtomicUsize,
    pub(in crate::native_surface) live_pixel_bytes: AtomicUsize,
    pub(in crate::native_surface) pixel_release_failed: AtomicBool,
    reserved_bytes: AtomicUsize,
    peak_reserved_bytes: AtomicUsize,
    peak_pixel_buffers: AtomicUsize,
    peak_pixel_bytes: AtomicUsize,
}

/// Retain fixed resource observations without extending a context, provider,
/// image, or pixel mapping's lifetime.
#[cfg(any(test, feature = "native-paged-production"))]
#[derive(Clone)]
pub struct NativeMarkdownViewportPixelObserver(
    pub(in crate::native_surface) Arc<NativeMarkdownViewportPixels>,
);

#[cfg(any(test, feature = "native-paged-production"))]
#[derive(Clone, Copy, Debug)]
pub struct NativeMarkdownViewportPixelSnapshot {
    pub live_pixel_buffers: usize,
    pub live_pixel_bytes: usize,
    pub release_failed: bool,
    pub reserved_pixel_bytes: usize,
    pub peak_reserved_pixel_bytes: usize,
    pub peak_pixel_buffers: usize,
    pub peak_pixel_bytes: usize,
}

#[cfg(any(test, feature = "native-paged-production"))]
impl NativeMarkdownViewportPixels {
    /// Return an observation claim, never an ownership claim on actual pixels.
    pub(in crate::native_surface) fn observer(
        self: &Arc<Self>,
    ) -> NativeMarkdownViewportPixelObserver {
        NativeMarkdownViewportPixelObserver(Arc::clone(self))
    }
    /// Read resource counters without acquiring Native or document objects.
    /// These separate atomics are diagnostics, not a process-RSS measurement.
    pub(in crate::native_surface) fn snapshot(&self) -> NativeMarkdownViewportPixelSnapshot {
        NativeMarkdownViewportPixelSnapshot {
            live_pixel_buffers: self.live_pixel_buffers.load(Ordering::Acquire),
            live_pixel_bytes: self.live_pixel_bytes.load(Ordering::Acquire),
            release_failed: self.pixel_release_failed.load(Ordering::Acquire),
            reserved_pixel_bytes: self.reserved_bytes.load(Ordering::Acquire),
            peak_reserved_pixel_bytes: self.peak_reserved_bytes.load(Ordering::Acquire),
            peak_pixel_buffers: self.peak_pixel_buffers.load(Ordering::Acquire),
            peak_pixel_bytes: self.peak_pixel_bytes.load(Ordering::Acquire),
        }
    }
}
#[cfg(any(test, feature = "native-paged-production"))]
impl NativeMarkdownViewportPixelObserver {
    /// Inspect only this owner's allocation facts, independent of AppKit caches.
    pub fn snapshot(&self) -> NativeMarkdownViewportPixelSnapshot {
        self.0.snapshot()
    }
    /// Require actual live/reserved pages to be returned and no recorded failure;
    /// an internal zero does not establish that unrelated framework memory fell.
    pub fn is_released(&self) -> bool {
        let value = self.snapshot();
        value.live_pixel_buffers == 0
            && value.live_pixel_bytes == 0
            && value.reserved_pixel_bytes == 0
            && !value.release_failed
    }
}

/// Callback claims retain pixels and atomic receipts only, never document
/// owners, Native views, or main-thread Rc values.
struct PixelAllocation {
    address: NonNull<c_void>,
    length: usize,
    active_contexts: AtomicUsize,
    provider_returned: AtomicBool,
    counters: Arc<NativeMarkdownViewportPixels>,
    release_receipt: Option<Arc<AtomicUsize>>,
}

// SAFETY: Only one main-thread Bitmap/tile context borrows the writable address.
// All contexts must actually release before provider construction makes the
// mapping read-only. Cross-thread callbacks return Arc/atomic claims only;
// they never read/write pixels or access AppKit.
unsafe impl Send for PixelAllocation {}
// SAFETY: Pixels are immutable after publication, as required for Send above;
// callback completion flags and release receipts are atomic.
unsafe impl Sync for PixelAllocation {}

impl Drop for PixelAllocation {
    fn drop(&mut self) {
        // SAFETY: This allocation owns the original mmap address and length; only its last Arc can unmap it.
        let result = unsafe { libc::munmap(self.address.as_ptr(), self.length) };
        if result == 0 {
            self.counters
                .released_allocation(self.length, self.release_receipt.as_deref());
        } else {
            // Failed unmapping must not subtract still-mapped pages. Keep the
            // sticky failure and independent receipt for strict close verification.
            self.counters
                .pixel_release_failed
                .store(true, Ordering::Release);
            if let Some(receipt) = &self.release_receipt {
                receipt.store(2, Ordering::Release);
            }
        }
    }
}

/// Own the writable context until painting ends, then publish an immutable
/// CGImage backed by the same actual allocation.
#[cfg(any(
    test,
    feature = "native-raster-surface-probe",
    feature = "native-paged-production"
))]
pub(in crate::native_surface) struct Bitmap {
    allocation: Arc<PixelAllocation>,
    context: Option<CFRetained<CGContext>>,
    width: usize,
    height: usize,
    row_bytes: usize,
}

fn rgba_info() -> CGBitmapInfo {
    // Explicit byte order and premultiplied alpha bind the shared RGBA memory to
    // Core Graphics' interpretation; native endianness cannot choose it implicitly.
    CGBitmapInfo(CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0)
}

#[cfg(any(
    test,
    feature = "native-raster-surface-probe",
    feature = "native-paged-production"
))]
impl Bitmap {
    /// Admit finite geometry before allocation, then acquire the real mapping
    /// and context. Locally owned claims unwind if context creation fails.
    pub(in crate::native_surface) fn new(
        width: usize,
        height: usize,
        counters: Arc<NativeMarkdownViewportPixels>,
    ) -> Result<Self, String> {
        let row_bytes = width.checked_mul(4).ok_or("光栅行宽超出预算")?;
        let length = row_bytes.checked_mul(height).ok_or("光栅像素超出预算")?;
        if width == 0 || height == 0 || width.checked_mul(height).is_none_or(|n| n > 4_000_000) {
            // Reject excessive geometry rather than reducing fidelity or
            // clipping valid content to manufacture a small allocation.
            return Err("光栅视口超过像素上限".into());
        }
        // Paged body and gutter share the same real page owner. Narrow images
        // also consume the existing live-pixel budget, including page padding.
        #[cfg(feature = "native-paged-production")]
        let budget = Some(tiled::LIVE_PIXEL_BUDGET);
        #[cfg(not(feature = "native-paged-production"))]
        let budget = None;
        let allocation = allocate(length, counters, budget).map_err(|error| error.to_string())?;
        let context = make_context(&allocation, 0, width, height, row_bytes)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            allocation,
            context: Some(context),
            width,
            height,
            row_bytes,
        })
    }

    /// Borrow the drawing context while this Bitmap still owns its write phase.
    pub(in crate::native_surface) fn context(&self) -> &CGContext {
        self.context
            .as_ref()
            .expect("drawing bitmap owns its context")
    }

    /// Flush and return the original context before sealing. External Native
    /// context claims must also end; the provider refuses remaining writers.
    pub(in crate::native_surface) fn finish(mut self) -> Result<CFRetained<CGImage>, String> {
        let context = self.context.take().ok_or("光栅上下文已经结束")?;
        CGContext::flush(Some(&context));
        drop(context); // The caller must already have ended temporary NSGraphicsContext claims.
        seal(&self.allocation, self.width, self.height, self.row_bytes)
            .map_err(|error| error.to_string())
    }
}

/// Each context has its own actual callback claim. One frame-wide boolean could
/// not establish that all tile contexts returned their independent claims.
struct ContextClaim {
    allocation: Arc<PixelAllocation>,
    data: NonNull<c_void>,
    returned: AtomicBool,
}
// SAFETY: The callback compares data addresses without dereferencing them; it
// returns only this claim and its allocation through atomic release bookkeeping.
unsafe impl Send for ContextClaim {}
// SAFETY: The data address is immutable, mutable release facts are atomic, and
// the callback does not access pixel contents.
unsafe impl Sync for ContextClaim {}

/// # Safety
/// `info` must be this context's reserved raw Arc<ContextClaim>; `data` must be
/// the original subcontext input. Core Graphics invokes its release once.
unsafe extern "C-unwind" fn context_release(info: *mut c_void, data: *mut c_void) {
    // SAFETY: The successful context or failure-compensation local Arc keeps
    // this claim alive; the actual release consumes its raw claim at most once.
    let claim = unsafe { &*info.cast::<ContextClaim>() };
    if !claim.returned.swap(true, Ordering::AcqRel) {
        if data != claim.data.as_ptr() {
            // An unexpected Native address is a sticky resource-contract
            // failure even though this original claim must still be returned.
            claim
                .allocation
                .counters
                .pixel_release_failed
                .store(true, Ordering::Release);
        }
        claim
            .allocation
            .active_contexts
            .fetch_sub(1, Ordering::AcqRel);
        // SAFETY: The once gate consumes only the corresponding into_raw claim.
        // Its last allocation Arc may now perform the actual munmap.
        drop(unsafe { Arc::from_raw(info.cast::<ContextClaim>()) });
    }
}

/// # Safety
/// `info` must be the one raw Arc claim reserved for this allocation's provider;
/// `data` and `size` identify that same read-only mapping.
unsafe extern "C-unwind" fn provider_release(
    info: *mut c_void,
    data: NonNull<c_void>,
    size: usize,
) {
    // SAFETY: The provider's independent Arc keeps the allocation live through
    // its callback; construction-failure compensation also retains a local Arc.
    let allocation = unsafe { &*info.cast::<PixelAllocation>() };
    if !allocation.provider_returned.swap(true, Ordering::AcqRel) {
        if data != allocation.address || size != allocation.length {
            // Return the original claim but preserve the provider-contract
            // failure; a mismatched callback must not certify a clean close.
            allocation
                .counters
                .pixel_release_failed
                .store(true, Ordering::Release);
        }
        // SAFETY: Consume the provider's single into_raw claim only. Actual
        // unmapping waits until the final allocation Arc is returned.
        drop(unsafe { Arc::from_raw(info.cast::<PixelAllocation>()) });
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::native_surface) enum PixelError {
    #[error("整个存活像素图组及候选超过32MiB预算")]
    BudgetExceeded,
    #[error("像素范围或地址计算越界")]
    InvalidGeometry,
    #[error("绘制context未真实归还，不能封存或复用像素")]
    ContextRetained,
    #[cfg(any(test, feature = "native-paged-production"))]
    #[error("原视口仍有未完成tile，不能发布部分图像")]
    IncompleteViewport,
    #[error("{0}")]
    Backend(&'static str),
}

/// Preflight and allocation share actual system-page rounding, so padding
/// cannot disappear from the candidate's resource budget.
fn allocation_length(length: usize) -> Result<usize, PixelError> {
    if length == 0 {
        // A zero-length mapping has no valid pixel/provider ownership contract.
        return Err(PixelError::InvalidGeometry);
    }
    // Small images still reserve complete pages; raw RGBA byte counts would
    // hide their true mapping cost when many such images coexist.
    // SAFETY: sysconf is a public read-only page-size query. Failure rejects the
    // allocation instead of guessing a 4KiB/16KiB platform constant.
    let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
        .ok()
        .filter(|page| page.is_power_of_two())
        .ok_or(PixelError::Backend("无法确定光栅映射页大小"))?;
    let length = length
        .checked_add(page - 1)
        .map(|n| n & !(page - 1))
        .ok_or(PixelError::InvalidGeometry)?;
    Ok(length)
}

/// Reserve this owner's aggregate budget before mmap. The release mutex orders
/// allocation against owner sealing, and every failed acquisition balances its
/// reservation without pretending an unsuccessful munmap returned real pages.
fn allocate(
    length: usize,
    counters: Arc<NativeMarkdownViewportPixels>,
    budget: Option<usize>,
) -> Result<Arc<PixelAllocation>, PixelError> {
    let length = allocation_length(length)?;
    let release = counters
        .release
        .lock()
        .expect("像素分配锁只覆盖真实资源操作");
    if release.sealed {
        return Err(PixelError::Backend("原视口像素拥有者已经封闭"));
    } // Old callbacks cannot create successor allocations under a sealed owner's receipts.
    // Reserve candidate, old-provider, and all other same-owner pages together;
    // counting only one tile would hide simultaneous live allocations.
    let previous_reserved = counters
        .reserved_bytes
        .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
            old.checked_add(length)
                .filter(|next| budget.is_none_or(|limit| *next <= limit))
        })
        .map_err(|_| PixelError::BudgetExceeded)?;
    counters
        .peak_reserved_bytes
        .fetch_max(previous_reserved + length, Ordering::AcqRel);
    // SAFETY: The checked anonymous private mapping stores only this pixel
    // allocation, with the required fd=-1 and offset=0.
    let raw = unsafe {
        libc::mmap(
            ptr::null_mut(),
            length,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if raw == libc::MAP_FAILED {
        // No mapping exists, so return only the reservation and preserve failure.
        counters.reserved_bytes.fetch_sub(length, Ordering::AcqRel);
        return Err(PixelError::Backend("无法分配光栅视口"));
    }
    let buffers = counters.live_pixel_buffers.fetch_add(1, Ordering::AcqRel) + 1;
    let bytes = counters
        .live_pixel_bytes
        .fetch_add(length, Ordering::AcqRel)
        + length;
    counters
        .peak_pixel_buffers
        .fetch_max(buffers, Ordering::AcqRel);
    counters.peak_pixel_bytes.fetch_max(bytes, Ordering::AcqRel);
    let Some(address) = NonNull::new(raw) else {
        // SAFETY: A zero address can be a valid system mapping but cannot satisfy
        // NonNull consumers. Return the actual extent and check its real result.
        if unsafe { libc::munmap(raw, length) } == 0 {
            counters.live_pixel_buffers.fetch_sub(1, Ordering::AcqRel);
            counters
                .live_pixel_bytes
                .fetch_sub(length, Ordering::AcqRel);
            counters.reserved_bytes.fetch_sub(length, Ordering::AcqRel);
        } else {
            // The page is still mapped; retain its counters and sticky failure.
            counters.pixel_release_failed.store(true, Ordering::Release);
        }
        return Err(PixelError::InvalidGeometry);
    };
    drop(release); // Account for the real mapping before moving this same owner into its allocation.
    Ok(Arc::new(PixelAllocation {
        address,
        length,
        active_contexts: AtomicUsize::new(0),
        provider_returned: AtomicBool::new(false),
        release_receipt: None,
        counters,
    }))
}

/// Bind a single writable context to an admitted subrange of the original
/// allocation. Its callback owns a separate raw claim until actual release.
fn make_context(
    allocation: &Arc<PixelAllocation>,
    offset: usize,
    width: usize,
    height: usize,
    row_bytes: usize,
) -> Result<CFRetained<CGContext>, PixelError> {
    let required = row_bytes
        .checked_mul(height)
        .and_then(|length| offset.checked_add(length))
        .ok_or(PixelError::InvalidGeometry)?;
    if width == 0
        || height == 0
        || width.checked_mul(4).is_none_or(|bytes| bytes > row_bytes)
        || required > allocation.length
        || allocation.active_contexts.load(Ordering::Acquire) != 0
    {
        // Invalid stride/extent or an existing writer cannot safely share this
        // writable allocation; reject before handing any address to Native.
        return Err(PixelError::InvalidGeometry);
    }
    // SAFETY: The complete offset + row_bytes * height extent is inside the real
    // allocation; callers retain the admitted tile range for this context.
    let data = unsafe {
        NonNull::new_unchecked(allocation.address.as_ptr().cast::<u8>().add(offset).cast())
    };
    // SAFETY: Core Graphics exports the immutable process-lifetime sRGB name.
    let color = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or(PixelError::Backend("无法建立光栅颜色空间"))?;
    let local = Arc::new(ContextClaim {
        allocation: Arc::clone(allocation),
        data,
        returned: AtomicBool::new(false),
    });
    allocation.active_contexts.fetch_add(1, Ordering::AcqRel);
    let claim = Arc::into_raw(Arc::clone(&local)).cast_mut().cast();
    // SAFETY: Complete width/height/stride storage is available. This context's
    // claim keeps the allocation live and its callback returns only that claim.
    let context = unsafe {
        CGBitmapContextCreateWithData(
            data.as_ptr(),
            width,
            height,
            8,
            row_bytes,
            Some(&color),
            rgba_info().0,
            Some(context_release),
            claim,
        )
    };
    match context {
        Some(context) => {
            // Require actual writes into the shared allocation; a framework-
            // allocated copy would not satisfy this candidate's storage contract.
            if objc2_core_graphics::CGBitmapContextGetData(Some(&context)) != data.as_ptr()
                || allocation.active_contexts.load(Ordering::Acquire) != 1
            {
                return Err(PixelError::Backend("光栅context未保持原实际像素存储"));
            }
            Ok(context)
        }
        None => {
            // SAFETY: The local claim is still live. The once gate permits a
            // synchronous Native failure callback or explicit compensation.
            unsafe { context_release(claim, data.as_ptr()) };
            Err(PixelError::Backend("无法建立光栅绘制上下文"))
        }
    }
}

/// Seal the original RGBA representation with its explicit sRGB format.
fn seal(
    allocation: &Arc<PixelAllocation>,
    width: usize,
    height: usize,
    row_bytes: usize,
) -> Result<CFRetained<CGImage>, PixelError> {
    // SAFETY: The exported sRGB name has process lifetime.
    let color = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or(PixelError::Backend("无法建立光栅图像颜色空间"))?;
    seal_format(
        allocation,
        ImageFormat {
            width,
            height,
            row_bytes,
            pixel_bits: 32,
            color: &color,
            info: rgba_info(),
        },
    )
}

/// Bind exact byte layout and color space together so an indexed image cannot
/// accidentally inherit RGBA's stride or alpha interpretation.
struct ImageFormat<'a> {
    width: usize,
    height: usize,
    row_bytes: usize,
    pixel_bits: usize,
    color: &'a CGColorSpace,
    info: CGBitmapInfo,
}

/// Lossless representations share the same actual provider/read-only/release
/// protocol instead of copying a full viewport image.
fn seal_format(
    allocation: &Arc<PixelAllocation>,
    format: ImageFormat<'_>,
) -> Result<CFRetained<CGImage>, PixelError> {
    let provider = image_provider(allocation)?;
    // SAFETY: The caller established matching stride, bits-per-pixel, and color
    // space, with provider storage for the complete finite image. Null decode
    // uses the default mapping for indexed/solid/RGBA without a document copy.
    unsafe {
        CGImage::new(
            format.width,
            format.height,
            8,
            format.pixel_bits,
            format.row_bytes,
            Some(format.color),
            format.info,
            Some(&provider),
            ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .ok_or(PixelError::Backend("无法建立光栅视口图像"))
}

/// Only an actually classified opaque-neutral tile may use this path. A8 stores
/// 255-gray so the established sRGB bytes can be reproduced exactly.
#[cfg(any(test, feature = "native-paged-production"))]
fn seal_neutral_mask(
    allocation: &Arc<PixelAllocation>,
    width: usize,
    height: usize,
    row_bytes: usize,
) -> Result<CFRetained<CGImage>, PixelError> {
    let provider = image_provider(allocation)?;
    let decode = [1.0, 0.0];
    // SAFETY: Finite A8 rows cover width * height and the provider retains their
    // read-only pages. Decode lives through synchronous construction and
    // explicitly reverses mask sampling; black ink over local white reproduces
    // the original opaque pixels.
    unsafe {
        CGImage::mask_create(
            width,
            height,
            8,
            8,
            row_bytes,
            Some(&provider),
            decode.as_ptr(),
            false,
        )
    }
    .ok_or(PixelError::Backend("无法建立精确中性像素遮罩"))
}

/// Create this allocation's single read-only provider. Construction compensation
/// and the final callback consume its reserved Arc claim only once.
fn image_provider(
    allocation: &Arc<PixelAllocation>,
) -> Result<CFRetained<CGDataProvider>, PixelError> {
    if allocation.active_contexts.load(Ordering::Acquire) != 0 {
        // Dropping a local context handle is insufficient when Native still
        // retains another writer; no immutable image can be published yet.
        return Err(PixelError::ContextRetained);
    }
    // SAFETY: Every actual context has ended through its own callback. Convert
    // the complete original mapping to read-only before the provider can see it.
    let sealed = unsafe {
        libc::mprotect(
            allocation.address.as_ptr(),
            allocation.length,
            libc::PROT_READ,
        )
    };
    if sealed != 0 {
        // Keep ownership local on a failed protection change; no provider was
        // created and the allocation's final Arc still returns its mapping.
        return Err(PixelError::Backend("无法封存只读光栅像素"));
    }
    let claim = Arc::into_raw(Arc::clone(allocation)).cast_mut().cast();
    // SAFETY: The provider receives its own Arc claim on read-only pages, which
    // remain live until its actual final release callback.
    let provider = unsafe {
        CGDataProvider::with_data(
            claim,
            allocation.address.as_ptr(),
            allocation.length,
            Some(provider_release),
        )
    };
    let Some(provider) = provider else {
        // SAFETY: The local allocation preserves this address; the once flag
        // prevents duplicate compensation after synchronous Native cleanup.
        unsafe { provider_release(claim, allocation.address, allocation.length) };
        return Err(PixelError::Backend("无法建立光栅图像数据源"));
    };
    Ok(provider)
}

#[cfg(any(test, feature = "native-paged-production"))]
mod encoding;
#[cfg(any(test, feature = "native-paged-production"))]
mod paired;
#[cfg(any(test, feature = "native-paged-production"))]
mod tiled;
#[cfg(any(test, feature = "native-paged-production"))]
pub(in crate::native_surface) use paired::{
    CertifiedMaskPairs, MaskCalibrationPixels, MaskCalibrationTable, MaskPaletteRequest,
};
#[cfg(any(test, feature = "native-paged-production"))]
pub(in crate::native_surface) use tiled::{
    MAX_TILE_SLOTS, MaskPairBlend, PixelPresentation, PixelTile, PreparedTileEncodingPlan,
    TileFrame, TiledBitmap, WritableTile,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abandoned_context_returns_the_actual_mapping_without_an_image() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let bitmap = Bitmap::new(17, 19, resources).unwrap();
        assert_eq!(observer.snapshot().live_pixel_buffers, 1);
        assert!(observer.snapshot().live_pixel_bytes >= 17 * 19 * 4);
        drop(bitmap); // The real context callback/final Arc unmaps; the observer owns no pixels.
        assert!(observer.is_released());
    }

    #[test]
    fn sealed_image_claims_keep_pixels_until_the_last_real_provider_release() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let bitmap = Bitmap::new(17, 19, resources).unwrap();
        let image = bitmap.finish().unwrap();
        let retained = image.clone();
        drop(image);
        // Ending Bitmap is not pixel release while a real image/provider claim remains.
        assert!(!observer.is_released());
        assert!(observer.snapshot().live_pixel_bytes >= 17 * 19 * 4);
        drop(retained);
        assert!(observer.is_released());
    }
}
