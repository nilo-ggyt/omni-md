//! 双A8只接受有限真实调色板的原生精确校准；这里仅管理像素与纯事实，不调用CALayer。

use super::*;

const COLORS: usize = 256;
const SAMPLES: usize = COLORS * COLORS;

/// 端点只是由原像素提出的有限候选，任何主题、样式或数学插值都不能签发双遮罩资格。
#[derive(Clone)]
pub(in crate::native_surface) struct MaskPaletteRequest {
    colors: [u32; COLORS],
    slots: [u16; COLORS * 2],
    count: usize,
    pub background: [u8; 3],
    pub foreground: [u8; 3],
}
impl MaskPaletteRequest {
    pub(super) fn new(values: &[u32]) -> Option<Self> {
        if values.len() < 2 || values.len() > COLORS {
            return None; // 固色已有更短的原表示；不截断第257色来制造校准资格。
        }
        let mut result = Self {
            colors: [0; COLORS],
            slots: [u16::MAX; COLORS * 2],
            count: values.len(),
            background: [0; 3],
            foreground: [0; 3],
        };
        result.colors[..values.len()].copy_from_slice(values);
        for (ordinal, &color) in values.iter().enumerate() {
            let mut slot = hash(color);
            for _ in 0..COLORS * 2 {
                if result.slots[slot] == u16::MAX {
                    result.slots[slot] = ordinal as u16;
                    break;
                }
                slot = (slot + 1) & (COLORS * 2 - 1);
            } // 表至多半满，所有原色均有固定上界内的位置。
        }
        let key = |color: &u32| {
            let [r, g, b] = components(*color);
            (
                54 * u32::from(r) + 183 * u32::from(g) + 19 * u32::from(b),
                *color,
            )
        };
        result.background = components(*values.iter().min_by_key(|color| key(color))?);
        result.foreground = components(*values.iter().max_by_key(|color| key(color))?);
        Some(result)
    }

    pub(in crate::native_surface) fn same_endpoints(&self, other: &Self) -> bool {
        self.background == other.background && self.foreground == other.foreground
    }

    fn locate(&self, color: u32) -> Option<usize> {
        let mut slot = hash(color);
        for _ in 0..COLORS * 2 {
            let ordinal = self.slots[slot];
            if ordinal == u16::MAX {
                return None;
            }
            if self.colors[usize::from(ordinal)] == color {
                return Some(usize::from(ordinal));
            }
            slot = (slot + 1) & (COLORS * 2 - 1);
        }
        None
    }
}

/// 只有实际256²输出完整匹配原色后才可构造；它证明颜色，不授予Source/Scene/Ready权限。
pub(in crate::native_surface) struct CertifiedMaskPairs {
    request: MaskPaletteRequest,
    blend: MaskPairBlend,
    pairs: [[u8; 2]; COLORS],
}
impl CertifiedMaskPairs {
    pub(super) fn permits(&self, request: &MaskPaletteRequest) -> bool {
        self.request.same_endpoints(request)
            && self.request.count == request.count
            && self.request.colors[..request.count] == request.colors[..request.count]
    }
    pub(super) fn pair(&self, color: u32) -> Option<[u8; 2]> {
        self.request.locate(color).map(|index| self.pairs[index])
    }
    pub(super) fn blend(&self) -> MaskPairBlend {
        self.blend
    }
    pub(super) fn presentation(&self) -> PixelPresentation {
        PixelPresentation::OpaquePairedMasks {
            background: self.request.background,
            foreground: self.request.foreground,
            blend: self.blend,
        }
    }
}

