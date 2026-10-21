//! Keep the complete resized picture while checking actual previous-frame role destruction instead of
//! an idle role pool.

use super::*;

pub(super) fn run() -> Result<()> {
    let counters = Arc::new(NativeMarkdownViewportPixels::default());
    let observer = counters.observer();
    let (passed, weaks) = autoreleasepool(|_| -> Result<_> {
        let mut source = Canvas::new()?;
        let wide = fixture(&mut source, Arc::clone(&counters))?;
        let wide_bounds = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH as f64, HEIGHT as f64));
        let backing = CALayer::new();
        backing.setFrame(wide_bounds);
        backing.setBounds(wide_bounds);
        let pool = LayerPool::new(&|| true)?;
        backing.addSublayer(pool.mount());
        let initially_empty = pool
            .groups
            .iter()
            .all(|group| group.slots.borrow().is_empty());
        let mut weaks = pool.weaks();
        weaks.push(layer_weak(&backing));
        let (old_roles, correct_roles) = autoreleasepool(|_| -> Result<_> {
            let attempt = pool.attempt()?;
            let published = pool.publish(&backing, &wide, &attempt, wide_bounds, 1.0, &|| true)?;
            pool.validate(&backing, &published, &|| true)?;
            let slots = pool.groups[0].slots.borrow().clone();
            let mut old_roles = Vec::new();
            for slot in &slots {
                slot.add_weaks(&mut old_roles);
            }
            // Complete Native validation certified the actual first single-mask and second
            // translucent-image mounting relationships.
            // Compare actual allocated weak identities individually, excluding extra preconstructed but
            // unmounted roles.
            let [container, foreground, mask] = slots[0].diagnostic_layers();
            let expected = [&*container, &*foreground, &*mask, &**slots[1].container()];
            let matches = |weak: &Weak<AnyObject>, expected: &CALayer| {
                weak.load().as_deref().is_some_and(|actual| {
                    std::ptr::from_ref(actual).cast::<()>()
                        == std::ptr::from_ref(expected).cast::<()>()
                })
            };
            let correct_roles = old_roles
                .iter()
                .all(|weak| expected.iter().any(|layer| matches(weak, layer)))
                && expected
                    .iter()
                    .all(|layer| old_roles.iter().any(|weak| matches(weak, layer)));
            Ok((old_roles, correct_roles))
        })?;
        weaks.extend(old_roles.iter().cloned());

        // Draw actual left-side pixels after narrowing, without scaling, downsampling, or deleting
        // colors to manufacture lower memory.
        let narrow_bounds = NSRect::new(NSPoint::ZERO, NSSize::new(512.0, HEIGHT as f64));
        let mut narrow_bitmap = TiledBitmap::new(512, HEIGHT, Arc::clone(&counters))?;
        let tile = narrow_bitmap
            .next_tile()
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        let writable = narrow_bitmap.begin_tile(tile)?;
        let row_bytes = CGBitmapContextGetBytesPerRow(Some(writable.context()));
        let address = CGBitmapContextGetData(Some(writable.context()));
        if address.is_null() || row_bytes < 512 * 4 {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        for row in 0..HEIGHT {
            // SAFETY: The unique writable context covers row_bytes*HEIGHT; each copy contains one
            // bounded tile row.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    source.pixels.as_ptr().add(row * source.row_bytes),
                    address.cast::<u8>().add(row * row_bytes),
                    512 * 4,
                );
            }
        }
        narrow_bitmap.finish_tile(writable, Some(&wide))?;
        let narrow = narrow_bitmap.finish()?;
        // The complete width changed, but the left tile was redrawn from the same actual pixels.
        // Compare actual
        // CGImage identity/presentation to establish old read-only-image reuse, rather than inferring
        // allocation behavior from a reuse count.
        let unchanged_image_reused =
            narrow
                .tiles
                .first()
                .zip(wide.tiles.first())
                .is_some_and(|(current, original)| {
                    std::ptr::eq(&*current.image, &*original.image)
                        && current.presentation == original.presentation
                        && match (&current.secondary_image, &original.secondary_image) {
                            (None, None) => true,
                            (Some(current), Some(original)) => {
                                std::ptr::eq(&**current, &**original)
                            }
                            _ => false,
                        }
                });
        autoreleasepool(|_| -> Result<()> {
            backing.setFrame(narrow_bounds);
            backing.setBounds(narrow_bounds);
            let attempt = pool.attempt()?;
            let published =
                pool.publish(&backing, &narrow, &attempt, narrow_bounds, 1.0, &|| true)?;
            pool.validate(&backing, &published, &|| true)?;
            weaks.extend(pool.weaks());
            Ok(())
        })?;
        drop(wide); // Return the caller old pixel claim only after atomic new-frame publication.
        CATransaction::flush();
        let old_roles_returned = old_roles.iter().all(|weak| weak.load().is_none());
        let retired_group_empty = pool.groups[0].slots.borrow().is_empty();
        // Clear the detached old group again as on later reuse; once actual weak identities are nil,
        // return their list allocation too.
        // Keep the narrow picture mounted while subsequent exact pixel comparison checks that resource
        // release preserved the live frame.
        let retired_storage_returned =
            pool.groups[0].clear() && pool.groups[0].retired.borrow().capacity() == 0;

        let rendered = Canvas::sized(512, HEIGHT)?;
        rendered.blue(narrow_bounds);
        pool.mount().renderInContext(&rendered.context);
        CGContext::flush(Some(&rendered.context));
        let exact = (0..HEIGHT).all(|row| {
            rendered.pixels[row * rendered.row_bytes..row * rendered.row_bytes + 512 * 4]
                == source.pixels[row * source.row_bytes..row * source.row_bytes + 512 * 4]
        });
        let detached = pool.detach(&backing);
        drop((narrow, pool, backing, source, rendered));
        CATransaction::flush();
        println!(
            "paged_lazy_layer_resize initially_empty={initially_empty} actual_representation_roles={correct_roles} actual_unchanged_image_reused={unchanged_image_reused} actual_old_role_weak_nil={old_roles_returned} retired_group_empty={retired_group_empty} retired_weak_storage_returned={retired_storage_returned} exact_narrow_rgba={exact} actual_detach={detached} whole_product=false formal=false"
        );
        Ok((
            initially_empty
                && correct_roles
                && unchanged_image_reused
                && old_roles_returned
                && retired_group_empty
                && retired_storage_returned
                && exact
                && detached,
            weaks,
        ))
    })?;
    CATransaction::flush();
    let native_returned = weaks.iter().all(|weak| weak.load().is_none());
    if !passed || !native_returned || !observer.is_released() {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    Ok(())
}
