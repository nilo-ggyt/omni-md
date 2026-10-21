//! 每个已结束context的有限RGBA瓦片独立无损编码；透明数据保留原alpha，多色不近似颜色。

use super::*;

#[path = "regions.rs"]
mod regions;
#[path = "reuse.rs"]
mod reuse;
pub(super) use regions::PreparedRegions;
use reuse::Format;
pub(super) use reuse::SealedPixels;

pub(super) struct EncodedImage {
    pub image: CFRetained<CGImage>,
    pub secondary_image: Option<CFRetained<CGImage>>,
    pub presentation: PixelPresentation,
    pub storage: SealedPixels,
    pub secondary_storage: Option<SealedPixels>,
    // 只记录本次已完成编码是否借回原活帧；不增长paint，也不为旧Scene延长权限。
    pub reused_image: bool,
    #[cfg(test)]
    pub allocation: Arc<PixelAllocation>,
    #[cfg(test)]
    pub row_bytes: usize,
}

/// 固定256色表及半满hash表，不按文件/画面长度扩容；值只来自原实际RGBA像素。
struct Palette {
    colors: [u32; 256],
    slots: [u16; 512],
    count: usize,
}
impl Palette {
    fn new() -> Self {
        Self {
            colors: [0; 256],
            slots: [u16::MAX; 512],
            count: 0,
        }
    }
    fn locate(&self, color: u32) -> Result<usize, usize> {
        let mut slot = (color.wrapping_mul(2_654_435_761) >> 23) as usize;
        // 表最多半满，512次是显式硬界；不会把碰撞退化成无界heap容器。
        for _ in 0..512 {
            let ordinal = self.slots[slot];
            if ordinal == u16::MAX {
                return Err(slot);
            }
            if self.colors[usize::from(ordinal)] == color {
                return Ok(usize::from(ordinal));
            }
            slot = (slot + 1) & 511;
        }
        unreachable!("有限半满色表必须存在空槽")
    }
    fn insert(&mut self, color: u32) -> bool {
        match self.locate(color) {
            Ok(_) => true,
            Err(slot) if self.count < 256 => {
                self.colors[self.count] = color;
                self.slots[slot] = self.count as u16;
                self.count += 1;
                true
            }
            Err(_) => false, // 第257色只终止索引方案；仍须检查其余alpha，不能量化、截色或抹去emoji。
        }
    }
}

enum Plan {
    Rgba,
    Rgb24,
    Solid([u8; 4]),
    Neutral,
    Indexed(Box<Palette>),
}

/// 连续行区间的精确可合并摘要；即使固色/中性色也保留实际首遇色集，不虚构整张灰阶表。
struct RegionScan {
    first: [u8; 4],
    solid: bool,
    opaque: bool,
    neutral: bool,
    palette_complete: bool,
    palette: Palette,
    #[cfg(test)]
    observed_pixels: usize,
}

