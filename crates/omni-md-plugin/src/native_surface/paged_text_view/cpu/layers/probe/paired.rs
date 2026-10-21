//! Exercise actual five-layer paired composition, pixel differences, and interrupted installation with
//! fixed synthetic bitmaps only.

use super::super::super::calibration::Calibrations;
use super::super::slot::{AssignmentPoint, Layout, Palette, Slot};
use super::*;
use crate::native_surface::viewport_pixels::MaskPairBlend;

#[derive(Clone, Copy, Debug)]
enum PaletteCase {
    CoverageRamp,
    GlyphColors,
}
impl PaletteCase {
    fn expected_blend(self) -> MaskPairBlend {
        match self {
            Self::CoverageRamp => MaskPairBlend::ForegroundThenForeground,
            Self::GlyphColors => MaskPairBlend::ForegroundThenBackground,
        }
    }
}

/// The left tile uses actual per-pixel sRGB blends and the right retains translucent color; both must
/// compose exactly in one frame.
fn fixture(
    source: &mut Canvas,
    counters: Arc<NativeMarkdownViewportPixels>,
    case: PaletteCase,
) -> Result<TileFrame> {
    let mut calibrations = Calibrations::default();
    fixture_with_prior(source, counters, None, false, case, &mut calibrations)
}

