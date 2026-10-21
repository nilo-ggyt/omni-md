//! 活帧内的封存像素比较；哈希只预筛，最终身份替换必须逐格式、色表及有效字节相等。

use super::super::*;
use super::{Plan, PreparedImage, project_row};

const FINGERPRINT_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FINGERPRINT_PRIME: u64 = 0x100_0000_01b3;

/// 新RGBA已实际只读，按原编码核每个有效byte；只保一行，不申请压缩映射。
/// miss多一次有限行投影，但不增加任何Source读取、glyph或Native调用配额。
pub(super) fn projection_fingerprint(prepared: &PreparedImage) -> Result<Option<u64>, PixelError> {
    // 投影只借固定瓦片行空间；越界不切片，由原编码路径负责原有结果与错误。
    if prepared.width > 512
        || prepared.height > 512
        || !matches!(
            &prepared.plan,
            Plan::Neutral | Plan::Indexed(_) | Plan::Rgb24
        )
    {
        return Ok(None);
    }
    let mut line = [0; 512 * 3];
    let mut fingerprint = FINGERPRINT_OFFSET;
    for y in 0..prepared.height {
        let active = project_row(prepared, y, &mut line)?;
        for &byte in &line[..active] {
            fingerprint = (fingerprint ^ u64::from(byte)).wrapping_mul(FINGERPRINT_PRIME);
        }
    }
    Ok(Some(fingerprint))
}

/// 构造参数与原CGImage对应；配对mask还记录Native解释模式，禁止同bytes跨合成语义复用。
#[derive(Clone, Eq, PartialEq)]
pub(in crate::native_surface::viewport_pixels) enum Format {
    Rgba,
    Rgb24,
    Indexed(Box<[u8]>),
    NeutralMask,
    PairedMask(MaskPairBlend),
}

/// 仅保原provider已持的同一只读映射；不复制像素、解码图像、保Scene或建立跨会话缓存。
#[derive(Clone)]
pub(in crate::native_surface::viewport_pixels) struct SealedPixels {
    allocation: Arc<PixelAllocation>,
    width: usize,
    height: usize,
    row_bytes: usize,
    format: Format,
    fingerprint: u64,
}

impl SealedPixels {
    /// 只接受已结束context的实际编码页；先真实改为只读，再记录本次编码的格式和有效字节。
    /// 此事实尚不代表CGImage已创建，也不授予Source/Scene权限；允许在建provider前比较原活帧。
    pub(super) fn new(
        allocation: Arc<PixelAllocation>,
        width: usize,
        height: usize,
        row_bytes: usize,
        format: Format,
    ) -> Result<Self, PixelError> {
        let bytes_per_pixel = match &format {
            Format::Rgba => 4,
            Format::Rgb24 => 3,
            Format::Indexed(table)
                if !table.is_empty() && table.len() <= 768 && table.len().is_multiple_of(3) =>
            {
                1
            }
            Format::Indexed(_) => return Err(PixelError::InvalidGeometry),
            Format::NeutralMask | Format::PairedMask(_) => 1,
        };
        if allocation.active_contexts.load(Ordering::Acquire) != 0 {
            return Err(PixelError::ContextRetained); // 不能以未完成绘图的可写页签发可比较只读视图。
        }
        if width == 0
            || height == 0
            || width > 512
            || height > 512
            || width
                .checked_mul(bytes_per_pixel)
                .is_none_or(|active| active > row_bytes)
            || row_bytes
                .checked_mul(height)
                .is_none_or(|length| length > allocation.length)
        {
            return Err(PixelError::InvalidGeometry);
        }
        super::super::paired::readonly(&allocation)?;
        let mut result = Self {
            allocation,
            width,
            height,
            row_bytes,
            format,
            fingerprint: 0,
        };
        // 只对同一真实封存映射有效像素作一次有限哈希；padding不参与内容判断，仍完整计入实际页账。
        let mut fingerprint = FINGERPRINT_OFFSET;
        result.visit(|bytes| {
            for byte in bytes {
                fingerprint = (fingerprint ^ u64::from(*byte)).wrapping_mul(FINGERPRINT_PRIME);
            }
        });
        result.fingerprint = fingerprint;
        Ok(result)
    }