#[cfg(test)]
thread_local! {
    // 每个测试线程只记录真实分类像素入口，不能用规划预算或旧颜色表代替实际扫描次数。
    static REGION_OBSERVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl RegionScan {
    fn new(first: [u8; 4]) -> Self {
        Self {
            first,
            solid: true,
            opaque: true,
            neutral: true,
            palette_complete: true,
            palette: Palette::new(),
            #[cfg(test)]
            observed_pixels: 0,
        }
    }

    #[cfg(test)]
    fn scan(
        allocation: &PixelAllocation,
        width: usize,
        height: usize,
        row_bytes: usize,
        first_row: usize,
    ) -> Self {
        let first = row(allocation, first_row, width, row_bytes)[..4]
            .try_into()
            .unwrap();
        let mut result = Self::new(first);
        for y in 0..height {
            for pixel in row(allocation, first_row + y, width, row_bytes)
                .as_chunks::<4>()
                .0
            {
                result.observe(*pixel);
            }
        } // 四个不重叠区间各扫描一次；借用只在纯像素循环内，不跨Native或Source回调。
        result
    }

    fn observe(&mut self, pixel: [u8; 4]) {
        #[cfg(test)]
        {
            self.observed_pixels += 1; // 只在真实像素入口计数，合并色表不伪装成像素访问。
            REGION_OBSERVATIONS.with(|count| count.set(count.get() + 1));
        }
        self.solid &= pixel == self.first;
        self.opaque &= pixel[3] == 255;
        self.neutral &= pixel[3] == 255 && pixel[0] == pixel[1] && pixel[1] == pixel[2];
        if self.palette_complete && !self.palette.insert(rgb(&pixel)) {
            self.palette_complete = false;
        } // 第257色后完整性永久关闭，仍逐像素核alpha/solid，不能因索引失效提前授予RGB24。
    }

    fn merge(left: &Self, right: &Self) -> Self {
        let mut result = Self {
            first: left.first,
            solid: left.solid && right.solid && left.first == right.first,
            opaque: left.opaque && right.opaque,
            neutral: left.neutral && right.neutral,
            palette_complete: left.palette_complete && right.palette_complete,
            palette: Palette::new(),
            #[cfg(test)]
            observed_pixels: 0,
        };
        // 任一子区已经超过256色，整体并集不可能重新变小；只合并仍完整的两个实际色集。
        if result.palette_complete {
            for color in left.palette.colors[..left.palette.count]
                .iter()
                .chain(&right.palette.colors[..right.palette.count])
            {
                if !result.palette.insert(*color) {
                    result.palette_complete = false;
                    break;
                }
            } // 左区首遇序后追加右区真正新增色，与原逐行扫描一致；绝不排序、补色或量化。
        }
        result
    }

    fn into_plan(self) -> Plan {
        if self.solid {
            Plan::Solid(self.first) // 所有RGBA都相同，包含原alpha，仍可使用原1×1固色表示。
        } else if !self.opaque {
            Plan::Rgba // 任意非固色透明像素保原RGBA，不能用不含alpha的局部表示。
        } else if self.neutral {
            Plan::Neutral // 每像素都通过真实opaque-neutral门，继续原单A8表示。
        } else if self.palette_complete {
            Plan::Indexed(Box::new(self.palette)) // 只有完整实际色表可申请原Indexed/Native配对资格。
        } else {
            Plan::Rgb24 // 完整opaque证明且超过256色，继续原无损RGB24回退。
        } // 与原逐区分类保持相同优先级；透明非固色始终保原RGBA，不因色集可合并取得mask资格。
    }
}

/// 仅完整opaque多色瓦片暂存固定摘要；不保第二份像素、Native对象或Source权限。
struct RegionScans {
    count: usize,
    parts: [Option<RegionScan>; 4],
}

struct PreparedClassification {
    plan: Plan,
    regions: Option<Box<RegionScans>>,
    #[cfg(test)]
    observed_pixels: usize,
}

/// 在prepare原遍历内同时产生整体分类与精确区间摘要，后续规划不再读取RGBA页。
fn classify(
    allocation: &PixelAllocation,
    width: usize,
    height: usize,
    row_bytes: usize,
) -> Result<PreparedClassification, PixelError> {
    let count = if height >= 4 {
        4
    } else if height >= 2 {
        2
    } else {
        1
    }; // 每区必须正高，2/3行沿原half边界，不制造空quarter或虚构first字段。
    let first: [u8; 4] = row(allocation, 0, width, row_bytes)[..4]
        .try_into()
        .unwrap();
    #[cfg(test)]
    let mut observed_pixels = 0;
    if first[3] != 255 {
        for y in 0..height {
            for pixel in row(allocation, y, width, row_bytes).as_chunks::<4>().0 {
                #[cfg(test)]
                {
                    observed_pixels += 1;
                    REGION_OBSERVATIONS.with(|count| count.set(count.get() + 1));
                }
                if *pixel != first {
                    return Ok(PreparedClassification {
                        plan: Plan::Rgba,
                        regions: None,
                        #[cfg(test)]
                        observed_pixels,
                    });
                }
            }
        }
        return Ok(PreparedClassification {
            plan: Plan::Solid(first),
            regions: None,
            #[cfg(test)]
            observed_pixels,
        }); // 首像素透明时只可能固色或原RGBA；保持原早退，不为不可能的RGB24建立色表或摘要。
    }
    let mut parts = std::array::from_fn(|_| None);
    for (index, target) in parts[..count].iter_mut().enumerate() {
        let start = height * index / count;
        let end = height * (index + 1) / count;
        let region_first = row(allocation, start, width, row_bytes)[..4]
            .try_into()
            .unwrap();
        let mut region = RegionScan::new(region_first);
        for y in start..end {
            for pixel in row(allocation, y, width, row_bytes).as_chunks::<4>().0 {
                region.observe(*pixel);
                #[cfg(test)]
                {
                    observed_pixels += 1;
                }
                if pixel[3] != 255 {
                    return Ok(PreparedClassification {
                        plan: Plan::Rgba,
                        regions: None,
                        #[cfg(test)]
                        observed_pixels,
                    }); // 原首像素alpha为255，此像素一旦透明必为非固色，沿原RGBA早退停止扫描。
                }
            }
        }
        *target = Some(region);
    }
    let get = |index: usize| parts[index].as_ref().ok_or(PixelError::InvalidGeometry);
    let whole = match count {
        4 => {
            let left = RegionScan::merge(get(0)?, get(1)?);
            let right = RegionScan::merge(get(2)?, get(3)?);
            RegionScan::merge(&left, &right)
        }
        2 => RegionScan::merge(get(0)?, get(1)?),
        _ => parts[0].take().ok_or(PixelError::InvalidGeometry)?,
    }; // 只合并真实首遇色集，整体颜色/alpha/solid优先级仍与原逐行分类相同。
    let plan = whole.into_plan();
    let regions =
        (matches!(plan, Plan::Rgb24) && count > 1).then(|| Box::new(RegionScans { count, parts }));
    Ok(PreparedClassification {
        plan,
        regions,
        #[cfg(test)]
        observed_pixels,
    }) // 其余整体表示立即丢弃固定摘要；RGB24仅短暂保纯metadata到本tile规划完成。
}

/// 只为实际可用的2/4分法建固定摘要；每个有效像素只进入一个非空区间。
#[cfg(test)]
fn scan_regions(original: &PreparedImage, count: usize) -> [Option<RegionScan>; 4] {
    let mut scans = std::array::from_fn(|_| None);
    for (index, target) in scans[..count].iter_mut().enumerate() {
        let first_row = original.height * index / count;
        let end = original.height * (index + 1) / count;
        *target = Some(RegionScan::scan(
            &original.original,
            original.width,
            end - first_row,
            original.row_bytes,
            original.first_row + first_row,
        ));
    }
    scans
}

/// 原context已真实归还、RGBA页已只读；允许Candidate在不借用像素slice时进行有限Native校准。
pub(super) struct PreparedImage {
    original: Arc<PixelAllocation>,
    pub(super) width: usize,
    pub(super) height: usize,
    row_bytes: usize,
    first_row: usize,
    plan: Plan,
    regions: Option<Box<RegionScans>>,
    #[cfg(test)]
    observed_pixels: usize,
}
impl PreparedImage {
    /// 投影只读已经归还context、实际mprotect后的原RGBA，不新增输出页或Native对象。
    /// 指纹只供有限旧tile预筛；真正复用仍须核格式、完整色表和逐个有效输出byte。
    pub(super) fn projection_fingerprint(&self) -> Result<Option<u64>, PixelError> {
        reuse::projection_fingerprint(self)
    }

    pub(super) fn projection_owner(&self) -> &Arc<NativeMarkdownViewportPixels> {
        &self.original.counters
    }

    pub(super) fn mask_request(&self) -> Option<MaskPaletteRequest> {
        match &self.plan {
            Plan::Indexed(palette) => MaskPaletteRequest::new(&palette.colors[..palette.count]),
            Plan::Rgba | Plan::Rgb24 | Plan::Solid(_) | Plan::Neutral => None,
        } // 原透明、固色和中性色走既有表示，不引入不必要的Native校准。
    }
}

fn rgb(pixel: &[u8]) -> u32 {
    (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2])
}

