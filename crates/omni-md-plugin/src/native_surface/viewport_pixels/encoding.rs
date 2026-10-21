//! Losslessly encode finite RGBA tiles after their contexts actually finish.
//! Preserve original alpha and exact multicolor values without quantization.

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
    // Record reuse of an already encoded live frame only. Reuse neither
    // increments paint completion nor extends an old Scene's authority.
    pub reused_image: bool,
    #[cfg(test)]
    pub allocation: Arc<PixelAllocation>,
    #[cfg(test)]
    pub row_bytes: usize,
}

/// Fixed 256-color palette and half-full hash directory, populated only from
/// actual RGBA values. Capacity never scales with document or viewport length.
struct Palette {
    colors: [u32; 256],
    slots: [u16; 512],
    count: usize,
}
impl Palette {
    /// Empty fixed storage requires no document-sized allocation.
    fn new() -> Self {
        Self {
            colors: [0; 256],
            slots: [u16::MAX; 512],
            count: 0,
        }
    }
    /// Return an exact color ordinal or its first empty insertion slot.
    fn locate(&self, color: u32) -> Result<usize, usize> {
        let mut slot = (color.wrapping_mul(2_654_435_761) >> 23) as usize;
        // A half-full directory has an empty slot within this fixed bound;
        // collisions never grow an auxiliary heap container.
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
    /// Preserve first-encounter order and reject overflow instead of losing ink.
    fn insert(&mut self, color: u32) -> bool {
        match self.locate(color) {
            Ok(_) => true,
            Err(slot) if self.count < 256 => {
                self.colors[self.count] = color;
                self.slots[slot] = self.count as u16;
                self.count += 1;
                true
            }
            // The 257th color ends only indexing eligibility. Classification
            // must still inspect alpha; no quantization or emoji removal applies.
            Err(_) => false,
        }
    }
}

/// Exact pixel facts determine representation; format names grant no Scene or
/// Native calibration authority by themselves.
enum Plan {
    Rgba,
    Rgb24,
    Solid([u8; 4]),
    Neutral,
    Indexed(Box<Palette>),
}

/// Mergeable facts for a contiguous row interval. Even solid/neutral summaries
/// retain actual encountered colors rather than inventing a grayscale palette.
struct RegionScan {
    first: [u8; 4],
    solid: bool,
    opaque: bool,
    neutral: bool,
    palette_complete: bool,
    palette: Palette,
}

impl RegionScan {
    /// Start from this interval's first actual RGBA pixel.
    fn new(first: [u8; 4]) -> Self {
        Self {
            first,
            solid: true,
            opaque: true,
            neutral: true,
            palette_complete: true,
            palette: Palette::new(),
        }
    }

    /// Fold one actual pixel into the interval's exact classification facts.
    fn observe(&mut self, pixel: [u8; 4]) {
        self.solid &= pixel == self.first;
        self.opaque &= pixel[3] == 255;
        self.neutral &= pixel[3] == 255 && pixel[0] == pixel[1] && pixel[1] == pixel[2];
        if self.palette_complete && !self.palette.insert(rgb(&pixel)) {
            self.palette_complete = false;
        } // Palette overflow stays sticky; later alpha/solid checks still govern RGB24 eligibility.
    }

    /// Combine adjacent intervals from their finite facts without rereading
    /// RGBA pages or introducing a second classification pass.
    fn merge(left: &Self, right: &Self) -> Self {
        let mut result = Self {
            first: left.first,
            solid: left.solid && right.solid && left.first == right.first,
            opaque: left.opaque && right.opaque,
            neutral: left.neutral && right.neutral,
            palette_complete: left.palette_complete && right.palette_complete,
            palette: Palette::new(),
        };
        // An overflowing child cannot yield a smaller complete union. Merge
        // only two still-complete actual palettes, in first-encounter order.
        if result.palette_complete {
            for color in left.palette.colors[..left.palette.count]
                .iter()
                .chain(&right.palette.colors[..right.palette.count])
            {
                if !result.palette.insert(*color) {
                    result.palette_complete = false;
                    break;
                }
            } // Append genuinely new right colors after left colors, without sorting or quantization.
        }
        result
    }

