//! 每个有限瓦片独立拥有只读像素；完整候选只交给图层组，不合成为整张CGImage。

use super::*;
use std::rc::Rc;

// 当前、候选及框架尚未归还的provider共用此实际页预算；不等于整产品内存已经通过。
pub(in crate::native_surface) const LIVE_PIXEL_BUDGET: usize = 32 * 1024 * 1024;
pub(in crate::native_surface) const MAX_TILE_SLOTS: usize = 64;
const TILE_EDGE: usize = 512;

/// y为provider自顶向下的像素行；bottom是原Y-up绘图和图层定位的真实下边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native_surface) struct PixelTile {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub bottom: usize,
}

/// 两个前景的有限合成模式；每种都须原生校准全部原色，模式自身不授予像素资格。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native_surface) enum MaskPairBlend {
    ForegroundThenForeground,
    ForegroundThenBackground,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 由封存器证明的实际像素表示；不能由Scene、裸图像或外部样式签发遮罩资格。
pub(in crate::native_surface) enum PixelPresentation {
    Image,
    /// 原RGBA全部alpha=255且R=G=B；只允许瓦片自己的白底、黑前景与A8遮罩合成。
    OpaqueNeutralMask,
    /// 原有限RGB均有实际原生精确alpha对；两端点只是字节值，不由主题名称授予资格。
    OpaquePairedMasks {
        background: [u8; 3],
        foreground: [u8; 3],
        blend: MaskPairBlend,
    },
}

/// 已结束实际context并封存的单块图像；没有整帧像素副本。
pub(in crate::native_surface) struct SealedTile {
    pub tile: PixelTile,
    pub image: CFRetained<CGImage>,
    pub secondary_image: Option<CFRetained<CGImage>>,
    pub presentation: PixelPresentation,
    storage: encoding::SealedPixels,
    secondary_storage: Option<encoding::SealedPixels>,
    #[cfg(test)]
    allocation: Arc<PixelAllocation>,
    #[cfg(test)]
    row_bytes: usize,
}

/// 只有全部瓦片成功才存在的候选；元数据上界固定，不保存正文或Scene。
pub(in crate::native_surface) struct TileFrame {
    pub width: usize,
    pub height: usize,
    pub tiles: Vec<SealedTile>,
    pub reused_images: usize,
}

impl TileFrame {
    /// 只在本次已认证活帧里借图；新绘制和编码已完成，两个plane均已真实只读。
    /// 先前在seal_region中才判等，会先创建随后立刻扔掉的provider/CGImage及索引色空间。
    /// 这里仍逐格式、尺寸、stride、端点解释和有效byte核对，不凭hash或主题猜相同画面。
    pub(super) fn reuse_image(
        &self,
        width: usize,
        height: usize,
        presentation: PixelPresentation,
        storage: &encoding::SealedPixels,
        secondary: Option<&encoding::SealedPixels>,
    ) -> Option<encoding::EncodedImage> {
        if self.tiles.len() > MAX_TILE_SLOTS
            || self.tiles.iter().any(|tile| {
                !tile.storage.same_owner(storage)
                    || tile
                        .secondary_storage
                        .as_ref()
                        .is_some_and(|other| !other.same_owner(storage))
            })
        {
            // 非法原帧不作为优化命中；后面的原seal_region仍在同一位置返回其真实错误。
            return None;
        }
        let original = self.tiles.iter().find(|original| {
            original.tile.width == width
                && original.tile.height == height
                && original.presentation == presentation
                && original.storage.identical(storage)
                && match (&original.secondary_storage, secondary) {
                    (None, None) => true,
                    (Some(left), Some(right)) => left.identical(right),
                    _ => false,
                }
        })?;
        // 只克隆原实际图和同一provider页；新编码页仍由调用者局部Arc归还，既不保新旧副本，
        // 也不继承旧坐标、Ready或完整Input。父CPU发布仍按新Scene核原Native图和原子提交。
        Some(encoding::EncodedImage {
            image: original.image.clone(),
            secondary_image: original.secondary_image.clone(),
            presentation,
            storage: original.storage.clone(),
            secondary_storage: original.secondary_storage.clone(),
            reused_image: true,
            #[cfg(test)]
            allocation: Arc::clone(&original.allocation),
            #[cfg(test)]
            row_bytes: original.row_bytes,
        })
    }
}

/// 只有有限实际mmap与context强边；Drop可在取消时归还，不拥有Scene或正文。
pub(in crate::native_surface) struct WritableTile {
    context: Option<CFRetained<CGContext>>,
    allocation: Arc<PixelAllocation>,
    identity: Rc<()>,
    pub tile: PixelTile,
    row_bytes: usize,
}
impl WritableTile {
    pub(in crate::native_surface) fn context(&self) -> &CGContext {
        self.context.as_deref().expect("可写tile仍持有原context")
    }
    pub(in crate::native_surface) fn context_claim(&self) -> CFRetained<CGContext> {
        self.context
            .as_ref()
            .expect("可写tile仍持有原context")
            .clone()
    }
}

/// 真实context已终结但尚未编码；Candidate可在不借用任何像素slice时校准此有限调色板。
pub(in crate::native_surface) struct PreparedTile {
    pixels: encoding::PreparedImage,
    tile: PixelTile,
    identity: Rc<()>,
}
impl PreparedTile {
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(in crate::native_surface) fn mask_request(&self) -> Option<MaskPaletteRequest> {
        self.pixels.mask_request()
    }
}

/// 一个原绘图格的私有编码计划；最多四个行区间，仍只推进一次原grid游标。
pub(in crate::native_surface) struct PreparedTileEncodingPlan {
    pixels: encoding::PreparedRegions,
    tile: PixelTile,
    identity: Rc<()>,
}
impl PreparedTileEncodingPlan {
    pub(in crate::native_surface) fn region_count(&self) -> usize {
        self.pixels.count()
    }
    pub(in crate::native_surface) fn mask_request(
        &self,
        index: usize,
    ) -> Result<Option<MaskPaletteRequest>, PixelError> {
        self.pixels.mask_request(index)
    }
}

pub(in crate::native_surface) struct TiledBitmap {
    identity: Rc<()>,
    counters: Arc<NativeMarkdownViewportPixels>,
    width: usize,
    height: usize,
    next_x: usize,
    next_y: usize,
    tiles: Vec<SealedTile>,
    reused_images: usize,
}
impl TiledBitmap {
    pub(in crate::native_surface) fn new(
        width: usize,
        height: usize,
        counters: Arc<NativeMarkdownViewportPixels>,
    ) -> Result<Self, PixelError> {
        if width == 0 || height == 0 {
            return Err(PixelError::InvalidGeometry);
        }
        let columns = width.div_ceil(TILE_EDGE);
        let rows = height.div_ceil(TILE_EDGE);
        let count = columns
            .checked_mul(rows)
            .ok_or(PixelError::InvalidGeometry)?;
        if count > MAX_TILE_SLOTS {
            return Err(PixelError::BudgetExceeded); // 有限Native池不能用无限小层或截掉尾部来冒充完整帧。
        }
        // 在任何绘制前核完整候选所需的真实页；逐分配CAS还会重核全部存活provider，不能只验单tile。
        let mut total = 0usize;
        for row in 0..rows {
            for column in 0..columns {
                let tile_width = (width - column * TILE_EDGE).min(TILE_EDGE);
                let tile_height = (height - row * TILE_EDGE).min(TILE_EDGE);
                let (_, length) = tile_storage(tile_width, tile_height)?;
                total = total
                    .checked_add(allocation_length(length)?)
                    .ok_or(PixelError::InvalidGeometry)?;
            }
        }
        if counters
            .reserved_bytes
            .load(Ordering::Acquire)
            .checked_add(total)
            .is_none_or(|bytes| bytes > LIVE_PIXEL_BUDGET)
        {
            return Err(PixelError::BudgetExceeded);
        }
        Ok(Self {
            identity: Rc::new(()),
            counters,
            width,
            height,
            next_x: 0,
            next_y: 0,
            tiles: Vec::with_capacity(MAX_TILE_SLOTS),
            reused_images: 0,
        })
    }