fn row(allocation: &PixelAllocation, y: usize, width: usize, row_bytes: usize) -> &[u8] {
    // SAFETY: prepare已验证原width/height/stride完整覆盖分配，所有真实context回调均已归还；
    // y只来自原行或私有连续分区的first_row+局部行，末行不越原height。借用只在纯像素循环内，
    // 不跨Native/current，不返回可写或持久裸指针，也不把局部子区当作独立文件/Source权限。
    unsafe {
        std::slice::from_raw_parts(
            allocation.address.as_ptr().cast::<u8>().add(y * row_bytes),
            width * 4,
        )
    }
}

/// 原编码与分配前比较共用同一个字节投影，不另写近似解码或放松alpha/色表资格。
/// source只含本行有效RGBA；调用者已由prepare证明大小，目标是同宽A8/索引有效行。
fn project_byte_row(
    source: &[u8],
    target: &mut [u8],
    palette: Option<&Palette>,
) -> Result<(), PixelError> {
    for (to, pixel) in target.iter_mut().zip(source.as_chunks::<4>().0) {
        *to = if let Some(palette) = palette {
            palette
                .locate(rgb(pixel))
                .map_err(|_| PixelError::Backend("原像素不属于已验证色表"))? as u8
        } else {
            255 - pixel[0] // 只有原完整opaque-neutral分类可调用无palette分支。
        };
    }
    Ok(())
}

/// RGB24仅省去原分类已证明恒为255的alpha，逐分量保留原sRGB字节。
fn project_rgb_row(source: &[u8], target: &mut [u8]) {
    for (to, pixel) in target
        .as_chunks_mut::<3>()
        .0
        .iter_mut()
        .zip(source.as_chunks::<4>().0)
    {
        to.copy_from_slice(&pixel[..3]);
    }
}