    /// Choose the eligible representation in the original priority order;
    /// transparent multicolor content always retains RGBA.
    fn into_plan(self) -> Plan {
        if self.solid {
            Plan::Solid(self.first) // Identical RGBA, including alpha, permits the original 1x1 image.
        } else if !self.opaque {
            Plan::Rgba // Non-solid transparency cannot use an alpha-free representation.
        } else if self.neutral {
            Plan::Neutral // Every actual pixel passed the opaque-neutral gate.
        } else if self.palette_complete {
            Plan::Indexed(Box::new(self.palette)) // Only a complete real palette may request Native pairing.
        } else {
            Plan::Rgb24 // Complete opacity with palette overflow permits lossless RGB24.
        }
    }
}

/// Keep fixed summaries temporarily for complete opaque multicolor tiles only;
/// they own no additional pixels, Native objects, or Source authority.
struct RegionScans {
    count: usize,
    parts: [Option<RegionScan>; 4],
}

struct PreparedClassification {
    plan: Plan,
    regions: Option<Box<RegionScans>>,
}

/// Produce whole-tile classification and exact region summaries during the same
/// preparation pass. Later region planning need not reread RGBA pages.
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
    }; // Positive-height intervals only: 2/3-row input cannot manufacture empty quarters.
    let first: [u8; 4] = row(allocation, 0, width, row_bytes)[..4]
        .try_into()
        .unwrap();
    if first[3] != 255 {
        // A transparent first pixel permits only a constant RGBA pixel or the
        // original RGBA tile. A different pixel settles the decision immediately.
        for y in 0..height {
            for pixel in row(allocation, y, width, row_bytes).as_chunks::<4>().0 {
                if *pixel != first {
                    return Ok(PreparedClassification {
                        plan: Plan::Rgba,
                        regions: None,
                    });
                }
            }
        }
        return Ok(PreparedClassification {
            plan: Plan::Solid(first),
            regions: None,
        }); // Identical pixels include alpha; no impossible RGB24 summaries were allocated.
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
                if pixel[3] != 255 {
                    return Ok(PreparedClassification {
                        plan: Plan::Rgba,
                        regions: None,
                    }); // A transparent pixel differs from the opaque first pixel: preserve original RGBA.
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
    }; // Merge actual first-encounter palettes without changing opacity/solid priority.
    let plan = whole.into_plan();
    let regions =
        (matches!(plan, Plan::Rgb24) && count > 1).then(|| Box::new(RegionScans { count, parts }));
    // Only RGB24 retains temporary region metadata until this tile's plan is chosen.
    Ok(PreparedClassification { plan, regions })
}

/// The actual context returned and RGBA pages are read-only. A candidate may
/// perform finite Native calibration without holding a pixel slice across it.
pub(super) struct PreparedImage {
    original: Arc<PixelAllocation>,
    pub(super) width: usize,
    pub(super) height: usize,
    row_bytes: usize,
    first_row: usize,
    plan: Plan,
    regions: Option<Box<RegionScans>>,
}
impl PreparedImage {
    /// Project read-only original RGBA without an output allocation or Native
    /// object. A fingerprint filters finite old tiles only; reuse still checks
    /// format, complete palette, and every active projected output byte.
    pub(super) fn projection_fingerprint(&self) -> Result<Option<u64>, PixelError> {
        reuse::projection_fingerprint(self)
    }

    /// Reuse must remain inside this actual allocation owner and page budget.
    pub(super) fn projection_owner(&self) -> &Arc<NativeMarkdownViewportPixels> {
        &self.original.counters
    }