fn fixture_with_prior(
    source: &mut Canvas,
    counters: Arc<NativeMarkdownViewportPixels>,
    prior: Option<&TileFrame>,
    change_pixel: bool,
    case: PaletteCase,
    calibrations: &mut Calibrations,
) -> Result<TileFrame> {
    let base = [19_u8, 21, 28];
    let ink = [219_u8, 224, 235];
    CGContext::set_rgb_fill_color(
        Some(&source.context),
        f64::from(base[0]) / 255.0,
        f64::from(base[1]) / 255.0,
        f64::from(base[2]) / 255.0,
        1.0,
    );
    CGContext::fill_rect(
        Some(&source.context),
        NSRect::new(NSPoint::ZERO, NSSize::new(512.0, HEIGHT as f64)),
    );
    for y in 0..HEIGHT {
        for x in 0..512 {
            // Vary both rows and columns so reversed orientation, exchanged masks, or wrong stride
            // cannot hide in constant rows.
            let alpha = ((x + y * 7) & 255) as f64 / 255.0;
            CGContext::set_rgb_fill_color(
                Some(&source.context),
                f64::from(ink[0]) / 255.0,
                f64::from(ink[1]) / 255.0,
                f64::from(ink[2]) / 255.0,
                alpha,
            );
            CGContext::fill_rect(
                Some(&source.context),
                NSRect::new(NSPoint::new(x as f64, y as f64), NSSize::new(1.0, 1.0)),
            );
        }
    }
    CGContext::flush(Some(&source.context));
    if matches!(case, PaletteCase::GlyphColors) {
        // These bounded colors were observed in actual glyph output, including 161820 that
        // same-foreground paired masks cannot express.
        // This controls exact pixel transport only; independent large-window scenarios own actual
        // font/selection-paint verification.
        let colors = [
            [19, 21, 28, 255],
            [219, 224, 235, 255],
            [22, 24, 32, 255],
            [99, 102, 110, 255],
            [181, 185, 196, 255],
            [171, 176, 186, 255],
            [77, 79, 88, 255],
            [44, 46, 53, 255],
            [147, 151, 161, 255],
            [193, 197, 208, 255],
            [133, 137, 146, 255],
            [29, 32, 39, 255],
            [42, 44, 52, 255],
            [216, 221, 232, 255],
            [200, 205, 216, 255],
            [23, 25, 32, 255],
        ];
        for y in 0..HEIGHT {
            for x in 0..512 {
                // Preserve exact 2D colors so pixel comparison detects flips, swapped masks, and wrong
                // stride.
                let pixel = colors[(x + y * 7) % colors.len()];
                source.pixels[(y * WIDTH + x) * 4..][..4].copy_from_slice(&pixel);
            }
        }
    }
    if change_pixel {
        // Replace one actual pixel with another color already in its palette, preserving endpoints,
        // geometry, and other tiles.
        let first: [u8; 4] = source.pixels[..4].try_into().unwrap();
        let other: [u8; 4] = source.pixels[4..512 * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .find(|pixel| **pixel != first)
            .copied()
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        source.pixels[..4].copy_from_slice(&other);
    }
    for y in 0..HEIGHT {
        for x in 512..WIDTH {
            source.pixels[(y * WIDTH + x) * 4..][..4].copy_from_slice(&[128, 0, 0, 128]);
        }
    }
    let mut bitmap = TiledBitmap::new(WIDTH, HEIGHT, counters.clone())?;
    calibrations.begin_frame(); // A successor candidate on the same surface opens this frame quota only; original pure tables still certify every color.
    while let Some(tile) = bitmap.next_tile() {
        let writable = bitmap.begin_tile(tile)?;
        let row = CGBitmapContextGetBytesPerRow(Some(writable.context()));
        let data = CGBitmapContextGetData(Some(writable.context()));
        if data.is_null() || row < tile.width * 4 {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        for y in 0..tile.height {
            // SAFETY: The unique writable context covers tile stride*height; write active pixels only
            // and end borrows before Native calls.
            let target = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<u8>().add(y * row), tile.width * 4)
            };
            let at = ((tile.y + y) * WIDTH + tile.x) * 4;
            target.copy_from_slice(&source.pixels[at..at + tile.width * 4]);
        }
        let prepared = bitmap.prepare_tile(writable)?;
        let pairs = match prepared.mask_request() {
            Some(request) => calibrations.pairs(&request, counters.clone(), &|| Ok(()))?,
            None => None, // An actually transparent tile receives no paired-mask proposal and retains original RGBA.
        };
        bitmap.finish_prepared(prepared, pairs.as_ref(), prior)?;
    }
    let frame = bitmap.finish()?;
    if frame.tiles.len() != 2
        || !matches!(
            frame.tiles[0].presentation,
            PixelPresentation::OpaquePairedMasks { blend, .. } if blend == case.expected_blend()
        )
        || frame.tiles[0].secondary_image.is_none()
        || frame.tiles[1].presentation != PixelPresentation::Image
        || frame.tiles[1].secondary_image.is_some()
    {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    Ok(frame)
}

/// Equal redraws reuse the pair together; changing one pixel replaces both image identities even if one
/// plane bytes happen to match.
fn paired_reuse(case: PaletteCase) -> Result<()> {
    let counters = Arc::new(NativeMarkdownViewportPixels::default());
    let observer = counters.observer();
    let (result, detached, weaks) = autoreleasepool(|_| -> Result<_> {
        let mut source = Canvas::new()?;
        let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH as f64, HEIGHT as f64));
        let backing = CALayer::new();
        backing.setFrame(bounds);
        backing.setBounds(bounds);
        let pool = LayerPool::new(&|| true)?;
        backing.addSublayer(pool.mount());
        let mut weaks = pool.weaks();
        weaks.push(layer_weak(&backing));
        // Original, equal, and one-pixel-changed frames share bounded pure tables for the same
        // surface-like lifetime.
        // Each still uses actual Native publication and full RGBA comparison; reuse counts cannot prove
        // color correctness.
        let mut calibrations = Calibrations::default();
        let result = (|| -> Result<()> {
            let original = fixture_with_prior(
                &mut source,
                counters.clone(),
                None,
                false,
                case,
                &mut calibrations,
            )?;
            let original_publication =
                render_exact(&pool, &backing, &original, &source, bounds, None)?;
            weaks.extend(pool.weaks());
            // Read original group identity from the actual mount; equal redraw should preserve that
            // Native graph rather than swap new shells.
            // SAFETY: Complete Native validation already verified the standard CALayer child array on
            // this mount.
            let original_group = unsafe { pool.mount().sublayers() }
                .filter(|children| children.len() == 1)
                .map(|children| children.objectAtIndex(0))
                .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
            // Read two weak slot identities from the completely published actual array, without
            // private-slot access or strong claims on
            // replaced old containers that would interfere with actual destruction and provider-release
            // observation.
            let original_slots = {
                // SAFETY: the retained fixture group is used on main and contains only CALayer
                // children installed by LayerPool; the returned typed array is inspected synchronously.
                let children = unsafe { original_group.sublayers() }
                    .filter(|children| children.len() == 2)
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
                [
                    layer_weak(&children.objectAtIndex(0)),
                    layer_weak(&children.objectAtIndex(1)),
                ]
            };
            let same = fixture_with_prior(
                &mut source,
                counters.clone(),
                Some(&original),
                false,
                case,
                &mut calibrations,
            )?;
            let same_publication = render_exact(
                &pool,
                &backing,
                &same,
                &source,
                bounds,
                Some(&original_publication),
            )?;
            // Continue exact composition comparison; unchanged actual group identity also establishes
            // no resubmitted group of equal layers.
            // SAFETY: Read identities from original mount standard children only, without private
            // counter inspection.
            let same_native_group = unsafe { pool.mount().sublayers() }.is_some_and(|children| {
                children.len() == 1
                    && std::ptr::eq::<CALayer>(&*children.objectAtIndex(0), &*original_group)
            });
            weaks.extend(pool.weaks());
            let changed = fixture_with_prior(
                &mut source,
                counters.clone(),
                Some(&original),
                true,
                case,
                &mut calibrations,
            )?;
            render_exact(
                &pool,
                &backing,
                &changed,
                &source,
                bounds,
                Some(&same_publication),
            )?;
            weaks.extend(pool.weaks());
            // Only the left tile changes color under the same geometry; beyond equal CGImage, the right
            // tile must retain its original Native
            // container and parent. Rebuilding the group may render correct pixels but would violate
            // this resource-reuse contract.
            // SAFETY: this live pool mount belongs to the main-thread fixture and its children
            // were installed exclusively through the CALayer sublayer API.
            let partial_group_same = unsafe { pool.mount().sublayers() }.is_some_and(|children| {
                children.len() == 1
                    && std::ptr::eq::<CALayer>(&*children.objectAtIndex(0), &*original_group)
            });
            // SAFETY: the local retained group remains alive on main; only LayerPool-created
            // CALayer entries inhabit this public array, and no borrowed element escapes it.
            let partial_slots = unsafe { original_group.sublayers() }
                .filter(|children| children.len() == 2)
                .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
            let changed_slot = partial_slots.objectAtIndex(0);
            let retained_slot = partial_slots.objectAtIndex(1);
            let changed_slot_is_new = original_slots[0]
                .load()
                .is_none_or(|old| !std::ptr::eq::<AnyObject>(&*old, &*changed_slot as &AnyObject));
            let retained_slot_same = original_slots[1]
                .load()
                .is_some_and(|old| std::ptr::eq::<AnyObject>(&*old, &*retained_slot as &AnyObject))
                && retained_slot
                    .superlayer()
                    .as_deref()
                    .is_some_and(|parent| std::ptr::eq::<CALayer>(parent, &*original_group));
            drop((changed_slot, retained_slot, partial_slots, original_group));
            let left = &original.tiles[0];
            let equal = &same.tiles[0];
            let altered = &changed.tiles[0];
            let first_equal = std::ptr::eq::<CGImage>(&*left.image, &*equal.image);
            let second_equal = std::ptr::eq::<CGImage>(
                left.secondary_image
                    .as_deref()
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?,
                equal
                    .secondary_image
                    .as_deref()
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?,
            );
            let first_changed = !std::ptr::eq::<CGImage>(&*left.image, &*altered.image);
            let second_changed = !std::ptr::eq::<CGImage>(
                left.secondary_image
                    .as_deref()
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?,
                altered
                    .secondary_image
                    .as_deref()
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?,
            );
            let valid = same_native_group
                && partial_group_same
                && changed_slot_is_new
                && retained_slot_same
                && same.reused_images == 2
                && changed.reused_images == 1
                && first_equal
                && second_equal
                && first_changed
                && second_changed;
            println!(
                "paged_paired_reuse case={case:?} actual_same_tile_reuses={} actual_changed_tile_reuses={} actual_both_equal={first_equal}/{second_equal} actual_both_changed={first_changed}/{second_changed} actual_same_native_group={same_native_group} partial_group_same={partial_group_same} changed_slot_is_new={changed_slot_is_new} unchanged_native_slot_and_parent_same={retained_slot_same} all_three_native_frames_pixel_exact=true shared_surface_calibration=true formal=false",
                same.reused_images, changed.reused_images
            );
            if !valid {
                return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
            }
            Ok(())
        })();
        let detached = pool.detach(&backing); // Clear original Native relationships after comparison/construction failure instead of bypassing release on early return.
        drop((calibrations, source, pool, backing)); // Surface-like teardown returns pure tables and actual providers/contexts together.
        CATransaction::flush();
        Ok((result, detached, weaks))
    })?;
    result?;
    if !detached || !weaks.iter().all(|weak| weak.load().is_none()) || !observer.is_released() {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    Ok(())
}