/// 原生校准只借这两个独立provider和一个有限RGBAcontext；不持Source、Font或Native图。
pub(in crate::native_surface) struct MaskCalibrationPixels {
    first: CFRetained<CGImage>,
    second: CFRetained<CGImage>,
    output: Arc<PixelAllocation>,
    context: Option<CFRetained<CGContext>>,
    source_claims: [Arc<AtomicUsize>; 2],
}
impl MaskCalibrationPixels {
    pub(in crate::native_surface) fn new(
        counters: Arc<NativeMarkdownViewportPixels>,
    ) -> Result<Self, PixelError> {
        let make_plane = |vertical: bool| -> Result<_, PixelError> {
            let mut allocation = allocate(
                SAMPLES,
                Arc::clone(&counters),
                Some(tiled::LIVE_PIXEL_BUDGET),
            )?;
            let receipt = Arc::new(AtomicUsize::new(0));
            Arc::get_mut(&mut allocation)
                .expect("新分配尚无context/provider")
                .release_receipt = Some(Arc::clone(&receipt));
            // SAFETY: 新分配没有context/provider，至少覆盖固定256²字节；每个A8索引都逐byte写入。
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(allocation.address.as_ptr().cast::<u8>(), SAMPLES)
            };
            for (index, to) in bytes.iter_mut().enumerate() {
                *to = if vertical {
                    index / COLORS
                } else {
                    index % COLORS
                } as u8;
            }
            let image = seal_neutral_mask(&allocation, COLORS, COLORS, COLORS)?;
            Ok((image, receipt))
        };
        let (first, first_claim) = make_plane(false)?;
        let (second, second_claim) = make_plane(true)?;
        let output = allocate(SAMPLES * 4, counters, Some(tiled::LIVE_PIXEL_BUDGET))?;
        let context = make_context(&output, 0, COLORS, COLORS, COLORS * 4)?;
        Ok(Self {
            first,
            second,
            output,
            context: Some(context),
            source_claims: [first_claim, second_claim],
        })
    }
    pub(in crate::native_surface) fn images(&self) -> (&CGImage, &CGImage) {
        (&self.first, &self.second)
    }
    pub(in crate::native_surface) fn context(&self) -> &CGContext {
        self.context.as_deref().expect("校准仍持原实际context")
    }
    /// 同步render已返回后复制有限首行；此借用绝不跨下一次Native调用。
    pub(in crate::native_surface) fn first_row(&self) -> [u8; COLORS * 4] {
        let mut result = [0; COLORS * 4];
        // SAFETY: 原独占context未在绘制，输出映射完整覆盖256²RGBA；这里只读并立即复制首行。
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.output.address.as_ptr().cast::<u8>(),
                result.as_mut_ptr(),
                result.len(),
            );
        }
        result
    }
    /// 最后真实context回调后输出页只读；两个provider同时归还，不保任何原生图。
    pub(in crate::native_surface) fn finish(
        mut self,
        request: &MaskPaletteRequest,
        blend: MaskPairBlend,
        single: &[u8; COLORS * 4],
    ) -> Result<MaskCalibrationTable, PixelError> {
        let context = self.context.take().ok_or(PixelError::ContextRetained)?;
        CGContext::flush(Some(&context));
        drop(context);
        if self.output.active_contexts.load(Ordering::Acquire) != 0 {
            return Err(PixelError::ContextRetained);
        }
        readonly(&self.output)?;
        // SAFETY: context真实结束且映射已只读，长度固定完整覆盖256²RGBA；读取不跨Native回调。
        let bytes = unsafe {
            std::slice::from_raw_parts(self.output.address.as_ptr().cast::<u8>(), SAMPLES * 4)
        };
        let first_matches = single == &bytes[..COLORS * 4];
        let last_matches = single == &bytes[(COLORS - 1) * COLORS * 4..SAMPLES * 4];
        let mut background = [0; 4];
        background[..3].copy_from_slice(&request.background);
        background[3] = 255;
        let mut foreground = [0; 4];
        foreground[..3].copy_from_slice(&request.foreground);
        foreground[3] = 255;
        if first_matches == last_matches
            || single[..4] != background
            || single[(COLORS - 1) * 4..] != foreground
            || bytes.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 255)
        {
            return Err(PixelError::Backend(
                "双遮罩原生校准的端点、Y方向或透明度不一致",
            ));
        } // 原生图错误不能作为优化miss静默吞掉；只允许真实颜色缺失回退。
        Ok(MaskCalibrationTable {
            request: request.clone(),
            blend,
            allocation: Arc::clone(&self.output),
            zero_first: first_matches,
            source_claims: Some(self.source_claims.clone()),
        })
    }
}

