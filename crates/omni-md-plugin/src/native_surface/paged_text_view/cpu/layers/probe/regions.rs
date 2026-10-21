//! Exercise irregular 2D-region Native differences, reuse, and external-parent cleanup without treating
//! saved pages as whole-product peak evidence.

use std::collections::HashSet;

use super::super::super::calibration::Calibrations;
use super::*;

const REGION_WIDTH: usize = 513;
const REGION_HEIGHT: usize = 259;
const FIRST_HEIGHT: usize = 129;
// Observe original structural layers and each actual publication roles separately, without empty layers
// for absent tiles.

fn invalid() -> NativeMarkdownPagedTextViewError {
    NativeMarkdownPagedTextViewError::InvalidGeometry
}

fn verify_page_costs() -> Result<()> {
    // SAFETY: sysconf queries this machine page size only, without borrowing Native objects or pixel
    // addresses.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page != 16_384 {
        return Err(invalid());
    } // This acceptance machine uses 16 KiB pages; another machine cannot silently reinterpret these independent expected costs.
    let aligned = |length: usize| length.div_ceil(16_384) * 16_384;
    let rgb24 = aligned(512 * 3 * 259);
    let two = 2 * (aligned(512 * 129) + aligned(512 * 130));
    let four = 2 * (aligned(512 * 64) + 3 * aligned(512 * 65));
    if rgb24 != 409_600 || two != 327_680 || four != 360_448 || !(two < four && four < rgb24) {
        return Err(invalid());
    }
    println!(
        "paged_region_slots actual_page_bytes={page} independent_rgb24_page_cost={rgb24} independent_two_region_worst_pair_cost={two} independent_four_region_worst_pair_cost={four} native_process_peak_claim=false formal=false"
    );
    Ok(())
}

/// Keep two actual 256-pixel sRGB ramps rather than assuming Core Graphics output bytes from a
/// mathematical blend formula.
fn ramp(background: [u8; 3], foreground: [u8; 3]) -> Result<Canvas> {
    let canvas = Canvas::sized(256, 1)?;
    CGContext::set_rgb_fill_color(
        Some(&canvas.context),
        f64::from(background[0]) / 255.0,
        f64::from(background[1]) / 255.0,
        f64::from(background[2]) / 255.0,
        1.0,
    );
    CGContext::fill_rect(
        Some(&canvas.context),
        NSRect::new(NSPoint::ZERO, NSSize::new(256.0, 1.0)),
    );
    for x in 0..256 {
        CGContext::set_rgb_fill_color(
            Some(&canvas.context),
            f64::from(foreground[0]) / 255.0,
            f64::from(foreground[1]) / 255.0,
            f64::from(foreground[2]) / 255.0,
            x as f64 / 255.0,
        );
        CGContext::fill_rect(
            Some(&canvas.context),
            NSRect::new(NSPoint::new(x as f64, 0.0), NSSize::new(1.0, 1.0)),
        );
    }
    CGContext::flush(Some(&canvas.context));
    Ok(canvas)
}

fn draw_source(source: &mut Canvas) -> Result<(usize, usize, usize)> {
    let ramps = [
        ramp([19, 21, 28], [219, 224, 235])?,
        ramp([31, 61, 93], [245, 173, 41])?,
    ];
    let mut palettes = [HashSet::with_capacity(256), HashSet::with_capacity(256)];
    let mut combined = HashSet::with_capacity(512);
    for y in 0..REGION_HEIGHT {
        let group = usize::from(y >= FIRST_HEIGHT);
        for x in 0..512 {
            // Each row includes all 256 coverages shifted by y, so reversed orientation, off-by-one
            // region boundaries, or bad row stride
            // cannot hide in constant rows. Use actual ramp RGBA without granting any mask calibration
            // certificate.
            let at = ((x + y * 7) & 255) * 4;
            let pixel: [u8; 4] = ramps[group].pixels[at..at + 4]
                .try_into()
                .map_err(|_| invalid())?;
            if pixel[3] != 255 {
                return Err(invalid());
            } // The actual source tile must be opaque; do not erase transparency merely to trigger partitioning.
            palettes[group].insert(pixel);
            combined.insert(pixel);
            let target = y * source.row_bytes + x * 4;
            source.pixels[target..target + 4].copy_from_slice(&pixel);
        }
        // The rightmost one-pixel tail has actual color throughout its height; preserve its final row
        // and the explicit 513-pixel row stride.
        let tail = y * source.row_bytes + 512 * 4;
        source.pixels[tail..tail + 4].copy_from_slice(&[7, (y & 255) as u8, 203, 255]);
    }
    CGContext::flush(Some(&source.context));
    let counts = (palettes[0].len(), palettes[1].len(), combined.len());
    if counts.0 <= 1 || counts.0 > 256 || counts.1 <= 1 || counts.1 > 256 || counts.2 <= 256 {
        return Err(invalid());
    } // Actual color cardinalities establish whole-tile RGB24 and two bounded local palettes, rather than endpoint-name guesses.
    Ok(counts)
}

