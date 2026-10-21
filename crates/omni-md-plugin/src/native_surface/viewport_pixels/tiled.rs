//! Each bounded tile owns read-only pixels; a complete candidate publishes as a layer group, not one full-frame image.

use super::*;
use std::rc::Rc;

// Current/candidate frames and providers still retained by the framework share this actual-page budget.
// This owned pixel limit does not certify whole-product kernel RSS.
pub(in crate::native_surface) const LIVE_PIXEL_BUDGET: usize = 32 * 1024 * 1024;
pub(in crate::native_surface) const MAX_TILE_SLOTS: usize = 64;
const TILE_EDGE: usize = 512;

/// `y` is the provider's top-down pixel row; `bottom` locates the same tile in Y-up drawing/layer coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native_surface) struct PixelTile {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub bottom: usize,
}

/// Bounded two-pass composition modes. Each needs Native calibration for every actual source color;
/// selecting a mode alone grants no eligibility to encode the pixels as masks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native_surface) enum MaskPairBlend {
    ForegroundThenForeground,
    ForegroundThenBackground,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// The sealer certifies the actual representation; a Scene, raw image, or style cannot authorize masks.
pub(in crate::native_surface) enum PixelPresentation {
    Image,
    /// Every original RGBA pixel is opaque and neutral. Only this tile's white background,
    /// black foreground, and A8 mask may reconstruct it.
    OpaqueNeutralMask,
    /// Every actual RGB value has an exact Native-calibrated alpha pair. Endpoints are byte values,
    /// not a theme name that could authorize unrelated colors.
    OpaquePairedMasks {
        background: [u8; 3],
        foreground: [u8; 3],
        blend: MaskPairBlend,
    },
}

/// One sealed image whose actual drawing context has ended, without another full-frame pixel copy.
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

impl SealedTile {
    /// Both reuse paths clone the same original image/provider pages without old coordinates or Scene authority.
    fn reused_image(&self) -> encoding::EncodedImage {
        encoding::EncodedImage {
            image: self.image.clone(),
            secondary_image: self.secondary_image.clone(),
            presentation: self.presentation,
            storage: self.storage.clone(),
            secondary_storage: self.secondary_storage.clone(),
            reused_image: true,
            #[cfg(test)]
            allocation: Arc::clone(&self.allocation),
            #[cfg(test)]
            row_bytes: self.row_bytes,
        }
    }
}

/// Exists only after every tile succeeds; metadata is bounded and retains no document or Scene.
pub(in crate::native_surface) struct TileFrame {
    pub width: usize,
    pub height: usize,
    pub tiles: Vec<SealedTile>,
    pub reused_images: usize,
}

impl TileFrame {
    /// Compares against the current live frame before allocating an encoded replacement. The page owner,
    /// original read-only storage, and single-plane format must match. A projection fingerprint bounds
    /// repeated comparisons; actual active bytes still decide equality and grant no old Scene authority.
    pub(super) fn reuse_projected_image(
        &self,
        prepared: &encoding::PreparedImage,
    ) -> Result<Option<encoding::EncodedImage>, PixelError> {
        if self.tiles.len() > MAX_TILE_SLOTS
            || self.tiles.iter().any(|tile| {
                !tile.storage.belongs_to(prepared.projection_owner())
                    || tile
                        .secondary_storage
                        .as_ref()
                        .is_some_and(|storage| !storage.belongs_to(prepared.projection_owner()))
            })
        {
            return Ok(None); // seal_region rejects foreign owners at its original boundary, preserving first-error order.
        }
        // Without an equal-sized single-plane candidate, skip the new RGBA scan and encode normally.
        if !self.tiles.iter().any(|original| {
            original.tile.width == prepared.width
                && original.tile.height == prepared.height
                && original.secondary_image.is_none()
                && original.secondary_storage.is_none()
        }) {
            return Ok(None);
        }
        let Some(fingerprint) = prepared.projection_fingerprint()? else {
            return Ok(None);
        };
        // The fingerprint is only a filter. matches_projection checks this actual representation
        // against the new pixels before an old provider is retained for the candidate.
        for original in &self.tiles {
            if original.tile.width == prepared.width
                && original.tile.height == prepared.height
                && original.secondary_image.is_none()
                && original.secondary_storage.is_none()
                && original.storage.matches_projection(
                    prepared,
                    original.presentation,
                    fingerprint,
                )?
            {
                return Ok(Some(original.reused_image()));
            }
        }
        Ok(None)
    }

