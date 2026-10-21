//! Paired A8 masks require exact Native calibration for the finite actual
//! palette. This module owns pixels/facts and never operates on CALayer.

use super::*;

const COLORS: usize = 256;
const SAMPLES: usize = COLORS * COLORS;

/// Endpoints are finite candidates derived from actual pixels. A theme, style,
/// or mathematical interpolation cannot authorize paired-mask presentation.
#[derive(Clone)]
pub(in crate::native_surface) struct MaskPaletteRequest {
    colors: [u32; COLORS],
    slots: [u16; COLORS * 2],
    count: usize,
    pub background: [u8; 3],
    pub foreground: [u8; 3],
}
impl MaskPaletteRequest {
    /// Build the complete bounded color directory before proposing endpoints.
    /// Admission retains every original color rather than quantizing excess ink.
    pub(super) fn new(values: &[u32]) -> Option<Self> {
        if values.len() < 2 || values.len() > COLORS {
            // Solid colors already have a smaller representation; discarding a
            // 257th color must not manufacture calibration eligibility.
            return None;
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
            } // At most half full, the directory admits every original color within a fixed bound.
        }
        // Deterministic luminance ordering proposes endpoints only. The Native
        // output table below must still reproduce every actual color exactly.
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

    /// Endpoint equality allows table reuse, not permission for another palette.
    pub(in crate::native_surface) fn same_endpoints(&self, other: &Self) -> bool {
        self.background == other.background && self.foreground == other.foreground
    }

    /// Locate an original exact RGB value in the bounded open-addressed table.
    fn locate(&self, color: u32) -> Option<usize> {
        let mut slot = hash(color);
        for _ in 0..COLORS * 2 {
            let ordinal = self.slots[slot];
            if ordinal == u16::MAX {
                // A real empty slot establishes absence without another scan.
                return None;
            }
            if self.colors[usize::from(ordinal)] == color {
                // A hash collision alone cannot identify a palette color.
                return Some(usize::from(ordinal));
            }
            slot = (slot + 1) & (COLORS * 2 - 1);
        }
        None
    }
}

/// Construct only after actual 256² Native output reproduces all target colors.
/// Color certification grants no Source, Scene, or Ready authority.
pub(in crate::native_surface) struct CertifiedMaskPairs {
    request: MaskPaletteRequest,
    blend: MaskPairBlend,
    pairs: [[u8; 2]; COLORS],
}
impl CertifiedMaskPairs {
    /// Match the full ordered palette as well as the proposed endpoints.
    pub(super) fn permits(&self, request: &MaskPaletteRequest) -> bool {
        self.request.same_endpoints(request)
            && self.request.count == request.count
            && self.request.colors[..request.count] == request.colors[..request.count]
    }
    /// Return the certified actual alpha pair for an exact original RGB value.
    pub(super) fn pair(&self, color: u32) -> Option<[u8; 2]> {
        self.request.locate(color).map(|index| self.pairs[index])
    }
    /// Preserve the actual Native blending mode used to issue this certificate.
    pub(super) fn blend(&self) -> MaskPairBlend {
        self.blend
    }
    /// Supply only certified endpoint/blending facts to the existing presenter.
    pub(super) fn presentation(&self) -> PixelPresentation {
        PixelPresentation::OpaquePairedMasks {
            background: self.request.background,
            foreground: self.request.foreground,
            blend: self.blend,
        }
    }
}

/// Native calibration borrows two independent providers and one finite RGBA
/// context. This owner retains no Source, font, or Native layer graph.
pub(in crate::native_surface) struct MaskCalibrationPixels {
    first: CFRetained<CGImage>,
    second: CFRetained<CGImage>,
    output: Arc<PixelAllocation>,
    context: Option<CFRetained<CGContext>>,
    source_claims: [Arc<AtomicUsize>; 2],
}
impl MaskCalibrationPixels {
    /// Allocate two complete alpha planes and one actual output mapping under
    /// the same live-pixel budget; partial construction drops acquired claims.
    pub(in crate::native_surface) fn new(
        counters: Arc<NativeMarkdownViewportPixels>,
    ) -> Result<Self, PixelError> {
        let make_plane = |vertical: bool| -> Result<_, PixelError> {
            // Each plane has its own provider and final-unmap receipt; one shared
            // flag could not prove both independent allocations were returned.
            let mut allocation = allocate(
                SAMPLES,
                Arc::clone(&counters),
                Some(tiled::LIVE_PIXEL_BUDGET),
            )?;
            let receipt = Arc::new(AtomicUsize::new(0));
            Arc::get_mut(&mut allocation)
                .expect("新分配尚无context/provider")
                .release_receipt = Some(Arc::clone(&receipt));
            // SAFETY: This fresh allocation has no context/provider and covers
            // the fixed 256² bytes; initialize every alpha byte before sealing.
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
    /// Borrow the two real plane images only for the caller's Native calibration.
    pub(in crate::native_surface) fn images(&self) -> (&CGImage, &CGImage) {
        (&self.first, &self.second)
    }
    /// Borrow the still-owned writable output context during calibration.
    pub(in crate::native_surface) fn context(&self) -> &CGContext {
        self.context.as_deref().expect("校准仍持原实际context")
    }
    /// Copy one fixed first row after synchronous rendering returns. This pixel
    /// borrow must end before another Native call can render into the context.
    pub(in crate::native_surface) fn first_row(&self) -> [u8; COLORS * 4] {
        let mut result = [0; COLORS * 4];
        // SAFETY: The exclusive context is not drawing; its output covers the
        // full 256² RGBA extent. Read and immediately copy only the first row.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.output.address.as_ptr().cast::<u8>(),
                result.as_mut_ptr(),
                result.len(),
            );
        }
        result
    }
    /// End the actual writable context before sealing output read-only. Dropping
    /// this owner returns both image claims; the table keeps their independent
    /// release receipts until actual Native retention has also ended.
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
            // Another actual context claim still permits writes, so a read-only
            // output table cannot yet be established.
            return Err(PixelError::ContextRetained);
        }
        readonly(&self.output)?;
        // SAFETY: The context actually ended and the fixed mapping is read-only,
        // covering all 256² RGBA samples. This read spans no Native callback.
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
        } // Native geometry/alpha failures are errors; only absent exact colors allow an optimization miss.
        Ok(MaskCalibrationTable {
            request: request.clone(),
            blend,
            allocation: Arc::clone(&self.output),
            zero_first: first_matches,
            source_claims: Some(self.source_claims.clone()),
        })
    }
}