    /// 旧页只来自SealedPixels::new的真实只读映射，Arc保活整个逐行比较。
    /// 哈希只跳过不可能的候选；命中还必须同stride/格式/完整色表及每个有效byte相等。
    pub(in crate::native_surface::viewport_pixels) fn matches_projection(
        &self,
        prepared: &PreparedImage,
        presentation: PixelPresentation,
        fingerprint: u64,
    ) -> Result<bool, PixelError> {
        let (row_bytes, same_format) = match (&self.format, &prepared.plan) {
            (Format::NeutralMask, Plan::Neutral) => (
                (prepared.width + 15) & !15,
                presentation == PixelPresentation::OpaqueNeutralMask,
            ),
            (Format::Indexed(table), Plan::Indexed(palette)) => (
                (prepared.width + 15) & !15,
                presentation == PixelPresentation::Image
                    && table.len() == palette.count * 3
                    && table
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .zip(&palette.colors[..palette.count])
                        .all(|(bytes, color)| {
                            *bytes == [(color >> 16) as u8, (color >> 8) as u8, *color as u8]
                        }),
            ),
            (Format::Rgb24, Plan::Rgb24) => (
                (prepared.width * 3 + 15) & !15,
                presentation == PixelPresentation::Image,
            ),
            _ => return Ok(false),
        };
        if !same_format
            || self.width != prepared.width
            || self.height != prepared.height
            || self.row_bytes != row_bytes
            || self.fingerprint != fingerprint
        {
            return Ok(false);
        }
        let mut line = [0; 512 * 3];
        for y in 0..prepared.height {
            let active = project_row(prepared, y, &mut line)?;
            // SAFETY: 本SealedPixels构造时已核width/height/stride完整覆盖，且未再开放写入。
            // 只比较本行有效编码字节，padding不伪装像素，页账仍保全部实际舍入长度。
            let previous = unsafe {
                std::slice::from_raw_parts(
                    self.allocation
                        .address
                        .as_ptr()
                        .cast::<u8>()
                        .add(y * self.row_bytes),
                    active,
                )
            };
            if &line[..active] != previous {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// 返回true只表示两个不可变编码相等；不能签当前Source、Scene、几何、Native关系或Ready。
    pub(in crate::native_surface::viewport_pixels) fn identical(&self, other: &Self) -> bool {
        if self.width != other.width
            || self.height != other.height
            || self.row_bytes != other.row_bytes
            || self.format != other.format
            || self.fingerprint != other.fingerprint
        {
            return false; // 哈希相同仍没有通过，任何尺寸、stride、色表、alpha或decode表示差异先拒绝。
        }
        let active = self.width * self.bytes_per_pixel();
        for y in 0..self.height {
            // SAFETY: 两个SealedPixels都来自真实seal，Arc保同一只读映射；new已核每行覆盖，
            // 纯循环内无Native/current回调，也没有可写或逃逸slice。
            let (left, right) = unsafe {
                (
                    std::slice::from_raw_parts(
                        self.allocation
                            .address
                            .as_ptr()
                            .cast::<u8>()
                            .add(y * self.row_bytes),
                        active,
                    ),
                    std::slice::from_raw_parts(
                        other
                            .allocation
                            .address
                            .as_ptr()
                            .cast::<u8>()
                            .add(y * other.row_bytes),
                        active,
                    ),
                )
            };
            if left != right {
                return false; // 哈希碰撞不能授权复用；任一真实有效byte不同都保新图像。
            }
        }
        true
    }

    pub(in crate::native_surface::viewport_pixels) fn belongs_to(
        &self,
        counters: &Arc<NativeMarkdownViewportPixels>,
    ) -> bool {
        Arc::ptr_eq(&self.allocation.counters, counters)
    }

    /// 仅核同一实际页账；旧帧全部plane都须属于本候选，不能靠相等字节借外会话provider。
    pub(in crate::native_surface::viewport_pixels) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.allocation.counters, &other.allocation.counters)
    }

    pub(in crate::native_surface::viewport_pixels) fn release_failed(&self) -> bool {
        self.allocation
            .counters
            .pixel_release_failed
            .load(Ordering::Acquire)
    }

    fn bytes_per_pixel(&self) -> usize {
        match self.format {
            Format::Rgba => 4,
            Format::Rgb24 => 3,
            Format::Indexed(_) | Format::NeutralMask | Format::PairedMask(_) => 1,
        }
    }

    fn visit(&self, mut visitor: impl FnMut(&[u8])) {
        let active = self.width * self.bytes_per_pixel();
        for y in 0..self.height {
            // SAFETY: 同上，映射已封存且被本Arc保活；一次只交付本行有效字节，visitor不获所有权。
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    self.allocation
                        .address
                        .as_ptr()
                        .cast::<u8>()
                        .add(y * self.row_bytes),
                    active,
                )
            };
            visitor(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealed_mask(
        resources: &Arc<NativeMarkdownViewportPixels>,
        blend: MaskPairBlend,
        first_byte: u8,
    ) -> (CFRetained<CGImage>, SealedPixels) {
        let allocation = allocate(32 * 32, Arc::clone(resources), None).unwrap();
        // SAFETY: 新独占映射覆盖32²A8，尚无context/provider；直接生成有限非恒定mask源。
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(allocation.address.as_ptr().cast::<u8>(), 32 * 32)
        };
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (index & 255) as u8;
        }
        bytes[0] = first_byte;
        let image = seal_neutral_mask(&allocation, 32, 32, 32).unwrap();
        let storage = SealedPixels::new(allocation, 32, 32, 32, Format::PairedMask(blend)).unwrap();
        (image, storage) // 只构造真实CGImage存储比较对象，不伪造校准票、SealedTile或Ready。
    }