    pub(in crate::native_surface) fn next_tile(&self) -> Option<PixelTile> {
        if self.next_y == self.height {
            return None;
        }
        let width = (self.width - self.next_x).min(TILE_EDGE);
        let height = (self.height - self.next_y).min(TILE_EDGE);
        Some(PixelTile {
            x: self.next_x,
            y: self.next_y,
            width,
            height,
            bottom: self.height - self.next_y - height,
        })
    }

    /// 可写状态只属于原有序候选；跨主队列让出时也不生成image或暴露部分图层。
    pub(in crate::native_surface) fn begin_tile(
        &self,
        tile: PixelTile,
    ) -> Result<WritableTile, PixelError> {
        if self.next_tile() != Some(tile) {
            return Err(PixelError::InvalidGeometry);
        }
        let (row_bytes, length) = tile_storage(tile.width, tile.height)?;
        let allocation = allocate(length, Arc::clone(&self.counters), Some(LIVE_PIXEL_BUDGET))?;
        let context = make_context(&allocation, 0, tile.width, tile.height, row_bytes)?;
        Ok(WritableTile {
            context: Some(context),
            allocation,
            identity: Rc::clone(&self.identity),
            tile,
            row_bytes,
        })
    }

    /// 原session归还它的context claim后才可封存；被外部clone保留的context会真实拒绝。
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(in crate::native_surface) fn finish_tile(
        &mut self,
        writable: WritableTile,
        prior: Option<&TileFrame>,
    ) -> Result<(), PixelError> {
        let prepared = self.prepare_tile(writable)?;
        self.finish_prepared(prepared, None, prior)
    }

    pub(in crate::native_surface) fn prepare_tile(
        &self,
        mut writable: WritableTile,
    ) -> Result<PreparedTile, PixelError> {
        if self.next_tile() != Some(writable.tile)
            || !Rc::ptr_eq(&self.identity, &writable.identity)
            || !Arc::ptr_eq(&self.counters, &writable.allocation.counters)
        {
            return Err(PixelError::InvalidGeometry); // 不能把其他候选的可写页接到本完整帧。
        }
        let context = writable.context.take().ok_or(PixelError::ContextRetained)?;
        CGContext::flush(Some(&context));
        drop(context); // 实际release callback归还后，seal才检查active_contexts并mprotect。
        let pixels = encoding::prepare(
            writable.allocation,
            writable.tile.width,
            writable.tile.height,
            writable.row_bytes,
        )?;
        Ok(PreparedTile {
            pixels,
            tile: writable.tile,
            identity: writable.identity,
        })
    }

    /// 原单块入口保持不拆分；旧像素控制和无校准调用者不隐式增加Native请求。
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(in crate::native_surface) fn finish_prepared(
        &mut self,
        prepared: PreparedTile,
        pairs: Option<&CertifiedMaskPairs>,
        prior: Option<&TileFrame>,
    ) -> Result<(), PixelError> {
        let plan = PreparedTileEncodingPlan {
            pixels: encoding::PreparedRegions::single(prepared.pixels),
            tile: prepared.tile,
            identity: prepared.identity,
        };
        self.finish_encoding(plan, std::slice::from_ref(&pairs), prior)
    }

    pub(in crate::native_surface) fn plan_encoding(
        &self,
        prepared: PreparedTile,
    ) -> Result<PreparedTileEncodingPlan, PixelError> {
        if self.next_tile() != Some(prepared.tile)
            || !Rc::ptr_eq(&self.identity, &prepared.identity)
        {
            return Err(PixelError::InvalidGeometry);
        }
        let columns = self.width.div_ceil(TILE_EDGE);
        let total = columns * self.height.div_ceil(TILE_EDGE);
        let current = self.next_y / TILE_EDGE * columns + self.next_x / TILE_EDGE;
        let remaining = total
            .checked_sub(current + 1)
            .ok_or(PixelError::InvalidGeometry)?;
        let available = MAX_TILE_SLOTS
            .checked_sub(self.tiles.len())
            .and_then(|slots| slots.checked_sub(remaining))
            .filter(|slots| *slots != 0)
            .ok_or(PixelError::InvalidGeometry)?;
        // 后继每个原绘图格至少留一个slot；先拆分不能把未画尾格挤出完整frame。
        let pixels = encoding::PreparedRegions::plan(prepared.pixels, available.min(4))?;
        Ok(PreparedTileEncodingPlan {
            pixels,
            tile: prepared.tile,
            identity: prepared.identity,
        })
    }

    pub(in crate::native_surface) fn finish_encoding(
        &mut self,
        prepared: PreparedTileEncodingPlan,
        pairs: &[Option<&CertifiedMaskPairs>],
        prior: Option<&TileFrame>,
    ) -> Result<(), PixelError> {
        if self.next_tile() != Some(prepared.tile)
            || !Rc::ptr_eq(&self.identity, &prepared.identity)
        {
            return Err(PixelError::InvalidGeometry);
        }
        let original = prepared.tile;
        let planned = prepared.pixels.count();
        let regions = prepared.pixels.finish(pairs, prior)?;
        if regions.is_empty()
            || regions.len() > planned
            || self.tiles.len() + regions.len() > MAX_TILE_SLOTS
        {
            return Err(PixelError::InvalidGeometry);
        }
        let mut completed = Vec::with_capacity(regions.len());
        let mut next_row = 0usize;
        let mut reused = 0usize;
        for region in regions {
            let end = region
                .first_row
                .checked_add(region.height)
                .filter(|end| *end <= original.height)
                .ok_or(PixelError::InvalidGeometry)?;
            if region.first_row != next_row || region.height == 0 {
                return Err(PixelError::InvalidGeometry); // 私有分区也须精确连续覆盖，不能留白、重叠或跳过最后像素。
            }
            let tile = PixelTile {
                x: original.x,
                y: original.y + region.first_row,
                width: original.width,
                height: region.height,
                bottom: original.bottom + original.height - end,
            };
            let (sealed, was_reused) = self.seal_region(tile, region.encoded, prior)?;
            reused += usize::from(was_reused);
            completed.push(sealed);
            next_row = end;
        }
        if next_row != original.height {
            return Err(PixelError::InvalidGeometry);
        }
        let total_reused = self
            .reused_images
            .checked_add(reused)
            .ok_or(PixelError::InvalidGeometry)?;
        self.tiles.extend(completed); // 全部分区成功后才改变本私有frame；失败不留下半个原工作格。
        self.reused_images = total_reused;
        self.next_x += original.width;
        if self.next_x == self.width {
            self.next_x = 0;
            self.next_y += original.height;
        } // 原512绘图格只推进一次，编码子区不增加glyph/context或提前签整帧完成。
        Ok(())
    }

    fn seal_region(
        &self,
        tile: PixelTile,
        encoded: encoding::EncodedImage,
        prior: Option<&TileFrame>,
    ) -> Result<(SealedTile, bool), PixelError> {
        let reused = encoded.reused_image;
        let sealed = SealedTile {
            tile,
            image: encoded.image,
            secondary_image: encoded.secondary_image,
            presentation: encoded.presentation,
            storage: encoded.storage,
            secondary_storage: encoded.secondary_storage,
            #[cfg(test)]
            allocation: encoded.allocation,
            #[cfg(test)]
            row_bytes: encoded.row_bytes,
        };
        if let Some(prior) = prior {
            if prior.tiles.len() > MAX_TILE_SLOTS {
                return Err(PixelError::InvalidGeometry); // 活帧池上界不能因复用变成历史帧或无限搜索。
            }
            if prior.tiles.iter().any(|tile| {
                !tile.storage.belongs_to(&self.counters)
                    || tile
                        .secondary_storage
                        .as_ref()
                        .is_some_and(|storage| !storage.belongs_to(&self.counters))
            }) {
                return Err(PixelError::InvalidGeometry); // 外会话像素不能绕开当前32MiB真实账本，即使内容逐byte相同。
            }
        }
        // 编码器已在创建provider前完整判等；这里仅接新tile坐标和本次真实复用收据。
        // 原帧的有限槽及页账门仍留在原位置，优化不把非法原帧洗成普通miss或合法发布。
        Ok((sealed, reused))
    }

