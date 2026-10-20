//! 仅在原opaque多色RGBA已经完整绘制后选择有限水平子区；决定前不创建探测图像或像素副本。

use super::*;

struct Fragment {
    first_row: usize,
    height: usize,
    plan: Plan,
}

/// 原只读输入保到全部子区成功或预算回退；最多四个私有行区间，不授予Scene或绘图权限。
pub(in crate::native_surface::viewport_pixels) struct PreparedRegions {
    original: PreparedImage,
    fragments: Vec<Fragment>,
}

pub(in crate::native_surface::viewport_pixels) struct EncodedRegion {
    pub first_row: usize,
    pub height: usize,
    pub encoded: EncodedImage,
}

impl PreparedRegions {
    pub(in crate::native_surface::viewport_pixels) fn single(mut original: PreparedImage) -> Self {
        original.regions = None; // 单块路径不保留不会消费的quarter摘要，没有额外像素或CF资源。
        Self {
            original,
            fragments: Vec::new(),
        }
    }

    pub(in crate::native_surface::viewport_pixels) fn plan(
        mut original: PreparedImage,
        maximum: usize,
    ) -> Result<Self, PixelError> {
        if !matches!(original.plan, Plan::Rgb24) || maximum < 2 || original.height < 2 {
            return Ok(Self::single(original)); // 原A8/索引/透明等表示不拆；有限槽不足时原RGB24仍完整可用。
        }
        let baseline = allocation_length(rgb_length(original.width, original.height)?)?;
        let mut best_cost = baseline;
        let mut best = Vec::new();
        let RegionScans {
            count: scanned,
            parts: mut scans,
        } = *original.regions.take().ok_or(PixelError::InvalidGeometry)?;
        for count in [2usize, 4] {
            if count > maximum || count > original.height {
                continue; // 每片必须正高，且为所有后继原绘图格留足至少一个真实slot。
            }
            let mut fragments = Vec::with_capacity(count);
            let mut cost = 0usize;
            for index in 0..count {
                let first_row = original.height * index / count;
                let end = original.height * (index + 1) / count;
                let height = end - first_row;
                let plan = if count == scanned {
                    scans[index]
                        .take()
                        .ok_or(PixelError::InvalidGeometry)?
                        .into_plan()
                } else {
                    let left = scans[index * 2]
                        .as_ref()
                        .ok_or(PixelError::InvalidGeometry)?;
                    let right = scans[index * 2 + 1]
                        .as_ref()
                        .ok_or(PixelError::InvalidGeometry)?;
                    RegionScan::merge(left, right).into_plan()
                }; // 四区只扫一次；相邻摘要合成原half，奇高的中线仍为floor(height/2)。
                let lengths = output_lengths(&plan, original.width, height, None, true)?;
                cost = add_cost(cost, lengths)?;
                fragments.push(Fragment {
                    first_row,
                    height,
                    plan,
                });
            }
            if cost < best_cost {
                best_cost = cost;
                best = fragments;
            } // 真实页量必须严格更少；等量、页padding反增或全区仍多色一律保持原RGB24。
        }
        Ok(Self {
            original,
            fragments: best,
        })
    }

    pub(in crate::native_surface::viewport_pixels) fn count(&self) -> usize {
        self.fragments.len().max(1)
    }

    pub(in crate::native_surface::viewport_pixels) fn mask_request(
        &self,
        index: usize,
    ) -> Result<Option<MaskPaletteRequest>, PixelError> {
        if self.fragments.is_empty() {
            return if index == 0 {
                Ok(self.original.mask_request())
            } else {
                Err(PixelError::InvalidGeometry)
            };
        }
        let fragment = self
            .fragments
            .get(index)
            .ok_or(PixelError::InvalidGeometry)?;
        Ok(match &fragment.plan {
            Plan::Indexed(palette) => MaskPaletteRequest::new(&palette.colors[..palette.count]),
            Plan::Rgba | Plan::Rgb24 | Plan::Solid(_) | Plan::Neutral => None,
        }) // 只有本区真实完整色表提出请求，真正双遮罩资格仍由原Native校准签发。
    }