/// Each candidate keeps at most two endpoint groups and two blend-mode tables
/// per group. Tables consume real PixelAllocation pages and unmap on final Drop.
pub(in crate::native_surface) struct MaskCalibrationTable {
    request: MaskPaletteRequest,
    blend: MaskPairBlend,
    allocation: Arc<PixelAllocation>,
    zero_first: bool,
    source_claims: Option<[Arc<AtomicUsize>; 2]>,
}
impl MaskCalibrationTable {
    /// After local Native layers/images return, require each original source
    /// allocation's own destruction receipt; a global count difference is not
    /// evidence that these particular providers actually released.
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
            // A retained provider or failed unmap cannot become a cached color
            // fact even when the output table itself is already readable.
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
        Ok(self) // Only the actual read-only table remains; source receipts do not enter the cache.
    }
    /// A sampled output table is reusable only for its endpoints and blend mode.
    pub(in crate::native_surface) fn matches(
        &self,
        request: &MaskPaletteRequest,
        blend: MaskPairBlend,
    ) -> bool {
        self.request.same_endpoints(request) && self.blend == blend
    }
    /// Certify a complete requested palette against the retained actual Native
    /// samples, after both source-plane allocations really returned.
    pub(in crate::native_surface) fn certify(
        &self,
        request: &MaskPaletteRequest,
        blend: MaskPairBlend,
    ) -> Option<CertifiedMaskPairs> {
        if self.source_claims.is_some() || !self.matches(request, blend) {
            // Unreturned sources or different Native blending facts cannot
            // authorize another set of paired masks.
            return None;
        }
        let mut pairs = [[0; 2]; COLORS];
        let mut found = [false; COLORS];
        let mut remaining = request.count;
        // SAFETY: The actual context returned before table construction and
        // mprotect sealed its fixed 256² RGBA mapping, retained by this Arc.
        let bytes = unsafe {
            std::slice::from_raw_parts(self.allocation.address.as_ptr().cast::<u8>(), SAMPLES * 4)
        };
        // Scan the 65,536 actual outputs once per finite target palette, rather
        // than searching every alpha pair separately for each original pixel.
        for (sample, pixel) in bytes.as_chunks::<4>().0.iter().enumerate() {
            let color =
                (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2]);
            let Some(ordinal) = request.locate(color) else {
                // Samples outside the requested palette grant no target pair.
                continue;
            };
            if !found[ordinal] {
                // Keep the first actual pair for each exact RGB value. Native
                // row orientation determines the second alpha coordinate.
                found[ordinal] = true;
                let row = sample / COLORS;
                pairs[ordinal] = [
                    (sample % COLORS) as u8,
                    if self.zero_first { row } else { 255 - row } as u8,
                ];
                remaining -= 1;
                if remaining == 0 {
                    // Publish only complete certification; no target color may
                    // be approximated or omitted from the original tile.
                    return Some(CertifiedMaskPairs {
                        request: request.clone(),
                        blend: self.blend,
                        pairs,
                    });
                }
            }
        }
        None // Any missing exact RGB keeps the tile's original lossless representation.
    }
}

pub(super) fn readonly(allocation: &PixelAllocation) -> Result<(), PixelError> {
    // SAFETY: The caller ended the exclusive context. This extent is the same
    // original mmap with its unchanged allocation length.
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
    // The fixed multiplier selects a slot in the half-full 512-entry directory.
    (color.wrapping_mul(2_654_435_761) >> 23) as usize
}
fn components(color: u32) -> [u8; 3] {
    // Decode explicit packed RGB bytes; no platform-native color ordering applies.
    [(color >> 16) as u8, (color >> 8) as u8, color as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(observer.is_released()); // Both real providers must return their distinct claims.
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
        assert!(observer.is_released()); // No table/Ready is invented; the actual last context releases its mapping.
    }
}
