//! Validate actual images, colors, order, masks, and parents whenever consuming a complete slot;
//! external replacement revokes original Ready.

use super::*;
use objc2::rc::autoreleasepool;

/// These scalar facts describe one sealed mask without retaining Native images or mutable slot borrows.
struct MaskIdentity {
    rect: NSRect,
    scale: f64,
    color: usize,
    image: usize,
}

impl Slot {
    /// Compare the original finite receipt against the actual graph after ending the payload borrow.
    pub(in super::super) fn validate(
        &self,
        receipt: &Receipt,
        rect: NSRect,
        scale: f64,
        palette: &Palette,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        let facts = {
            let payload = self
                .payload
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedTextViewError::Pending)?;
            payload.as_ref().map(|value| {
                (
                    value.presentation,
                    value
                        .image
                        .as_ref()
                        .map(|image| std::ptr::from_ref(&**image) as usize),
                    value
                        .secondary_image
                        .as_ref()
                        .map(|image| std::ptr::from_ref(&**image) as usize),
                    value.paired_colors.as_ref().map(|colors| {
                        (
                            std::ptr::from_ref(&*colors.background) as usize,
                            std::ptr::from_ref(&*colors.foreground) as usize,
                            std::ptr::from_ref(&*colors.secondary_foreground) as usize,
                        )
                    }),
                )
            })
        }; // Extract scalar identities only and end the slot borrow before Native/current calls.
        let Some((presentation, image, secondary, colors)) = facts else {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        };
        require(
            presentation == receipt.presentation
                && image == Some(receipt.image)
                && secondary == receipt.secondary_image,
            current,
        )?;
        let paired = matches!(
            receipt.presentation,
            PixelPresentation::OpaquePairedMasks { .. }
        );
        require(
            paired == receipt.secondary_image.is_some() && paired == colors.is_some(),
            current,
        )?;
        common(&self.container, rect, scale, false, current)?;
        let local = NSRect::new(NSPoint::ZERO, rect.size);
        no_mask(&self.container, current)?;
        match receipt.presentation {
            PixelPresentation::Image => {
                // Ordinary images have no mask roles; another presentation cannot use this payload to
                // access absent roles.
                require(
                    self.foreground.is_none() && self.second_foreground.is_none(),
                    current,
                )?;
                no_color(&self.container, current)?;
                no_children(&self.container, current)?;
                contents(&self.container, receipt.image, current)?;
            }
            PixelPresentation::OpaqueNeutralMask => {
                // The white/black path retains single-mask behavior without leaving a second image from
                // paired roles.
                color(
                    &self.container,
                    std::ptr::from_ref(&*palette.white) as usize,
                    current,
                )?;
                no_contents(&self.container, current)?;
                mask_group(
                    &self.container,
                    self.primary().0,
                    self.primary().1,
                    MaskIdentity {
                        rect: local,
                        scale,
                        color: std::ptr::from_ref(&*palette.black) as usize,
                        image: receipt.image,
                    },
                    current,
                )?;
                children(&self.container, &[self.primary().0], current)?;
                require(
                    self.second_foreground.is_none() && self.second_mask.is_none(),
                    current,
                )?;
            }
            PixelPresentation::OpaquePairedMasks { .. } => {
                let (background, foreground, secondary_foreground) =
                    colors.ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
                color(&self.container, background, current)?;
                no_contents(&self.container, current)?;
                mask_group(
                    &self.container,
                    self.primary().0,
                    self.primary().1,
                    MaskIdentity {
                        rect: local,
                        scale,
                        color: foreground,
                        image: receipt.image,
                    },
                    current,
                )?;
                mask_group(
                    &self.container,
                    self.secondary().0,
                    self.secondary().1,
                    MaskIdentity {
                        rect: local,
                        scale,
                        color: secondary_foreground,
                        image: receipt
                            .secondary_image
                            .ok_or(NativeMarkdownPagedTextViewError::Revoked)?,
                    },
                    current,
                )?;
                children(
                    &self.container,
                    &[self.primary().0, self.secondary().0],
                    current,
                )?;
            }
        }
        Ok(())
    }
}

/// Validate foreground/mask pairs separately, including the actual mask parent still pointing to its
/// original foreground.
fn mask_group(
    container: &CALayer,
    foreground: &CALayer,
    mask: &CALayer,
    identity: MaskIdentity,
    current: &impl Fn() -> bool,
) -> Result<()> {
    common(foreground, identity.rect, identity.scale, false, current)?;
    common(mask, identity.rect, identity.scale, true, current)?;
    color(foreground, identity.color, current)?;
    no_contents(foreground, current)?;
    no_children(foreground, current)?;
    no_children(mask, current)?;
    no_color(mask, current)?;
    no_mask(mask, current)?;
    let actual = checked(current, || foreground.mask())?;
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, mask));
    drop(actual);
    require(same, current)?;
    parent(foreground, container, current)?;
    parent(mask, foreground, current)?;
    contents(mask, identity.image, current)
}

