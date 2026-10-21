//! Calibrate a fixed five-layer graph under the original candidate, returning exact color facts without
//! a Native/Source cache.

use super::*;
use crate::native_surface::viewport_pixels::{
    CertifiedMaskPairs, MaskCalibrationPixels, MaskCalibrationTable, MaskPairBlend,
    MaskPaletteRequest, PixelError,
};
use objc2::rc::{Weak as NativeWeak, autoreleasepool};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGColor;
use objc2_foundation::NSSize;

const MAX_ENDPOINTS: usize = 2;

struct Entry {
    request: MaskPaletteRequest,
    tables: [Option<MaskCalibrationTable>; 2],
    attempted: [bool; 2],
    used_for_frame: bool,
}

/// The surface retains at most two endpoint groups with two pure output tables each, at most 1 MiB in
/// the existing 32 MiB pixel ledger.
/// Calibration fixes sRGB/RGBA8 output, Gray8 masks, 256x256 geometry, and 1x scale, independent of
/// windows, theme, font, or Source.
/// Tables certify only these exact RGB endpoints and blend; each new palette still needs per-color
/// certification.
#[derive(Default)]
pub(super) struct Calibrations {
    entries: Vec<Entry>,
}
impl Calibrations {
    /// A new candidate retains certified pure tables and renews only the two-endpoint frame quota,
    /// without increasing Native attempt limits.
    /// A budget miss without a table belongs to its frame and cannot permanently block a later frame
    /// with available budget.
    pub(super) fn begin_frame(&mut self) {
        for entry in &mut self.entries {
            entry.used_for_frame = false;
            for (attempted, table) in entry.attempted.iter_mut().zip(&entry.tables) {
                *attempted = table.is_some();
            }
        }
    }

    /// Report actual retained calibration outputs; attempted misses alone do not consume page storage.
    pub(super) fn has_pages(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.tables.iter().any(Option::is_some))
    }

    /// Reuse or measure the exact endpoint pair under original candidate validation; a miss keeps
    /// lossless encoding.
    pub(super) fn pairs(
        &mut self,
        request: &MaskPaletteRequest,
        pixels: Arc<NativeMarkdownViewportPixels>,
        validate: &impl Fn() -> Result<()>,
    ) -> Result<Option<CertifiedMaskPairs>> {
        validate()?;
        let endpoint = match self
            .entries
            .iter()
            .position(|entry| entry.request.same_endpoints(request))
        {
            Some(index) => index,
            None => {
                if self.entries.len() == MAX_ENDPOINTS {
                    let Some(unused) = self.entries.iter().position(|entry| !entry.used_for_frame)
                    else {
                        return Ok(None); // A third group keeps its original lossless representation without enlarging this frame calibration quota.
                    };
                    // Evict only old colors unused by this frame, actually unmapping their pure tables
                    // before creating a new Native graph.
                    // Two slots neither pin the first colors forever nor temporarily retain a third
                    // output-page group.
                    drop(self.entries.remove(unused));
                    if pixels.observer().snapshot().release_failed {
                        return Err(NativeMarkdownPagedTextViewError::Raster(
                            "旧校准纯颜色表未能实际归还".into(),
                        ));
                    }
                }
                if self.entries.capacity() == 0 {
                    self.entries.reserve_exact(MAX_ENDPOINTS);
                }
                self.entries.push(Entry {
                    request: request.clone(),
                    tables: [None, None],
                    attempted: [false; 2],
                    used_for_frame: false,
                });
                self.entries.len() - 1
            }
        }; // A blend mode is not another endpoint group; the second actual color pair retains its own two fixed slots.
        self.entries[endpoint].used_for_frame = true;
        for (mode, blend) in [
            MaskPairBlend::ForegroundThenForeground,
            MaskPairBlend::ForegroundThenBackground,
        ]
        .into_iter()
        .enumerate()
        {
            // Attempt each mode once per candidate, including budget misses, instead of rebuilding a
            // Native graph for every tile.
            if !self.entries[endpoint].attempted[mode] {
                let table = capture_checked(request, blend, Arc::clone(&pixels), validate)?;
                validate()?;
                self.entries[endpoint].tables[mode] = table;
                self.entries[endpoint].attempted[mode] = true;
            }
            let table = &self.entries[endpoint].tables[mode];
            let pairs = table
                .as_ref()
                .and_then(|table| table.certify(request, blend));
            validate()?;
            if pairs.is_some() {
                return Ok(pairs); // Prefer the original same-foreground mode; try the independently measured background blend only if full palette matching fails.
            }
        }
        Ok(None) // Without complete coverage from either fixed mode, original Indexed/RGBA preserves every color and existing behavior.
    }
}