/// 单区域最多512列，一份1536byte栈行足够三种原编码；不建立完整压缩副本。
/// prepared仍保真实原RGBA与first_row，比较期间无Native、Source读取或可逃逸slice。
fn project_row(
    prepared: &PreparedImage,
    y: usize,
    target: &mut [u8; 512 * 3],
) -> Result<usize, PixelError> {
    let source = row(
        &prepared.original,
        prepared.first_row + y,
        prepared.width,
        prepared.row_bytes,
    );
    match &prepared.plan {
        Plan::Neutral => {
            project_byte_row(source, &mut target[..prepared.width], None)?;
            Ok(prepared.width)
        }
        Plan::Indexed(palette) => {
            project_byte_row(source, &mut target[..prepared.width], Some(palette))?;
            Ok(prepared.width)
        }
        Plan::Rgb24 => {
            let active = prepared.width * 3;
            project_rgb_row(source, &mut target[..active]);
            Ok(active)
        }
        _ => Err(PixelError::InvalidGeometry), // RGBA/固色/双mask仍走各自原路径。
    }
}

#[cfg(test)]
fn plan(allocation: &PixelAllocation, width: usize, height: usize, row_bytes: usize) -> Plan {
    plan_region(allocation, width, height, row_bytes, 0)
}

#[cfg(test)]
fn plan_region(
    allocation: &PixelAllocation,
    width: usize,
    height: usize,
    row_bytes: usize,
    first_row: usize,
) -> Plan {
    let first: [u8; 4] = row(allocation, first_row, width, row_bytes)[..4]
        .try_into()
        .unwrap();
    let mut solid = true;
    let mut palette = Palette::new();
    let mut opaque = first[3] == 255;
    let mut indexed = opaque;
    let mut neutral = indexed;
    // 最多512²个像素；只检查本瓦片，不能因压缩扫描离屏图像、正文或整篇文档。
    for y in 0..height {
        for pixel in row(allocation, first_row + y, width, row_bytes)
            .as_chunks::<4>()
            .0
        {
            solid &= *pixel == first;
            opaque &= pixel[3] == 255;
            neutral &= pixel[3] == 255 && pixel[0] == pixel[1] && pixel[1] == pixel[2];
            if indexed && (pixel[3] != 255 || !palette.insert(rgb(pixel))) {
                indexed = false;
            }
            if !solid && !opaque {
                return Plan::Rgba; // 已知存在透明且非固色时只有原RGBA合法，不再做无关色表工作。
            }
            // 第257色后indexed永久关闭，但不能提前授予RGB24；本次有限扫描继续证明所有后继alpha。
        }
    }
    if solid {
        Plan::Solid(first) // 仅完全相同的RGBA才能用1×1恒定图像，透明度也逐byte相同。
    } else if neutral {
        Plan::Neutral // 每个原像素均通过opaque-neutral门；不能因字体、主题或样式名称猜测资格。
    } else if indexed {
        Plan::Indexed(Box::new(palette))
    } else {
        Plan::Rgb24 // 只有完整扫描结束、所有alpha均255且原色超过256时，才能省略原恒定alpha通道。
    }
}

#[cfg(test)]
pub(super) fn finish(
    original: Arc<PixelAllocation>,
    width: usize,
    height: usize,
    row_bytes: usize,
) -> Result<EncodedImage, PixelError> {
    finish_prepared(prepare(original, width, height, row_bytes)?, None)
}

pub(super) fn prepare(
    original: Arc<PixelAllocation>,
    width: usize,
    height: usize,
    row_bytes: usize,
) -> Result<PreparedImage, PixelError> {
    if original.active_contexts.load(Ordering::Acquire) != 0 {
        return Err(PixelError::ContextRetained);
    }
    if width == 0
        || height == 0
        || width > 512
        || height > 512
        || width.checked_mul(4).is_none_or(|bytes| bytes > row_bytes)
        || row_bytes
            .checked_mul(height)
            .is_none_or(|bytes| bytes > original.length)
    {
        return Err(PixelError::InvalidGeometry);
    }
    let classification = classify(&original, width, height, row_bytes)?;
    paired::readonly(&original)?; // Native校准前先终结可写阶段，回调不能看到仍可变的原RGBA。
    Ok(PreparedImage {
        original,
        width,
        height,
        row_bytes,
        first_row: 0,
        plan: classification.plan,
        regions: classification.regions,
        #[cfg(test)]
        observed_pixels: classification.observed_pixels,
    })
}

