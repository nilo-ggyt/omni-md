//! Image, single-mask, and paired-mask tiles create one, three, and five actual roles respectively,
//! without document or callback ownership.

use super::*;
use crate::native_surface::viewport_pixels::{MaskPairBlend, PixelPresentation};
use objc2_core_graphics::CGColor;

mod assignment;
mod cleanup;
mod validate;
use assignment::Assignment;
pub(super) use assignment::AssignmentPoint;

/// Neutral presentation shares original white/black endpoints; paired presentation retains exact colors
/// for this installation.
pub(super) struct Palette {
    white: CFRetained<CGColor>,
    black: CFRetained<CGColor>,
}
impl Palette {
    pub(super) fn new(current: &impl Fn() -> bool) -> Result<Self> {
        let white = checked(current, || CGColor::new_srgb(1.0, 1.0, 1.0, 1.0))?;
        let black = checked(current, || CGColor::new_srgb(0.0, 0.0, 0.0, 1.0))?;
        Ok(Self { white, black })
    }
}

struct PairedColors {
    background: CFRetained<CGColor>,
    foreground: CFRetained<CGColor>,
    secondary_foreground: CFRetained<CGColor>,
}
impl PairedColors {
    /// Construct endpoints with the exact byte/255 sRGB rule used by calibration; colors alone do not
    /// certify an encoding.
    fn new(
        background: [u8; 3],
        foreground: [u8; 3],
        blend: MaskPairBlend,
        current: &impl Fn() -> bool,
    ) -> Result<Self> {
        let make = |rgb: [u8; 3]| {
            checked(current, || {
                CGColor::new_srgb(
                    f64::from(rgb[0]) / 255.0,
                    f64::from(rgb[1]) / 255.0,
                    f64::from(rgb[2]) / 255.0,
                    1.0,
                )
            })
        };
        let background = make(background)?;
        let foreground = make(foreground)?;
        // Both modes decompose the same original RGBA pixels; retain the second actual blend color
        // identity with its sealed certificate.
        let secondary_foreground = match blend {
            MaskPairBlend::ForegroundThenForeground => foreground.clone(),
            MaskPairBlend::ForegroundThenBackground => background.clone(),
        };
        Ok(Self {
            background,
            foreground,
            secondary_foreground,
        })
    }
}

/// Record each actual contents installation separately so mid-install revocation returns only owned
/// relationships.
struct Payload {
    token: Rc<()>,
    image: Option<CFRetained<CGImage>>,
    secondary_image: Option<CFRetained<CGImage>>,
    presentation: PixelPresentation,
    paired_colors: Option<PairedColors>,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct Receipt {
    pub(super) tile: PixelTile,
    image: usize,
    secondary_image: Option<usize>,
    presentation: PixelPresentation,
}
impl Receipt {
    pub(super) fn new(
        tile: PixelTile,
        image: &CGImage,
        secondary_image: Option<&CGImage>,
        presentation: PixelPresentation,
    ) -> Self {
        Self {
            tile,
            image: std::ptr::from_ref(image) as usize,
            secondary_image: secondary_image.map(|value| std::ptr::from_ref(value) as usize),
            presentation,
        }
    }
}

pub(super) struct Layout {
    pub(super) rect: NSRect,
    pub(super) scale: f64,
    pub(super) presentation: PixelPresentation,
}

/// The container owns background; create only foreground/mask roles required by actual presentation,
/// preserving exact Native composition order.
pub(super) struct Slot {
    container: Retained<CALayer>,
    foreground: Option<Retained<CALayer>>,
    mask: Option<Retained<CALayer>>,
    second_foreground: Option<Retained<CALayer>>,
    second_mask: Option<Retained<CALayer>>,
    payload: RefCell<Option<Payload>>,
    installing: RefCell<Option<std::rc::Weak<()>>>,
    background_installed: Cell<bool>,
    foreground_color_installed: Cell<bool>,
    mask_installed: Cell<bool>,
    foreground_installed: Cell<bool>,
    second_color_installed: Cell<bool>,
    second_mask_installed: Cell<bool>,
    second_foreground_installed: Cell<bool>,
}
impl Slot {
    /// Create roles for this tile actual presentation only, without masks for images or a second group
    /// for single masks.
    pub(super) fn new(
        presentation: PixelPresentation,
        current: &impl Fn() -> bool,
    ) -> Result<Self> {
        let masked = presentation != PixelPresentation::Image;
        let paired = matches!(presentation, PixelPresentation::OpaquePairedMasks { .. });
        let layer = |needed| {
            if needed {
                checked(current, CALayer::new).map(Some)
            } else {
                Ok(None) // An absent role owns no Native object, idle backing, or close obligation.
            }
        };
        Ok(Self {
            container: checked(current, CALayer::new)?,
            foreground: layer(masked)?,
            mask: layer(masked)?,
            second_foreground: layer(paired)?,
            second_mask: layer(paired)?,
            payload: RefCell::new(None),
            installing: RefCell::new(None),
            background_installed: Cell::new(false),
            foreground_color_installed: Cell::new(false),
            mask_installed: Cell::new(false),
            foreground_installed: Cell::new(false),
            second_color_installed: Cell::new(false),
            second_mask_installed: Cell::new(false),
            second_foreground_installed: Cell::new(false),
        })
    }
    pub(super) fn container(&self) -> &Retained<CALayer> {
        &self.container
    }

