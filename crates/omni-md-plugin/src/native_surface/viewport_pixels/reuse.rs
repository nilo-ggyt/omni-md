//! Compare sealed pixels within the live frame; reuse requires matching format, palette, and active
//! bytes after hash prefiltering.

use super::super::*;
use super::{Plan, PreparedImage, project_row};

const FINGERPRINT_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FINGERPRINT_PRIME: u64 = 0x100_0000_01b3;

/// Project actual read-only RGBA into its planned encoding, keeping one stack row instead of allocating
/// an encoded mapping.
/// A miss adds one bounded row projection, without additional Source reads, glyph work, or Native call
/// quota.
pub(super) fn projection_fingerprint(prepared: &PreparedImage) -> Result<Option<u64>, PixelError> {
    // Use the fixed tile-row capacity only; unsupported geometry stays with the existing encoding path
    // and its errors.
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

/// Record the original CGImage encoding; paired masks include Native blend interpretation so equal
/// bytes cannot cross blend semantics.
#[derive(Clone, Eq, PartialEq)]
pub(in crate::native_surface::viewport_pixels) enum Format {
    Rgba,
    Rgb24,
    Indexed(Box<[u8]>),
    NeutralMask,
    PairedMask(MaskPairBlend),
}

/// Retain the original read-only mapping used by the provider, without pixel copies, decoded images,
/// Scenes, or a cross-session cache.
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
    /// Accept actual encoded pages only after all contexts end, make them read-only, and record their
    /// format and active bytes.
    /// This certifies neither CGImage creation nor Source/Scene authority; comparison may precede
    /// provider construction.
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
            return Err(PixelError::ContextRetained); // An unfinished drawing context cannot issue a comparable read-only view of its writable pages.
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
        // Hash active bytes of this sealed mapping once; exclude row padding from content identity
        // while retaining its full allocation accounting.
        let mut fingerprint = FINGERPRINT_OFFSET;
        result.visit(|bytes| {
            for byte in bytes {
                fingerprint = (fingerprint ^ u64::from(*byte)).wrapping_mul(FINGERPRINT_PRIME);
            }
        });
        result.fingerprint = fingerprint;
        Ok(result)
    }

    /// The old pages came from the actual read-only seal in new; their Arc remains alive throughout row
    /// comparison.
    /// The hash eliminates impossible matches only; reuse still requires equal stride, format, full
    /// palette, and every active byte.
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
            // SAFETY: Construction verified full width/height/stride coverage, and this mapping has not
            // been reopened for writing.
            // Compare active encoded bytes only; padding is not pixel content but remains part of the
            // actual rounded page accounting.
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

    /// True certifies equal immutable encodings only, without current Source, Scene, geometry, Native
    /// relationship, or Ready authority.
    pub(in crate::native_surface::viewport_pixels) fn identical(&self, other: &Self) -> bool {
        if self.width != other.width
            || self.height != other.height
            || self.row_bytes != other.row_bytes
            || self.format != other.format
            || self.fingerprint != other.fingerprint
        {
            return false; // Matching hashes cannot override differences in dimensions, stride, palette, alpha, or decode interpretation.
        }
        let active = self.width * self.bytes_per_pixel();
        for y in 0..self.height {
            // SAFETY: Both values came from actual seals; each Arc retains its read-only mapping with
            // row coverage checked by new.
            // This comparison performs no Native/current callbacks and exposes no writable or escaping
            // slices.
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
                return false; // A hash collision cannot authorize reuse; retain the new image whenever any actual active byte differs.
            }
        }
        true
    }

    /// Match the original allocation ledger by Arc identity, independently of encoded content equality.
    pub(in crate::native_surface::viewport_pixels) fn belongs_to(
        &self,
        counters: &Arc<NativeMarkdownViewportPixels>,
    ) -> bool {
        Arc::ptr_eq(&self.allocation.counters, counters)
    }

    /// Require the same actual allocation ledger for every old plane; equal bytes cannot borrow another
    /// session provider.
    pub(in crate::native_surface::viewport_pixels) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.allocation.counters, &other.allocation.counters)
    }

    /// Preserve the actual ledger's sticky unmapping failure even when this particular tile compares
    /// equal.
    pub(in crate::native_surface::viewport_pixels) fn release_failed(&self) -> bool {
        self.allocation
            .counters
            .pixel_release_failed
            .load(Ordering::Acquire)
    }

    // The sealed format determines active row width; row padding is never interpreted as another pixel.
    fn bytes_per_pixel(&self) -> usize {
        match self.format {
            Format::Rgba => 4,
            Format::Rgb24 => 3,
            Format::Indexed(_) | Format::NeutralMask | Format::PairedMask(_) => 1,
        }
    }

    // Lend one active row at a time from the original mapping, with no whole-image allocation.
    fn visit(&self, mut visitor: impl FnMut(&[u8])) {
        let active = self.width * self.bytes_per_pixel();
        for y in 0..self.height {
            // SAFETY: This Arc retains the sealed mapping; lend only the current active row, without
            // transferring ownership to the visitor.
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
        // SAFETY: The new exclusive mapping covers 32x32 A8 with no context/provider; write a bounded,
        // nonconstant mask source.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(allocation.address.as_ptr().cast::<u8>(), 32 * 32)
        };
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (index & 255) as u8;
        }
        bytes[0] = first_byte;
        let image = seal_neutral_mask(&allocation, 32, 32, 32).unwrap();
        let storage = SealedPixels::new(allocation, 32, 32, 32, Format::PairedMask(blend)).unwrap();
        (image, storage) // Construct actual CGImage storage comparison values, without calibration tickets, SealedTiles, or Ready authority.
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
                assert_eq!(observer.snapshot().live_pixel_buffers, 2); // Storage still retains the mappings after provider release; actual live allocations cannot yet be zero.
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
            right.fingerprint = left.fingerprint; // This existing test forces a hash collision while the actual read-only mappings still differ in one active byte.
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
            // SAFETY: The test exclusively owns this writable allocation with no context/provider;
            // subsequent encoding performs the real seal.
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
        right.storage.fingerprint = left.storage.fingerprint; // This existing test forces a collision while retaining different actual read-only pixels.
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
        // SAFETY: This exclusive 32x32 RGBA test mapping has no context/provider; write more than 256
        // opaque colors.
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(allocation.address.as_ptr().cast::<u8>(), row * 32)
        };
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *pixel = [(index % 32) as u8, (index / 32) as u8, 0, 255];
        }
        pixels[row * 32 - 2] = delta; // The final active blue byte changes and must participate in actual RGB24 row comparison.
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
        right.storage.fingerprint = left.storage.fingerprint; // This existing test forces only the hash collision, without changing read-only storage.
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