    #[test]
    fn identical_actual_mask_bytes_do_not_cross_native_blend_interpretations() {
        for left_blend in [
            MaskPairBlend::ForegroundThenForeground,
            MaskPairBlend::ForegroundThenBackground,
        ] {
            for right_blend in [
                MaskPairBlend::ForegroundThenForeground,
                MaskPairBlend::ForegroundThenBackground,
            ] {
                let resources = Arc::new(NativeMarkdownViewportPixels::default());
                let observer = resources.observer();
                let (left_image, left) = sealed_mask(&resources, left_blend, 0);
                let (right_image, right) = sealed_mask(&resources, right_blend, 0);
                assert!(
                    CGImage::is_mask(Some(&left_image)) && CGImage::is_mask(Some(&right_image))
                );
                assert_eq!(left.fingerprint, right.fingerprint);
                assert_eq!(left.identical(&right), left_blend == right_blend);
                drop((left_image, right_image));
                assert_eq!(observer.snapshot().live_pixel_buffers, 2); // 存储还实际持原映射，不能因provider归还就记零。
                drop((left, right));
                assert!(observer.is_released());
            }
        }
    }

    #[test]
    fn a_paired_mask_byte_change_rejects_a_forced_digest_collision_in_both_blends() {
        for blend in [
            MaskPairBlend::ForegroundThenForeground,
            MaskPairBlend::ForegroundThenBackground,
        ] {
            let resources = Arc::new(NativeMarkdownViewportPixels::default());
            let observer = resources.observer();
            let (left_image, left) = sealed_mask(&resources, blend, 0);
            let (right_image, mut right) = sealed_mask(&resources, blend, 1);
            right.fingerprint = left.fingerprint; // 只在测试制造hash碰撞，两个真实只读映射仍有一个有效byte不同。
            assert!(!left.identical(&right));
            drop((left_image, right_image, left, right));
            assert!(observer.is_released());
        }
    }