/// After actual Native publication, compare complete original RGBA composition, preserving paired
/// colors and the translucent right tile.
fn render_exact(
    pool: &LayerPool,
    backing: &CALayer,
    frame: &TileFrame,
    source: &Canvas,
    bounds: NSRect,
    previous: Option<&Publication>,
) -> Result<Publication> {
    let reference = CGBitmapContextCreateImage(Some(&source.context))
        .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
    let baseline = Canvas::new()?;
    baseline.blue(bounds);
    CGContext::draw_image(Some(&baseline.context), bounds, Some(&reference));
    CGContext::flush(Some(&baseline.context));
    let rendered = Canvas::new()?;
    rendered.blue(bounds);
    // Use production admission, actually changing unequal frames; even successful reuse still requires
    // complete rendering and byte comparison.
    let reused = previous
        .map(|previous| pool.reuse_publication(backing, frame, previous, bounds, 1.0, &|| true))
        .transpose()?
        .flatten();
    let publication = match reused {
        Some(publication) => publication,
        None => {
            let mut attempt = pool.attempt()?;
            // Offer the same original predecessor to production partial publication first; use full
            // two-group publication only when explicitly inapplicable.
            // Return errors after actual replacement directly, without a full replay hiding
            // partial-commit effects.
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
    if rendered.pixels != baseline.pixels {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    Ok(publication)
}

/// Transfer mount externally only after the first actual slot replacement; cleanup must preserve the
/// external parent,
/// while returning all image obligations from the displayed fresh slot, unchanged old slot, and
/// uncommitted candidate.
fn interrupted_partial_publication(case: PaletteCase) -> Result<()> {
    let counters = Arc::new(NativeMarkdownViewportPixels::default());
    let observer = counters.observer();
    let (passed, weaks) = autoreleasepool(|_| -> Result<_> {
        let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH as f64, HEIGHT as f64));
        let successor = CALayer::new();
        successor.setFrame(bounds);
        successor.setBounds(bounds);
        let foreign_owner = layer_weak(&successor);
        let (passed, weaks) = autoreleasepool(|_| -> Result<_> {
            let mut source = Canvas::new()?;
            let backing = CALayer::new();
            backing.setFrame(bounds);
            backing.setBounds(bounds);
            let pool = LayerPool::new(&|| true)?;
            backing.addSublayer(pool.mount());
            let observed = RefCell::new(pool.weaks());
            let backing_weak = layer_weak(&backing);
            observed.borrow_mut().push(backing_weak);
            let mut calibrations = Calibrations::default();
            // This boolean records observed actual model-graph replacement only, without selecting
            // revocation by call counts or private serials.
            let actual_replacement_seen = Cell::new(false);
            let result = (|| -> Result<bool> {
                let original = fixture_with_prior(
                    &mut source,
                    counters.clone(),
                    None,
                    false,
                    case,
                    &mut calibrations,
                )?;
                let previous = render_exact(&pool, &backing, &original, &source, bounds, None)?;
                let roles = pool.weaks();
                observed.borrow_mut().extend(roles);
                // The original graph passed full pixel comparison; read original group/two-slot
                // identities from public Native arrays only.
                // SAFETY: the fixture owns the live main-thread mount and installs only
                // CALayer children, so the public sublayer array has the declared element type.
                let group = unsafe { pool.mount().sublayers() }
                    .filter(|children| children.len() == 1)
                    .map(|children| children.objectAtIndex(0))
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
                let original_slots = {
                    // SAFETY: the retained fixture group has only CALayer children; this
                    // synchronous main-thread snapshot lends weak identities without changing
                    // ownership.
                    let children = unsafe { group.sublayers() }
                        .filter(|children| children.len() == 2)
                        .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
                    [
                        layer_weak(&children.objectAtIndex(0)),
                        layer_weak(&children.objectAtIndex(1)),
                    ]
                };
                let changed = fixture_with_prior(
                    &mut source,
                    counters.clone(),
                    Some(&original),
                    true,
                    case,
                    &mut calibrations,
                )?;
                // The platform current boundary observes the actual graph only; constructing a
                // candidate leaves the old left tile and triggers no transfer;
                // transfer occurs once the first replacement actually installs a new left tile while
                // the right remains unchanged.
                let current = || {
                    if !parent_is(pool.mount(), &backing) {
                        return false;
                    }
                    // SAFETY: this callback runs synchronously on main while the fixture retains
                    // the original group; all public-array entries were installed as CALayer objects.
                    let children = unsafe { group.sublayers() };
                    if !actual_replacement_seen.get()
                        && let Some(children) = children.filter(|children| children.len() == 2)
                    {
                        let left = children.objectAtIndex(0);
                        let right = children.objectAtIndex(1);
                        let replaced = original_slots[0].load().is_none_or(|old| {
                            !std::ptr::eq::<AnyObject>(&*old, &*left as &AnyObject)
                        });
                        let unchanged = original_slots[1].load().is_some_and(|old| {
                            std::ptr::eq::<AnyObject>(&*old, &*right as &AnyObject)
                        });
                        if replaced && unchanged {
                            // End observation-slot borrows before actual Native transfer, additionally
                            // recording
                            // the actual newly mounted container rather than assuming internal pending
                            // participates correctly in weak accounting.
                            let roles = pool.weaks();
                            let actual_slot = layer_weak(&left);
                            observed.borrow_mut().extend(roles);
                            observed.borrow_mut().push(actual_slot);
                            actual_replacement_seen.set(true);
                            successor.addSublayer(pool.mount());
                        }
                    }
                    parent_is(pool.mount(), &backing)
                };
                let mut attempt = pool.attempt()?;
                let published =
                    pool.publish_partial(&backing, &changed, &previous, &mut attempt, &current);
                let rejected = matches!(published, Err(NativeMarkdownPagedTextViewError::Revoked));
                let old_refused = matches!(
                    pool.validate(&backing, &previous, &|| true),
                    Err(NativeMarkdownPagedTextViewError::Revoked)
                );
                let foreign_preserved = parent_is(pool.mount(), &successor);
                // Failure is not absence of commit: the external graph actually contained a fresh slot.
                // After rollback, rendering must contain no
                // old/new body pixels; Err or a local cleanup flag alone cannot establish release.
                let empty = Canvas::new()?;
                empty.blue(bounds);
                let rendered = Canvas::new()?;
                rendered.blue(bounds);
                successor.renderInContext(&rendered.context);
                CGContext::flush(Some(&rendered.context));
                let cleared_pixels = rendered.pixels == empty.pixels;
                println!(
                    "paged_partial_interrupted case={case:?} actual_replacement_seen={} actual_revoked={rejected} original_publication_rejected={old_refused} foreign_parent_preserved={foreign_preserved} actual_foreign_tree_empty_rgba={cleared_pixels} source_failure_claim=false formal=false",
                    actual_replacement_seen.get(),
                );
                Ok(actual_replacement_seen.get()
                    && rejected
                    && old_refused
                    && foreign_preserved
                    && cleared_pixels)
            })();
            // Comparison/construction failure follows the same actual cleanup; this pool cannot detach
            // the external parent.
            // All original/new frames have left their local stack; do not relax release checks for
            // providers retained by test-owned frames.
            let detached = pool.detach(&backing);
            let weaks = std::mem::take(&mut *observed.borrow_mut());
            println!(
                "paged_partial_interrupted case={case:?} actual_result={result:?} actual_detach_intact={detached} formal=false"
            );
            let passed = result.is_ok_and(|passed| passed) && !detached;
            drop((calibrations, pool, backing, source));
            CATransaction::flush();
            Ok((passed, weaks))
        })?;
        // Return the original owner and temporary Native arrays first; keep the external owner alive
        // while checking every provider/
        // mapping and other role so destroying the transferee cannot hide omitted business-image
        // cleanup.
        CATransaction::flush();
        let pixels_released = observer.is_released();
        let original_mount = weaks[0].load();
        let original_mount_alive = original_mount.is_some();
        // Each actual publication may observe the same mount again; its external retention is expected,
        // so exempt that role by exact object identity only, without exempting other live empty layers.
        let other_roles_released = weaks.iter().all(|weak| {
            weak.load().is_none_or(|actual| {
                original_mount
                    .as_deref()
                    .is_some_and(|mount| std::ptr::eq::<AnyObject>(&*actual, mount))
            })
        });
        let foreign_alive = foreign_owner.load().is_some();
        println!(
            "paged_partial_interrupted case={case:?} original_pixels_released_with_foreign_alive={pixels_released} original_mount_alive={original_mount_alive} foreign_owner_alive={foreign_alive} other_native_roles_released={other_roles_released} actual_pixels={:?} formal=false",
            observer.snapshot(),
        );
        drop(original_mount); // Temporary weak-observation claims must not block final nil checks after external release.
        // The external owner returns its transferred empty mount only after original-owner release was
        // observed.
        // SAFETY: This synthetic control owns successor exclusively; None takes or deletes no product
        // Native relationships.
        unsafe { successor.setSublayers(None) };
        drop(successor);
        CATransaction::flush();
        let foreign_released = foreign_owner.load().is_none();
        Ok((
            passed
                && pixels_released
                && original_mount_alive
                && foreign_alive
                && other_roles_released
                && foreign_released,
            weaks,
        ))
    })?;
    if !passed || !weaks.iter().all(|weak| weak.load().is_none()) || !observer.is_released() {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum Mutation {
    None,
    SecondaryImage,
    SecondaryMask,
    SecondaryColor,
    Background,
    ReversedChildren,
    Hidden,
    Opacity,
    ZPosition,
    GroupHidden,
    GroupOpacity,
    GroupZPosition,
    MountHidden,
    MountOpacity,
    MountZPosition,
}

/// After complete actual publication, controls modify the second group or composition order rather than
/// exercising the neutral first image alone.
fn visible_graphs(case: PaletteCase) -> Result<()> {
    for mutation in [
        Mutation::None,
        Mutation::SecondaryImage,
        Mutation::SecondaryMask,
        Mutation::SecondaryColor,
        Mutation::Background,
        Mutation::ReversedChildren,
        Mutation::Hidden,
        Mutation::Opacity,
        Mutation::ZPosition,
        Mutation::GroupHidden,
        Mutation::GroupOpacity,
        Mutation::GroupZPosition,
        Mutation::MountHidden,
        Mutation::MountOpacity,
        Mutation::MountZPosition,
    ] {
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let (passed, weaks) = autoreleasepool(|_| -> Result<_> {
            let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH as f64, HEIGHT as f64));
            let mut source = Canvas::new()?;
            let frame = fixture(&mut source, counters.clone(), case)?;
            let reference = CGBitmapContextCreateImage(Some(&source.context))
                .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
            let baseline = Canvas::new()?;
            baseline.blue(bounds);
            CGContext::draw_image(Some(&baseline.context), bounds, Some(&reference));
            CGContext::flush(Some(&baseline.context));
            let rendered = Canvas::new()?;
            rendered.blue(bounds);
            let backing = CALayer::new();
            backing.setFrame(bounds);
            backing.setBounds(bounds);
            let pool = LayerPool::new(&|| true)?;
            backing.addSublayer(pool.mount());
            let mut weaks = pool.weaks();
            weaks.push(layer_weak(&backing));
            let successor = CALayer::new();
            weaks.push(layer_weak(&successor));
            let foreign_color = CGColor::new_srgb(0.2, 0.3, 0.4, 1.0);
            let attempt = pool.attempt()?;
            let publication = match pool.publish(&backing, &frame, &attempt, bounds, 1.0, &|| true)
            {
                Ok(publication) => publication,
                Err(error) => {
                    pool.detach(&backing); // Publication failure returns the constructed candidate before exiting the control, preserving actual cleanup.
                    return Err(error);
                }
            };
            weaks.extend(pool.weaks());
            let [container, first, mask, second, second_mask] =
                pool.groups[0].slots.borrow()[0].diagnostic_all_layers();
            let result = (|| -> Result<(bool, bool)> {
                pool.validate(&backing, &publication, &|| true)?;
                pool.mount().renderInContext(&rendered.context);
                CGContext::flush(Some(&rendered.context));
                let exact = rendered.pixels == baseline.pixels;
                match mutation {
                    Mutation::None => {}
                    Mutation::SecondaryImage => {
                        // SAFETY: The external control retains a complete RGBA replacement, without
                        // pretending it is paired A8.
                        unsafe {
                            second_mask.setContents(Some(
                                &*std::ptr::from_ref(&*reference).cast::<AnyObject>(),
                            ))
                        };
                    }
                    Mutation::SecondaryMask => {
                        // SAFETY: An independent standard layer replaces the second mask; original
                        // cleanup must preserve that successor relationship.
                        unsafe { second.setMask(Some(&successor)) };
                    }
                    Mutation::SecondaryColor => second.setBackgroundColor(Some(&foreign_color)),
                    Mutation::Background => container.setBackgroundColor(Some(&foreign_color)),
                    Mutation::ReversedChildren => {
                        let reversed = NSArray::from_slice(&[&*second, &*first]);
                        // SAFETY: Reorder the two actual owned roles only, checking that order itself
                        // belongs to composition authority.
                        unsafe { container.setSublayers(Some(&reversed)) };
                    }
                    Mutation::Hidden => second.setHidden(true),
                    Mutation::Opacity => second.setOpacity(0.0),
                    Mutation::ZPosition => second.setZPosition(-1.0),
                    Mutation::GroupHidden => pool.groups[0].layer.setHidden(true),
                    Mutation::GroupOpacity => pool.groups[0].layer.setOpacity(0.5),
                    Mutation::GroupZPosition => pool.groups[0].layer.setZPosition(1.0),
                    Mutation::MountHidden => pool.mount().setHidden(true),
                    Mutation::MountOpacity => pool.mount().setOpacity(0.5),
                    Mutation::MountZPosition => pool.mount().setZPosition(1.0),
                }
                let reused =
                    pool.reuse_publication(&backing, &frame, &publication, bounds, 1.0, &|| true);
                let rejected = if matches!(mutation, Mutation::None) {
                    matches!(reused, Ok(Some(_)))
                } else {
                    matches!(reused, Err(NativeMarkdownPagedTextViewError::Revoked))
                };
                Ok((exact, rejected))
            })();
            let detached = pool.detach(&backing);
            let preserved = match mutation {
                Mutation::SecondaryImage => {
                    // SAFETY: Compare the original external opaque identity without reading revoked
                    // image contents.
                    unsafe { second_mask.contents() }
                        .as_deref()
                        .is_some_and(|actual| {
                            std::ptr::from_ref(actual).cast::<()>()
                                == std::ptr::from_ref(&*reference).cast::<()>()
                        })
                }
                Mutation::SecondaryMask => second
                    .mask()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<CALayer>(actual, &*successor)),
                Mutation::SecondaryColor => second
                    .backgroundColor()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<CGColor>(actual, &*foreign_color)),
                Mutation::Background => container
                    .backgroundColor()
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq::<CGColor>(actual, &*foreign_color)),
                Mutation::Hidden => second.isHidden(),
                Mutation::Opacity => second.opacity() == 0.0,
                Mutation::ZPosition => second.zPosition() == -1.0,
                Mutation::GroupHidden => pool.groups[0].layer.isHidden(),
                Mutation::GroupOpacity => pool.groups[0].layer.opacity() == 0.5,
                Mutation::GroupZPosition => pool.groups[0].layer.zPosition() == 1.0,
                Mutation::MountHidden => pool.mount().isHidden(),
                Mutation::MountOpacity => pool.mount().opacity() == 0.5,
                Mutation::MountZPosition => pool.mount().zPosition() == 1.0,
                Mutation::None | Mutation::ReversedChildren => true,
            };
            let detach_expected = matches!(mutation, Mutation::None | Mutation::ReversedChildren);
            println!(
                "paged_paired_slots case={case:?} mutation={mutation:?} actual_result={result:?} actual_detach={detached} expected_detach={detach_expected} successor_preserved={preserved} actual_roles={} formal=false",
                weaks.len()
            );
            // The external control returns its replacements before final Native weak checks; this is
            // not part of the close action under test.
            // SAFETY: These five roles and external successor belong to this synthetic control only,
            // after business cleanup completed.
            unsafe {
                second.setMask(None);
                second_mask.setContents(None);
                successor.setSublayers(None);
            }
            second.setBackgroundColor(None);
            container.setBackgroundColor(None);
            second.setHidden(false);
            second.setOpacity(1.0);
            second.setZPosition(0.0);
            // The control restores its own external mutations after close was recorded; restoration
            // cannot revise the close outcome.
            for layer in [&*pool.groups[0].layer, pool.mount()] {
                layer.setHidden(false);
                layer.setOpacity(1.0);
                layer.setZPosition(0.0);
            }
            drop(frame);
            CATransaction::flush();
            let pixels_released = observer.is_released();
            let passed = result.is_ok_and(|(exact, rejected)| exact && rejected)
                && preserved
                && detached == detach_expected
                && pixels_released;
            drop((
                pool,
                backing,
                container,
                first,
                mask,
                second,
                second_mask,
                successor,
                foreign_color,
                reference,
                source,
                baseline,
                rendered,
            ));
            CATransaction::flush();
            Ok((passed, weaks))
        })?;
        let native = weaks.iter().all(|value| value.load().is_none());
        println!(
            "paged_paired_slots case={case:?} mutation={mutation:?} exact_and_negative_checks={passed} actual_all_weak_nil={native} actual_pixels={:?} formal=false",
            observer.snapshot()
        );
        if !passed || !native || !observer.is_released() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
    }
    Ok(())
}

