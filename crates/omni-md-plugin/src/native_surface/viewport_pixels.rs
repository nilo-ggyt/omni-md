//! 每块有界像素分配分别交给绘制context和只读image provider，不复制整帧位图。

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

/// 跨线程释放回调只更新固定原子资源事实，不拥有Native图、正文或主线程闭包。
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

/// 只保活固定原子事实；观察者自身不会延长context、provider、image或mmap生命期。
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
    pub(in crate::native_surface) fn observer(
        self: &Arc<Self>,
    ) -> NativeMarkdownViewportPixelObserver {
        NativeMarkdownViewportPixelObserver(Arc::clone(self))
    }
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
    pub fn snapshot(&self) -> NativeMarkdownViewportPixelSnapshot {
        self.0.snapshot()
    }
    pub fn is_released(&self) -> bool {
        let value = self.snapshot();
        value.live_pixel_buffers == 0
            && value.live_pixel_bytes == 0
            && value.reserved_pixel_bytes == 0
            && !value.release_failed
    }
}

/// 回调只持像素和原子收据，不持正文、原生视图或主线程 Rc。
struct PixelAllocation {
    address: NonNull<c_void>,
    length: usize,
    active_contexts: AtomicUsize,
    provider_returned: AtomicBool,
    counters: Arc<NativeMarkdownViewportPixels>,
    release_receipt: Option<Arc<AtomicUsize>>,
}

// SAFETY: 可写地址每次只借给主线程的一个Bitmap或tile context；provider建立前所有context
// 都必须真实释放，随后mmap改为只读。跨线程回调只归还Arc和原子计数，不读写像素或访问AppKit。
unsafe impl Send for PixelAllocation {}
// SAFETY: 与 Send 相同，发布后像素不可写；回调归还标志和资源收据均为原子变量。
unsafe impl Sync for PixelAllocation {}

impl Drop for PixelAllocation {
    fn drop(&mut self) {
        // SAFETY: This allocation owns the original mmap address and length; only its last Arc can unmap it.
        let result = unsafe { libc::munmap(self.address.as_ptr(), self.length) };
        if result == 0 {
            self.counters
                .released_allocation(self.length, self.release_receipt.as_deref());
        } else {
            // 归还失败不能把仍映射的页记成零；独立收据让严格释放验收明确失败。
            self.counters
                .pixel_release_failed
                .store(true, Ordering::Release);
            if let Some(receipt) = &self.release_receipt {
                receipt.store(2, Ordering::Release);
            }
        }
    }
}

/// 绘制完成前独占 context；完成后只交出同一分配的不可变 CGImage。
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
    CGBitmapInfo(CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0)
}