    /// Observe actual constructed roles only, without instantiating idle masks merely for verification.
    pub(super) fn add_weaks(&self, values: &mut Vec<Weak<AnyObject>>) {
        values.push(layer_weak(&self.container));
        for layer in self.auxiliary_layers() {
            values.push(layer_weak(layer));
        }
    }

    fn auxiliary_layers(&self) -> impl Iterator<Item = &CALayer> {
        [
            self.foreground.as_deref(),
            self.mask.as_deref(),
            self.second_foreground.as_deref(),
            self.second_mask.as_deref(),
        ]
        .into_iter()
        .flatten()
    }

    fn primary(&self) -> (&CALayer, &CALayer) {
        // The sealed construction presentation determines required roles; a missing role is an assembly
        // error, without silent reconstruction.
        (
            self.foreground.as_deref().expect("遮罩表示拥有前景"),
            self.mask.as_deref().expect("遮罩表示拥有遮罩"),
        )
    }

    fn secondary(&self) -> (&CALayer, &CALayer) {
        // Only paired presentation may access this second group; images and single masks allocate no
        // such objects.
        (
            self.second_foreground
                .as_deref()
                .expect("双遮罩表示拥有第二前景"),
            self.second_mask.as_deref().expect("双遮罩表示拥有第二遮罩"),
        )
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn diagnostic_layers(&self) -> [Retained<CALayer>; 3] {
        [
            self.container.clone(),
            self.foreground.as_ref().expect("真实单遮罩控制").clone(),
            self.mask.as_ref().expect("真实单遮罩控制").clone(),
        ]
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn diagnostic_all_layers(&self) -> [Retained<CALayer>; 5] {
        [
            self.container.clone(),
            self.foreground.as_ref().expect("真实双遮罩控制").clone(),
            self.mask.as_ref().expect("真实双遮罩控制").clone(),
            self.second_foreground
                .as_ref()
                .expect("真实双遮罩控制")
                .clone(),
            self.second_mask.as_ref().expect("真实双遮罩控制").clone(),
        ]
    }

    pub(super) fn install(
        &self,
        image: &CFRetained<CGImage>,
        secondary_image: Option<&CFRetained<CGImage>>,
        layout: Layout,
        palette: &Palette,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        let assignment =
            Assignment::begin(self, image, secondary_image, layout.presentation, palette)?;
        self.install_assigned(image, secondary_image, layout, palette, current, assignment)
    }

    /// Only the gated diagnostic entry can revoke between the actual setter and persistent
    /// registration; the default product stores no hook.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn install_with_probe<'a>(
        &'a self,
        image: &CFRetained<CGImage>,
        secondary_image: Option<&CFRetained<CGImage>>,
        layout: Layout,
        palette: &Palette,
        current: &impl Fn() -> bool,
        probe: &'a dyn Fn(AssignmentPoint),
    ) -> Result<()> {
        let mut assignment =
            Assignment::begin(self, image, secondary_image, layout.presentation, palette)?;
        assignment.set_probe(probe);
        self.install_assigned(image, secondary_image, layout, palette, current, assignment)
    }

    // The independent assignment retains compensation claims before payload publication and each Native
    // effect.
    fn install_assigned(
        &self,
        image: &CFRetained<CGImage>,
        secondary_image: Option<&CFRetained<CGImage>>,
        layout: Layout,
        palette: &Palette,
        current: &impl Fn() -> bool,
        mut assignment: Assignment<'_>,
    ) -> Result<()> {
        let paired = matches!(
            layout.presentation,
            PixelPresentation::OpaquePairedMasks { .. }
        );
        if paired != secondary_image.is_some() {
            // A missing paired image or extra image for ordinary presentation is not a complete tile
            // delivered by the sealer.
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        configure(&self.container, layout.rect, layout.scale, false, current)?;
        let colors = match layout.presentation {
            PixelPresentation::OpaquePairedMasks {
                background,
                foreground,
                blend,
            } => Some(PairedColors::new(background, foreground, blend, current)?),
            PixelPresentation::Image | PixelPresentation::OpaqueNeutralMask => None,
        };
        // Acquire temporary strong claims before slot borrowing; later Native/current and actual CF
        // release cross no RefCell borrow.
        let color_claims = colors.as_ref().map(|value| {
            (
                value.background.clone(),
                value.foreground.clone(),
                value.secondary_foreground.clone(),
            )
        });
        assignment.capture_colors(colors.as_ref());
        assignment.publish_payload(colors)?;
        let local = NSRect::new(NSPoint::ZERO, layout.rect.size);
        match layout.presentation {
            PixelPresentation::Image => {
                self.install_contents(&self.container, image, false, current, &assignment)?
            }
            PixelPresentation::OpaqueNeutralMask => {
                self.install_mask_geometry(local, layout.scale, false, current)?;
                self.install_colors(&palette.white, &palette.black, None, current, &assignment)?;
                self.install_mask_relations(false, current, &assignment)?;
                self.install_contents(self.primary().1, image, false, current, &assignment)?;
            }
            PixelPresentation::OpaquePairedMasks { .. } => {
                let (background, foreground, secondary_foreground) = color_claims
                    .as_ref()
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
                self.install_mask_geometry(local, layout.scale, true, current)?;
                self.install_colors(
                    background,
                    foreground,
                    Some(secondary_foreground),
                    current,
                    &assignment,
                )?;
                self.install_mask_relations(true, current, &assignment)?;
                self.install_contents(self.primary().1, image, false, current, &assignment)?;
                self.install_contents(
                    self.secondary().1,
                    secondary_image.ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?,
                    true,
                    current,
                    &assignment,
                )?;
            }
        }
        assignment.commit()
    }

    fn install_mask_geometry(
        &self,
        rect: NSRect,
        scale: f64,
        paired: bool,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        configure(self.primary().0, rect, scale, false, current)?;
        configure(self.primary().1, rect, scale, true, current)?;
        if paired {
            // The second mask uses the same actual local geometry, without approximating original
            // pixels through scaling or displacement.
            configure(self.secondary().0, rect, scale, false, current)?;
            configure(self.secondary().1, rect, scale, true, current)?;
        }
        Ok(())
    }

    fn install_colors(
        &self,
        background: &CGColor,
        foreground: &CGColor,
        secondary_foreground: Option<&CGColor>,
        current: &impl Fn() -> bool,
        assignment: &Assignment<'_>,
    ) -> Result<()> {
        assignment.apply(AssignmentPoint::Background, current, || {
            self.container.setBackgroundColor(Some(background));
        })?;
        self.background_installed.set(true);
        assignment.apply(AssignmentPoint::ForegroundColor, current, || {
            self.primary().0.setBackgroundColor(Some(foreground));
        })?;
        self.foreground_color_installed.set(true);
        if let Some(secondary_foreground) = secondary_foreground {
            // Background remix and repeated foreground must use certified original identities rather
            // than RGB inference after installation.
            assignment.apply(AssignmentPoint::SecondColor, current, || {
                self.secondary()
                    .0
                    .setBackgroundColor(Some(secondary_foreground));
            })?;
            self.second_color_installed.set(true);
        }
        Ok(())
    }

    fn install_mask_relations(
        &self,
        paired: bool,
        current: &impl Fn() -> bool,
        assignment: &Assignment<'_>,
    ) -> Result<()> {
        assignment.apply(AssignmentPoint::Mask, current, || {
            // SAFETY: This slot exclusively owns its fixed standard CALayer, without View, document, or
            // business callback ownership.
            unsafe { self.primary().0.setMask(Some(self.primary().1)) };
        })?;
        self.mask_installed.set(true);
        if paired {
            assignment.apply(AssignmentPoint::SecondMask, current, || {
                // SAFETY: This slot also exclusively owns the second group; close and validation check
                // each actual identity/parent separately.
                unsafe { self.secondary().0.setMask(Some(self.secondary().1)) };
            })?;
            self.second_mask_installed.set(true);
        }
        let children = checked(current, || {
            if paired {
                NSArray::from_slice(&[self.primary().0, self.secondary().0])
            } else {
                NSArray::from_slice(&[self.primary().0])
            }
        })?;
        assignment.apply(AssignmentPoint::Children, current, || {
            // SAFETY: Retain original foregrounds in fixed order; the unpublished candidate container
            // cannot swap the two composition steps.
            unsafe { self.container.setSublayers(Some(&children)) };
        })?;
        self.foreground_installed.set(true);
        self.second_foreground_installed.set(paired);
        drop(children);
        Ok(())
    }

    // Register contents only after the actual setter and same-token checks; release displaced claims
    // outside the payload borrow.
    fn install_contents(
        &self,
        target: &CALayer,
        image: &CFRetained<CGImage>,
        secondary: bool,
        current: &impl Fn() -> bool,
        assignment: &Assignment<'_>,
    ) -> Result<()> {
        let claim = image.clone();
        assignment.apply(
            if secondary {
                AssignmentPoint::SecondaryImage
            } else {
                AssignmentPoint::Image
            },
            current,
            || {
                // SAFETY: The original sealed CGImage stays retained and is passed to CALayer as opaque
                // contents identity only.
                unsafe {
                    target.setContents(Some(&*std::ptr::from_ref(&**image).cast::<AnyObject>()))
                };
            },
        )?;
        let previous = {
            let mut payload = self.payload.borrow_mut();
            let value = payload
                .as_mut()
                .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
            if !assignment.owns(&value.token) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if secondary {
                value.secondary_image.replace(claim)
            } else {
                value.image.replace(claim)
            }
        };
        drop(previous); // The slot borrow ended before any final CF release.
        Ok(())
    }
}

fn configure(
    layer: &CALayer,
    rect: NSRect,
    scale: f64,
    mask_plane: bool,
    current: &impl Fn() -> bool,
) -> Result<()> {
    checked(current, || layer.setFrame(rect))?;
    checked(current, || {
        layer.setBounds(NSRect::new(NSPoint::ZERO, rect.size))
    })?;
    checked(current, || layer.setContentsScale(scale))?;
    // A8 masks consume alpha only and request single-channel storage; actual benefit still requires
    // real-window peak measurement.
    // Containers and solid colors retain RGBA8; the mask hint was byte-calibrated against the original
    // format and cannot alter source pixels.
    // SAFETY: Both QuartzCore format constants are static system values; the actual installation target
    // determines the role.
    checked(current, || unsafe {
        layer.setContentsFormat(if mask_plane {
            kCAContentsFormatGray8Uint
        } else {
            kCAContentsFormatRGBA8Uint
        })
    })?;
    // SAFETY: Solid colors and masks use the actual tile geometry delivered by the sealer.
    checked(current, || unsafe {
        layer.setContentsGravity(kCAGravityResize)
    })?;
    Ok(())
}