    /// Only actual indexed colors propose a Native calibration request.
    pub(super) fn mask_request(&self) -> Option<MaskPaletteRequest> {
        match &self.plan {
            Plan::Indexed(palette) => MaskPaletteRequest::new(&palette.colors[..palette.count]),
            Plan::Rgba | Plan::Rgb24 | Plan::Solid(_) | Plan::Neutral => None,
        } // Transparent, solid, and neutral forms need no additional Native calibration.
    }
}

fn rgb(pixel: &[u8]) -> u32 {
    // Pack explicit RGB components, independent of host byte order and alpha.
    (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2])
}

fn row(allocation: &PixelAllocation, y: usize, width: usize, row_bytes: usize) -> &[u8] {
    // SAFETY: Callers establish the complete width/height/stride extent. Product
    // preparation requires all actual contexts returned; isolated CG oracles
    // instead read after synchronous flush while no Native write is active.
    // The row belongs to the original tile or an admitted contiguous subregion.
    // This Rust-only borrow spans no Native callback, exports no writable
    // pointer, and grants no Source authority.
    unsafe {
        std::slice::from_raw_parts(
            allocation.address.as_ptr().cast::<u8>().add(y * row_bytes),
            width * 4,
        )
    }
}

/// Encoding and preallocation comparison share this exact projection. Source is
/// one admitted active RGBA row; target is an equal-width A8/indexed row. No
/// approximate decoder or weaker alpha/palette eligibility applies.
fn project_byte_row(
    source: &[u8],
    target: &mut [u8],
    palette: Option<&Palette>,
) -> Result<(), PixelError> {
    for (to, pixel) in target.iter_mut().zip(source.as_chunks::<4>().0) {
        *to = if let Some(palette) = palette {
            // Every exact RGB must belong to the complete actual palette.
            palette
                .locate(rgb(pixel))
                .map_err(|_| PixelError::Backend("原像素不属于已验证色表"))? as u8
        } else {
            255 - pixel[0] // Only complete opaque-neutral classification admits this branch.
        };
    }
    Ok(())
}

/// RGB24 omits only alpha already proven constant at 255, preserving every
/// original sRGB component byte.
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

/// At most 512 columns fit in this one fixed stack row for all three projected
/// encodings. PreparedImage retains the actual original RGBA and row interval;
/// comparison creates no complete compressed copy, Native call, or escaped slice.
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
        _ => Err(PixelError::InvalidGeometry), // RGBA, solid, and paired masks keep their own paths.
    }
}

#[cfg(test)]
/// Existing resource/image regressions use the actual preparation and seal path.
pub(super) fn finish(
    original: Arc<PixelAllocation>,
    width: usize,
    height: usize,
    row_bytes: usize,
) -> Result<EncodedImage, PixelError> {
    finish_prepared(prepare(original, width, height, row_bytes)?, None)
}

/// Validate/classify the complete finite tile, then seal original RGBA read-only
/// before any Native calibration may inspect its actual output.
pub(super) fn prepare(
    original: Arc<PixelAllocation>,
    width: usize,
    height: usize,
    row_bytes: usize,
) -> Result<PreparedImage, PixelError> {
    if original.active_contexts.load(Ordering::Acquire) != 0 {
        // A surviving writable context prevents immutable publication.
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
        // Reject invalid tile/stride/storage extents before raw row access.
        return Err(PixelError::InvalidGeometry);
    }
    let classification = classify(&original, width, height, row_bytes)?;
    paired::readonly(&original)?; // Native calibration must never see mutable original RGBA.
    Ok(PreparedImage {
        original,
        width,
        height,
        row_bytes,
        first_row: 0,
        plan: classification.plan,
        regions: classification.regions,
    })
}

#[cfg(test)]
/// Tests seal through the real output allocator and provider ownership boundary.
pub(super) fn finish_prepared(
    prepared: PreparedImage,
    pairs: Option<&CertifiedMaskPairs>,
) -> Result<EncodedImage, PixelError> {
    finish_with_allocations(prepared, pairs, None, None)
}