fn children(layer: &CALayer, expected: &[&CALayer], current: &impl Fn() -> bool) -> Result<()> {
    // SAFETY: Compare type-correct original layer identities from the public array without business
    // messages to external layers.
    let actual = checked(current, || unsafe { layer.sublayers() })?;
    let same = if let Some(actual) = actual {
        if checked(current, || actual.len())? != expected.len() {
            false // A changed child count cannot be hidden by original objects still retained in private fields.
        } else {
            let mut same = true;
            for (index, expected) in expected.iter().enumerate() {
                let value = checked(current, || actual.objectAtIndex(index))?;
                same &= std::ptr::eq::<CALayer>(&*value, *expected);
            }
            same
        }
    } else {
        expected.is_empty()
    };
    require(same, current)
}

fn parent(layer: &CALayer, expected: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    let actual = checked(current, || layer.superlayer())?;
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::eq(value, expected));
    drop(actual);
    require(same, current)
}

fn contents(layer: &CALayer, expected: usize, current: &impl Fn() -> bool) -> Result<()> {
    // This read needs scalar identity only; end the +0 getter temporary claim with this consumption,
    // rather than retaining it in
    // an outer event pool that would keep validated pixels alive beyond a later document close.
    let same = autoreleasepool(|_| -> Result<bool> {
        // SAFETY: Compare original opaque CGImage identity without dereferencing external contents as a
        // CF image.
        let actual = checked(current, || unsafe { layer.contents() })?;
        Ok(actual
            .as_deref()
            .is_some_and(|value| std::ptr::from_ref(value) as usize == expected))
    })?;
    require(same, current)
}

fn require(fact: bool, current: &impl Fn() -> bool) -> Result<()> {
    if fact && current() {
        Ok(())
    } else {
        Err(NativeMarkdownPagedTextViewError::Revoked)
    }
}

fn common(
    layer: &CALayer,
    rect: NSRect,
    scale: f64,
    mask_plane: bool,
    current: &impl Fn() -> bool,
) -> Result<()> {
    // Hiding or fading an auxiliary layer changes actual composition; equal images and parents alone
    // cannot certify Ready.
    require(!checked(current, || layer.isHidden())?, current)?;
    require(checked(current, || layer.opacity())? == 1.0, current)?;
    // Actual Z order overrides the child-array order; background remix cannot accept reversed stacking
    // with equal array identities.
    require(checked(current, || layer.zPosition())? == 0.0, current)?;
    require(checked(current, || layer.frame())? == rect, current)?;
    require(
        checked(current, || layer.bounds())? == NSRect::new(NSPoint::ZERO, rect.size),
        current,
    )?;
    require(
        checked(current, || layer.contentsScale())? == scale,
        current,
    )?;
    let format = checked(current, || layer.contentsFormat())?;
    require(
        checked(current, || {
            // SAFETY: System constants are static; actual masks require the single-channel hint while
            // other roles still require RGBA8.
            // An externally changed hint revokes authority even while the original image identity
            // remains equal.
            unsafe {
                format.isEqualToString(if mask_plane {
                    kCAContentsFormatGray8Uint
                } else {
                    kCAContentsFormatRGBA8Uint
                })
            }
        })?,
        current,
    )?;
    let gravity = checked(current, || layer.contentsGravity())?;
    require(
        checked(current, || {
            // SAFETY: The resize constant consumes original actual tile geometry without accepting a
            // scaled successor screenshot.
            unsafe { gravity.isEqualToString(kCAGravityResize) }
        })?,
        current,
    )?;
    require(checked(current, || layer.delegate())?.is_none(), current)
}

fn color(layer: &CALayer, expected: usize, current: &impl Fn() -> bool) -> Result<()> {
    let actual = checked(current, || layer.backgroundColor())?;
    let same = actual
        .as_deref()
        .is_some_and(|value| std::ptr::from_ref(value) as usize == expected);
    drop(actual);
    require(same, current)
}
fn no_color(layer: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    require(
        checked(current, || layer.backgroundColor())?.is_none(),
        current,
    )
}
fn no_mask(layer: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    require(checked(current, || layer.mask())?.is_none(), current)
}
fn no_children(layer: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    // SAFETY: Test emptiness of the public standard-CALayer array without entering external drawing.
    let actual = checked(current, || unsafe { layer.sublayers() })?;
    let empty = actual.as_ref().is_none_or(|value| value.is_empty());
    drop(actual);
    require(empty, current)
}
fn no_contents(layer: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    // SAFETY: Test opaque getter emptiness without accessing any external object CF contents.
    require(
        checked(current, || unsafe { layer.contents() })?.is_none(),
        current,
    )
}
