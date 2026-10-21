//! Choose finite horizontal regions only after complete opaque multicolor RGBA
//! painting. Planning creates no speculative image or extra pixel copy.

use super::*;

struct Fragment {
    first_row: usize,
    height: usize,
    plan: Plan,
}

/// Retain the original read-only input until all regions succeed or budget
/// fallback completes. At most four private row intervals grant no Scene/paint
/// authority of their own.
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
    /// Keep the original complete representation when splitting cannot improve
    /// actual page cost or would exceed the existing presentation slot budget.
    pub(in crate::native_surface::viewport_pixels) fn single(mut original: PreparedImage) -> Self {
        original.regions = None; // A single region needs no unused quarter summaries or extra Native resources.
        Self {
            original,
            fragments: Vec::new(),
        }
    }

    /// Compare complete page-rounded plans using already scanned summaries.
    /// Preserve the original RGB24 representation unless the cost truly falls.
    pub(in crate::native_surface::viewport_pixels) fn plan(
        mut original: PreparedImage,
        maximum: usize,
    ) -> Result<Self, PixelError> {
        if !matches!(original.plan, Plan::Rgb24) || maximum < 2 || original.height < 2 {
            // Existing mask/indexed/transparent forms remain unsplit. Insufficient
            // slots still leave the original complete RGB24 representation valid.
            return Ok(Self::single(original));
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
                // Every region must have positive height, with the caller's
                // slots still leaving room for subsequent actual paint tiles.
                continue;
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
                }; // Merge adjacent scanned quarters; odd-height halves still split at floor(height/2).
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
            } // Equal or larger rounded costs retain RGB24; raw byte savings alone do not justify splitting.
        }
        Ok(Self {
            original,
            fragments: best,
        })
    }

    /// The unsplit original still occupies one actual output/presentation slot.
    pub(in crate::native_surface::viewport_pixels) fn count(&self) -> usize {
        self.fragments.len().max(1)
    }

    /// Propose a region's actual complete palette. Only the Native calibration
    /// boundary can later authorize a paired-mask encoding.
    pub(in crate::native_surface::viewport_pixels) fn mask_request(
        &self,
        index: usize,
    ) -> Result<Option<MaskPaletteRequest>, PixelError> {
        if self.fragments.is_empty() {
            // The single-image path accepts exactly its original slot zero.
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
        }) // Non-indexed representations have no paired-mask palette request.
    }

    /// Acquire every output page before sealing any region. A budget miss can
    /// still use the untouched complete original; real backend failures remain
    /// failures and all unpublished local claims unwind through their owners.
    pub(in crate::native_surface::viewport_pixels) fn finish(
        self,
        pairs: &[Option<&CertifiedMaskPairs>],
        prior: Option<&TileFrame>,
    ) -> Result<Vec<EncodedRegion>, PixelError> {
        if pairs.len() != self.count() {
            // Every actual region must receive its own optional certification.
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
                            // Return all candidate pages before fallback. No
                            // provider was sealed and original RGBA is unchanged.
                            drop(output);
                            drop(allocations);
                            if counters.pixel_release_failed.load(Ordering::Acquire) {
                                return Err(PixelError::Backend(
                                    "分区预算回退时临时像素页未能真实归还",
                                ));
                            }
                            return single(original, None, prior); // Only budget exhaustion falls back to complete RGB24.
                        }
                        // A real mapping failure cannot be disguised as an
                        // optimization miss; already acquired pages still drop.
                        Err(error) => return Err(error),
                    }
                }
            }
            allocations.push(output);
        } // Actual formats/certificates determine the complete finite page group before any sealing.
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
            };
            let image = finish_with_allocations(prepared, *pairs, Some(allocations), prior)?;
            encoded.push(EncodedRegion {
                first_row: fragment.first_row,
                height: fragment.height,
                encoded: image,
            });
        } // Pixel borrows end in Rust-only loops before CF sealing; no slice/context/RefCell spans it.
        // Release shared RGBA only after all independent outputs succeed.
        // Earlier failure drops unpublished images and original input together.
        drop(original);
        if counters.pixel_release_failed.load(Ordering::Acquire) {
            return Err(PixelError::Backend("分区编码后的原RGBA未能真实归还"));
        }
        Ok(encoded)
    }
}

/// Finish the unchanged complete image under the same encoding/reuse contract.
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

/// Include RGB24's 16-byte row alignment before actual system-page rounding.
fn rgb_length(width: usize, height: usize) -> Result<usize, PixelError> {
    width
        .checked_mul(3)
        .and_then(|bytes| bytes.checked_add(15))
        .map(|bytes| bytes & !15)
        .and_then(|row| row.checked_mul(height))
        .ok_or(PixelError::InvalidGeometry)
}

/// Compute real representation lengths. Planning accounts for the more costly
/// dual-A8 possibility; execution additionally validates its actual certificate.
fn output_lengths(
    plan: &Plan,
    width: usize,
    height: usize,
    pairs: Option<&CertifiedMaskPairs>,
    worst_case: bool,
) -> Result<[Option<usize>; 2], PixelError> {
    if pairs.is_some() && !matches!(plan, Plan::Indexed(_)) {
        // A certificate for indexed colors cannot authorize another pixel form.
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
        // Only complete opaque RGB24 input can split. A transparent region
        // contradicts the admitted original classification.
        Plan::Rgba => Err(PixelError::InvalidGeometry),
        Plan::Rgb24 => Ok([Some(rgb_length(width, height)?), None]),
        Plan::Solid(value) if value[3] == 255 => Ok([Some(4), None]),
        Plan::Solid(_) => Err(PixelError::InvalidGeometry),
        Plan::Neutral => Ok([Some(byte_plane()?), None]),
        Plan::Indexed(palette) => {
            if let Some(pairs) = pairs {
                let request = MaskPaletteRequest::new(&palette.colors[..palette.count])
                    .ok_or(PixelError::InvalidGeometry)?;
                if !pairs.permits(&request) {
                    // Matching endpoints alone cannot certify a different
                    // region palette or color ordering.
                    return Err(PixelError::InvalidGeometry);
                }
            }
            let plane = byte_plane()?;
            Ok([
                Some(plane),
                (worst_case || pairs.is_some()).then_some(plane),
            ])
        } // Uncertified single-index output cannot exceed the planned dual-A8 page cost.
    }
}

/// Sum each real allocation's rounded page cost, including both independent
/// planes when required; combining raw bytes would undercount small mappings.
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