// End the autorelease scope and verify all five actual Native objects before certifying their output
// table.
fn capture_checked(
    request: &MaskPaletteRequest,
    blend: MaskPairBlend,
    pixels: Arc<NativeMarkdownViewportPixels>,
    validate: &impl Fn() -> Result<()>,
) -> Result<Option<MaskCalibrationTable>> {
    let (result, weaks) = autoreleasepool(|_| {
        let mut weaks = Vec::with_capacity(5);
        let result = capture(request, blend, pixels, validate, &mut weaks);
        (result, weaks)
    });
    CATransaction::flush(); // Flush this five-layer graph and provider release before constructing the next mode Native graph.
    let returned = weaks.iter().all(|weak| weak.load().is_none());
    validate()?; // Original Source/font failure precedes Native cleanup or optimization errors.
    let table = result?;
    if !returned {
        return Err(NativeMarkdownPagedTextViewError::Raster(
            "双遮罩校准的原生图未实际释放".into(),
        ));
    }
    table
        .map(MaskCalibrationTable::confirm_sources_returned)
        .transpose()
        .map_err(Into::into)
}

fn capture(
    request: &MaskPaletteRequest,
    blend: MaskPairBlend,
    counters: Arc<NativeMarkdownViewportPixels>,
    validate: &impl Fn() -> Result<()>,
    weaks: &mut Vec<NativeWeak<AnyObject>>,
) -> Result<Option<MaskCalibrationTable>> {
    validate()?;
    let observer = counters.observer();
    let pixels = MaskCalibrationPixels::new(counters);
    validate()?;
    if observer.snapshot().release_failed {
        return Err(NativeMarkdownPagedTextViewError::Raster(
            "校准分配失败后原像素页未归还".into(),
        ));
    }
    let pixels = match pixels {
        Ok(value) => value,
        Err(PixelError::BudgetExceeded) => return Ok(None),
        Err(error) => return Err(error.into()),
    }; // Only optional-page budget exhaustion is an optimization miss; actual Native/context/mapping failures reject the candidate.
    let graph = Graph::new(validate, weaks)?;
    graph.install(request, blend, &pixels, validate)?;
    native(validate, || graph.second.setOpacity(0.0))?;
    native(validate, || {
        graph.container.renderInContext(pixels.context())
    })?;
    native(validate, || CGContext::flush(Some(pixels.context())))?;
    let single = pixels.first_row(); // Complete the bounded copy before the next Native call, without a live output-slice borrow.
    native(validate, || graph.second.setOpacity(1.0))?;
    native(validate, || {
        CGContext::clear_rect(
            Some(pixels.context()),
            NSRect::new(NSPoint::ZERO, NSSize::new(256.0, 256.0)),
        )
    })?;
    native(validate, || {
        graph.container.renderInContext(pixels.context())
    })?;
    native(validate, || CGContext::flush(Some(pixels.context())))?;
    drop(graph); // Clear masks, contents, colors, and child relationships before returning providers/contexts, with no live RefCell borrow.
    validate()?;
    let table = pixels.finish(request, blend, &single);
    validate()?;
    if observer.snapshot().release_failed {
        return Err(NativeMarkdownPagedTextViewError::Raster(
            "校准provider或context的真实归还失败".into(),
        ));
    }
    Ok(Some(table?))
}