    #[cfg(test)]
    pub(in crate::native_surface) fn paint_tile<E: From<PixelError>>(
        &mut self,
        tile: PixelTile,
        paint: impl FnOnce(&CGContext) -> Result<(), E>,
    ) -> Result<(), E> {
        let writable = self.begin_tile(tile)?;
        paint(writable.context())?; // 首错丢弃本可写页；不能发布或在失败后继续封存。
        self.finish_tile(writable, None)?;
        Ok(())
    }

    pub(in crate::native_surface) fn finish(self) -> Result<TileFrame, PixelError> {
        if self.next_tile().is_some() {
            return Err(PixelError::IncompleteViewport);
        }
        let frame = TileFrame {
            width: self.width,
            height: self.height,
            tiles: self.tiles,
            reused_images: self.reused_images,
        };
        if frame.reused_images > frame.tiles.len() {
            return Err(PixelError::InvalidGeometry); // 每个新tile最多借一个原图，不接受重复计数或半帧收据。
        }
        Ok(frame)
    }
}

fn tile_storage(width: usize, height: usize) -> Result<(usize, usize), PixelError> {
    // 每块地址由mmap对齐，行宽独立补到16byte；没有共享大跨度，尾块也无需额外整行padding。
    let row_bytes = width
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_add(15))
        .map(|bytes| bytes & !15)
        .ok_or(PixelError::InvalidGeometry)?;
    let length = row_bytes
        .checked_mul(height)
        .ok_or(PixelError::InvalidGeometry)?;
    Ok((row_bytes, length))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_foundation::{CGAffineTransform, CGRect};

    fn horizontal_palettes(context: &CGContext) {
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null() && width <= 512 && height >= 2);
        CGContext::flush(Some(context));
        for y in 0..height {
            // SAFETY: 受控实际context独占完整stride*height；每行写原有效RGBA，不借padding制造色数。
            let pixels = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * stride), width * 4)
            };
            for (x, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                *pixel = if y < height / 2 {
                    [x as u8, 0, 0, 255]
                } else {
                    [0, x as u8, 1, 255]
                }; // 每半区最多256个实际原色，整块超过256；切割点来自原整数行而非主题语义。
            }
        }
    }

    fn adaptive_frame(
        width: usize,
        height: usize,
        resources: &Arc<NativeMarkdownViewportPixels>,
        paint: impl Fn(&CGContext),
        prior: Option<&TileFrame>,
    ) -> TileFrame {
        let mut bitmap = TiledBitmap::new(width, height, Arc::clone(resources)).unwrap();
        while let Some(tile) = bitmap.next_tile() {
            let writable = bitmap.begin_tile(tile).unwrap();
            paint(writable.context());
            let prepared = bitmap.prepare_tile(writable).unwrap();
            let plan = bitmap.plan_encoding(prepared).unwrap();
            let count = plan.region_count();
            let pairs = [None; 4]; // 纯像素控制不伪造Native双遮罩资格，局部真实色表保原Indexed。
            bitmap
                .finish_encoding(plan, &pairs[..count], prior)
                .unwrap();
        }
        bitmap.finish().unwrap()
    }

    #[test]
    fn adaptive_odd_height_regions_cover_every_pixel_and_the_last_partial_row() {
        let oracle = Bitmap::new(509, 511, Arc::default()).unwrap();
        horizontal_palettes(oracle.context());
        CGContext::flush(Some(oracle.context()));
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut bitmap = TiledBitmap::new(509, 511, resources).unwrap();
        let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        horizontal_palettes(writable.context());
        let prepared = bitmap.prepare_tile(writable).unwrap();
        let plan = bitmap.plan_encoding(prepared).unwrap();
        assert_eq!(plan.region_count(), 2);
        bitmap.finish_encoding(plan, &[None, None], None).unwrap();
        assert!(bitmap.next_tile().is_none());
        assert_eq!(bitmap.tiles.len(), 2);
        assert_eq!(bitmap.tiles[0].tile.y, 0);
        assert_eq!(bitmap.tiles[0].tile.height, 255);
        assert_eq!(bitmap.tiles[0].tile.bottom, 256);
        assert_eq!(bitmap.tiles[1].tile.y, 255);
        assert_eq!(bitmap.tiles[1].tile.height, 256);
        assert_eq!(bitmap.tiles[1].tile.bottom, 0);
        assert!(
            bitmap
                .tiles
                .iter()
                .all(|tile| CGImage::bits_per_pixel(Some(&tile.image)) == 8)
        );
        // SAFETY: oracle为独立紧密RGBA映射，flush后同步只读全部行，包含非整除末行和裁片接缝。
        let expected = unsafe {
            std::slice::from_raw_parts(
                oracle.allocation.address.as_ptr().cast::<u8>(),
                509 * 511 * 4,
            )
        };
        assert_eq!(active_pixel_mismatch(&bitmap, expected), None);
        drop(bitmap.finish().unwrap());
        assert!(observer.is_released());
    }

    #[test]
    fn adaptive_planning_preserves_a_late_transparent_pixel_as_complete_rgba() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut bitmap = TiledBitmap::new(512, 511, resources).unwrap();
        let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        dense_rgba(writable.context());
        let prepared = bitmap.prepare_tile(writable).unwrap();
        let plan = bitmap.plan_encoding(prepared).unwrap();
        assert_eq!(plan.region_count(), 1);
        bitmap.finish_encoding(plan, &[None], None).unwrap();
        assert_eq!(bitmap.tiles.len(), 1);
        assert_eq!(CGImage::bits_per_pixel(Some(&bitmap.tiles[0].image)), 32);
        drop(bitmap.finish().unwrap());
        assert!(observer.is_released());
    }

    #[test]
    fn adaptive_planning_rejects_page_overhead_without_a_true_page_saving() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut bitmap = TiledBitmap::new(256, 2, resources).unwrap();
        let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        horizontal_palettes(writable.context());
        let prepared = bitmap.prepare_tile(writable).unwrap();
        let plan = bitmap.plan_encoding(prepared).unwrap();
        assert_eq!(plan.region_count(), 1); // 两索引行各占整页，比原一页RGB24更多，不能只按紧密字节决定。
        bitmap.finish_encoding(plan, &[None], None).unwrap();
        assert_eq!(CGImage::bits_per_pixel(Some(&bitmap.tiles[0].image)), 24);
        drop(bitmap.finish().unwrap());
        assert!(observer.is_released());
    }

    #[test]
    fn adaptive_planning_reserves_a_slot_for_every_remaining_original_work_cell() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut bitmap = TiledBitmap::new(63 * 512 + 256, 256, resources).unwrap();
        let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        horizontal_palettes(writable.context());
        let prepared = bitmap.prepare_tile(writable).unwrap();
        let plan = bitmap.plan_encoding(prepared).unwrap();
        assert_eq!(plan.region_count(), 1); // 首格虽可省页，64个原工作格已占满64slot，尾部不得被压掉。
        bitmap.finish_encoding(plan, &[None], None).unwrap();
        assert_eq!(bitmap.tiles.len(), 1);
        assert_eq!(bitmap.next_tile().unwrap().x, 512);
        drop(bitmap); // 此处明确取消尚未绘制的尾格，不把部分候选当整帧PASS。
        assert!(observer.is_released());
    }

    #[test]
    fn adaptive_second_allocation_budget_miss_returns_all_temporary_pages_before_fallback() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let half_index = allocation_length(512 * 256).unwrap();
        let held = allocate(
            LIVE_PIXEL_BUDGET - 512 * 512 * 4 - half_index,
            Arc::clone(&resources),
            Some(LIVE_PIXEL_BUDGET),
        )
        .unwrap();
        let mut bitmap = TiledBitmap::new(512, 512, resources).unwrap();
        let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        horizontal_palettes(writable.context());
        let prepared = bitmap.prepare_tile(writable).unwrap();
        let plan = bitmap.plan_encoding(prepared).unwrap();
        assert_eq!(plan.region_count(), 2);
        bitmap.finish_encoding(plan, &[None, None], None).unwrap();
        assert_eq!(bitmap.tiles.len(), 1);
        assert_eq!(CGImage::bits_per_pixel(Some(&bitmap.tiles[0].image)), 32);
        assert_eq!(observer.snapshot().live_pixel_buffers, 2);
        assert_eq!(
            observer.snapshot().peak_reserved_pixel_bytes,
            LIVE_PIXEL_BUDGET
        );
        assert_eq!(
            observer.snapshot().reserved_pixel_bytes,
            LIVE_PIXEL_BUDGET - half_index
        );
        drop(bitmap.finish().unwrap());
        drop(held);
        assert!(observer.is_released());
    }

    #[test]
    fn adaptive_exact_region_reuse_does_not_hide_a_real_pixel_change() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let original = adaptive_frame(509, 511, &resources, horizontal_palettes, None);
        let same = adaptive_frame(509, 511, &resources, horizontal_palettes, Some(&original));
        let changed = adaptive_frame(
            509,
            511,
            &resources,
            |context| {
                horizontal_palettes(context);
                change_first_red(context);
            },
            Some(&original),
        );
        assert_eq!(original.tiles.len(), 2);
        assert_eq!(same.reused_images, 2);
        assert_eq!(changed.reused_images, 1);
        assert!(std::ptr::eq::<CGImage>(
            &*original.tiles[0].image,
            &*same.tiles[0].image
        ));
        assert!(!std::ptr::eq::<CGImage>(
            &*original.tiles[0].image,
            &*changed.tiles[0].image
        ));
        drop((original, same, changed));
        assert!(observer.is_released());
    }

    fn four_horizontal_palettes(context: &CGContext) {
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null() && height >= 4);
        CGContext::flush(Some(context));
        for band in 0..4 {
            for y in height * band / 4..height * (band + 1) / 4 {
                // SAFETY: 原有限实际context逐整数行分区，四区恰好覆盖全部height，不写padding。
                let pixels = unsafe {
                    std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * stride), width * 4)
                };
                for (x, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    *pixel = [x as u8, band as u8, 17, 255];
                }
            }
        }
    }

    #[test]
    fn adaptive_four_regions_are_chosen_only_when_two_regions_cannot_save_real_pages() {
        let oracle = Bitmap::new(509, 509, Arc::default()).unwrap();
        four_horizontal_palettes(oracle.context());
        CGContext::flush(Some(oracle.context()));
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut bitmap = TiledBitmap::new(509, 509, resources).unwrap();
        let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        four_horizontal_palettes(writable.context());
        let prepared = bitmap.prepare_tile(writable).unwrap();
        let plan = bitmap.plan_encoding(prepared).unwrap();
        assert_eq!(plan.region_count(), 4);
        bitmap.finish_encoding(plan, &[None; 4], None).unwrap();
        assert_eq!(bitmap.tiles.len(), 4);
        assert_eq!(bitmap.tiles[3].tile.y, 381);
        assert_eq!(bitmap.tiles[3].tile.height, 128);
        assert_eq!(bitmap.tiles[3].tile.bottom, 0);
        assert!(observer.snapshot().live_pixel_bytes < allocation_length(1536 * 509).unwrap());
        // SAFETY: 原独立紧密oracle覆盖509²RGBA，所有context已flush，完整往返包括三条接缝和末行。
        let expected = unsafe {
            std::slice::from_raw_parts(
                oracle.allocation.address.as_ptr().cast::<u8>(),
                509 * 509 * 4,
            )
        };
        assert_eq!(active_pixel_mismatch(&bitmap, expected), None);
        drop(bitmap.finish().unwrap());
        assert!(observer.is_released());
    }

    #[test]
    fn cancelling_a_private_region_plan_returns_the_original_mapping_without_any_provider() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let bitmap = TiledBitmap::new(509, 509, resources).unwrap();
        let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        four_horizontal_palettes(writable.context());
        let prepared = bitmap.prepare_tile(writable).unwrap();
        let plan = bitmap.plan_encoding(prepared).unwrap();
        assert_eq!(plan.region_count(), 4);
        assert_eq!(observer.snapshot().live_pixel_buffers, 1);
        drop(plan); // 计划只有原只读Arc和有限metadata；取消不能留下region原图、context或探测provider。
        assert!(observer.is_released());
        drop(bitmap);
    }

    fn frame_with_prior(
        width: usize,
        height: usize,
        resources: &Arc<NativeMarkdownViewportPixels>,
        paint: impl Fn(&CGContext),
        prior: Option<&TileFrame>,
    ) -> TileFrame {
        let mut bitmap = TiledBitmap::new(width, height, Arc::clone(resources)).unwrap();
        while let Some(tile) = bitmap.next_tile() {
            let writable = bitmap.begin_tile(tile).unwrap();
            paint(writable.context());
            bitmap.finish_tile(writable, prior).unwrap();
        } // 真实context完成及封存路径与生产相同，测试不构造伪SealedTile或替代image。
        bitmap.finish().unwrap()
    }

    fn change_first_red(context: &CGContext) {
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null());
        // SAFETY: 本测试唯一可写context完整覆盖首RGBA像素；没有provider/外部context读取此页。
        unsafe { *data.cast::<u8>() = (*data.cast::<u8>()).wrapping_add(1) };
    }

    #[test]
    fn prepared_opaque_palette_is_a_proposal_and_transparent_pixels_cannot_request_masks() {
        for transparent in [false, true] {
            let resources = Arc::new(NativeMarkdownViewportPixels::default());
            let observer = resources.observer();
            let bitmap = TiledBitmap::new(32, 32, resources).unwrap();
            let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
            palette_rgba(writable.context());
            if transparent {
                let data = objc2_core_graphics::CGBitmapContextGetData(Some(writable.context()));
                // SAFETY: 此实际context独占首RGBA像素，尚无image/provider；只将该像素变成有效透明黑。
                unsafe {
                    std::ptr::copy_nonoverlapping([0u8, 0, 0, 128].as_ptr(), data.cast::<u8>(), 4)
                };
            }
            let prepared = bitmap.prepare_tile(writable).unwrap();
            assert_eq!(prepared.mask_request().is_none(), transparent);
            assert_eq!(observer.snapshot().live_pixel_buffers, 1);
            drop((prepared, bitmap));
            assert!(observer.is_released()); // 只提出调色板不构造伪双mask或保活取消后的原RGBA。
        }
    }

    #[test]
    fn exact_sealed_rgba_reuses_original_image_and_releases_duplicate_mapping() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let original = frame_with_prior(32, 32, &resources, dense_rgba, None);
        let before = observer.snapshot().live_pixel_buffers;
        let next = frame_with_prior(32, 32, &resources, dense_rgba, Some(&original));
        assert_eq!(next.reused_images, 1);
        assert!(std::ptr::eq::<CGImage>(
            &*original.tiles[0].image,
            &*next.tiles[0].image
        ));
        assert_eq!(observer.snapshot().live_pixel_buffers, before);
        drop(original);
        assert!(!observer.is_released()); // 当前真正image仍持原页；不能把旧frame归还误称像素归零。
        drop(next);
        assert!(observer.is_released());
    }

    #[test]
    fn one_real_rgba_byte_difference_does_not_reuse_the_prior_image() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let original = frame_with_prior(32, 32, &resources, dense_rgba, None);
        let next = frame_with_prior(
            32,
            32,
            &resources,
            |context| {
                dense_rgba(context);
                change_first_red(context);
            },
            Some(&original),
        );
        assert_eq!(CGImage::bits_per_pixel(Some(&original.tiles[0].image)), 32);
        assert_eq!(CGImage::bits_per_pixel(Some(&next.tiles[0].image)), 32);
        assert_eq!(next.reused_images, 0);
        assert!(!std::ptr::eq::<CGImage>(
            &*original.tiles[0].image,
            &*next.tiles[0].image
        ));
        drop((original, next));
        assert!(observer.is_released());
    }

    #[test]
    fn actual_indexed_palette_change_rejects_identical_index_pattern() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let original = frame_with_prior(32, 32, &resources, palette_rgba, None);
        let next = frame_with_prior(
            32,
            32,
            &resources,
            |context| {
                palette_rgba(context);
                let row = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
                let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
                for y in 0..32 {
                    // SAFETY: 唯一实际context覆盖32行；只改变本行有效RGB蓝通道，索引出现顺序仍相同。
                    let pixels = unsafe {
                        std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * row), 32 * 4)
                    };
                    for pixel in pixels.as_chunks_mut::<4>().0 {
                        pixel[2] += 1;
                    }
                }
            },
            Some(&original),
        );
        assert_eq!(CGImage::bits_per_pixel(Some(&original.tiles[0].image)), 8);
        assert_eq!(CGImage::bits_per_pixel(Some(&next.tiles[0].image)), 8);
        assert_eq!(next.reused_images, 0);
        assert!(!std::ptr::eq::<CGImage>(
            &*original.tiles[0].image,
            &*next.tiles[0].image
        ));
        drop((original, next));
        assert!(observer.is_released());
    }

    #[test]
    fn same_neutral_pixels_at_two_tile_positions_share_only_the_sealed_image() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let original = frame_with_prior(1024, 16, &resources, neutral_rgba, None);
        let next = frame_with_prior(1024, 16, &resources, neutral_rgba, Some(&original));
        assert_eq!(next.reused_images, 2);
        assert_ne!(next.tiles[0].tile.x, next.tiles[1].tile.x);
        assert!(
            next.tiles
                .iter()
                .all(|tile| CGImage::is_mask(Some(&tile.image))
                    && std::ptr::eq::<CGImage>(&*original.tiles[0].image, &*tile.image))
        );
        drop((original, next));
        assert!(observer.is_released());
    }

    #[test]
    fn exact_pixels_from_another_resource_scope_cannot_bypass_the_current_budget() {
        let original_resources = Arc::new(NativeMarkdownViewportPixels::default());
        let current_resources = Arc::new(NativeMarkdownViewportPixels::default());
        let original_observer = original_resources.observer();
        let current_observer = current_resources.observer();
        let original = frame_with_prior(32, 32, &original_resources, dense_rgba, None);
        let mut current = TiledBitmap::new(32, 32, current_resources).unwrap();
        let writable = current.begin_tile(current.next_tile().unwrap()).unwrap();
        dense_rgba(writable.context());
        assert!(matches!(
            current.finish_tile(writable, Some(&original)),
            Err(PixelError::InvalidGeometry)
        ));
        assert!(current_observer.is_released()); // 拒绝后也真实归还已完整绘制的新候选页，不把外scope活页记作当前0。
        assert!(!original_observer.is_released());
        drop((original, current));
        assert!(original_observer.is_released());
    }

    fn dense_opaque_rgba(context: &CGContext) {
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null() && width <= 512 && height <= 512 && stride >= width * 4);
        CGContext::flush(Some(context));
        for y in 0..height {
            // SAFETY: 本测试唯一context公开RGBA数据覆盖完整stride*height；只在其同步栈写当前行。
            let row = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * stride), width * 4)
            };
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                *pixel = [x as u8, y as u8, ((x >> 8) * 2 + (y >> 8)) as u8, 255];
            }
        } // 多行tile具有超过256个真实颜色；它们只有在完整alpha证明后才能无损转为RGB24。
    }

    fn dense_rgba(context: &CGContext) {
        dense_opaque_rgba(context);
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        // SAFETY: 实际唯一context覆盖完整stride*height；只在最后一个有效像素写入合法半透明黑。
        unsafe {
            std::ptr::copy_nonoverlapping(
                [0u8, 0, 0, 128].as_ptr(),
                data.cast::<u8>()
                    .add((height - 1) * stride + (width - 1) * 4),
                4,
            )
        }; // 第257色出现后很久才到透明像素，原RGBA预算和晚到alpha反例不能被早退误授RGB24。
    }

    fn palette_rgba(context: &CGContext) {
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null() && width <= 512 && height <= 512 && stride >= width * 4);
        CGContext::flush(Some(context));
        for y in 0..height {
            // SAFETY: 同上，受控有限测试数据在真实可写context中逐像素生成，不改产品解码路径。
            let row = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * stride), width * 4)
            };
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let value = (x + y) as u8;
                *pixel = [value, 255 - value, value / 2, 255];
            }
        }
    }

    /// 直接写真实context的全部256个原灰值；不从待测A8编码反算参考输入。
    fn neutral_rgba(context: &CGContext) {
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null() && stride >= width * 4);
        CGContext::flush(Some(context));
        for y in 0..height {
            // SAFETY: 唯一可写测试context覆盖整行；没有provider或外部context持有这块像素。
            let row = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * stride), width * 4)
            };
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let gray = (x + y) as u8;
                *pixel = [gray, gray, gray, 255];
            }
        }
    }

    #[test]
    fn actual_opaque_neutral_mask_preserves_all_256_original_gray_levels_and_releases_pages() {
        let oracle = Bitmap::new(512, 512, Arc::default()).unwrap();
        neutral_rgba(oracle.context());
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut bitmap = TiledBitmap::new(512, 512, resources).unwrap();
        bitmap
            .paint_tile::<PixelError>(bitmap.next_tile().unwrap(), |context| {
                neutral_rgba(context);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            bitmap.tiles[0].presentation,
            PixelPresentation::OpaqueNeutralMask
        );
        assert!(CGImage::is_mask(Some(&bitmap.tiles[0].image)));
        assert_eq!(observer.snapshot().live_pixel_bytes, 512 * 512);
        // SAFETY: 原独立RGBA参考context在栈上保活；只读取真实有效像素，不使用待测mask构造期数据。
        let expected = unsafe {
            std::slice::from_raw_parts(
                oracle.allocation.address.as_ptr().cast::<u8>(),
                512 * 512 * 4,
            )
        };
        assert_eq!(active_pixel_mismatch(&bitmap, expected), None);
        drop(bitmap.finish().unwrap());
        assert!(observer.is_released());
    }

    #[test]
    fn one_colored_or_transparent_pixel_cannot_acquire_the_neutral_mask_presentation() {
        for colored in [false, true] {
            let mut bitmap = TiledBitmap::new(512, 512, Arc::default()).unwrap();
            bitmap
                .paint_tile::<PixelError>(bitmap.next_tile().unwrap(), |context| {
                    neutral_rgba(context);
                    let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
                    let pixel = if colored {
                        [10, 11, 10, 255]
                    } else {
                        [10, 10, 10, 128]
                    };
                    // SAFETY: 原真实context独占第一像素的4byte；边缘差异必须撤销整块mask资格。
                    unsafe { std::ptr::copy_nonoverlapping(pixel.as_ptr(), data.cast::<u8>(), 4) };
                    Ok(())
                })
                .unwrap();
            assert_eq!(bitmap.tiles[0].presentation, PixelPresentation::Image);
            assert!(!CGImage::is_mask(Some(&bitmap.tiles[0].image)));
            // 第257色且opaque可精确RGB24；任何透明仍必须RGBA，不能量化或强制刷白。
            assert_eq!(
                CGImage::bits_per_pixel(Some(&bitmap.tiles[0].image)),
                if colored { 24 } else { 32 }
            );
        }
    }

    #[test]
    fn actual_indexed_color_image_roundtrips_all_rgba_and_releases_the_original_pages() {
        let oracle = Bitmap::new(512, 512, Arc::default()).unwrap();
        palette_rgba(oracle.context());
        CGContext::flush(Some(oracle.context()));
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut bitmap = TiledBitmap::new(512, 512, resources).unwrap();
        bitmap
            .paint_tile::<PixelError>(bitmap.next_tile().unwrap(), |context| {
                palette_rgba(context);
                Ok(())
            })
            .unwrap();
        assert_eq!(CGImage::bits_per_pixel(Some(&bitmap.tiles[0].image)), 8);
        assert_eq!(observer.snapshot().live_pixel_bytes, 512 * 512);
        assert_eq!(observer.snapshot().live_pixel_buffers, 1);
        // SAFETY: 有限oracle固定RGBA分配在同步栈存活且已flush；逐byte比较包括真实Native索引解码。
        let expected = unsafe {
            std::slice::from_raw_parts(
                oracle.allocation.address.as_ptr().cast::<u8>(),
                512 * 512 * 4,
            )
        };
        assert_eq!(active_pixel_mismatch(&bitmap, expected), None);
        drop(bitmap.finish().unwrap());
        assert!(observer.is_released());
    }

    #[test]
    fn optional_encoding_cannot_exceed_live_budget_and_keeps_original_rgba_when_no_room_remains() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let held = allocate(
            LIVE_PIXEL_BUDGET - 512 * 512 * 4,
            Arc::clone(&resources),
            Some(LIVE_PIXEL_BUDGET),
        )
        .unwrap();
        let mut bitmap = TiledBitmap::new(512, 512, Arc::clone(&resources)).unwrap();
        bitmap
            .paint_tile::<PixelError>(bitmap.next_tile().unwrap(), |context| {
                palette_rgba(context);
                Ok(())
            })
            .unwrap();
        // 原完整RGBA候选恰好可容纳；额外索引表示无空间时保留它，不能拒收、跳色或越过共同预算。
        assert_eq!(CGImage::bits_per_pixel(Some(&bitmap.tiles[0].image)), 32);
        assert_eq!(observer.snapshot().reserved_pixel_bytes, LIVE_PIXEL_BUDGET);
        assert_eq!(
            observer.snapshot().peak_reserved_pixel_bytes,
            LIVE_PIXEL_BUDGET
        );
        drop(bitmap.finish().unwrap());
        drop(held);
        assert!(observer.is_released());
    }

    #[test]
    fn a_late_transparent_pixel_after_palette_overflow_keeps_full_rgba() {
        let oracle = Bitmap::new(512, 512, Arc::default()).unwrap();
        dense_rgba(oracle.context());
        CGContext::flush(Some(oracle.context()));
        let mut bitmap = TiledBitmap::new(512, 512, Arc::default()).unwrap();
        bitmap
            .paint_tile::<PixelError>(bitmap.next_tile().unwrap(), |context| {
                dense_rgba(context);
                Ok(())
            })
            .unwrap();
        assert_eq!(CGImage::bits_per_pixel(Some(&bitmap.tiles[0].image)), 32);
        // SAFETY: 同上，完整真实RGBA参考输入只属于本固定有限测试，不进入产品。
        let expected = unsafe {
            std::slice::from_raw_parts(
                oracle.allocation.address.as_ptr().cast::<u8>(),
                512 * 512 * 4,
            )
        };
        assert_eq!(active_pixel_mismatch(&bitmap, expected), None);
    }

    #[test]
    fn actual_opaque_multicolor_rgb24_preserves_partial_stride_and_last_row() {
        let oracle = Bitmap::new(509, 31, Arc::default()).unwrap();
        dense_opaque_rgba(oracle.context());
        CGContext::flush(Some(oracle.context()));
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut bitmap = TiledBitmap::new(509, 31, resources).unwrap();
        bitmap
            .paint_tile::<PixelError>(bitmap.next_tile().unwrap(), |context| {
                dense_opaque_rgba(context);
                Ok(())
            })
            .unwrap();
        let tile = &bitmap.tiles[0];
        assert_eq!(tile.presentation, PixelPresentation::Image);
        assert!(tile.secondary_image.is_none());
        assert_eq!(CGImage::bits_per_component(Some(&tile.image)), 8);
        assert_eq!(CGImage::bits_per_pixel(Some(&tile.image)), 24);
        assert_eq!(
            CGImage::alpha_info(Some(&tile.image)),
            CGImageAlphaInfo::None
        );
        assert_eq!(tile.row_bytes, 1536);
        assert_eq!(observer.snapshot().live_pixel_buffers, 1);
        assert_eq!(
            observer.snapshot().live_pixel_bytes,
            allocation_length(1536 * 31).unwrap()
        );
        // SAFETY: oracle实际紧密RGBA映射同步持活，flush后只读全部有效行，包括末行和最后像素。
        let expected = unsafe {
            std::slice::from_raw_parts(
                oracle.allocation.address.as_ptr().cast::<u8>(),
                509 * 31 * 4,
            )
        };
        assert_eq!(active_pixel_mismatch(&bitmap, expected), None);
        drop(bitmap.finish().unwrap());
        assert!(observer.is_released());
    }

    #[test]
    fn rgb24_budget_miss_keeps_original_rgba_without_exceeding_the_shared_limit() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let held = allocate(
            LIVE_PIXEL_BUDGET - 512 * 512 * 4,
            Arc::clone(&resources),
            Some(LIVE_PIXEL_BUDGET),
        )
        .unwrap();
        let mut bitmap = TiledBitmap::new(512, 512, resources).unwrap();
        bitmap
            .paint_tile::<PixelError>(bitmap.next_tile().unwrap(), |context| {
                dense_opaque_rgba(context);
                Ok(())
            })
            .unwrap();
        // RGB24也必须额外申请真实页；没有临时空间时原完整RGBA仍合法，不能越界或删色。
        assert_eq!(CGImage::bits_per_pixel(Some(&bitmap.tiles[0].image)), 32);
        assert_eq!(observer.snapshot().reserved_pixel_bytes, LIVE_PIXEL_BUDGET);
        assert_eq!(
            observer.snapshot().peak_reserved_pixel_bytes,
            LIVE_PIXEL_BUDGET
        );
        drop(bitmap.finish().unwrap());
        drop(held);
        assert!(observer.is_released());
    }

    #[test]
    fn rgb24_provider_returns_only_after_the_last_real_image_claim() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let frame = frame_with_prior(509, 31, &resources, dense_opaque_rgba, None);
        assert_eq!(CGImage::bits_per_pixel(Some(&frame.tiles[0].image)), 24);
        let allocation = Arc::clone(&frame.tiles[0].allocation);
        let weak = Arc::downgrade(&allocation);
        let last_image = frame.tiles[0].image.clone();
        drop(frame);
        assert!(!allocation.provider_returned.load(Ordering::Acquire));
        assert_eq!(observer.snapshot().live_pixel_buffers, 1);
        drop(last_image); // 真实CGImage最后claim消失，才允许原provider的一次release callback发生。
        assert!(allocation.provider_returned.load(Ordering::Acquire));
        assert_eq!(observer.snapshot().live_pixel_buffers, 1); // 测试仍显式持分配，不把CF归还误记为munmap。
        drop(allocation);
        assert!(weak.upgrade().is_none());
        assert!(observer.is_released());
    }

    #[test]
    fn equal_rgb24_reuses_the_real_image_but_one_rgb_byte_change_does_not() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let original = frame_with_prior(32, 32, &resources, dense_opaque_rgba, None);
        let same = frame_with_prior(32, 32, &resources, dense_opaque_rgba, Some(&original));
        let changed = frame_with_prior(
            32,
            32,
            &resources,
            |context| {
                dense_opaque_rgba(context);
                change_first_red(context);
            },
            Some(&original),
        );
        assert_eq!(same.reused_images, 1);
        assert_eq!(changed.reused_images, 0);
        assert_eq!(CGImage::bits_per_pixel(Some(&original.tiles[0].image)), 24);
        assert_eq!(CGImage::bits_per_pixel(Some(&changed.tiles[0].image)), 24);
        assert!(std::ptr::eq::<CGImage>(
            &*original.tiles[0].image,
            &*same.tiles[0].image
        ));
        assert!(!std::ptr::eq::<CGImage>(
            &*original.tiles[0].image,
            &*changed.tiles[0].image
        ));
        assert_eq!(observer.snapshot().live_pixel_buffers, 2);
        drop((original, same, changed));
        assert!(observer.is_released());
    }

    #[test]
    fn actual_transparency_keeps_rgba_and_only_a_true_constant_can_use_one_pixel() {
        for uniform in [false, true] {
            let paint = |context: &CGContext| {
                CGContext::set_rgb_fill_color(Some(context), 0.5, 0.25, 0.125, 0.5);
                let rect = if uniform {
                    CGRect::new(
                        objc2_core_foundation::CGPoint::new(0.0, 0.0),
                        objc2_core_foundation::CGSize::new(512.0, 512.0),
                    )
                } else {
                    CGRect::new(
                        objc2_core_foundation::CGPoint::new(64.0, 32.0),
                        objc2_core_foundation::CGSize::new(300.0, 240.0),
                    )
                };
                CGContext::fill_rect(Some(context), rect);
            };
            let oracle = Bitmap::new(512, 512, Arc::default()).unwrap();
            paint(oracle.context());
            CGContext::flush(Some(oracle.context()));
            let mut bitmap = TiledBitmap::new(512, 512, Arc::default()).unwrap();
            bitmap
                .paint_tile::<PixelError>(bitmap.next_tile().unwrap(), |context| {
                    paint(context);
                    Ok(())
                })
                .unwrap();
            assert_eq!(CGImage::bits_per_pixel(Some(&bitmap.tiles[0].image)), 32);
            assert_eq!(
                CGImage::width(Some(&bitmap.tiles[0].image)),
                if uniform { 1 } else { 512 }
            );
            // SAFETY: 本有限透明输入保留真实premultiplied字节；Native往返后必须逐RGBA相同。
            let expected = unsafe {
                std::slice::from_raw_parts(
                    oracle.allocation.address.as_ptr().cast::<u8>(),
                    512 * 512 * 4,
                )
            };
            assert_eq!(active_pixel_mismatch(&bitmap, expected), None);
        }
    }

    fn draw(context: &CGContext) {
        CGContext::set_rgb_fill_color(Some(context), 0.2, 0.4, 0.8, 1.0);
        CGContext::fill_rect(
            Some(context),
            CGRect::new(
                objc2_core_foundation::CGPoint::new(0.0, 0.0),
                objc2_core_foundation::CGSize::new(1031.0, 1033.0),
            ),
        );
        CGContext::set_rgb_fill_color(Some(context), 0.9, 0.1, 0.7, 0.6);
        CGContext::fill_rect(
            Some(context),
            CGRect::new(
                objc2_core_foundation::CGPoint::new(507.0, 499.0),
                objc2_core_foundation::CGSize::new(517.0, 521.0),
            ),
        );
        CGContext::set_rgb_fill_color(Some(context), 0.1, 0.95, 0.2, 1.0);
        CGContext::fill_rect(
            Some(context),
            CGRect::new(
                objc2_core_foundation::CGPoint::new(1028.0, 1030.0),
                objc2_core_foundation::CGSize::new(3.0, 3.0),
            ),
        );
    }

    #[test]
    fn retained_real_context_blocks_sealing_until_the_last_pending_claim_is_returned() {
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let mut bitmap = TiledBitmap::new(32, 24, counters).unwrap();
        let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        let pending_claim = writable.context_claim();
        assert!(matches!(
            bitmap.finish_tile(writable, None),
            Err(PixelError::ContextRetained)
        ));
        assert!(bitmap.next_tile().is_some());
        assert_eq!(observer.snapshot().live_pixel_buffers, 1);
        drop(bitmap); // 没有封存图像；真实context还活时原映射不能伪称已归还。
        assert!(!observer.is_released());
        drop(pending_claim);
        assert!(observer.is_released());
    }

    #[test]
    fn identical_geometry_and_shared_budget_cannot_authorize_a_foreign_writable_tile() {
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let mut original = TiledBitmap::new(32, 24, Arc::clone(&counters)).unwrap();
        let foreign = TiledBitmap::new(32, 24, counters).unwrap();
        let writable = foreign.begin_tile(foreign.next_tile().unwrap()).unwrap();
        assert!(matches!(
            original.finish_tile(writable, None),
            Err(PixelError::InvalidGeometry)
        ));
        assert!(original.next_tile().is_some());
        drop((original, foreign));
        assert!(observer.is_released());
    }

    #[test]
    fn cancellation_releases_completed_and_pending_tiles_without_publishing_partial_frame() {
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let mut bitmap = TiledBitmap::new(900, 24, counters).unwrap();
        let first = bitmap.next_tile().unwrap();
        let writable = bitmap.begin_tile(first).unwrap();
        bitmap.finish_tile(writable, None).unwrap();
        let pending = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        assert_eq!(observer.snapshot().live_pixel_buffers, 2);
        drop(pending); // 取消当前可写tile先结束实际context，再归还它的mmap。
        drop(bitmap); // 同一候选已完成的有限images也没有挂载，最后provider逐个实际munmap。
        assert!(observer.is_released());
    }

    #[test]
    fn actual_tiled_rgba_matches_full_context_at_all_seams_and_last_partial_tiles() {
        let full = Bitmap::new(1031, 1033, Arc::default()).unwrap();
        draw(full.context());
        CGContext::flush(Some(full.context()));
        let mut tiled = TiledBitmap::new(1031, 1033, Arc::default()).unwrap();
        let mut count = 0;
        while let Some(tile) = tiled.next_tile() {
            tiled
                .paint_tile::<PixelError>(tile, |context| {
                    CGContext::concat_ctm(
                        Some(context),
                        CGAffineTransform {
                            a: 1.0,
                            b: 0.0,
                            c: 0.0,
                            d: 1.0,
                            tx: -(tile.x as f64),
                            ty: -(tile.bottom as f64),
                        },
                    );
                    draw(context);
                    Ok(())
                })
                .unwrap();
            count += 1;
        }
        assert_eq!(count, 9);
        // SAFETY: 原oracle在同步栈持有紧密RGBA布局，flush后不再写，只读原可见矩形。
        let expected = unsafe {
            std::slice::from_raw_parts(
                full.allocation.address.as_ptr().cast::<u8>(),
                1031 * 1033 * 4,
            )
        };
        let mismatch = active_pixel_mismatch(&tiled, expected);
        assert_eq!(mismatch, None, "同一有限视口的首个RGBA差异字节");
        drop(tiled.finish().unwrap());
    }

    #[test]
    fn actual_color_emoji_and_latin_glyphs_match_the_finite_full_bitmap_oracle() {
        use objc2_core_foundation::{CFString, CGPoint, CGSize};
        use objc2_core_text::CTFont;
        // 此处只核像素后端，不以nominal字符映射冒称产品整形；真实产品仍消费已Ready shaping spool。
        // SAFETY: 两个有限字体名称是有效CFString；空matrix使用公开默认矩阵，font仅活本控制栈。
        let emoji = unsafe {
            CTFont::with_name(
                &CFString::from_static_str("Apple Color Emoji"),
                80.0,
                ptr::null(),
            )
        };
        // SAFETY: 同上，系统基础字体的有限控制不建立正文字符串或CTLine。
        let latin = unsafe {
            CTFont::with_name(&CFString::from_static_str("Helvetica"), 80.0, ptr::null())
        };
        // SAFETY: 对真实存活CTFont读取有限名称，确认没有把fallback字体当作彩色emoji证据。
        assert_eq!(
            // SAFETY: emoji为此同步栈持有的真实CTFont；公开PSName getter不借正文或外部缓冲。
            unsafe { emoji.post_script_name() }.to_string(),
            "AppleColorEmoji"
        );
        let glyph = |font: &CTFont, scalar: u16| {
            let mut source = scalar;
            let mut output = 0u16;
            // SAFETY: 两个栈上单unit的有效指针覆盖count=1，字符均BMP，不切代理项。
            assert!(unsafe {
                font.glyphs_for_characters(
                    NonNull::from_mut(&mut source),
                    NonNull::from_mut(&mut output),
                    1,
                )
            });
            assert_ne!(output, 0);
            output
        };
        let emoji_glyph = glyph(&emoji, 0x2600);
        let latin_glyph = glyph(&latin, b'A' as u16);
        let paint = |context: &CGContext| {
            CGContext::set_rgb_fill_color(Some(context), 1.0, 1.0, 1.0, 1.0);
            CGContext::fill_rect(
                Some(context),
                CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1031.0, 1033.0)),
            );
            CGContext::set_rgb_fill_color(Some(context), 0.0, 0.0, 0.0, 1.0);
            let mut first = CGPoint::new(507.0, 511.0);
            let mut second = CGPoint::new(1004.0, 1002.0);
            let mut eg = emoji_glyph;
            let mut lg = latin_glyph;
            // SAFETY: 每次只读取一个真实字体glyph和一个位置；context仍为实际可写有限位图。
            unsafe {
                emoji.draw_glyphs(
                    NonNull::from_mut(&mut eg),
                    NonNull::from_mut(&mut first),
                    1,
                    context,
                );
                latin.draw_glyphs(
                    NonNull::from_mut(&mut lg),
                    NonNull::from_mut(&mut second),
                    1,
                    context,
                );
            }
        };
        let full = Bitmap::new(1031, 1033, Arc::default()).unwrap();
        paint(full.context());
        CGContext::flush(Some(full.context()));
        let mut tiled = TiledBitmap::new(1031, 1033, Arc::default()).unwrap();
        while let Some(tile) = tiled.next_tile() {
            tiled
                .paint_tile::<PixelError>(tile, |context| {
                    CGContext::concat_ctm(
                        Some(context),
                        CGAffineTransform {
                            a: 1.0,
                            b: 0.0,
                            c: 0.0,
                            d: 1.0,
                            tx: -(tile.x as f64),
                            ty: -(tile.bottom as f64),
                        },
                    );
                    paint(context);
                    Ok(())
                })
                .unwrap();
        }
        // SAFETY: flush已完成，两个分配均活在同步栈；只读有限原RGBA长度、不读整篇文字或padding。
        let expected = unsafe {
            std::slice::from_raw_parts(
                full.allocation.address.as_ptr().cast::<u8>(),
                1031 * 1033 * 4,
            )
        };
        let colored = expected
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|rgba| rgba[3] != 0 && (rgba[0] != rgba[1] || rgba[1] != rgba[2]))
            .count();
        assert!(
            colored > 100,
            "必须实际画出彩色emoji像素，不能以空白低内存过门"
        );
        let mismatch = active_pixel_mismatch(&tiled, expected);
        assert_eq!(mismatch, None, "有限彩色字形的首个RGBA差异字节");
        drop(tiled.finish().unwrap());
    }

    fn active_pixel_mismatch(bitmap: &TiledBitmap, tightly_packed: &[u8]) -> Option<usize> {
        let active_row_bytes = bitmap.width * 4;
        assert_eq!(tightly_packed.len(), active_row_bytes * bitmap.height);
        let rendered = Bitmap::new(bitmap.width, bitmap.height, Arc::default()).unwrap();
        CGContext::set_interpolation_quality(
            Some(rendered.context()),
            objc2_core_graphics::CGInterpolationQuality::None,
        );
        for image in &bitmap.tiles {
            assert_eq!(CGImage::bytes_per_row(Some(&image.image)), image.row_bytes);
            let rect = CGRect::new(
                objc2_core_foundation::CGPoint::new(image.tile.x as f64, image.tile.bottom as f64),
                objc2_core_foundation::CGSize::new(
                    image.tile.width as f64,
                    image.tile.height as f64,
                ),
            );
            // 独立CoreGraphics路径核mask真实decode；Native CALayer合成另由实窗测试覆盖。
            if image.presentation == PixelPresentation::OpaqueNeutralMask {
                CGContext::save_g_state(Some(rendered.context()));
                CGContext::set_rgb_fill_color(Some(rendered.context()), 1.0, 1.0, 1.0, 1.0);
                CGContext::fill_rect(Some(rendered.context()), rect);
                CGContext::clip_to_mask(Some(rendered.context()), rect, Some(&image.image));
                CGContext::set_rgb_fill_color(Some(rendered.context()), 0.0, 0.0, 0.0, 1.0);
                CGContext::fill_rect(Some(rendered.context()), rect);
                CGContext::restore_g_state(Some(rendered.context()));
            } else {
                // 普通图像保持真实CGImage颜色空间、透明度与1×1固色缩放语义。
                CGContext::draw_image(Some(rendered.context()), rect, Some(&image.image));
            }
        }
        CGContext::flush(Some(rendered.context()));
        // SAFETY: 固定有限测试oracle持真实完整RGBA mmap；flush后不再写，只读有效像素不读padding。
        let actual = unsafe {
            std::slice::from_raw_parts(
                rendered.allocation.address.as_ptr().cast::<u8>(),
                tightly_packed.len(),
            )
        };
        actual.iter().zip(tightly_packed).position(|(a, b)| a != b)
    }

    #[test]
    fn sealed_viewport_is_multiple_independent_small_images_with_a_fixed_metadata_bound() {
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let mut bitmap = TiledBitmap::new(1031, 1033, Arc::clone(&counters)).unwrap();
        while let Some(tile) = bitmap.next_tile() {
            bitmap.paint_tile::<PixelError>(tile, |_| Ok(())).unwrap();
        }
        let frame = bitmap.finish().unwrap();
        assert_eq!(frame.tiles.len(), 9);
        assert_eq!(observer.snapshot().live_pixel_buffers, 9);
        for (index, tile) in frame.tiles.iter().enumerate() {
            // 本固定空图全部RGBA相同，只有经过完整逐像素证明才允许1×1恒定图像。
            assert_eq!(CGImage::width(Some(&tile.image)), 1);
            assert_eq!(CGImage::height(Some(&tile.image)), 1);
            assert!(tile.tile.width <= 512 && tile.tile.height <= 512);
            assert!(tile.allocation.length <= 512 * 512 * 4);
            assert!(
                frame.tiles[..index]
                    .iter()
                    .all(|prior| prior.allocation.address != tile.allocation.address)
            );
        } // 各恒定表示仍独立mmap/provider；不是把整个视口合成一张常驻大图。
        drop(frame);
        assert!(observer.is_released());
        assert!(matches!(
            TiledBitmap::new(usize::MAX, 1, counters),
            Err(PixelError::BudgetExceeded)
        ));
        assert!(observer.is_released()); // 超固定Native槽在任何像素分配前拒绝，不截断或降采样。
    }

    #[test]
    fn whole_live_budget_counts_old_images_and_rejects_candidate_without_partial_publication() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut first = TiledBitmap::new(2049, 2048, Arc::clone(&resources)).unwrap();
        while let Some(tile) = first.next_tile() {
            first
                .paint_tile::<PixelError>(tile, |context| {
                    dense_rgba(context);
                    Ok(())
                })
                .unwrap();
        }
        let image = first.finish().unwrap(); // 第一帧所有独立provider仍实际存活，候选必须计入共同真实页预算。
        let before = observer.snapshot().live_pixel_bytes;
        assert!(matches!(
            TiledBitmap::new(2049, 2048, Arc::clone(&resources)),
            Err(PixelError::BudgetExceeded)
        ));
        assert_eq!(observer.snapshot().live_pixel_bytes, before);
        assert_eq!(observer.snapshot().reserved_pixel_bytes, before);
        drop(image);
        assert!(observer.is_released());
        let partial = TiledBitmap::new(10, 10, resources).unwrap();
        assert!(matches!(
            partial.finish(),
            Err(PixelError::IncompleteViewport)
        ));
        assert!(observer.is_released());
    }

    #[test]
    fn failed_later_tile_never_becomes_a_sealed_partial_viewport() {
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut bitmap = TiledBitmap::new(1031, 21, resources).unwrap();
        let first = bitmap.next_tile().unwrap();
        bitmap.paint_tile::<PixelError>(first, |_| Ok(())).unwrap();
        let second = bitmap.next_tile().unwrap();
        let result =
            bitmap.paint_tile::<PixelError>(second, |_| Err(PixelError::Backend("原scene已撤票")));
        assert!(matches!(result, Err(PixelError::Backend("原scene已撤票"))));
        assert_eq!(bitmap.next_tile(), Some(second));
        assert!(matches!(
            bitmap.finish(),
            Err(PixelError::IncompleteViewport)
        ));
        assert!(observer.is_released());
    }
}