/// 每候选最多两组端点、各两个模式表；所有表仍占真实PixelAllocation页账，Drop实际munmap。
pub(in crate::native_surface) struct MaskCalibrationTable {
    request: MaskPaletteRequest,
    blend: MaskPairBlend,
    allocation: Arc<PixelAllocation>,
    zero_first: bool,
    source_claims: Option<[Arc<AtomicUsize>; 2]>,
}
impl MaskCalibrationTable {
    /// 局部Native图和图像已归还后核原两份allocation确实销毁，不能用全局计数差猜provider释放。
    pub(in crate::native_surface) fn confirm_sources_returned(
        mut self,
    ) -> Result<Self, PixelError> {
        let claims = self
            .source_claims
            .take()
            .ok_or(PixelError::InvalidGeometry)?;
        if claims
            .iter()
            .any(|claim| claim.load(Ordering::Acquire) != 1)
        {
            return Err(PixelError::Backend(
                "校准两个原mask provider和映射尚未实际归还",
            ));
        }
        if self
            .allocation
            .counters
            .pixel_release_failed
            .load(Ordering::Acquire)
        {
            return Err(PixelError::Backend("校准mask的实际映射归还失败"));
        }
        Ok(self) // 只剩真实只读输出表；固定原子收据也释放，不进入候选事实缓存。
    }
    pub(in crate::native_surface) fn matches(
        &self,
        request: &MaskPaletteRequest,
        blend: MaskPairBlend,
    ) -> bool {
        self.request.same_endpoints(request) && self.blend == blend
    }
    pub(in crate::native_surface) fn certify(
        &self,
        request: &MaskPaletteRequest,
        blend: MaskPairBlend,
    ) -> Option<CertifiedMaskPairs> {
        if self.source_claims.is_some() || !self.matches(request, blend) {
            return None;
        }
        let mut pairs = [[0; 2]; COLORS];
        let mut found = [false; COLORS];
        let mut remaining = request.count;
        // SAFETY: 表构造前实际context已归还并mprotect只读；本Arc保同一固定262KiB映射。
        let bytes = unsafe {
            std::slice::from_raw_parts(self.allocation.address.as_ptr().cast::<u8>(), SAMPLES * 4)
        };
        // 每种有限目标调色板只扫描一次65,536个真实输出，不对原图的每个pixel穷举alpha对。
        for (sample, pixel) in bytes.as_chunks::<4>().0.iter().enumerate() {
            let color =
                (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2]);
            let Some(ordinal) = request.locate(color) else {
                continue;
            };
            if !found[ordinal] {
                found[ordinal] = true;
                let row = sample / COLORS;
                pairs[ordinal] = [
                    (sample % COLORS) as u8,
                    if self.zero_first { row } else { 255 - row } as u8,
                ];
                remaining -= 1;
                if remaining == 0 {
                    return Some(CertifiedMaskPairs {
                        request: request.clone(),
                        blend: self.blend,
                        pairs,
                    });
                }
            }
        }
        None // 任一真实RGB没有完全相同的Native输出，整个瓦片沿原表示，不近似或舍弃颜色。
    }
}

pub(super) fn readonly(allocation: &PixelAllocation) -> Result<(), PixelError> {
    // SAFETY: 调用方已结束本独占context，整块映射来自同一mmap且保持创建长度。
    if unsafe {
        libc::mprotect(
            allocation.address.as_ptr(),
            allocation.length,
            libc::PROT_READ,
        )
    } != 0
    {
        return Err(PixelError::Backend("无法封存原生校准的只读颜色表"));
    }
    Ok(())
}
fn hash(color: u32) -> usize {
    (color.wrapping_mul(2_654_435_761) >> 23) as usize
}
fn components(color: u32) -> [u8; 3] {
    [(color >> 16) as u8, (color >> 8) as u8, color as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_hash_covers_every_original_color_without_granting_a_native_pair() {
        let colors: Vec<_> = (0..256).map(|index| index * 65_793).collect();
        let request = MaskPaletteRequest::new(&colors).unwrap();
        for (index, &color) in colors.iter().enumerate() {
            assert_eq!(request.locate(color), Some(index));
        }
        assert_eq!(request.locate(0x0012_3456), None);
        assert_eq!(request.background, [0; 3]);
        assert_eq!(request.foreground, [255; 3]);
        assert!(MaskPaletteRequest::new(&[]).is_none());
        assert!(MaskPaletteRequest::new(&[1]).is_none());
        assert!(MaskPaletteRequest::new(&[0; 257]).is_none());
    }

    #[test]
    fn both_native_mask_providers_hold_separate_actual_mappings_and_return_them() {
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let pixels = MaskCalibrationPixels::new(counters).unwrap();
        let first = pixels.first.clone();
        let second = pixels.second.clone();
        assert!(!std::ptr::eq::<CGImage>(&*first, &*second));
        assert_eq!(observer.snapshot().live_pixel_buffers, 3);
        assert_eq!(observer.snapshot().live_pixel_bytes, SAMPLES * 6);
        drop(pixels);
        assert_eq!(observer.snapshot().live_pixel_buffers, 2);
        drop(first);
        assert_eq!(observer.snapshot().live_pixel_buffers, 1);
        drop(second);
        assert!(observer.is_released()); // 两个provider分别真实callback，不能借一个once标志归还两份claim。
    }

    #[test]
    fn an_external_context_claim_prevents_a_calibration_table_and_returns_on_last_drop() {
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let pixels = MaskCalibrationPixels::new(counters).unwrap();
        let context = pixels.context.as_ref().unwrap().clone();
        let request = MaskPaletteRequest::new(&[0x0013_151c, 0x00db_e0eb]).unwrap();
        assert!(matches!(
            pixels.finish(
                &request,
                MaskPairBlend::ForegroundThenForeground,
                &[0; 1024]
            ),
            Err(PixelError::ContextRetained)
        ));
        assert_eq!(observer.snapshot().live_pixel_buffers, 1);
        drop(context);
        assert!(observer.is_released()); // 没有伪表或Ready；实际最后context释放后才真正munmap。
    }
}