#[cfg(test)]
pub(super) fn finish_prepared(
    prepared: PreparedImage,
    pairs: Option<&CertifiedMaskPairs>,
) -> Result<EncodedImage, PixelError> {
    finish_with_allocations(prepared, pairs, None, None)
}

/// 私有子区先申请全部实际输出页；封存阶段不再申请页，也不把局部区域误退成整块RGBA。
#[derive(Default)]
struct OutputAllocations([Option<Arc<PixelAllocation>>; 2]);

fn output_allocation(
    original: &Arc<PixelAllocation>,
    length: usize,
    allocations: &mut Option<OutputAllocations>,
    index: usize,
) -> Result<Arc<PixelAllocation>, PixelError> {
    let Some(allocations) = allocations else {
        return allocate(
            length,
            Arc::clone(&original.counters),
            Some(tiled::LIVE_PIXEL_BUDGET),
        ); // 单块原路径仍在同一真实页账中分配，只有BudgetExceeded才可保原RGBA。
    };
    let allocation = allocations.0[index]
        .take()
        .ok_or(PixelError::InvalidGeometry)?;
    if allocation.length != allocation_length(length)?
        || !Arc::ptr_eq(&allocation.counters, &original.counters)
        || allocation.active_contexts.load(Ordering::Acquire) != 0
        || allocation.provider_returned.load(Ordering::Acquire)
    {
        return Err(PixelError::InvalidGeometry);
    }
    Ok(allocation) // 这些私有新页尚无context/provider；只有对应原区的编码器可消费一次。
}

fn finish_with_allocations(
    prepared: PreparedImage,
    pairs: Option<&CertifiedMaskPairs>,
    mut allocations: Option<OutputAllocations>,
    prior: Option<&TileFrame>,
) -> Result<EncodedImage, PixelError> {
    let encoded = finish_prepared_inner(prepared, pairs, &mut allocations, prior)?;
    if allocations
        .as_ref()
        .is_some_and(|allocations| allocations.0.iter().any(Option::is_some))
    {
        return Err(PixelError::InvalidGeometry); // 多余输出页说明计划与实际格式不一致，不能隐藏未消费的claim。
    }
    if encoded.reused_image && encoded.storage.release_failed() {
        return Err(PixelError::Backend("等值候选像素页未能真实归还"));
    } // 内层返回后新编码的所有局部Arc均已Drop；复用不能吞真实munmap失败。
    Ok(encoded)
}