fn encode(
    source: &Canvas,
    counters: Arc<NativeMarkdownViewportPixels>,
    prior: Option<&TileFrame>,
) -> Result<TileFrame> {
    let mut bitmap = TiledBitmap::new(REGION_WIDTH, REGION_HEIGHT, counters.clone())?;
    let mut calibrations = Calibrations::default();
    while let Some(tile) = bitmap.next_tile() {
        let writable = bitmap.begin_tile(tile)?;
        let row = CGBitmapContextGetBytesPerRow(Some(writable.context()));
        let data = CGBitmapContextGetData(Some(writable.context()));
        if data.is_null() || row < tile.width * 4 {
            return Err(invalid());
        }
        for y in 0..tile.height {
            // SAFETY: The unique WritableTile context covers actual stride*height; write this row
            // active RGBA only,
            // ending the slice before prepare/calibration/Native sealing and skipping 513-wide Canvas
            // padding by its actual row_bytes.
            let target = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * row), tile.width * 4)
            };
            let source_at = (tile.y + y) * source.row_bytes + tile.x * 4;
            target.copy_from_slice(&source.pixels[source_at..source_at + tile.width * 4]);
        }
        let prepared = bitmap.prepare_tile(writable)?;
        let plan = bitmap.plan_encoding(prepared)?;
        let count = plan.region_count();
        let expected = if tile.x == 0 { 2 } else { 1 };
        if count != expected {
            return Err(invalid());
        } // With 16 KiB pages, two first-tile regions cost less than whole/four-region layouts; retain the complete tail slot.
        let mut pairs = Vec::with_capacity(count);
        for index in 0..count {
            let certificate = match plan.mask_request(index)? {
                Some(request) => calibrations.pairs(&request, counters.clone(), &|| Ok(()))?,
                None => None,
            }; // Calibration may legitimately miss; local Indexed/RGB24 must remain exact without assuming paired masks exist.
            pairs.push(certificate);
        }
        let borrowed = pairs.iter().map(Option::as_ref).collect::<Vec<_>>();
        bitmap.finish_encoding(plan, &borrowed, prior)?;
    }
    drop(calibrations); // End bounded calibration-table lifetime with this candidate; actual temporary pages are not free.
    let frame = bitmap.finish()?;
    let expected = [
        (0, 0, 512, 129, 130),
        (0, 129, 512, 130, 0),
        (512, 0, 1, 259, 0),
    ];
    if frame.width != REGION_WIDTH
        || frame.height != REGION_HEIGHT
        || frame.tiles.len() != expected.len()
        || frame.tiles.iter().zip(expected).any(|(sealed, expected)| {
            let tile = sealed.tile;
            (tile.x, tile.y, tile.width, tile.height, tile.bottom) != expected
        })
    {
        return Err(invalid());
    } // Bottom equals parent.bottom+parent.height-end; both boundary rows and the full-height one-pixel tail belong to this frame.
    Ok(frame)
}

fn same_images(left: &TileFrame, right: &TileFrame, index: usize) -> bool {
    let (Some(left), Some(right)) = (left.tiles.get(index), right.tiles.get(index)) else {
        return false; // A missing actual slot cannot pass through truncated zip or an empty all result.
    };
    left.tile == right.tile
        && left.presentation == right.presentation
        && std::ptr::eq::<CGImage>(&*left.image, &*right.image)
        && match (&left.secondary_image, &right.secondary_image) {
            (None, None) => true, // An unpaired Image still requires exact actual primary identity checked above.
            (Some(left), Some(right)) => std::ptr::eq::<CGImage>(&**left, &**right),
            _ => false, // The calibrated Some/None category belongs to reuse qualification; never discard an actual mask image.
        }
}

fn changed_images(left: &TileFrame, right: &TileFrame, index: usize) -> bool {
    let (Some(left), Some(right)) = (left.tiles.get(index), right.tiles.get(index)) else {
        return false;
    };
    left.tile == right.tile
        && !std::ptr::eq::<CGImage>(&*left.image, &*right.image)
        && match (&left.secondary_image, &right.secondary_image) {
            (None, None) => true, // An unpaired representation needs actual replacement of its original primary image only.
            (Some(left), Some(right)) => !std::ptr::eq::<CGImage>(&**left, &**right),
            _ => false, // A same-palette one-pixel change must not invent/remove the second image; category changes are not partial reuse.
        } // Pair qualification is indivisible; a coincidentally equal plane cannot retain half an old pair after changing its other image.
}