    /// Borrows an image only from this authenticated live frame after drawing/encoding is complete
    /// and both planes are read-only. Compare before provider/CGImage/indexed-space creation to avoid
    /// temporary Native objects. Format, size, stride, endpoint interpretation, and bytes must match;
    /// neither a hash nor a theme name proves the same visible result.
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
            // An invalid prior frame is not an optimization hit. The later seal_region boundary
            // still reports its actual error instead of laundering an invalid owner into a miss.
            return None;
        }
        let original = self.tiles.iter().find(|original| {
            original.tile.width == width
                && original.tile.height == height
                && original.presentation == presentation
                && original.storage.identical(storage)
                // A paired representation must match both planes; one matching primary is insufficient.
                && match (&original.secondary_storage, secondary) {
                    (None, None) => true,
                    (Some(left), Some(right)) => left.identical(right),
                    _ => false,
                }
        })?;
        // Clone the actual image and its existing provider pages only. The caller returns the newly
        // encoded allocation, avoiding simultaneous duplicate retained representations. Old coordinates,
        // Ready state, and Input authority do not transfer: parent CPU publication still verifies the
        // new Scene's original Native graph and commits the complete candidate atomically.
        Some(original.reused_image())
    }
}

/// Holds bounded mmap/context claims only. Cancellation drops them without retaining a Scene or document.
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
        // A caller may retain the actual context while drawing. Sealing remains blocked until that
        // real claim returns; this clone is not a scalar promise that the external operation ended.
        self.context
            .as_ref()
            .expect("可写tile仍持有原context")
            .clone()
    }
}

/// Its actual context ended, but encoding has not. The candidate can calibrate this bounded palette
/// without holding any pixel-slice borrow across Native calibration.
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

/// A private encoding plan for one original work cell: at most four row regions, advancing the grid once.
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
    // Rc identity binds writable/prepared work to this exact candidate. The shared counters own
    // the real page budget across frames; neither identity nor counters represent Scene authority.
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
            return Err(PixelError::BudgetExceeded); // Do not evade the bounded Native pool with unlimited layers or a truncated tail.
        }
        // Before drawing, include page rounding for every work cell in the complete candidate.
        // Each allocation's CAS also rechecks all live providers; a per-tile check alone is insufficient.
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
            // Existing image claims remain charged while a replacement is prepared. This preflight
            // does not reserve pages; actual allocation still arbitrates the shared live budget.
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
        // The provider scans from the top, while drawing/layer placement uses the tile's Y-up bottom.
        Some(PixelTile {
            x: self.next_x,
            y: self.next_y,
            width,
            height,
            bottom: self.height - self.next_y - height,
        })
    }

    /// Writable work belongs to the original ordered candidate; yielding does not expose images or partial layers.
    pub(in crate::native_surface) fn begin_tile(
        &self,
        tile: PixelTile,
    ) -> Result<WritableTile, PixelError> {
        if self.next_tile() != Some(tile) {
            return Err(PixelError::InvalidGeometry);
        }
        // Charge this real mapping against the shared limit before creating its context. If context
        // creation fails, the local allocation drops through the same actual unmapping owner.
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

    /// Seals only after the original session returns its context; a real external clone prevents sealing.
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
            return Err(PixelError::InvalidGeometry); // Another candidate's writable pages cannot join this complete frame.
        }
        let context = writable.context.take().ok_or(PixelError::ContextRetained)?;
        CGContext::flush(Some(&context));
        // The actual context release callback must return its claim before prepare checks active
        // contexts and seals the mapping read-only. A retained external context causes a real error.
        drop(context);
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

    /// Keeps the single-region control entry intact without silently adding Native calibration requests.
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
        // Reserve one output slot for every untouched original cell before this cell may split.
        let available = MAX_TILE_SLOTS
            .checked_sub(self.tiles.len())
            .and_then(|slots| slots.checked_sub(remaining))
            .filter(|slots| *slots != 0)
            .ok_or(PixelError::InvalidGeometry)?;
        // Splitting this cell cannot consume the tail's reserved slots or truncate the complete frame.
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
        // Encoding may use certified masks or lossless fallbacks, but must return a nonempty bounded
        // partition of this original cell before any completed region joins the private candidate.
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
                return Err(PixelError::InvalidGeometry); // Regions must cover every row exactly once, without gaps or overlap.
            }
            let tile = PixelTile {
                x: original.x,
                y: original.y + region.first_row,
                width: original.width,
                height: region.height,
                bottom: original.bottom + original.height - end,
            };
            // Preserve the original work cell's full width and exact top/down-to-bottom/up placement.
            // Each sealed region owns its actual provider(s); failure drops the whole local batch.
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
        // Publish into the private frame only after every region succeeds; errors leave no half-cell behind.
        self.tiles.extend(completed);
        self.reused_images = total_reused;
        self.next_x += original.width;
        if self.next_x == self.width {
            self.next_x = 0;
            self.next_y += original.height;
        } // Advance the original work cell once; encoding regions add no glyph/context work or early completion.
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
                return Err(PixelError::InvalidGeometry); // Reuse cannot turn the bounded live frame into unbounded history/search.
            }
            if prior.tiles.iter().any(|tile| {
                !tile.storage.belongs_to(&self.counters)
                    || tile
                        .secondary_storage
                        .as_ref()
                        .is_some_and(|storage| !storage.belongs_to(&self.counters))
            }) {
                return Err(PixelError::InvalidGeometry); // Byte-identical foreign pixels cannot bypass this session's real page budget.
            }
        }
        // The encoder compared complete pixels before creating a provider. Attach only new coordinates
        // and the actual reuse result here. Preserve the original prior-frame bounds/owner checks:
        // optimization cannot turn an invalid prior frame into a normal miss or legitimate publication.
        Ok((sealed, reused))
    }

    #[cfg(test)]
    pub(in crate::native_surface) fn paint_tile<E: From<PixelError>>(
        &mut self,
        tile: PixelTile,
        paint: impl FnOnce(&CGContext) -> Result<(), E>,
    ) -> Result<(), E> {
        let writable = self.begin_tile(tile)?;
        paint(writable.context())?; // On the first error, drop writable pages without sealing or publishing them.
        self.finish_tile(writable, None)?;
        Ok(())
    }

    pub(in crate::native_surface) fn finish(self) -> Result<TileFrame, PixelError> {
        if self.next_tile().is_some() {
            return Err(PixelError::IncompleteViewport);
        }
        // Only a fully advanced original grid becomes a frame; completed leading tiles alone are insufficient.
        let frame = TileFrame {
            width: self.width,
            height: self.height,
            tiles: self.tiles,
            reused_images: self.reused_images,
        };
        if frame.reused_images > frame.tiles.len() {
            return Err(PixelError::InvalidGeometry); // Each new tile reuses at most one image result, never a partial-frame receipt.
        }
        Ok(frame)
    }
}