/// The gated hook runs strictly after the actual setter and before slot registration; external
/// container retention must not keep original pixels.
fn interrupted_installations(case: PaletteCase) -> Result<()> {
    for point in [
        AssignmentPoint::Image,
        AssignmentPoint::SecondaryImage,
        AssignmentPoint::Background,
        AssignmentPoint::ForegroundColor,
        AssignmentPoint::SecondColor,
        AssignmentPoint::Mask,
        AssignmentPoint::SecondMask,
        AssignmentPoint::Children,
    ] {
        let counters = Arc::new(NativeMarkdownViewportPixels::default());
        let observer = counters.observer();
        let (passed, weaks) = autoreleasepool(|_| -> Result<_> {
            let mut source = Canvas::new()?;
            let frame = fixture(&mut source, counters.clone(), case)?;
            let image = &frame.tiles[0];
            let slot = Slot::new(image.presentation, &|| true)?;
            let palette = Palette::new(&|| true)?;
            let group = CALayer::new();
            let mut weaks = Vec::new();
            slot.add_weaks(&mut weaks);
            weaks.push(layer_weak(&group));
            let container = slot.container().clone(); // The external control retains the actual original layer until after pixel-release verification.
            let fired = Cell::new(false);
            let installed = slot.install_with_probe(
                &image.image,
                image.secondary_image.as_ref(),
                Layout {
                    rect: NSRect::new(NSPoint::ZERO, NSSize::new(512.0, HEIGHT as f64)),
                    scale: 1.0,
                    presentation: image.presentation,
                },
                &palette,
                &|| true,
                &|observed| {
                    if observed == point && !fired.replace(true) {
                        let _ = slot.clear(&group, &palette);
                    }
                },
            );
            let rejected = matches!(installed, Err(NativeMarkdownPagedTextViewError::Revoked));
            let cleared = slot.clear(&group, &palette);
            drop(frame);
            CATransaction::flush();
            let pixels_released_with_foreign_container = observer.is_released();
            println!(
                "paged_paired_assignment case={case:?} point={point:?} effect_before_record_hook={} actual_rejected={rejected} final_clear={cleared} original_pixels_released_with_container_alive={pixels_released_with_foreign_container} actual_pixels={:?} formal=false",
                fired.get(),
                observer.snapshot()
            );
            let passed =
                fired.get() && rejected && cleared && pixels_released_with_foreign_container;
            drop((container, slot, group, palette, source));
            CATransaction::flush();
            Ok((passed, weaks))
        })?;
        let native = weaks.iter().all(|value| value.load().is_none());
        if !passed || !native || !observer.is_released() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
    }
    Ok(())
}

pub(super) fn run(_mtm: MainThreadMarker) -> Result<()> {
    for case in [PaletteCase::CoverageRamp, PaletteCase::GlyphColors] {
        // Each actual composition order independently exercises reuse, external mutation, and
        // revocation between setter and registration.
        paired_reuse(case)?;
        interrupted_partial_publication(case)?;
        visible_graphs(case)?;
        interrupted_installations(case)?;
    }
    Ok(())
}
