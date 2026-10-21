//! Revocation still releases original relationships while preserving images, colors, masks, and parents
//! installed later by another owner.

use super::*;

impl Slot {
    /// Detach this slot's original Native relationships without destroying successor ownership.
    pub(in super::super) fn clear(&self, group: &CALayer, palette: &Palette) -> bool {
        let payload = self.payload.borrow_mut().take();
        let mut intact = true;
        if let Some(payload) = &payload {
            let target = match payload.presentation {
                PixelPresentation::Image => &*self.container,
                PixelPresentation::OpaqueNeutralMask
                | PixelPresentation::OpaquePairedMasks { .. } => self.primary().1,
            };
            intact &= clear_image(target, payload.image.as_deref());
            if let Some(second_mask) = self.second_mask.as_deref() {
                // A second provider exists only for actual paired masks; an empty claim cannot
                // authorize deleting another owner's later contents.
                intact &= clear_image(second_mask, payload.secondary_image.as_deref());
            }
        }
        if let (Some(foreground), Some(mask)) = (&self.foreground, &self.mask) {
            intact &= clear_mask(foreground, mask, &self.mask_installed);
            intact &= clear_parent(foreground, &self.container, &self.foreground_installed);
        }
        if let (Some(foreground), Some(mask)) = (&self.second_foreground, &self.second_mask) {
            intact &= clear_mask(foreground, mask, &self.second_mask_installed);
            intact &= clear_parent(
                foreground,
                &self.container,
                &self.second_foreground_installed,
            );
        }
        let colors = payload
            .as_ref()
            .and_then(|value| value.paired_colors.as_ref());
        let (background, foreground, secondary_foreground) = colors.map_or(
            (&*palette.white, &*palette.black, &*palette.black),
            |value| {
                (
                    &*value.background,
                    &*value.foreground,
                    &*value.secondary_foreground,
                )
            },
        );
        if self.background_installed.replace(false) {
            intact &= clear_color(&self.container, background);
        }
        if self.foreground_color_installed.replace(false) {
            intact &= clear_color(self.primary().0, foreground);
        }
        if self.second_color_installed.replace(false) {
            intact &= clear_color(self.secondary().0, secondary_foreground);
        }
        drop(payload); // Return original image/color claims only after checking each relationship, keeping CFRelease outside the slot borrow.

        let parent = self.container.superlayer();
        if parent
            .as_deref()
            .is_some_and(|value| !std::ptr::eq(value, group))
        {
            intact = false; // Preserve another owner's reparenting; close cannot reclaim the successor parent.
        }
        drop(parent);
        if parent_is(&self.container, group) {
            self.container.removeFromSuperlayer();
        }
        intact &= empty(&self.container);
        for layer in self.auxiliary_layers() {
            // Check only actual roles; absent roles own no Native resource, so never create empty
            // objects merely to prove release.
            intact &= empty(layer) && layer.superlayer().is_none();
        }
        intact
    }
}

/// Release only an image relationship whose original identity is still installed.
fn clear_image(layer: &CALayer, expected: Option<&CGImage>) -> bool {
    // Return only an identity result; the local pool ends the +0 getter's temporary claim so an old
    // provider cannot remain retained across frames.
    let same = objc2::rc::autoreleasepool(|_| {
        // SAFETY: The getter returns opaque identity only; compare addresses while the original strong
        // claim is alive, without dereferencing foreign objects.
        let actual = unsafe { layer.contents() };
        match expected {
            Some(expected) => actual.as_deref().is_some_and(|value| {
                std::ptr::from_ref(value).cast::<()>() == std::ptr::from_ref(expected).cast::<()>()
            }),
            None => actual.is_none(), // A role never installed by this owner cannot clear successor contents using an empty record.
        }
    });
    if same && expected.is_some() {
        // SAFETY: Clear only the slot's original image identity; preserve even equal-valued images with
        // a different owner identity.
        unsafe { layer.setContents(None) };
    }
    same
}

/// Consume this installation claim once and detach only the original mask.
fn clear_mask(foreground: &CALayer, mask: &CALayer, installed: &Cell<bool>) -> bool {
    if !installed.replace(false) {
        return true; // Without installing this relationship, this close operation has no authority over a foreign mask.
    }
    let actual = foreground.mask();
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, mask));
    drop(actual);
    if same {
        // SAFETY: The installed mask still has its original identity; detaching it cannot modify a
        // later foreign replacement.
        unsafe { foreground.setMask(None) };
    }
    same
}

/// Remove an original parent relationship while preserving external reparenting.
fn clear_parent(layer: &CALayer, parent: &CALayer, installed: &Cell<bool>) -> bool {
    if !installed.replace(false) {
        return true;
    }
    if parent_is(layer, parent) {
        layer.removeFromSuperlayer();
        true
    } else {
        false // Preserve the successor parent relationship while the original close receipt still reports revocation.
    }
}

/// Clear the actual installed color identity rather than comparing color values.
fn clear_color(layer: &CALayer, expected: &CGColor) -> bool {
    let actual = layer.backgroundColor();
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, expected));
    drop(actual);
    if same {
        layer.setBackgroundColor(None);
    }
    same // Clear only the color identity recorded at installation; equal-valued foreign colors do not belong to this owner.
}

/// Verify the actual terminal layer shell after owned relationships were detached.
fn empty(layer: &CALayer) -> bool {
    // SAFETY: Public getters test emptiness only, without sending CF or business messages to opaque
    // foreign contents.
    let contents = unsafe { layer.contents() };
    // SAFETY: An empty public CALayer children array contains no subgraph resource.
    let children = unsafe { layer.sublayers() };
    contents.is_none()
        && children.as_ref().is_none_or(|value| value.is_empty())
        && !layer.isHidden()
        && layer.opacity() == 1.0
        && layer.zPosition() == 0.0
        && layer.backgroundColor().is_none()
        && layer.mask().is_none()
        && layer.delegate().is_none()
}