fn render_exact(
    pool: &LayerPool,
    backing: &CALayer,
    frame: &TileFrame,
    source: &Canvas,
    bounds: NSRect,
    previous: Option<&Publication>,
) -> Result<Publication> {
    let reference = CGBitmapContextCreateImage(Some(&source.context)).ok_or_else(invalid)?;
    let baseline = Canvas::sized(REGION_WIDTH, REGION_HEIGHT)?;
    baseline.blue(bounds);
    CGContext::draw_image(Some(&baseline.context), bounds, Some(&reference));
    CGContext::flush(Some(&baseline.context));
    let rendered = Canvas::sized(REGION_WIDTH, REGION_HEIGHT)?;
    rendered.blue(bounds);
    // Region boundaries and tail columns use production admission; keep the Native graph only for
    // actual identical slots,
    // and route single-pixel changes through production partial publication while independent RGBA
    // determines complete color correctness.
    let reused = previous
        .map(|previous| pool.reuse_publication(backing, frame, previous, bounds, 1.0, &|| true))
        .transpose()?
        .flatten();
    let publication = match reused {
        Some(publication) => publication,
        None => {
            let mut attempt = pool.attempt()?;
            let partial = previous
                .map(|previous| {
                    pool.publish_partial(backing, frame, previous, &mut attempt, &|| true)
                })
                .transpose()?
                .flatten();
            match partial {
                Some(publication) => publication,
                None => pool.publish(backing, frame, &attempt, bounds, 1.0, &|| true)?,
            }
        }
    };
    pool.validate(backing, &publication, &|| true)?;
    pool.mount().renderInContext(&rendered.context);
    CGContext::flush(Some(&rendered.context));
    if baseline.width != rendered.width || baseline.height != rendered.height {
        return Err(invalid());
    }
    for y in 0..REGION_HEIGHT {
        let before = y * baseline.row_bytes;
        let after = y * rendered.row_bytes;
        let active = REGION_WIDTH * 4;
        if baseline.pixels[before..before + active] != rendered.pixels[after..after + active] {
            let byte = baseline.pixels[before..before + active]
                .iter()
                .zip(&rendered.pixels[after..after + active])
                .position(|(expected, actual)| expected != actual)
                .ok_or_else(invalid)?;
            let x = byte / 4;
            println!(
                "paged_region_slots first_active_rgba_mismatch_x={x} first_active_rgba_mismatch_y={y} expected={:?} actual={:?} exact_required=true formal=false",
                &baseline.pixels[before + x * 4..before + x * 4 + 4],
                &rendered.pixels[after + x * 4..after + x * 4 + 4],
            );
            return Err(invalid());
        } // Compare all 513 active RGBA pixels per row exactly, including boundary rows 129/130, the tail column, and row 259 without tolerance.
    }
    Ok(publication)
}