fn finish_prepared_inner(
    mut prepared: PreparedImage,
    pairs: Option<&CertifiedMaskPairs>,
    allocations: &mut Option<OutputAllocations>,
    prior: Option<&TileFrame>,
) -> Result<EncodedImage, PixelError> {
    drop(prepared.regions.take()); // 独立单块封存不需要分区摘要，进入CF构造前先归还这份纯metadata。
    if pairs.is_some() && !matches!(&prepared.plan, Plan::Indexed(_)) {
        return Err(PixelError::InvalidGeometry); // 透明、固色或中性原表示不能领用其他调色板的颜色票。
    }
    if allocations.is_none()
        && pairs.is_none()
        && let Some(reused) = prior
            .map(|prior| prior.reuse_projected_image(&prepared))
            .transpose()?
            .flatten()
    {
        // 原glyph和分类已经完整完成；这里只省去随后会立即销毁的压缩输出页。
        // 分区的全页预扣和双mask原Native证书/回退顺序完全保留，不在这里消费它们。
        drop(prepared);
        if reused.storage.release_failed() {
            return Err(PixelError::Backend("投影复用后的原RGBA页未能真实归还"));
        }
        return Ok(reused);
    }
    if let (Some(pairs), Plan::Indexed(palette)) = (pairs, &prepared.plan) {
        let request = MaskPaletteRequest::new(&palette.colors[..palette.count])
            .ok_or(PixelError::InvalidGeometry)?;
        if !pairs.permits(&request) {
            return Err(PixelError::InvalidGeometry); // 外调色板的颜色票不能签本瓦片，即使端点相同。
        }
        if let Some(encoded) = encode_paired(&prepared, pairs, allocations, prior)? {
            drop(prepared);
            if encoded.storage.release_failed() {
                return Err(PixelError::Backend("双遮罩原RGBA未能真实归还"));
            }
            return Ok(encoded);
        } // 只有额外像素页预算不足允许保原Indexed/RGBA；真实释放/构造失败仍返回原错误。
    }
    let PreparedImage {
        original,
        width,
        height,
        row_bytes,
        first_row,
        plan,
        regions: _,
        #[cfg(test)]
            observed_pixels: _,
    } = prepared;
    let (image_width, image_height, encoded_row, palette, solid, presentation) = match plan {
        Plan::Rgba if first_row == 0 => return rgba(original, width, height, row_bytes, prior),
        Plan::Rgba => return Err(PixelError::InvalidGeometry),
        Plan::Rgb24 => {
            return rgb24(
                original,
                width,
                height,
                row_bytes,
                first_row,
                allocations,
                prior,
            );
        }
        Plan::Solid(value) => (1, 1, 4, None, Some(value), PixelPresentation::Image),
        Plan::Neutral => (
            width,
            height,
            (width + 15) & !15,
            None,
            None,
            PixelPresentation::OpaqueNeutralMask,
        ),
        Plan::Indexed(palette) => (
            width,
            height,
            (width + 15) & !15,
            Some(palette),
            None,
            PixelPresentation::Image,
        ),
    };
    let length = encoded_row
        .checked_mul(image_height)
        .ok_or(PixelError::InvalidGeometry)?;
    // 临时新表示也计入当前、旧provider和候选共同32MiB账；优化不能绕开实际存活页预算。
    let allocation = match output_allocation(&original, length, allocations, 0) {
        Ok(value) => value,
        Err(PixelError::BudgetExceeded) if allocations.is_none() && first_row == 0 => {
            return rgba(original, width, height, row_bytes, prior);
        }
        Err(error) => return Err(error), // 真分配/清理错误不洗成成功或换来源重试。
    };
    if let Some(value) = solid {
        // SAFETY: 新私有映射至少覆盖4byte，尚无context/provider读者；只写这个恒定RGBA像素。
        unsafe {
            std::ptr::copy_nonoverlapping(
                value.as_ptr(),
                allocation.address.as_ptr().cast::<u8>(),
                4,
            )
        };
    } else if let Some(palette) = &palette {
        for y in 0..height {
            // SAFETY: 新映射覆盖encoded_row*height，width<=encoded_row；每行只写有效索引字节。
            let target = unsafe {
                std::slice::from_raw_parts_mut(
                    allocation
                        .address
                        .as_ptr()
                        .cast::<u8>()
                        .add(y * encoded_row),
                    width,
                )
            };
            project_byte_row(
                row(&original, first_row + y, width, row_bytes),
                target,
                Some(palette),
            )?;
        }
    } else {
        // 只有Neutral分支没有palette/solid；逐byte反相原灰值，不量化、不改gamma或清晰度。
        for y in 0..height {
            // SAFETY: 新独占映射至少encoded_row*height，width<=encoded_row；只写本行有效A8像素。
            let target = unsafe {
                std::slice::from_raw_parts_mut(
                    allocation
                        .address
                        .as_ptr()
                        .cast::<u8>()
                        .add(y * encoded_row),
                    width,
                )
            };
            project_byte_row(
                row(&original, first_row + y, width, row_bytes),
                target,
                None,
            )?;
        }
    }
    // 本区的目标字节已经完整写入独立映射，后续颜色空间/provider/image只消费新页。
    // 先归还不再读取的原RGBA，避免其最后强边跨Native封存保活；分区若仍需后续行，
    // 原计划自己的Arc继续保留同一输入页，不能在这里声称它已销毁或改写实际页账。
    drop(original);
    if allocation
        .counters
        .pixel_release_failed
        .load(Ordering::Acquire)
    {
        return Err(PixelError::Backend("原RGBA像素页未能真实归还"));
    } // 实际释放失败先终止，不创建新Native图掩盖原资源首错或重放已经完成的像素编码。
    // 先把本次实际编码页密封并记录格式；只有真byte相等才可借原图，不能先建一份
    // 新provider/索引色空间/CGImage再立即扔掉。该比较不改变上面的格式选择和像素生成。
    let mut colors = [0u8; 768];
    let format = if presentation == PixelPresentation::OpaqueNeutralMask {
        Format::NeutralMask
    } else if let Some(palette) = &palette {
        for (out, color) in colors
            .as_chunks_mut::<3>()
            .0
            .iter_mut()
            .zip(&palette.colors[..palette.count])
        {
            *out = [(color >> 16) as u8, (color >> 8) as u8, *color as u8];
        }
        Format::Indexed(colors[..palette.count * 3].into())
    } else {
        Format::Rgba
    };
    let storage = SealedPixels::new(
        Arc::clone(&allocation),
        image_width,
        image_height,
        encoded_row,
        format,
    )?;
    if let Some(reused) =
        prior.and_then(|prior| prior.reuse_image(width, height, presentation, &storage, None))
    {
        return Ok(reused); // 新页的局部Arc在返回前真实归还；外层继续检查同页账的release首错。
    }
    let image = if presentation == PixelPresentation::OpaqueNeutralMask {
        seal_neutral_mask(&allocation, image_width, image_height, encoded_row)?
    } else {
        // SAFETY: sRGB常量静态有效；色表只存精确RGB，不切换灰阶gamma或以近似色换内存。
        let base = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
            .ok_or(PixelError::Backend("无法建立原sRGB空间"))?;
        let (color, bits, info) = if let Some(palette) = palette {
            // SAFETY: 有限色数2..=256，表在同步构造期完整覆盖3*count byte；CoreGraphics复制原表。
            let indexed = unsafe {
                CGColorSpace::new_indexed(Some(&base), palette.count - 1, colors.as_ptr())
            }
            .ok_or(PixelError::Backend("无法建立精确索引色表"))?;
            (indexed, 8, CGBitmapInfo(CGImageAlphaInfo::None.0))
        } else {
            (base, 32, rgba_info())
        };
        seal_format(
            &allocation,
            ImageFormat {
                width: image_width,
                height: image_height,
                row_bytes: encoded_row,
                pixel_bits: bits,
                color: &color,
                info,
            },
        )?
    };
    // Native封存期间其它原provider仍可真正退还；保留原出口检查，不让前置释放检查替代后置首错。
    if allocation
        .counters
        .pixel_release_failed
        .load(Ordering::Acquire)
    {
        return Err(PixelError::Backend("原RGBA像素页未能真实归还"));
    }
    Ok(EncodedImage {
        image,
        secondary_image: None,
        presentation,
        storage,
        secondary_storage: None,
        reused_image: false,
        #[cfg(test)]
        allocation,
        #[cfg(test)]
        row_bytes: encoded_row,
    })
}