    fn encoded(
        resources: &Arc<NativeMarkdownViewportPixels>,
        palette: bool,
        delta: u8,
    ) -> super::super::EncodedImage {
        let row = 32 * 4;
        let allocation = allocate(row * 32, Arc::clone(resources), None).unwrap();
        for y in 0..32 {
            // SAFETY: 测试刚取得唯一私有可写分配，未创建任何context或provider；写完后走真实encoding seal。
            let pixels = unsafe {
                std::slice::from_raw_parts_mut(
                    allocation.address.as_ptr().cast::<u8>().add(y * row),
                    row,
                )
            };
            for (x, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                if palette {
                    let value = (x + y) as u8;
                    *pixel = [value, 255 - value, value / 2 + delta, 255];
                } else {
                    *pixel = [
                        x as u8,
                        y as u8,
                        0,
                        if x == 0 && y == 0 { 254 - delta } else { 255 },
                    ];
                }
            }
        }
        super::super::finish(allocation, 32, 32, row).unwrap()
    }

    #[test]
    fn real_encoded_byte_difference_rejects_even_for_a_forced_hash_collision() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let left = encoded(&resources, false, 0);
        let mut right = encoded(&resources, false, 1);
        assert!(matches!(left.storage.format, Format::Rgba));
        assert!(matches!(right.storage.format, Format::Rgba));
        right.storage.fingerprint = left.storage.fingerprint; // 仅私有测试制造碰撞，真实只读像素仍保持不同。
        assert!(!left.storage.identical(&right.storage));
        drop((left, right));
        assert!(observer.is_released());
    }

    #[test]
    fn equal_real_index_bytes_do_not_hide_a_changed_actual_srgb_palette() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let left = encoded(&resources, true, 0);
        let right = encoded(&resources, true, 1);
        assert!(matches!(left.storage.format, Format::Indexed(_)));
        assert!(matches!(right.storage.format, Format::Indexed(_)));
        assert_eq!(left.storage.fingerprint, right.storage.fingerprint);
        assert!(!left.storage.identical(&right.storage));
        drop((left, right));
        assert!(observer.is_released());
    }

    fn encoded_rgb24(
        resources: &Arc<NativeMarkdownViewportPixels>,
        delta: u8,
    ) -> super::super::EncodedImage {
        let row = 32 * 4;
        let allocation = allocate(row * 32, Arc::clone(resources), None).unwrap();
        // SAFETY: 新独占测试映射覆盖32²RGBA，尚无context或provider；逐像素写入超过256个opaque颜色。
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(allocation.address.as_ptr().cast::<u8>(), row * 32)
        };
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *pixel = [(index % 32) as u8, (index / 32) as u8, 0, 255];
        }
        pixels[row * 32 - 2] = delta; // 最后一个有效蓝色byte变化，必须进入RGB24的实际行比较。
        super::super::finish(allocation, 32, 32, row).unwrap()
    }

    #[test]
    fn rgb24_last_byte_difference_rejects_a_forced_hash_collision() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let left = encoded_rgb24(&resources, 0);
        let mut right = encoded_rgb24(&resources, 1);
        assert!(matches!(left.storage.format, Format::Rgb24));
        assert!(matches!(right.storage.format, Format::Rgb24));
        assert_eq!(CGImage::bits_per_pixel(Some(&left.image)), 24);
        right.storage.fingerprint = left.storage.fingerprint; // 私有测试仅强制哈希碰撞，不修改原只读存储。
        assert!(!left.storage.identical(&right.storage));
        drop((left, right));
        assert!(observer.is_released());
    }

    #[test]
    fn rgb24_and_rgba_cannot_cross_reuse_even_with_a_forced_equal_hash() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let rgb24 = encoded_rgb24(&resources, 0);
        let mut rgba = encoded(&resources, false, 0);
        assert!(matches!(rgb24.storage.format, Format::Rgb24));
        assert!(matches!(rgba.storage.format, Format::Rgba));
        rgba.storage.fingerprint = rgb24.storage.fingerprint;
        assert!(!rgb24.storage.identical(&rgba.storage));
        assert!(!rgba.storage.identical(&rgb24.storage));
        drop((rgb24, rgba));
        assert!(observer.is_released());
    }
}