    pub(in crate::native_surface::viewport_pixels) fn finish(
        self,
        pairs: &[Option<&CertifiedMaskPairs>],
        prior: Option<&TileFrame>,
    ) -> Result<Vec<EncodedRegion>, PixelError> {
        if pairs.len() != self.count() {
            return Err(PixelError::InvalidGeometry);
        }
        let Self {
            original,
            fragments,
        } = self;
        if fragments.is_empty() {
            return single(original, pairs[0], prior);
        }
        let counters = Arc::clone(&original.original.counters);
        let mut allocations = Vec::with_capacity(fragments.len());
        for (fragment, pairs) in fragments.iter().zip(pairs) {
            let lengths = output_lengths(
                &fragment.plan,
                original.width,
                fragment.height,
                *pairs,
                false,
            )?;
            let mut output = OutputAllocations::default();
            for (index, length) in lengths.into_iter().enumerate() {
                if let Some(length) = length {
                    let allocation = allocate(
                        length,
                        Arc::clone(&counters),
                        Some(tiled::LIVE_PIXEL_BUDGET),
                    );
                    match allocation {
                        Ok(value) => output.0[index] = Some(value),
                        Err(PixelError::BudgetExceeded) => {
                            drop(output);
                            drop(allocations);
                            if counters.pixel_release_failed.load(Ordering::Acquire) {
                                return Err(PixelError::Backend(
                                    "分区预算回退时临时像素页未能真实归还",
                                ));
                            }
                            return single(original, None, prior); // 尚无子区provider，原RGBA未改；只预算不足回整块RGB24。
                        }
                        Err(error) => return Err(error), // 真映射失败不能被压缩回退掩盖，已有临时页照常析构。
                    }
                }
            }
            allocations.push(output);
        } // 所有实际证书和格式确定后才申请输出页；任何图像封存开始前已持完整有限页组。
        let mut encoded = Vec::with_capacity(fragments.len());
        for ((fragment, allocations), pairs) in fragments.into_iter().zip(allocations).zip(pairs) {
            let prepared = PreparedImage {
                original: Arc::clone(&original.original),
                width: original.width,
                height: fragment.height,
                row_bytes: original.row_bytes,
                first_row: original.first_row + fragment.first_row,
                plan: fragment.plan,
                regions: None,
                #[cfg(test)]
                observed_pixels: 0,
            };
            let image = finish_with_allocations(prepared, *pairs, Some(allocations), prior)?;
            encoded.push(EncodedRegion {
                first_row: fragment.first_row,
                height: fragment.height,
                encoded: image,
            });
        } // 只读输入行借用都在纯像素循环内结束，CF封存不保slice、context或RefCell借用。
        drop(original); // 全部独立输出成功后才归还共同原RGBA；失败则所有未发布图像和原输入一起析构。
        if counters.pixel_release_failed.load(Ordering::Acquire) {
            return Err(PixelError::Backend("分区编码后的原RGBA未能真实归还"));
        }
        Ok(encoded)
    }
}

fn single(
    original: PreparedImage,
    pairs: Option<&CertifiedMaskPairs>,
    prior: Option<&TileFrame>,
) -> Result<Vec<EncodedRegion>, PixelError> {
    let height = original.height;
    let encoded = finish_with_allocations(original, pairs, None, prior)?;
    Ok(std::iter::once(EncodedRegion {
        first_row: 0,
        height,
        encoded,
    })
    .collect())
}

fn rgb_length(width: usize, height: usize) -> Result<usize, PixelError> {
    width
        .checked_mul(3)
        .and_then(|bytes| bytes.checked_add(15))
        .map(|bytes| bytes & !15)
        .and_then(|row| row.checked_mul(height))
        .ok_or(PixelError::InvalidGeometry)
}

fn output_lengths(
    plan: &Plan,
    width: usize,
    height: usize,
    pairs: Option<&CertifiedMaskPairs>,
    worst_case: bool,
) -> Result<[Option<usize>; 2], PixelError> {
    if pairs.is_some() && !matches!(plan, Plan::Indexed(_)) {
        return Err(PixelError::InvalidGeometry);
    }
    let byte_plane = || {
        width
            .checked_add(15)
            .map(|bytes| bytes & !15)
            .and_then(|row| row.checked_mul(height))
            .ok_or(PixelError::InvalidGeometry)
    };
    match plan {
        Plan::Rgba => Err(PixelError::InvalidGeometry), // 只有完整opaque原RGB24能分区，局部RGBA说明计划不成立。
        Plan::Rgb24 => Ok([Some(rgb_length(width, height)?), None]),
        Plan::Solid(value) if value[3] == 255 => Ok([Some(4), None]),
        Plan::Solid(_) => Err(PixelError::InvalidGeometry),
        Plan::Neutral => Ok([Some(byte_plane()?), None]),
        Plan::Indexed(palette) => {
            if let Some(pairs) = pairs {
                let request = MaskPaletteRequest::new(&palette.colors[..palette.count])
                    .ok_or(PixelError::InvalidGeometry)?;
                if !pairs.permits(&request) {
                    return Err(PixelError::InvalidGeometry);
                }
            }
            let plane = byte_plane()?;
            Ok([
                Some(plane),
                (worst_case || pairs.is_some()).then_some(plane),
            ])
        } // 统计时按更贵的双A8算页；未拿Native证书的实际单索引图不会比这个预算更多。
    }
}

fn add_cost(cost: usize, lengths: [Option<usize>; 2]) -> Result<usize, PixelError> {
    lengths
        .into_iter()
        .flatten()
        .try_fold(cost, |cost, length| {
            cost.checked_add(allocation_length(length)?)
                .ok_or(PixelError::InvalidGeometry)
        })
}

#[cfg(test)]
#[path = "region_scan_tests.rs"]
mod tests;