/// Private regions acquire all output pages before sealing. That phase cannot
/// allocate another page or treat a local region as a complete RGBA fallback.
#[derive(Default)]
struct OutputAllocations([Option<Arc<PixelAllocation>>; 2]);

/// Consume a pre-admitted output page once, or allocate under the same aggregate
/// owner budget for the unsplit representation.
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
        ); // Only budget exhaustion permits keeping the original RGBA on the unsplit path.
    };
    let allocation = allocations.0[index]
        .take()
        .ok_or(PixelError::InvalidGeometry)?;
    if allocation.length != allocation_length(length)?
        || !Arc::ptr_eq(&allocation.counters, &original.counters)
        || allocation.active_contexts.load(Ordering::Acquire) != 0
        || allocation.provider_returned.load(Ordering::Acquire)
    {
        // An output page must retain the exact rounded length, original owner,
        // and fresh no-writer/no-provider state promised by its region plan.
        return Err(PixelError::InvalidGeometry);
    }
    Ok(allocation) // Fresh private pages belong to this region's encoder and are consumed once.
}

/// Require consumption of the complete planned page group. Reuse also requires
/// actual disposal of any newly created temporary allocations.
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
        // Extra pages reveal a plan/format mismatch; do not silently hide an
        // unconsumed allocation claim behind a successful image result.
        return Err(PixelError::InvalidGeometry);
    }
    if encoded.reused_image && encoded.storage.release_failed() {
        return Err(PixelError::Backend("等值候选像素页未能真实归还"));
    } // Inner temporary Arcs have dropped; reuse cannot hide a real munmap failure.
    Ok(encoded)
}