fn rgba(
    allocation: Arc<PixelAllocation>,
    width: usize,
    height: usize,
    row_bytes: usize,
    prior: Option<&TileFrame>,
) -> Result<EncodedImage, PixelError> {
    let storage = SealedPixels::new(
        Arc::clone(&allocation),
        width,
        height,
        row_bytes,
        Format::Rgba,
    )?;
    if let Some(reused) = prior.and_then(|prior| {
        prior.reuse_image(width, height, PixelPresentation::Image, &storage, None)
    }) {
        return Ok(reused);
    }
    let image = seal(&allocation, width, height, row_bytes)?;
    Ok(EncodedImage {
        image,
        secondary_image: None,
        presentation: PixelPresentation::Image,
        storage,
        secondary_storage: None,
        reused_image: false,
        #[cfg(test)]
        allocation,
        #[cfg(test)]
        row_bytes,
    })
}

/// 完整opaque证明后的多色瓦片按原RGB逐byte封存；不改sRGB、不量化、不创建整帧副本。
fn rgb24(
    original: Arc<PixelAllocation>,
    width: usize,
    height: usize,
    row_bytes: usize,
    first_row: usize,
    allocations: &mut Option<OutputAllocations>,
    prior: Option<&TileFrame>,
) -> Result<EncodedImage, PixelError> {
    let encoded_row = width
        .checked_mul(3)
        .and_then(|bytes| bytes.checked_add(15))
        .map(|bytes| bytes & !15)
        .ok_or(PixelError::InvalidGeometry)?;
    let length = encoded_row
        .checked_mul(height)
        .ok_or(PixelError::InvalidGeometry)?;
    let allocation = match output_allocation(&original, length, allocations, 0) {
        Ok(value) => value,
        Err(PixelError::BudgetExceeded) if allocations.is_none() && first_row == 0 => {
            return rgba(original, width, height, row_bytes, prior);
        }
        Err(error) => return Err(error), // 可选压缩仅允许额外页不足回退，真实映射故障不能洗成成功。
    };
    for y in 0..height {
        // SAFETY: 新映射独占且覆盖encoded_row*height，尚无context/provider；有效RGB行恰好width*3。
        let target = unsafe {
            std::slice::from_raw_parts_mut(
                allocation
                    .address
                    .as_ptr()
                    .cast::<u8>()
                    .add(y * encoded_row),
                width * 3,
            )
        };
        project_rgb_row(row(&original, first_row + y, width, row_bytes), target);
    }
    // RGB三通道已逐byte复制完成，颜色空间和CGImage不再访问原RGBA。
    // 仅提前释放这一个有限工作格的原Arc；原32MiB预算及真实munmap收据仍由分配owner维护。
    drop(original);
    if allocation
        .counters
        .pixel_release_failed
        .load(Ordering::Acquire)
    {
        return Err(PixelError::Backend("原RGB24编码后的RGBA页未能真实归还"));
    }
    let storage = SealedPixels::new(
        Arc::clone(&allocation),
        width,
        height,
        encoded_row,
        Format::Rgb24,
    )?;
    if let Some(reused) = prior.and_then(|prior| {
        prior.reuse_image(width, height, PixelPresentation::Image, &storage, None)
    }) {
        return Ok(reused);
    }
    // SAFETY: 原sRGB常量静态有效，RGB24与原RGBA使用完全相同的真实颜色空间。
    let color = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or(PixelError::Backend("无法建立原RGB24的sRGB空间"))?;
    let image = seal_format(
        &allocation,
        ImageFormat {
            width,
            height,
            row_bytes: encoded_row,
            pixel_bits: 24,
            color: &color,
            info: CGBitmapInfo(CGImageAlphaInfo::None.0),
        },
    )?;
    // 与原实现相同，封存后的归还首错仍须核验；前移原RGBA只缩短所有权，不缩减资源门槛。
    if storage.release_failed() {
        return Err(PixelError::Backend("原RGB24编码后的RGBA页未能真实归还"));
    }
    Ok(EncodedImage {
        image,
        secondary_image: None,
        presentation: PixelPresentation::Image,
        storage,
        secondary_storage: None,
        reused_image: false,
        #[cfg(test)]
        allocation,
        #[cfg(test)]
        row_bytes: encoded_row,
    })
}