/// Use the published five-role composition: local-background container and two mode-selected
/// foregrounds with independent A8 masks.
struct Graph {
    container: Retained<CALayer>,
    first: Retained<CALayer>,
    second: Retained<CALayer>,
    first_mask: Retained<CALayer>,
    second_mask: Retained<CALayer>,
}
impl Graph {
    fn new(
        validate: &impl Fn() -> Result<()>,
        weaks: &mut Vec<NativeWeak<AnyObject>>,
    ) -> Result<Self> {
        let mut create = || -> Result<Retained<CALayer>> {
            validate()?;
            let layer = CALayer::new();
            let object: &AnyObject = &layer;
            weaks.push(NativeWeak::from(object));
            validate()?; // Register the weak observation immediately after creation so post-call revocation still observes actual release.
            Ok(layer)
        };
        Ok(Self {
            container: create()?,
            first: create()?,
            second: create()?,
            first_mask: create()?,
            second_mask: create()?,
        })
    }
    fn install(
        &self,
        request: &MaskPaletteRequest,
        blend: MaskPairBlend,
        pixels: &MaskCalibrationPixels,
        validate: &impl Fn() -> Result<()>,
    ) -> Result<()> {
        let rect = NSRect::new(NSPoint::ZERO, NSSize::new(256.0, 256.0));
        for (layer, mask) in [
            (&self.container, false),
            (&self.first, false),
            (&self.second, false),
            (&self.first_mask, true),
            (&self.second_mask, true),
        ] {
            native(validate, || layer.setBounds(rect))?;
            native(validate, || layer.setFrame(rect))?;
            native(validate, || layer.setContentsScale(1.0))?;
            // SAFETY: Both public QuartzCore format constants are static; use the Gray8 storage hint
            // for actual A8 masks only.
            let format = unsafe {
                if mask {
                    objc2_quartz_core::kCAContentsFormatGray8Uint
                } else {
                    objc2_quartz_core::kCAContentsFormatRGBA8Uint
                }
            };
            native(validate, || {
                // The standard setter receives the acquired public format value without changing
                // original image pixels.
                layer.setContentsFormat(format);
            })?;
            let actual = native(validate, || layer.contentsFormat())?;
            if !native(validate, || actual.isEqualToString(format))? {
                return Err(NativeMarkdownPagedTextViewError::Raster(
                    "双遮罩校准图的实际存储格式不一致".into(),
                )); // Setter return does not prove equal actual format; reject calibration instead of certifying Gray with an old RGBA table.
            }
            native(validate, || {
                // SAFETY: The original A8 mask and layer both measure 256x256; fixed resize neither
                // discards pixels nor changes glyphs.
                unsafe { layer.setContentsGravity(objc2_quartz_core::kCAGravityResize) };
            })?;
        }
        let color = |value: [u8; 3]| -> Result<CFRetained<CGColor>> {
            native(validate, || {
                CGColor::new_srgb(
                    f64::from(value[0]) / 255.0,
                    f64::from(value[1]) / 255.0,
                    f64::from(value[2]) / 255.0,
                    1.0,
                )
            })
        };
        let background = color(request.background)?;
        let foreground = color(request.foreground)?;
        native(validate, || {
            self.container.setBackgroundColor(Some(&background))
        })?;
        native(validate, || {
            self.first.setBackgroundColor(Some(&foreground))
        })?;
        let second = match blend {
            MaskPairBlend::ForegroundThenForeground => &foreground,
            MaskPairBlend::ForegroundThenBackground => &background,
        }; // The second layer selects an original endpoint only; the complete actual RGB table still determines certification.
        native(validate, || self.second.setBackgroundColor(Some(second)))?;
        let (first, second) = pixels.images();
        native(validate, || {
            // SAFETY: Both CGImages hold independent actual read-only providers retained by local
            // pixels; send no ObjC message to CF values.
            unsafe {
                self.first_mask
                    .setContents(Some(&*std::ptr::from_ref(first).cast::<AnyObject>()))
            };
        })?;
        native(validate, || {
            // SAFETY: The second mask has its own original provider; the first allocation callback
            // cannot certify both allocations.
            unsafe {
                self.second_mask
                    .setContents(Some(&*std::ptr::from_ref(second).cast::<AnyObject>()))
            };
        })?;
        native(validate, || {
            // SAFETY: This local graph exclusively owns its fixed mask, without text, View, delegate,
            // or business callbacks.
            unsafe { self.first.setMask(Some(&self.first_mask)) };
        })?;
        native(validate, || {
            // SAFETY: Build a bounded main-thread relationship from the second foreground to its own
            // original mask only.
            unsafe { self.second.setMask(Some(&self.second_mask)) };
        })?;
        let children = native(validate, || {
            NSArray::from_retained_slice(&[self.first.clone(), self.second.clone()])
        })?;
        native(validate, || {
            // SAFETY: The array contains this graph two fixed foregrounds only; no product View
            // receives this partial graph.
            unsafe { self.container.setSublayers(Some(&children)) };
        })?;
        Ok(())
    }
}
impl Drop for Graph {
    fn drop(&mut self) {
        // Cleanup owns this local unpublished graph only and must release its payload even after
        // current authority is revoked.
        // SAFETY: All objects and relationships belong to this main-thread instance, without user View
        // or successor-owner graph cleanup.
        unsafe {
            self.first.setMask(None);
            self.second.setMask(None);
            self.first_mask.setContents(None);
            self.second_mask.setContents(None);
            self.container.setSublayers(None);
        }
        self.container.setBackgroundColor(None);
        self.first.setBackgroundColor(None);
        self.second.setBackgroundColor(None);
    }
}

// Validate around each potentially reentrant Native call; acquired results remain local on failure.
fn native<T>(validate: &impl Fn() -> Result<()>, call: impl FnOnce() -> T) -> Result<T> {
    validate()?;
    let result = call();
    validate()?;
    Ok(result)
}