#[cfg(any(
    test,
    feature = "native-raster-surface-probe",
    feature = "native-paged-production"
))]
impl Bitmap {
    pub(in crate::native_surface) fn new(
        width: usize,
        height: usize,
        counters: Arc<NativeMarkdownViewportPixels>,
    ) -> Result<Self, String> {
        let row_bytes = width.checked_mul(4).ok_or("光栅行宽超出预算")?;
        let length = row_bytes.checked_mul(height).ok_or("光栅像素超出预算")?;
        if width == 0 || height == 0 || width.checked_mul(height).is_none_or(|n| n > 4_000_000) {
            return Err("光栅视口超过像素上限".into()); // 不降采样或裁切来制造低内存通过。
        }
        // Paged正文与行号共用同一真实页账；窄图也不能在原32MiB存活预算外分配。
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

    pub(in crate::native_surface) fn context(&self) -> &CGContext {
        self.context
            .as_ref()
            .expect("drawing bitmap owns its context")
    }

    pub(in crate::native_surface) fn finish(mut self) -> Result<CFRetained<CGImage>, String> {
        let context = self.context.take().ok_or("光栅上下文已经结束")?;
        CGContext::flush(Some(&context));
        drop(context); // 调用方必须先结束所有临时 NSGraphicsContext，不能发布仍可被写入的页。
        seal(&self.allocation, self.width, self.height, self.row_bytes)
            .map_err(|error| error.to_string())
    }
}

/// 每个context各有一份真实callback claim，不能用全帧的单个bool替代多tile实际归还。
struct ContextClaim {
    allocation: Arc<PixelAllocation>,
    data: NonNull<c_void>,
    returned: AtomicBool,
}
// SAFETY: claim中的地址只比较、不解引用；跨线程回调只原子归还对应allocation和本claim。
unsafe impl Send for ContextClaim {}
// SAFETY: data创建后不变；所有可变释放事实都是原子变量，callback不读写像素。
unsafe impl Sync for ContextClaim {}

/// # Safety
/// info必须是本次context预留的Arc<ContextClaim>指针，data与该子context原始输入一致。
unsafe extern "C-unwind" fn context_release(info: *mut c_void, data: *mut c_void) {
    // SAFETY: 成功context或失败补偿的本地Arc保护此claim，实际callback最多消费一次raw claim。
    let claim = unsafe { &*info.cast::<ContextClaim>() };
    if !claim.returned.swap(true, Ordering::AcqRel) {
        if data != claim.data.as_ptr() {
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
        // SAFETY: 原子一次门保证只消费对应into_raw；最后claim可能触发allocation真实munmap。
        drop(unsafe { Arc::from_raw(info.cast::<ContextClaim>()) });
    }
}

/// # Safety
/// info 必须是为此 provider 单独预留的 Arc claim；data/size 对应同一只读分配。
unsafe extern "C-unwind" fn provider_release(
    info: *mut c_void,
    data: NonNull<c_void>,
    size: usize,
) {
    // SAFETY: provider 的独立 Arc 保证回调期间对象存活；失败补偿另有本地 Arc。
    let allocation = unsafe { &*info.cast::<PixelAllocation>() };
    if !allocation.provider_returned.swap(true, Ordering::AcqRel) {
        if data != allocation.address || size != allocation.length {
            allocation
                .counters
                .pixel_release_failed
                .store(true, Ordering::Release);
        }
        // SAFETY: 此回调只消费 provider 的一次 into_raw claim，像素在最后一个 Arc 才 munmap。
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

/// 预检和实际分配使用同一页舍入，避免把padding排除在候选预算外。
fn allocation_length(length: usize) -> Result<usize, PixelError> {
    if length == 0 {
        return Err(PixelError::InvalidGeometry);
    }
    // 总预算按真实系统页向上对齐，不能让大量小图以少量RGBA字节掩盖整页mmap驻留。
    // SAFETY: sysconf公开只读查询页面大小；失败明确拒绝，不猜4KiB或16KiB平台常量。
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
    } // 关闭后的旧Native回调不能在同一原子收据下制造后继分配。
    // CAS在mmap前预占候选、旧provider和其他同owner分配的总量；失败必须配平，不只数单tile。
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
    // SAFETY: 匿名私有映射只保存本分配的RGBA字节；length已检查，fd=-1/offset=0。
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
        // SAFETY: 零地址也可能是系统合法映射；无法交给NonNull消费者，仍必须真实归还并核结果。
        if unsafe { libc::munmap(raw, length) } == 0 {
            counters.live_pixel_buffers.fetch_sub(1, Ordering::AcqRel);
            counters
                .live_pixel_bytes
                .fetch_sub(length, Ordering::AcqRel);
            counters.reserved_bytes.fetch_sub(length, Ordering::AcqRel);
        } else {
            counters.pixel_release_failed.store(true, Ordering::Release);
        }
        return Err(PixelError::InvalidGeometry);
    };
    drop(release); // 原映射已记入账本；归还锁后才把同一个原子拥有者移入实际分配。
    Ok(Arc::new(PixelAllocation {
        address,
        length,
        active_contexts: AtomicUsize::new(0),
        provider_returned: AtomicBool::new(false),
        release_receipt: None,
        counters,
    }))
}

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
        return Err(PixelError::InvalidGeometry);
    }
    // SAFETY: offset和整个rowBytes*height都已证明在真实allocation内；调用方固定原tile范围。
    let data = unsafe {
        NonNull::new_unchecked(allocation.address.as_ptr().cast::<u8>().add(offset).cast())
    };
    // SAFETY: Core Graphics sRGB公开常量静态有效。
    let color = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or(PixelError::Backend("无法建立光栅颜色空间"))?;
    let local = Arc::new(ContextClaim {
        allocation: Arc::clone(allocation),
        data,
        returned: AtomicBool::new(false),
    });
    allocation.active_contexts.fetch_add(1, Ordering::AcqRel);
    let claim = Arc::into_raw(Arc::clone(&local)).cast_mut().cast();
    // SAFETY: 子context具有完整width/height/stride存储，claim保持整个分配；callback只归还本次claim。
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
            // 真实context必须仍写入原共享分配，不能把框架另建的副本误认为零拷贝候选。
            if objc2_core_graphics::CGBitmapContextGetData(Some(&context)) != data.as_ptr()
                || allocation.active_contexts.load(Ordering::Acquire) != 1
            {
                return Err(PixelError::Backend("光栅context未保持原实际像素存储"));
            }
            Ok(context)
        }
        None => {
            // SAFETY: 本地claim仍存活，一次门兼容create失败时框架同步callback与显式补偿。
            unsafe { context_release(claim, data.as_ptr()) };
            Err(PixelError::Backend("无法建立光栅绘制上下文"))
        }
    }
}