/// 两个独立provider各有一次callback义务；不能让两个图像共享同一分配的一次归还标志。
fn encode_paired(
    prepared: &PreparedImage,
    pairs: &CertifiedMaskPairs,
    allocations: &mut Option<OutputAllocations>,
    prior: Option<&TileFrame>,
) -> Result<Option<EncodedImage>, PixelError> {
    // 两plane读取同一个已完成原区；借实际PreparedImage，避免拆散其唯一映射与行范围事实。
    let original = &prepared.original;
    let (width, height, row_bytes, first_row) = (
        prepared.width,
        prepared.height,
        prepared.row_bytes,
        prepared.first_row,
    );
    let encoded_row = (width + 15) & !15;
    let length = encoded_row
        .checked_mul(height)
        .ok_or(PixelError::InvalidGeometry)?;
    let first = match output_allocation(original, length, allocations, 0) {
        Ok(value) => value,
        Err(PixelError::BudgetExceeded) => return Ok(None),
        Err(error) => return Err(error),
    };
    let second = match output_allocation(original, length, allocations, 1) {
        Ok(value) => value,
        Err(PixelError::BudgetExceeded) => {
            drop(first);
            if original
                .counters
                .pixel_release_failed
                .load(Ordering::Acquire)
            {
                return Err(PixelError::Backend("双遮罩预算回退未能归还原临时页"));
            }
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    for y in 0..height {
        // SAFETY: 两个新独占映射各自覆盖encoded_row*height，尚无context/provider；原RGBA只读。
        let (left, right) = unsafe {
            (
                std::slice::from_raw_parts_mut(
                    first.address.as_ptr().cast::<u8>().add(y * encoded_row),
                    width,
                ),
                std::slice::from_raw_parts_mut(
                    second.address.as_ptr().cast::<u8>().add(y * encoded_row),
                    width,
                ),
            )
        };
        for ((a, b), pixel) in left.iter_mut().zip(right).zip(
            row(original, first_row + y, width, row_bytes)
                .as_chunks::<4>()
                .0,
        ) {
            let [first_alpha, second_alpha] = pairs
                .pair(rgb(pixel))
                .ok_or(PixelError::Backend("原像素不属于实际双遮罩校准表"))?;
            *a = first_alpha;
            *b = second_alpha;
        }
    }
    let storage = SealedPixels::new(
        Arc::clone(&first),
        width,
        height,
        encoded_row,
        Format::PairedMask(pairs.blend()),
    )?;
    let secondary_storage = SealedPixels::new(
        Arc::clone(&second),
        width,
        height,
        encoded_row,
        Format::PairedMask(pairs.blend()),
    )?;
    let presentation = pairs.presentation();
    if let Some(reused) = prior.and_then(|prior| {
        prior.reuse_image(
            width,
            height,
            presentation,
            &storage,
            Some(&secondary_storage),
        )
    }) {
        return Ok(Some(reused)); // 必须两plane与端点/blend都相同，不能只复用第一张mask。
    }
    let image = seal_neutral_mask(&first, width, height, encoded_row)?;
    let secondary_image = seal_neutral_mask(&second, width, height, encoded_row)?;
    Ok(Some(EncodedImage {
        image,
        secondary_image: Some(secondary_image),
        presentation,
        storage,
        secondary_storage: Some(secondary_storage),
        reused_image: false,
        #[cfg(test)]
        allocation: first,
        #[cfg(test)]
        row_bytes: encoded_row,
    }))
}