fn tile_storage(width: usize, height: usize) -> Result<(usize, usize), PixelError> {
    // mmap aligns each allocation; independently round its RGBA stride to 16 bytes. No shared
    // full-frame stride forces a partial edge tile to retain padding for unrelated columns.
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
        // Generate actual writable RGBA input: each half fits a palette, while their union exceeds
        // one indexed palette. The encoder must discover any useful split from these real rows.
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null() && width <= 512 && height >= 2);
        CGContext::flush(Some(context));
        for y in 0..height {
            // SAFETY: This real context exclusively owns stride * height bytes; write active RGBA only,
            // without using padding to manufacture extra colors or extending beyond the current row.
            let pixels = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * stride), width * 4)
            };
            for (x, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                *pixel = if y < height / 2 {
                    [x as u8, 0, 0, 255]
                } else {
                    [0, x as u8, 1, 255]
                }; // Each half has at most 256 colors; the whole has more. Splits use integer rows, not theme names.
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
        // Exercise production allocation, context retirement, region planning, and complete-frame sealing.
        // No manually constructed image or resource receipt substitutes for that owner chain.
        let mut bitmap = TiledBitmap::new(width, height, Arc::clone(resources)).unwrap();
        while let Some(tile) = bitmap.next_tile() {
            let writable = bitmap.begin_tile(tile).unwrap();
            paint(writable.context());
            let prepared = bitmap.prepare_tile(writable).unwrap();
            let plan = bitmap.plan_encoding(prepared).unwrap();
            let count = plan.region_count();
            let pairs = [None; 4]; // Pixel controls grant no Native mask certificate; bounded palettes stay indexed.
            bitmap
                .finish_encoding(plan, &pairs[..count], prior)
                .unwrap();
        }
        bitmap.finish().unwrap()
    }

    #[test]
    fn adaptive_odd_height_regions_cover_every_pixel_and_the_last_partial_row() {
        // Draw the same actual input into an independent full bitmap and the production tile.
        // Odd height makes both the region seam and the final partial row observable.
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
        // SAFETY: The independent tight RGBA oracle remains live and flushed; read all active rows,
        // including the uneven last row and split seam, within this synchronous comparison.
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
        // dense_rgba places transparency after palette overflow; the whole input must be checked
        // before planning can select an opaque representation or split away a counterexample.
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
        // Use real tiny mappings where separate page rounding outweighs the smaller encoded byte counts.
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut bitmap = TiledBitmap::new(256, 2, resources).unwrap();
        let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        horizontal_palettes(writable.context());
        let prepared = bitmap.prepare_tile(writable).unwrap();
        let plan = bitmap.plan_encoding(prepared).unwrap();
        assert_eq!(plan.region_count(), 1); // Two indexed rows each need a page, exceeding one RGB24 allocation.
        bitmap.finish_encoding(plan, &[None], None).unwrap();
        assert_eq!(CGImage::bits_per_pixel(Some(&bitmap.tiles[0].image)), 24);
        drop(bitmap.finish().unwrap());
        assert!(observer.is_released());
    }

    #[test]
    fn adaptive_planning_reserves_a_slot_for_every_remaining_original_work_cell() {
        // The complete grid already needs every slot. A smaller encoding for the first cell must
        // not consume the slot reserved for a real but not-yet-drawn tail cell.
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let mut bitmap = TiledBitmap::new(63 * 512 + 256, 256, resources).unwrap();
        let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        horizontal_palettes(writable.context());
        let prepared = bitmap.prepare_tile(writable).unwrap();
        let plan = bitmap.plan_encoding(prepared).unwrap();
        assert_eq!(plan.region_count(), 1); // Saving pages cannot exceed the complete grid's 64-slot limit.
        bitmap.finish_encoding(plan, &[None], None).unwrap();
        assert_eq!(bitmap.tiles.len(), 1);
        assert_eq!(bitmap.next_tile().unwrap().x, 512);
        drop(bitmap); // Cancel the untouched tail explicitly; a leading partial candidate is not a complete-frame pass.
        assert!(observer.is_released());
    }

    #[test]
    fn adaptive_second_allocation_budget_miss_returns_all_temporary_pages_before_fallback() {
        // Hold real pages so one indexed region fits but a second does not. The fallback must return
        // the partial encoding before retaining the original RGBA, without exceeding peak reservation.
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
        // Reuse both byte-identical regions, then change one actual source pixel so only its region
        // loses reuse. All frames share real providers rather than fabricated image identities.
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
        // Integer boundaries partition every row once, including an uneven fourth band. Different
        // band colors make four palettes useful only after a two-region plan cannot save real pages.
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null() && height >= 4);
        CGContext::flush(Some(context));
        for band in 0..4 {
            for y in height * band / 4..height * (band + 1) / 4 {
                // SAFETY: The four integer row ranges exactly cover this bounded context; write no padding.
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
        // Compare the four-region reconstruction with an independent actual RGBA drawing, including
        // all three seams and the uneven last region; metadata counts alone cannot prove fidelity.
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
        // SAFETY: The independent tight 509² RGBA oracle is live and flushed; read every active pixel.
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
        // Cancel after the context has ended but before any region provider is constructed. Dropping
        // the plan must return the actual original mapping, even while the candidate metadata remains.
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let bitmap = TiledBitmap::new(509, 509, resources).unwrap();
        let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        four_horizontal_palettes(writable.context());
        let prepared = bitmap.prepare_tile(writable).unwrap();
        let plan = bitmap.plan_encoding(prepared).unwrap();
        assert_eq!(plan.region_count(), 4);
        assert_eq!(observer.snapshot().live_pixel_buffers, 1);
        drop(plan); // Only the original read-only allocation and bounded metadata exist; no probe provider may survive.
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
        // The single-region production owner path lets reuse/lifetime tests retain actual CGImages
        // without constructing a SealedTile or bypassing context release and read-only sealing.
        let mut bitmap = TiledBitmap::new(width, height, Arc::clone(resources)).unwrap();
        while let Some(tile) = bitmap.next_tile() {
            let writable = bitmap.begin_tile(tile).unwrap();
            paint(writable.context());
            bitmap.finish_tile(writable, prior).unwrap();
        } // Use real context completion/sealing, with no substitute image or fabricated SealedTile.
        bitmap.finish().unwrap()
    }

    fn change_first_red(context: &CGContext) {
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null());
        // SAFETY: This sole writable context covers the first RGBA pixel; no provider/external context reads it.
        unsafe { *data.cast::<u8>() = (*data.cast::<u8>()).wrapping_add(1) };
    }

    #[test]
    fn prepared_opaque_palette_is_a_proposal_and_transparent_pixels_cannot_request_masks() {
        // Preparation proposes actual opaque colors for later Native calibration. Transparency
        // prevents that proposal; neither branch invents a calibration certificate from theme data.
        for transparent in [false, true] {
            let resources = Arc::new(NativeMarkdownViewportPixels::default());
            let observer = resources.observer();
            let bitmap = TiledBitmap::new(32, 32, resources).unwrap();
            let writable = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
            palette_rgba(writable.context());
            if transparent {
                let data = objc2_core_graphics::CGBitmapContextGetData(Some(writable.context()));
                // SAFETY: This actual context exclusively owns the first pixel before any image/provider;
                // replace it with valid premultiplied translucent black.
                unsafe {
                    std::ptr::copy_nonoverlapping([0u8, 0, 0, 128].as_ptr(), data.cast::<u8>(), 4)
                };
            }
            let prepared = bitmap.prepare_tile(writable).unwrap();
            assert_eq!(prepared.mask_request().is_none(), transparent);
            assert_eq!(observer.snapshot().live_pixel_buffers, 1);
            drop((prepared, bitmap));
            assert!(observer.is_released()); // Proposing a palette creates no fake masks or cancelled RGBA retention.
        }
    }

    #[test]
    fn exact_sealed_rgba_reuses_original_image_and_releases_duplicate_mapping() {
        // Identical drawing shares the actual previous image, so the duplicate candidate mapping
        // returns. Dropping one frame must still preserve pixels held by the surviving image owner.
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
        assert!(!observer.is_released()); // The current image still owns the original pages after the old frame drops.
        drop(next);
        assert!(observer.is_released());
    }

    #[test]
    fn one_real_rgba_byte_difference_does_not_reuse_the_prior_image() {
        // Change actual writable RGBA before sealing, rather than mutating a private reuse marker.
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
        // Keep index occurrence order while changing actual palette colors. Equal index bytes
        // must not let reuse silently display the previous palette's different visible image.
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
                    // SAFETY: The sole actual context covers these 32 rows; change only active blue channels,
                    // preserving index occurrence order without touching padding or sealed storage.
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
        // Two destination positions may share one exact neutral image, but each output tile must
        // retain its newly assigned coordinates instead of inheriting the prior tile's location.
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
        // Draw equal pixels in two real accounting scopes. Foreign ownership must fail at sealing,
        // return the new candidate pages, and leave the original scope's still-live provider charged.
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
        assert!(current_observer.is_released()); // Actual new pages return; foreign live pages remain in their own scope.
        assert!(!original_observer.is_released());
        drop((original, current));
        assert!(original_observer.is_released());
    }

    fn dense_opaque_rgba(context: &CGContext) {
        // Generate more than 256 actual opaque colors for lossless RGB24 coverage, including tail
        // rows/columns and padded strides. This input is independent of the encoder's output format.
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null() && width <= 512 && height <= 512 && stride >= width * 4);
        CGContext::flush(Some(context));
        for y in 0..height {
            // SAFETY: The sole context exposes stride * height RGBA bytes; write only this active row.
            let row = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * stride), width * 4)
            };
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                *pixel = [x as u8, y as u8, ((x >> 8) * 2 + (y >> 8)) as u8, 255];
            }
        } // More than 256 colors require complete opacity proof before lossless RGB24 can replace RGBA.
    }

    fn dense_rgba(context: &CGContext) {
        dense_opaque_rgba(context);
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        // SAFETY: The sole actual context covers the entire layout; change only the last active
        // pixel to valid premultiplied translucent black, not row padding.
        unsafe {
            std::ptr::copy_nonoverlapping(
                [0u8, 0, 0, 128].as_ptr(),
                data.cast::<u8>()
                    .add((height - 1) * stride + (width - 1) * 4),
                4,
            )
        }; // Transparency appears long after palette overflow; early exit cannot incorrectly authorize RGB24.
    }

    fn palette_rgba(context: &CGContext) {
        // Generate bounded real color input without consulting the candidate's indexed representation.
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null() && width <= 512 && height <= 512 && stride >= width * 4);
        CGContext::flush(Some(context));
        for y in 0..height {
            // SAFETY: Generate bounded test pixels in the actual writable context, within active row bytes.
            let row = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * stride), width * 4)
            };
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let value = (x + y) as u8;
                *pixel = [value, 255 - value, value / 2, 255];
            }
        }
    }

    /// Writes all 256 original gray values into a real context, without deriving input from the A8 encoder.
    fn neutral_rgba(context: &CGContext) {
        let width = objc2_core_graphics::CGBitmapContextGetWidth(Some(context));
        let height = objc2_core_graphics::CGBitmapContextGetHeight(Some(context));
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(context));
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(context));
        assert!(!data.is_null() && stride >= width * 4);
        CGContext::flush(Some(context));
        for y in 0..height {
            // SAFETY: The sole writable context covers this active row; no provider/external context owns these pixels.
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
        // Independently draw all gray levels, then reconstruct the actual sealed A8 mask through
        // Core Graphics and require every visible RGBA byte to match before checking final release.
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
        // SAFETY: The independent RGBA oracle remains live; read active pixels without using the mask's encoding data.
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
        // Either one non-neutral color or one translucent pixel invalidates the whole tile's neutral
        // mask certificate. The fallback must preserve that counterexample, not quantize or whiten it.
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
                    // SAFETY: The actual context exclusively owns the first four-byte pixel before sealing.
                    unsafe { std::ptr::copy_nonoverlapping(pixel.as_ptr(), data.cast::<u8>(), 4) };
                    Ok(())
                })
                .unwrap();
            assert_eq!(bitmap.tiles[0].presentation, PixelPresentation::Image);
            assert!(!CGImage::is_mask(Some(&bitmap.tiles[0].image)));
            // A 257th opaque color can use exact RGB24; any transparency still requires RGBA.
            assert_eq!(
                CGImage::bits_per_pixel(Some(&bitmap.tiles[0].image)),
                if colored { 24 } else { 32 }
            );
        }
    }

    #[test]
    fn actual_indexed_color_image_roundtrips_all_rgba_and_releases_the_original_pages() {
        // Use an independent full RGBA drawing to check actual Native indexed-color decoding.
        // The final release assertion observes real mappings/providers, not just a smaller format tag.
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
        // SAFETY: The bounded RGBA oracle is live and flushed; read every active byte for Native decode comparison.
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
        // A real held mapping leaves room for the original RGBA only. Optional indexed storage must
        // fail within the same budget and retain complete RGBA without losing the valid drawing.
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
        // No room remains for an extra indexed representation: keep complete RGBA without crossing the shared limit.
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
        // The final translucent pixel must survive even after earlier rows exceed palette capacity.
        // Compare actual decoded RGBA rather than checking only the selected bits-per-pixel value.
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
        // SAFETY: The live independent RGBA reference covers only this finite test image's active pixels.
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
        // Width 509 requires RGB24 row padding. Independent Native reconstruction covers the last
        // active pixel/row, so a wrong stride cannot hide behind correct image metadata.
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
        // SAFETY: The tight RGBA oracle remains live and flushed; read all active rows and the final pixel.
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
        // A real reserved allocation exhausts optional conversion headroom while the original RGBA
        // still fits. Lack of temporary RGB24 pages must not discard the complete candidate.
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
        // RGB24 also allocates real pages; without headroom, preserve valid RGBA and every source color.
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
        // Keep an actual CGImage claim beyond frame destruction, then an explicit allocation claim
        // beyond provider return. These are different lifetime boundaries and must not report early munmap.
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
        drop(last_image); // Only the final actual CGImage claim permits its provider's release callback.
        assert!(allocation.provider_returned.load(Ordering::Acquire));
        assert_eq!(observer.snapshot().live_pixel_buffers, 1); // The explicit allocation claim still prevents munmap.
        drop(allocation);
        assert!(weak.upgrade().is_none());
        assert!(observer.is_released());
    }

    #[test]
    fn equal_rgb24_reuses_the_real_image_but_one_rgb_byte_change_does_not() {
        // Equal padded RGB24 can reuse its original image, while a single actual RGB-byte change
        // requires a different image. Verify visible output and final provider lifetime below.
        let resources = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = resources.observer();
        let original = frame_with_prior(509, 31, &resources, dense_opaque_rgba, None);
        let same = frame_with_prior(509, 31, &resources, dense_opaque_rgba, Some(&original));
        let changed = frame_with_prior(
            509,
            31,
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
        // Padded RGB rows exercise final pixels and stride handling. Independently draw the original,
        // reused, and changed cases into RGBA. CGImage identity or a reuse flag alone cannot prove
        // that an incorrectly projected image is visually equal to the newly drawn source.
        for (frame, change) in [(&original, false), (&same, false), (&changed, true)] {
            let expected = Bitmap::new(509, 31, Arc::default()).unwrap();
            dense_opaque_rgba(expected.context());
            if change {
                change_first_red(expected.context());
            }
            let rendered = Bitmap::new(509, 31, Arc::default()).unwrap();
            CGContext::draw_image(
                Some(rendered.context()),
                CGRect::new(
                    objc2_core_foundation::CGPoint::new(0.0, 0.0),
                    objc2_core_foundation::CGSize::new(509.0, 31.0),
                ),
                Some(&frame.tiles[0].image),
            );
            CGContext::flush(Some(expected.context()));
            CGContext::flush(Some(rendered.context()));
            // SAFETY: Both real RGBA contexts and tight mappings remain live/flushed during this finite read.
            let (actual, expected_bytes) = unsafe {
                (
                    std::slice::from_raw_parts(
                        rendered.allocation.address.as_ptr().cast::<u8>(),
                        509 * 31 * 4,
                    ),
                    std::slice::from_raw_parts(
                        expected.allocation.address.as_ptr().cast::<u8>(),
                        509 * 31 * 4,
                    ),
                )
            };
            assert_eq!(actual, expected_bytes);
        }
        assert_eq!(observer.snapshot().live_pixel_buffers, 2);
        let allocation = Arc::clone(&same.tiles[0].allocation);
        let last_image = same.tiles[0].image.clone();
        drop((original, same, changed));
        // After both frame owners return, the last actual image claim must still keep its provider
        // alive. Only returning that image and the explicit allocation claim permits real unmapping.
        assert!(!allocation.provider_returned.load(Ordering::Acquire));
        drop(last_image);
        assert!(allocation.provider_returned.load(Ordering::Acquire));
        drop(allocation);
        assert!(observer.is_released());
    }

    #[test]
    fn actual_transparency_keeps_rgba_and_only_a_true_constant_can_use_one_pixel() {
        // A uniform translucent image may use an exact one-pixel representation. Partial coverage
        // must retain spatial RGBA information; both cases are compared through actual Native decoding.
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
            // SAFETY: The live finite oracle retains original premultiplied bytes for exact RGBA comparison.
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
        // Independent geometric input crosses tile seams and paints the final partial corner.
        // Include translucency so compositing/placement errors remain visible in the RGBA comparison.
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
        // Retain the actual CGContext, not a synthetic active flag. Finishing must reject sealing,
        // preserve the original mapping for that real user, and release only after the claim drops.
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
        drop(bitmap); // No sealed image exists, but the real retained context still prevents mapping release.
        assert!(!observer.is_released());
        drop(pending_claim);
        assert!(observer.is_released());
    }

    #[test]
    fn identical_geometry_and_shared_budget_cannot_authorize_a_foreign_writable_tile() {
        // Two real candidates share dimensions and accounting, but only the originating candidate
        // may consume its writable tile. Rejecting the foreign identity must also return its resources.
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
        // Cancel with one real sealed image and one actual pending context. Both owner chains must
        // unwind even though no complete frame was ever published to Native layers.
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let mut bitmap = TiledBitmap::new(900, 24, counters).unwrap();
        let first = bitmap.next_tile().unwrap();
        let writable = bitmap.begin_tile(first).unwrap();
        bitmap.finish_tile(writable, None).unwrap();
        let pending = bitmap.begin_tile(bitmap.next_tile().unwrap()).unwrap();
        assert_eq!(observer.snapshot().live_pixel_buffers, 2);
        drop(pending); // End the writable tile's actual context before returning its mapping.
        drop(bitmap); // Completed images are still private; their final provider releases return their mappings.
        assert!(observer.is_released());
    }

    #[test]
    fn actual_tiled_rgba_matches_full_context_at_all_seams_and_last_partial_tiles() {
        // Draw identical geometric input through an independent full context and the real tile path.
        // Translate each work cell by its actual Y-up bottom so seams and partial edge tiles are compared.
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
        // SAFETY: The synchronous oracle owns a flushed tight RGBA mapping; read its finite active rectangle.
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
        // This controls the pixel backend only. Nominal glyph lookup does not certify product shaping;
        // the actual editor still consumes its already Ready shaping spool.
        // SAFETY: The bounded font name is a valid CFString; nil matrix uses the public default.
        let emoji = unsafe {
            CTFont::with_name(
                &CFString::from_static_str("Apple Color Emoji"),
                80.0,
                ptr::null(),
            )
        };
        // SAFETY: This bounded system-font control uses the public default matrix and creates no body string/CTLine.
        let latin = unsafe {
            CTFont::with_name(&CFString::from_static_str("Helvetica"), 80.0, ptr::null())
        };
        // Check the actual live font's name so a substituted fallback cannot count as color-emoji evidence.
        assert_eq!(
            // SAFETY: The synchronous stack owns this actual CTFont; the public name getter borrows no body buffer.
            unsafe { emoji.post_script_name() }.to_string(),
            "AppleColorEmoji"
        );
        let glyph = |font: &CTFont, scalar: u16| {
            let mut source = scalar;
            let mut output = 0u16;
            // SAFETY: Valid one-unit stack pointers cover count=1; both characters are BMP scalars, not split surrogates.
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
            // SAFETY: Each draw reads one actual font glyph and position into a bounded writable context.
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
        // SAFETY: The flushed allocations remain on this stack; read finite active RGBA only,
        // without source-document materialization or row-padding reads.
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
        // A blank or monochrome fallback image cannot pass as successful color-glyph rendering.
        assert!(
            colored > 100,
            "必须实际画出彩色emoji像素，不能以空白低内存过门"
        );
        let mismatch = active_pixel_mismatch(&tiled, expected);
        assert_eq!(mismatch, None, "有限彩色字形的首个RGBA差异字节");
        drop(tiled.finish().unwrap());
    }

    fn active_pixel_mismatch(bitmap: &TiledBitmap, tightly_packed: &[u8]) -> Option<usize> {
        // Reconstruct real sealed images through Core Graphics into a separate finite RGBA oracle.
        // Do not decode from the encoder's own palette/storage, which could repeat the same bug.
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
            // Independently exercise actual CG mask decoding. Native CALayer composition requires
            // its separate real-window acceptance and is not certified by this reconstruction.
            if image.presentation == PixelPresentation::OpaqueNeutralMask {
                CGContext::save_g_state(Some(rendered.context()));
                CGContext::set_rgb_fill_color(Some(rendered.context()), 1.0, 1.0, 1.0, 1.0);
                CGContext::fill_rect(Some(rendered.context()), rect);
                CGContext::clip_to_mask(Some(rendered.context()), rect, Some(&image.image));
                CGContext::set_rgb_fill_color(Some(rendered.context()), 0.0, 0.0, 0.0, 1.0);
                CGContext::fill_rect(Some(rendered.context()), rect);
                CGContext::restore_g_state(Some(rendered.context()));
            } else {
                // Ordinary images retain actual color-space, alpha, and one-pixel constant-image scaling semantics.
                CGContext::draw_image(Some(rendered.context()), rect, Some(&image.image));
            }
        }
        CGContext::flush(Some(rendered.context()));
        // SAFETY: The finite oracle owns a complete RGBA mmap; after flush, read active bytes without padding.
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
        // Produce a complete real 3×3 tile grid. Its constant images may each shrink to one pixel,
        // but must remain separately owned provider mappings instead of a hidden full-frame image.
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
            // The actual empty drawing is uniform; only the encoder's complete pixel check allows 1×1 storage.
            assert_eq!(CGImage::width(Some(&tile.image)), 1);
            assert_eq!(CGImage::height(Some(&tile.image)), 1);
            assert!(tile.tile.width <= 512 && tile.tile.height <= 512);
            assert!(tile.allocation.length <= 512 * 512 * 4);
            assert!(
                frame.tiles[..index]
                    .iter()
                    .all(|prior| prior.allocation.address != tile.allocation.address)
            );
        } // Each constant representation keeps its own mmap/provider, without one persistent full-viewport allocation.
        drop(frame);
        assert!(observer.is_released());
        assert!(matches!(
            TiledBitmap::new(usize::MAX, 1, counters),
            Err(PixelError::BudgetExceeded)
        ));
        assert!(observer.is_released()); // Excess Native slots fail before pixel allocation, without truncation/downsampling.
    }

    #[test]
    fn whole_live_budget_counts_old_images_and_rejects_candidate_without_partial_publication() {
        // Retain an actual opaque/translucent RGBA frame while attempting another full candidate.
        // Old provider pages remain charged, so the new candidate fails before any partial allocation.
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
        let image = first.finish().unwrap(); // Every first-frame provider is still live and charged to the shared budget.
        let before = observer.snapshot().live_pixel_bytes;
        assert!(matches!(
            TiledBitmap::new(2049, 2048, Arc::clone(&resources)),
            Err(PixelError::BudgetExceeded)
        ));
        assert_eq!(observer.snapshot().live_pixel_bytes, before);
        assert_eq!(observer.snapshot().reserved_pixel_bytes, before);
        drop(image);
        assert!(observer.is_released());
        // A newly allocated candidate with no completed work cell still cannot become a complete frame.
        let partial = TiledBitmap::new(10, 10, resources).unwrap();
        assert!(matches!(
            partial.finish(),
            Err(PixelError::IncompleteViewport)
        ));
        assert!(observer.is_released());
    }
}