fn seal(
    allocation: &Arc<PixelAllocation>,
    width: usize,
    height: usize,
    row_bytes: usize,
) -> Result<CFRetained<CGImage>, PixelError> {
    // SAFETY: sRGB 导出名称具有静态生命周期。
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

/// 精确图像布局与颜色空间共同绑定，防止索引图用RGBA的stride或alpha解释。
struct ImageFormat<'a> {
    width: usize,
    height: usize,
    row_bytes: usize,
    pixel_bits: usize,
    color: &'a CGColorSpace,
    info: CGBitmapInfo,
}

/// 各无损像素表示共用同一个真实provider、只读转换和释放协议，不复制整帧图像。
fn seal_format(
    allocation: &Arc<PixelAllocation>,
    format: ImageFormat<'_>,
) -> Result<CFRetained<CGImage>, PixelError> {
    let provider = image_provider(allocation)?;
    // SAFETY: 调用者已证明本表示的行宽、bpp与颜色空间一致，provider覆盖整幅有限图像；
    // decode=null使用默认映射，indexed/固色/RGBA均不引入正文或整帧副本。
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

/// 只有已逐像素证明opaque-neutral的瓦片能调用；A8保存255-gray，原sRGB字节不变。
#[cfg(any(test, feature = "native-paged-production"))]
fn seal_neutral_mask(
    allocation: &Arc<PixelAllocation>,
    width: usize,
    height: usize,
    row_bytes: usize,
) -> Result<CFRetained<CGImage>, PixelError> {
    let provider = image_provider(allocation)?;
    let decode = [1.0, 0.0];
    // SAFETY: 有限A8行覆盖width*height，provider真实保活只读页；decode在同步构造期有效。
    // image mask的反向采样由decode明确反转；Native以黑前景/局部白底重现原opaque像素。
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

/// 所有表示共用同一个真实provider协议；失败补偿与最后callback都只消费一次Arc。
fn image_provider(
    allocation: &Arc<PixelAllocation>,
) -> Result<CFRetained<CGDataProvider>, PixelError> {
    if allocation.active_contexts.load(Ordering::Acquire) != 0 {
        return Err(PixelError::ContextRetained);
    }
    // SAFETY: 所有实际context均已通过各自callback结束；整块原映射现在改为只读供provider使用。
    let sealed = unsafe {
        libc::mprotect(
            allocation.address.as_ptr(),
            allocation.length,
            libc::PROT_READ,
        )
    };
    if sealed != 0 {
        return Err(PixelError::Backend("无法封存只读光栅像素"));
    }
    let claim = Arc::into_raw(Arc::clone(allocation)).cast_mut().cast();
    // SAFETY: provider 取得独立 Arc claim；输入页已只读，并活到其最后一次真实 callback。
    let provider = unsafe {
        CGDataProvider::with_data(
            claim,
            allocation.address.as_ptr(),
            allocation.length,
            Some(provider_release),
        )
    };
    let Some(provider) = provider else {
        // SAFETY: 本地 allocation 保持地址有效；一次标志防止与 create 的同步清理重复归还。
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
        drop(bitmap); // 真实context callback与最后Arc触发munmap，观察者不拥有像素。
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
        assert!(!observer.is_released()); // Bitmap已结束仍不能冒称归还，CGImage的真实provider还持像素。
        assert!(observer.snapshot().live_pixel_bytes >= 17 * 19 * 4);
        drop(retained);
        assert!(observer.is_released());
    }
}