pub(super) fn run(_mtm: MainThreadMarker) -> Result<()> {
    verify_page_costs()?;
    let counters = Arc::new(NativeMarkdownViewportPixels::default());
    let observer = counters.observer();
    let (passed, weaks) = autoreleasepool(|_| -> Result<_> {
        let successor = CALayer::new();
        let foreign_owner = layer_weak(&successor);
        let (intact, weaks) = autoreleasepool(|_| -> Result<_> {
            let bounds = NSRect::new(
                NSPoint::ZERO,
                NSSize::new(REGION_WIDTH as f64, REGION_HEIGHT as f64),
            );
            let mut source = Canvas::sized(REGION_WIDTH, REGION_HEIGHT)?;
            let colors = draw_source(&mut source)?;
            let backing = CALayer::new();
            backing.setFrame(bounds);
            backing.setBounds(bounds);
            let pool = LayerPool::new(&|| true)?;
            backing.addSublayer(pool.mount());
            let mut weaks = pool.weaks();
            weaks.push(layer_weak(&backing));
            let mut frames = Vec::with_capacity(3);
            let mut publication = None;
            let result = (|| -> Result<usize> {
                frames.push(encode(&source, counters.clone(), None)?);
                publication = Some(render_exact(
                    &pool, &backing, &frames[0], &source, bounds, None,
                )?);
                // Observe roles from each actual publication; original weak identities must still
                // establish old-role release after resize/group changes.
                for group in &pool.groups {
                    let slots = group.slots.borrow().clone();
                    for slot in slots {
                        slot.add_weaks(&mut weaks);
                    }
                }
                let same = encode(&source, counters.clone(), Some(&frames[0]))?;
                let same_reused = same.reused_images == 3
                    && (0..3).all(|index| same_images(&frames[0], &same, index));
                frames.push(same);
                publication = Some(render_exact(
                    &pool,
                    &backing,
                    &frames[1],
                    &source,
                    bounds,
                    publication.as_ref(),
                )?);
                // Observe roles from each actual publication; original weak identities must still
                // establish old-role release after resize/group changes.
                for group in &pool.groups {
                    let slots = group.slots.borrow().clone();
                    for slot in slots {
                        slot.add_weaks(&mut weaks);
                    }
                }
                // Change one actual pixel within its original palette, preserving other rows, local
                // endpoints, geometry, and the other two subtiles.
                let first: [u8; 4] = source.pixels[..4].try_into().map_err(|_| invalid())?;
                let other = source.pixels[4..512 * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .find(|pixel| **pixel != first)
                    .copied()
                    .ok_or_else(invalid)?;
                source.pixels[..4].copy_from_slice(&other);
                CGContext::flush(Some(&source.context));
                let changed = encode(&source, counters.clone(), Some(&frames[0]))?;
                let changed_reused = changed.reused_images == 2
                    && changed_images(&frames[0], &changed, 0)
                    && (1..3).all(|index| same_images(&frames[0], &changed, index));
                frames.push(changed);
                publication = Some(render_exact(
                    &pool,
                    &backing,
                    &frames[2],
                    &source,
                    bounds,
                    publication.as_ref(),
                )?);
                // Observe roles from each actual publication; original weak identities must still
                // establish old-role release after resize/group changes.
                for group in &pool.groups {
                    let slots = group.slots.borrow().clone();
                    for slot in slots {
                        slot.add_weaks(&mut weaks);
                    }
                }
                let secondary = frames[0]
                    .tiles
                    .iter()
                    .filter(|tile| tile.secondary_image.is_some())
                    .count();
                println!(
                    "paged_region_slots actual_width=513 actual_height=259 actual_source_colors={colors:?} actual_slots=3 actual_same_primary_images=3 actual_same_secondary_images={secondary} actual_changed_tile_reuses=2 original_slot_image_pairs_verified={same_reused}/{changed_reused} exact_active_rgba=true rowstride={} assumed_mask_certificates=false formal=false whole_product=false",
                    source.row_bytes,
                );
                if !same_reused || !changed_reused {
                    return Err(invalid());
                }
                Ok(secondary)
            })();
            successor.addSublayer(pool.mount()); // Transfer after actual commit; the old owner cleanup must preserve this external parent relationship.
            let rejected = publication.as_ref().is_some_and(|publication| {
                matches!(
                    pool.validate(&backing, publication, &|| true),
                    Err(NativeMarkdownPagedTextViewError::Revoked)
                )
            });
            let detached = pool.detach(&backing);
            let foreign_preserved = pool
                .mount()
                .superlayer()
                .as_deref()
                .is_some_and(|actual| std::ptr::eq::<CALayer>(actual, &*successor));
            drop((frames, publication)); // The external owner retains the original mount; return caller frames first so dropping that owner cannot hide leaked original resources.
            CATransaction::flush();
            println!(
                "paged_region_slots actual_result={result:?} actual_detach={detached} foreign_parent_preserved={foreign_preserved} actual_rejected={rejected} formal=false"
            );
            drop((pool, backing, source));
            CATransaction::flush();
            Ok((
                result.is_ok() && rejected && !detached && foreign_preserved,
                weaks,
            ))
        })?; // Drain this owner and temporary Native arrays while the outer successor stays alive, without relaxing weak checks for cache timing.
        CATransaction::flush();
        let pixels_released_with_foreign = observer.is_released();
        let foreign_mount_alive = weaks[0].load().is_some();
        let foreign_owner_alive = foreign_owner.load().is_some();
        let other_owned_released = weaks[1..].iter().all(|weak| weak.load().is_none());
        println!(
            "paged_region_slots original_provider_maps_zero_with_foreign_alive={pixels_released_with_foreign} foreign_mount_still_alive={foreign_mount_alive} foreign_owner_still_alive={foreign_owner_alive} other_pool_and_backing_weak_nil={other_owned_released} actual_pixels={:?} formal=false",
            observer.snapshot(),
        );
        // After actual provider/mapping/owned-role checks, the external owner returns its mount; that
        // is not the original owner close action.
        // SAFETY: This bounded control owns successor; None removes only the actual mount it attached
        // itself.
        unsafe { successor.setSublayers(None) };
        drop(successor);
        CATransaction::flush();
        Ok((
            intact
                && pixels_released_with_foreign
                && foreign_mount_alive
                && foreign_owner_alive
                && other_owned_released,
            weaks,
        ))
    })?;
    let native_released = weaks.iter().all(|weak| weak.load().is_none());
    println!(
        "paged_region_slots actual_native_roles={} actual_all_weak_nil={native_released} actual_pixels_zero={} actual_pixels={:?} whole_product=false formal=false",
        weaks.len(),
        observer.is_released(),
        observer.snapshot(),
    );
    if !passed || !native_released || !observer.is_released() {
        return Err(invalid());
    }
    Ok(())
}