/// Encode the established representation. Reuse follows actual paint and exact
/// classification; budget fallback preserves complete content and real backend
/// failures are never converted to successful optimization misses.
fn finish_prepared_inner(
    mut prepared: PreparedImage,
    pairs: Option<&CertifiedMaskPairs>,
    allocations: &mut Option<OutputAllocations>,
    prior: Option<&TileFrame>,
) -> Result<EncodedImage, PixelError> {
    drop(prepared.regions.take()); // Return unused region metadata before single-image CF construction.
    if pairs.is_some() && !matches!(&prepared.plan, Plan::Indexed(_)) {
        return Err(PixelError::InvalidGeometry); // A palette certificate cannot authorize transparent/solid/neutral forms.
    }
    if allocations.is_none()
        && pairs.is_none()
        && let Some(reused) = prior
            .map(|prior| prior.reuse_projected_image(&prepared))
            .transpose()?
            .flatten()
    {
        // Actual glyph paint/classification already completed. Exact projection
        // reuse avoids throwaway output pages only; region pre-reservations and
        // paired-mask certification retain their original admission/ordering.
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
            return Err(PixelError::InvalidGeometry); // Equal endpoints cannot certify another actual palette.
        }
        if let Some(encoded) = encode_paired(&prepared, pairs, allocations, prior)? {
            drop(prepared);
            if encoded.storage.release_failed() {
                return Err(PixelError::Backend("双遮罩原RGBA未能真实归还"));
            }
            return Ok(encoded);
        } // Only additional-page budget exhaustion permits fallback; real release/construction failures remain errors.
    }
    let PreparedImage {
        original,
        width,
        height,
        row_bytes,
        first_row,
        plan,
        regions: _,
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
    // Temporary output shares the live budget with current/old-provider/candidate
    // pages; an optimization cannot exempt simultaneous real mappings.
    let allocation = match output_allocation(&original, length, allocations, 0) {
        Ok(value) => value,
        Err(PixelError::BudgetExceeded) if allocations.is_none() && first_row == 0 => {
            return rgba(original, width, height, row_bytes, prior);
        }
        Err(error) => return Err(error), // Preserve real allocation/cleanup failure without Source substitution or replay.
    };
    if let Some(value) = solid {
        // SAFETY: New private storage covers at least four bytes and has no
        // context/provider readers. Write its single constant RGBA pixel.
        unsafe {
            std::ptr::copy_nonoverlapping(
                value.as_ptr(),
                allocation.address.as_ptr().cast::<u8>(),
                4,
            )
        };
    } else if let Some(palette) = &palette {
        for y in 0..height {
            // SAFETY: New storage covers encoded_row * height and width does
            // not exceed stride. Write only this row's active index bytes.
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
        // Only Neutral lacks both palette and solid. Invert original gray bytes
        // exactly, without quantization, gamma changes, or reduced fidelity.
        for y in 0..height {
            // SAFETY: Exclusive new storage covers encoded_row * height with
            // width no greater than stride. Write only the active A8 row bytes.
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
    // Complete output bytes are independent, so Native sealing reads new pages
    // only. Return this no-longer-needed original RGBA claim first. A region plan
    // can retain its own Arc for later rows; this drop alone cannot prove unmapping.
    drop(original);
    if allocation
        .counters
        .pixel_release_failed
        .load(Ordering::Acquire)
    {
        return Err(PixelError::Backend("原RGBA像素页未能真实归还"));
    } // Failed release ends work before another Native graph or replay of completed pixel encoding.
    // Seal actual output bytes and record their format before exact reuse. Avoid
    // constructing a provider/color-space/image merely to discard it; equality
    // changes neither the established format nor generated pixel values.
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
        return Ok(reused); // Local new-page Arcs return before the outer owner checks sticky release failures.
    }
    let image = if presentation == PixelPresentation::OpaqueNeutralMask {
        seal_neutral_mask(&allocation, image_width, image_height, encoded_row)?
    } else {
        // SAFETY: The sRGB name has process lifetime. Palette values are exact
        // RGB, without another grayscale gamma or approximate color conversion.
        let base = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
            .ok_or(PixelError::Backend("无法建立原sRGB空间"))?;
        let (color, bits, info) = if let Some(palette) = palette {
            // SAFETY: The 2..=256-color table covers 3 * count bytes throughout
            // synchronous construction; Core Graphics copies the exact table.
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
    // Other original providers can actually release during Native construction;
    // keep the final sticky-error check as well as the earlier preflight check.
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

/// Preserve complete original RGBA under its provider contract, with exact
/// live-frame equality reuse before constructing another image.
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

/// Seal exact RGB components after complete opacity proof, retaining original
/// sRGB without quantization or a whole-viewport intermediate image.
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
        Err(error) => return Err(error), // Only a page-budget miss permits optional-compression fallback.
    };
    for y in 0..height {
        // SAFETY: Exclusive new storage covers encoded_row * height without a
        // context/provider. Its active RGB row is exactly width * 3 bytes.
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
    // Exact RGB bytes are complete; color-space/image construction needs no
    // original RGBA claim. Return this tile's Arc early while its actual owner
    // retains the same live budget and final-unmap receipts.
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
    // SAFETY: The sRGB name has process lifetime and selects the same actual
    // color space as the original RGBA representation.
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
    // Earlier RGBA release shortens ownership, not the verification contract;
    // retain the original post-seal sticky failure check.
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

/// Each independent provider has its own callback obligation. Two images cannot
/// share one allocation's single provider-return flag.
fn encode_paired(
    prepared: &PreparedImage,
    pairs: &CertifiedMaskPairs,
    allocations: &mut Option<OutputAllocations>,
    prior: Option<&TileFrame>,
) -> Result<Option<EncodedImage>, PixelError> {
    // Both planes read the same completed region. Borrow PreparedImage directly
    // so its original mapping and row-range authority remain one actual owner.
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
            // The first plane is unpublished. Return it before fallback and
            // preserve any actual unmap failure instead of hiding that error.
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
        // SAFETY: Both exclusive new mappings cover encoded_row * height with
        // no context/provider; original RGBA is read-only throughout this loop.
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
        return Ok(Some(reused)); // Exact reuse requires both planes and endpoints/blend, never the first mask alone.
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
